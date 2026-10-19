//! Писатель журнала: отдельная задача, неограниченный канал и групповая фиксация (FR-20).
//!
//! Решение и его обоснование — ADR `docs/dev/adr/0001-journal-writer.md`:
//!
//! - событие уходит в неограниченный [`mpsc`]-канал и не ждёт записи на диск: медленный диск
//!   не задерживает пересылку байтов (NFR-6);
//! - задача-писатель забирает всё накопленное (`recv` + `try_recv` в цикле), пишет строки,
//!   делает один `flush` и один `sync_data` на пачку (FR-20);
//! - пачка ограничена сверху [`MAX_BATCH_BYTES`], чтобы журнал не отставал на большую очередь;
//! - файл открыт в режиме дописывания и удерживается под эксклюзивной блокировкой до конца
//!   сессии: пока сессия пишет, `note` в тот же файл не дописывается;
//! - заголовок `session` пишется синхронно до запуска задачи: файл без заголовка не появляется;
//! - после аварийного завершения в худшем случае теряется последняя неполная строка:
//!   записанные пачки уже прошли `sync_data`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead as _, ErrorKind, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::journal::event::{Event, Mode, Note, Session};

/// Верхняя граница одной пачки.
///
/// Медленный диск и большой поток событий не должны растягивать пачку на мегабайты:
/// остаток канала забирается следующей пачкой, потому что канал не теряет порядок.
pub const MAX_BATCH_BYTES: usize = 256 * 1024;

/// Сколько имён пробует [`Journal::create`], если файл с таким именем уже есть: `-2`…`-99`.
const MAX_NAME_ATTEMPTS: u32 = 99;

/// Дескриптор писателя журнала.
///
/// Клонируется: событие можно писать из нескольких задач одной сессии.
#[derive(Clone)]
pub struct Journal {
    tx: mpsc::UnboundedSender<Command>,
    path: Arc<PathBuf>,
    failure: Arc<Mutex<Option<String>>>,
    pending: Arc<AtomicUsize>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

enum Command {
    Record(Event),
    Flush(oneshot::Sender<io::Result<()>>),
    Stop(oneshot::Sender<io::Result<()>>),
}

impl Journal {
    /// Создаёт файл журнала, пишет заголовок сессии и запускает задачу-писателя.
    ///
    /// Вызывается внутри runtime `tokio`; без него возвращается ошибка, и файл не создаётся.
    /// Родительские папки создаются. Существующий файл не перезаписывается: занятое имя
    /// получает суффикс `-2`, `-3`… до [`MAX_NAME_ATTEMPTS`], после чего возвращается ошибка
    /// с путём.
    ///
    /// Заголовок `session` записывается и фиксируется на диске синхронно, до запуска задачи:
    /// если его не удалось записать, файл журнала не считается созданным.
    pub fn create(path: impl AsRef<Path>, session: Session) -> io::Result<Self> {
        // Проверка до создания файла: без runtime задачу-писателя запускать некуда.
        tokio::runtime::Handle::try_current()
            .map_err(|_| io::Error::other("журнал создаётся внутри runtime tokio"))?;

        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let (path, file) = open_new(&path)?;
        lock_exclusive(&file)?;
        write_header(&file, &session)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let failure = Arc::new(Mutex::new(None));
        let pending = Arc::new(AtomicUsize::new(0));
        let task = Arc::new(Mutex::new(None));
        let journal = Self {
            tx,
            path: Arc::new(path),
            failure: Arc::clone(&failure),
            pending: Arc::clone(&pending),
            task: Arc::clone(&task),
        };
        let handle = tokio::spawn(run(rx, file, failure, pending));
        if let Ok(mut slot) = task.lock() {
            *slot = Some(handle);
        } else {
            handle.abort();
            return Err(io::Error::other("писатель журнала не запустился"));
        }
        Ok(journal)
    }

    /// Путь к файлу журнала.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Кладёт событие в канал писателя. Никогда не ждёт диск.
    ///
    /// Возвращает `false`, если писатель уже остановлен: событие потеряно.
    pub fn record(&self, event: Event) -> bool {
        // Счётчик растёт до отправки, иначе писатель успел бы вычесть событие, ещё не
        // посчитанное здесь; при отказе отправки рост откатывается.
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(Command::Record(event)).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Сколько событий ждёт записи — метрика отставания журнала от сессии.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// Дожидается, пока всё отправленное окажется на диске.
    pub async fn flush(&self) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Flush(tx)).is_err() {
            return Err(self.stopped_error());
        }
        rx.await.map_err(|_| self.stopped_error())?
    }

    /// Дописывает накопленное, останавливает задачу-писателя и дожидается записи.
    ///
    /// Обязателен: без него последняя пачка может остаться в памяти, а блокировка файла
    /// снимается только при выгрузке задачи. Повторный вызов возвращает ошибку: писатель
    /// уже остановлен.
    pub async fn shutdown(&self) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Stop(tx)).is_err() {
            return Err(self.stopped_error());
        }
        let result = rx.await.map_err(|_| self.stopped_error())?;
        let task = self.task.lock().ok().and_then(|mut task| task.take());
        if let Some(task) = task {
            task.await
                .map_err(|err| io::Error::other(format!("задача-писатель упала: {err}")))?;
        }
        result
    }

    fn stopped_error(&self) -> io::Error {
        let cause = self
            .failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
            .unwrap_or_else(|| "писатель журнала остановлен".to_owned());
        io::Error::other(cause)
    }
}

/// Имя файла сессии: `bytetap-<YYYYMMDD-HHMMSS>-<режим>.jsonl` (FR-19).
///
/// Время — тот же момент, что и `started_at` заголовка: его берут один раз и передают
/// сюда и в [`Session::at`].
#[must_use]
pub fn session_file_name(started: DateTime<Local>, mode: Mode) -> String {
    format!(
        "bytetap-{}-{}.jsonl",
        started.format("%Y%m%d-%H%M%S"),
        mode.as_str()
    )
}

/// Папка журналов по умолчанию: `Documents/bytetap` в домашней папке пользователя (FR-19).
///
/// Это именно `<home>/Documents`, а не «Документы» из known folders Windows: при Known Folder
/// Move системная папка «Документы» переносится в OneDrive, а журналы с данными пациентов
/// не должны синхронизироваться в облако.
#[must_use]
pub fn default_log_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join("Documents").join("bytetap"))
}

/// Предупреждение о папке журналов, если она уезжает за пределы машины.
///
/// `None` — папка обычная. Иначе текст для инженера: журналы могут содержать данные
/// пациентов и не должны синхронизироваться в OneDrive или лежать на сетевом диске.
/// Путь проверяется по компонентам (`OneDrive`, `OneDrive - Contoso`) и по UNC-записи `\\…`.
#[must_use]
pub fn log_dir_warning(path: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") || text.starts_with("//") {
        return Some(format!(
            "папка журналов {} — сетевой путь: журналы с данными пациентов останутся на другом компьютере",
            path.display()
        ));
    }
    // Путь проверяется по тексту: на любой ОС разделители `/` и `\\` разбираются одинаково,
    // а Windows-путь, записанный на Unix, остаётся одной компонентой.
    let in_onedrive = text
        .split(['/', '\\'])
        .any(|segment| segment.to_lowercase().starts_with("onedrive"));
    in_onedrive.then(|| {
        format!(
            "папка журналов {} внутри OneDrive: синхронизация может отправить журналы с данными пациентов в облако",
            path.display()
        )
    })
}

/// Дописывает событие `note` в конец готового журнала, не переписывая начало файла (FR-23).
///
/// Файл должен существовать и не должен быть занят идущей сессией: писатель держит
/// эксклюзивную блокировку до остановки. Обрезанный хвост (последняя строка без `\n`, обычно
/// после аварийного завершения) — отказ: комментарий не дописывается и файл не меняется,
/// иначе неполная строка слилась бы с комментарием и журнал перестал читаться.
pub fn append_note(path: impl AsRef<Path>, note: &Note) -> io::Result<()> {
    let mut file = OpenOptions::new().append(true).read(true).open(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(io::Error::new(
                ErrorKind::WouldBlock,
                "журнал ещё пишется, дождитесь остановки сессии",
            ));
        }
        Err(TryLockError::Error(err)) => return Err(err),
    }

    let mut first = String::new();
    {
        let mut reader = io::BufReader::new(&file);
        reader.read_line(&mut first)?;
    }
    let header: Value = serde_json::from_str(first.trim_end()).map_err(|err| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("первая строка журнала не JSON: {err}"),
        )
    })?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "это не журнал сессии: первая строка не `session`",
        ));
    }

    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "журнал оканчивается неполной строкой (вероятно, после аварийного завершения); комментарий не добавлен, чтобы не повредить файл",
        ));
    }

    let mut line = serde_json::to_vec(&Event::Note(note.clone())).map_err(io::Error::other)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.sync_data()
}

/// Создаёт новый файл по пути, подбирая свободное имя.
fn open_new(path: &Path) -> io::Result<(PathBuf, File)> {
    for suffix in 1..=MAX_NAME_ATTEMPTS {
        let candidate = if suffix == 1 {
            path.to_path_buf()
        } else {
            suffixed(path, suffix)
        };
        match create_new(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        format!(
            "не удалось подобрать свободное имя журнала рядом с {}: заняты имена до -{MAX_NAME_ATTEMPTS}",
            path.display()
        ),
    ))
}

/// `<имя>-<суффикс>.<расширение>` — суффикс вставляется перед расширением.
fn suffixed(path: &Path, suffix: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(extension) => format!("{stem}-{suffix}.{}", extension.to_string_lossy()),
        None => format!("{stem}-{suffix}"),
    };
    path.with_file_name(name)
}

fn create_new(path: &Path) -> io::Result<File> {
    // `read(true)` нужен ради блокировки: LockFileEx на Windows требует GENERIC_READ или
    // GENERIC_WRITE, а режим дописывания сам по себе даёт только FILE_APPEND_DATA.
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .open(path)
}

/// Эксклюзивная блокировка файла на всё время сессии.
fn lock_exclusive(file: &File) -> io::Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(io::Error::new(
            ErrorKind::WouldBlock,
            "файл журнала занят другой программой",
        )),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// Пишет и фиксирует заголовок сессии до запуска задачи-писателя.
fn write_header(file: &File, session: &Session) -> io::Result<()> {
    let mut line =
        serde_json::to_vec(&Event::Session(session.clone())).map_err(io::Error::other)?;
    line.push(b'\n');
    let mut file = file;
    file.write_all(&line)?;
    file.sync_data()
}

/// Накопленная пачка: строки, ожидающие пачку сигналы и признак остановки.
#[derive(Default)]
struct Batch {
    bytes: Vec<u8>,
    events: usize,
    waiters: Vec<oneshot::Sender<io::Result<()>>>,
    stop: bool,
    error: Option<io::Error>,
}

impl Batch {
    fn push(&mut self, command: Command) {
        match command {
            Command::Record(event) => {
                let start = self.bytes.len();
                match serde_json::to_writer(&mut self.bytes, &event) {
                    Ok(()) => {
                        self.bytes.push(b'\n');
                        self.events += 1;
                    }
                    Err(err) => {
                        self.bytes.truncate(start);
                        self.error.get_or_insert_with(|| {
                            io::Error::other(format!("не удалось сериализовать событие: {err}"))
                        });
                    }
                }
            }
            Command::Flush(waiter) => self.waiters.push(waiter),
            Command::Stop(waiter) => {
                self.waiters.push(waiter);
                self.stop = true;
            }
        }
    }
}

/// Задача-писатель: забирает пачку, пишет, фиксирует.
async fn run(
    mut rx: mpsc::UnboundedReceiver<Command>,
    file: File,
    failure: Arc<Mutex<Option<String>>>,
    pending: Arc<AtomicUsize>,
) {
    let mut file = Some(file);
    loop {
        let mut batch = Batch::default();
        match rx.recv().await {
            Some(command) => batch.push(command),
            None => break,
        }
        while !batch.stop && batch.error.is_none() && batch.bytes.len() < MAX_BATCH_BYTES {
            match rx.try_recv() {
                Ok(command) => batch.push(command),
                Err(_) => break,
            }
        }

        if batch.stop {
            // Остановка: новые события не принимаются, а всё отправленное до неё дочитывается
            // в эту же пачку.
            rx.close();
            while let Ok(command) = rx.try_recv() {
                batch.push(command);
            }
        }

        let events = batch.events;
        // Ошибка сериализации одного события не выбрасывает всю пачку: остальные строки
        // пишутся, а причина запоминается и уходит ожидающим.
        let mut outcome = Ok(());
        if let Some(err) = batch.error.take() {
            remember(&failure, &err);
            outcome = Err(err);
        }
        let mut fatal = false;
        if !batch.bytes.is_empty() {
            let writer = file.take().expect("файл принадлежит задаче-писателю");
            match append(writer, std::mem::take(&mut batch.bytes)).await {
                Ok(writer) => file = Some(writer),
                Err(err) => {
                    remember(&failure, &err);
                    outcome = Err(err);
                    fatal = true;
                }
            }
        }
        pending.fetch_sub(events, Ordering::Relaxed);

        for waiter in batch.waiters {
            let _ = waiter.send(outcome.as_ref().map(|&()| ()).map_err(clone_error));
        }
        if fatal || batch.stop {
            return;
        }
    }
}

/// Пишет пачку, делает один `flush` и один `sync_data` (FR-20).
async fn append(mut file: File, bytes: Vec<u8>) -> io::Result<File> {
    tokio::task::spawn_blocking(move || {
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_data()?;
        Ok::<File, io::Error>(file)
    })
    .await
    .map_err(|err| io::Error::other(format!("задача записи журнала упала: {err}")))?
}

fn remember(failure: &Mutex<Option<String>>, err: &io::Error) {
    if let Ok(mut failure) = failure.lock()
        && failure.is_none()
    {
        *failure = Some(err.to_string());
    }
}

fn clone_error(err: &io::Error) -> io::Error {
    io::Error::new(err.kind(), err.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use chrono::{SecondsFormat, TimeZone as _};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::journal::event::{Autoresponder, Data, Dir, Host, SessionEnd, Stats};
    use crate::journal::reader::read_all;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().expect("временная папка")
    }

    fn session() -> Session {
        Session::new(
            Mode::TcpServer,
            Autoresponder::Hl7Mllp,
            json!({"listen": "0.0.0.0:5000"}),
            Host {
                os: "Linux".to_owned(),
            },
        )
    }

    fn note(text: &str) -> Note {
        Note {
            t: "2026-10-19T10:20:00.000000+03:00".to_owned(),
            mono_us: None,
            text: text.to_owned(),
        }
    }

    fn data(index: u64) -> Event {
        Event::Data(Data::new(
            "2026-10-19T10:15:03.000512+03:00",
            index,
            1,
            Dir::AnalyzerToLis,
            false,
            format!("MSH|{index}").as_bytes(),
        ))
    }

    #[tokio::test]
    async fn writes_every_event_as_a_line_in_order() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        for index in 0..500 {
            assert!(journal.record(data(index)));
        }
        journal.flush().await.unwrap();
        journal.shutdown().await.unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 501);
        assert_eq!(
            serde_json::from_str::<Event>(lines[0]).unwrap().kind(),
            "session"
        );
        for (offset, line) in lines[1..].iter().enumerate() {
            let event: Event = serde_json::from_str(line).unwrap();
            let Event::Data(data) = event else {
                panic!("ожидалось data, получено {line}");
            };
            assert_eq!(data.mono_us, offset as u64, "порядок строк");
        }
    }

    #[tokio::test]
    async fn writes_a_batch_larger_than_the_cap_completely() {
        let dir = temp_dir();
        let path = dir.path().join("big.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        // Заведомо больше MAX_BATCH_BYTES: пачка режется, но ни одно событие не теряется.
        let events = MAX_BATCH_BYTES / 16 + 100;
        for index in 0..events {
            journal.record(data(index as u64));
        }
        journal.flush().await.unwrap();
        journal.shutdown().await.unwrap();

        let outcome = read_all(&path).unwrap();
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.lines.len(), events + 1, "заголовок и все события");
    }

    #[tokio::test]
    async fn append_note_keeps_the_beginning_of_the_file() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        journal.record(data(1));
        journal.record(Event::SessionEnd(SessionEnd {
            t: "t".to_owned(),
            mono_us: 2,
            stats: Stats::default(),
        }));
        journal.flush().await.unwrap();
        journal.shutdown().await.unwrap();

        let before = std::fs::read(&path).unwrap();
        append_note(&path, &note("прогнали три пробы")).unwrap();
        let after = std::fs::read(&path).unwrap();

        assert!(after.starts_with(&before), "начало файла не переписано");
        assert_eq!(after[before.len()..].last(), Some(&b'\n'));
        let outcome = read_all(&path).unwrap();
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let last = outcome.lines.last().unwrap();
        assert_eq!(last.event.kind(), "note");
    }

    #[test]
    fn append_note_refuses_a_missing_file() {
        let dir = temp_dir();
        let path = dir.path().join("nope.jsonl");
        assert!(append_note(&path, &note("x")).is_err());
        assert!(!path.exists(), "новый файл не создаётся");
    }

    #[test]
    fn append_note_refuses_a_file_without_a_session_header() {
        let dir = temp_dir();
        let path = dir.path().join("notes.jsonl");
        std::fs::write(&path, "{\"type\":\"note\",\"t\":\"t\",\"text\":\"x\"}\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let err = append_note(&path, &note("комментарий")).unwrap_err();
        assert!(err.to_string().contains("не журнал сессии"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "файл не изменён");
    }

    #[tokio::test]
    async fn append_note_refuses_a_truncated_tail_and_keeps_the_file() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        journal.record(data(1));
        journal.flush().await.unwrap();
        journal.shutdown().await.unwrap();

        // Обрыв посреди строки: файл заканчивается без `\n`.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"note\",\"t\":\"2026-10-1");
        std::fs::write(&path, text.as_bytes()).unwrap();
        let before = std::fs::read(&path).unwrap();

        let err = append_note(&path, &note("комментарий")).unwrap_err();
        assert!(err.to_string().contains("неполной строкой"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "файл не изменён");
    }

    #[tokio::test]
    async fn append_note_refuses_a_journal_still_being_written() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        journal.record(data(1));

        let err = append_note(&path, &note("во время сессии")).unwrap_err();
        assert!(err.to_string().contains("ещё пишется"), "{err}");

        journal.flush().await.unwrap();
        journal.shutdown().await.unwrap();
        append_note(&path, &note("после остановки")).unwrap();

        let outcome = read_all(&path).unwrap();
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.lines.last().unwrap().event.kind(), "note");
        assert!(
            !outcome.lines.iter().any(
                |line| matches!(&line.event, Event::Note(note) if note.text == "во время сессии")
            ),
            "отказанный комментарий в журнал не попал"
        );
    }

    #[tokio::test]
    async fn existing_file_is_not_overwritten() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "старый журнал\n").unwrap();
        let journal = Journal::create(&path, session()).unwrap();
        assert_eq!(
            journal.path().file_name().unwrap(),
            "session-2.jsonl",
            "занятое имя получает суффикс"
        );
        journal.shutdown().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "старый журнал\n");
    }

    #[tokio::test]
    async fn occupied_names_are_tried_up_to_the_limit() {
        let dir = temp_dir();
        let path = dir.path().join("bytetap-20261019-101500-tcp_server.jsonl");
        for suffix in 1..=MAX_NAME_ATTEMPTS {
            let taken = if suffix == 1 {
                path.clone()
            } else {
                suffixed(&path, suffix)
            };
            std::fs::write(taken, "").unwrap();
        }
        let err = match Journal::create(&path, session()) {
            Ok(journal) => {
                journal.shutdown().await.unwrap();
                panic!("все 99 имён заняты, создание должно отказать");
            }
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(err.to_string().contains("tcp_server"), "{err}");
    }

    #[test]
    fn session_start_and_file_name_share_one_instant() {
        let started = Local.with_ymd_and_hms(2026, 10, 19, 10, 15, 0).unwrap();
        let session = Session::at(
            started,
            Mode::TcpServer,
            Autoresponder::Hl7Mllp,
            json!({}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        assert_eq!(
            session.started_at,
            started.to_rfc3339_opts(SecondsFormat::Micros, false)
        );
        assert!(session_file_name(started, Mode::TcpServer).contains("20261019-101500"));
    }

    #[test]
    fn file_name_matches_fr19() {
        let started = Local.with_ymd_and_hms(2026, 10, 19, 10, 15, 0).unwrap();
        assert_eq!(
            session_file_name(started, Mode::TcpServer),
            "bytetap-20261019-101500-tcp_server.jsonl"
        );
        assert_eq!(
            session_file_name(started, Mode::ComMonitor),
            "bytetap-20261019-101500-com_monitor.jsonl"
        );
    }

    #[test]
    fn suffix_goes_before_the_extension() {
        let path = Path::new("/logs/bytetap-20261019-101500-proxy.jsonl");
        assert_eq!(
            suffixed(path, 2),
            PathBuf::from("/logs/bytetap-20261019-101500-proxy-2.jsonl")
        );
    }

    #[test]
    fn default_log_dir_is_documents_bytetap() {
        let Some(dir) = default_log_dir() else {
            return;
        };
        assert!(dir.ends_with("Documents/bytetap"), "{}", dir.display());
    }

    #[cfg(windows)]
    #[test]
    fn default_log_dir_is_home_documents_bytetap_on_windows() {
        let home = dirs::home_dir().expect("домашняя папка");
        let dir = default_log_dir().expect("папка журналов");
        assert!(dir.starts_with(&home), "{}", dir.display());
        assert_eq!(dir.parent().unwrap().file_name().unwrap(), "Documents");
        assert_eq!(dir.file_name().unwrap(), "bytetap");
    }

    #[test]
    fn log_dir_warning_flags_onedrive_and_network_paths() {
        assert!(log_dir_warning("/home/user/Documents/bytetap").is_none());
        assert!(log_dir_warning(r"C:\Users\user\Documents\bytetap").is_none());

        let onedrive = log_dir_warning(r"C:\Users\user\OneDrive\Documents\bytetap").unwrap();
        assert!(onedrive.contains("OneDrive"), "{onedrive}");
        assert!(
            log_dir_warning(r"C:\Users\user\OneDrive - Contoso\Documents\bytetap").is_some(),
            "компонент, начинающийся с OneDrive"
        );
        let network = log_dir_warning(r"\\fileserver\share\bytetap").unwrap();
        assert!(network.contains("сетевой"), "{network}");
    }

    #[tokio::test]
    async fn flush_without_events_succeeds() {
        let dir = temp_dir();
        let path = dir.path().join("empty.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        journal.flush().await.unwrap();
        assert_eq!(journal.pending(), 0);
        journal.shutdown().await.unwrap();
        assert!(
            journal.flush().await.is_err(),
            "после остановки писатель закрыт"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_keeps_every_event_that_record_accepted() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        let journal = Journal::create(&path, session()).unwrap();
        let writer = journal.clone();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let producer = {
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move {
                for index in 0..50_000u64 {
                    if !writer.record(data(index)) {
                        break;
                    }
                    recorded.lock().unwrap().push(index);
                    if index % 256 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            })
        };

        // Даём производителю наполнить канал, затем останавливаем писателя параллельно.
        tokio::time::sleep(Duration::from_millis(20)).await;
        journal.shutdown().await.unwrap();
        producer.await.unwrap();

        let recorded = recorded.lock().unwrap().clone();
        assert!(!recorded.is_empty(), "производитель успел записать события");
        let outcome = read_all(&path).unwrap();
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let written: HashSet<u64> = outcome
            .lines
            .iter()
            .filter_map(|line| match &line.event {
                Event::Data(data) => Some(data.mono_us),
                _ => None,
            })
            .collect();
        for index in recorded {
            assert!(written.contains(&index), "событие {index} потеряно");
        }
        assert_eq!(journal.pending(), 0, "все принятые события разобраны");
    }
}
