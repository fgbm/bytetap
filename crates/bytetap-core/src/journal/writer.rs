//! Писатель журнала: отдельная задача, неограниченный канал и групповая фиксация (FR-20).
//!
//! Решение и его обоснование — ADR `docs/dev/adr/0001-journal-writer.md`:
//!
//! - событие уходит в неограниченный [`mpsc`]-канал и не ждёт записи на диск: медленный диск
//!   не задерживает пересылку байтов (NFR-6);
//! - задача-писатель забирает всё накопленное (`recv` + `try_recv` в цикле), пишет строки,
//!   делает один `flush` и один `sync_data` на пачку (FR-20);
//! - пачка ограничена сверху [`MAX_BATCH_BYTES`], чтобы журнал не отставал на большую очередь;
//! - файл открыт в режиме дописывания и защищён от второй записи до конца сессии: на Unix —
//!   рекомендательной `flock`, на Windows — режимом совместного использования без
//!   `FILE_SHARE_WRITE`. Читать журнал при этом можно и во время сессии (`verify`, FR-29),
//!   а `note` в тот же файл не дописывается;
//! - заголовок `session` пишется синхронно до запуска задачи: файл без заголовка не появляется;
//! - после аварийного завершения в худшем случае теряется последняя неполная строка:
//!   записанные пачки уже прошли `sync_data`.

#[cfg(unix)]
use std::fs::TryLockError;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead as _, ErrorKind, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
#[cfg(feature = "test-hooks")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::journal::JournalFailureReason;
use crate::journal::event::{Event, Mode, Note, Session};

/// Верхняя граница одной пачки.
///
/// Медленный диск и большой поток событий не должны растягивать пачку на мегабайты:
/// остаток канала забирается следующей пачкой, потому что канал не теряет порядок.
pub const MAX_BATCH_BYTES: usize = 256 * 1024;

/// Сколько имён пробует [`Journal::create`], если файл с таким именем уже есть: `-2`…`-99`.
const MAX_NAME_ATTEMPTS: u32 = 99;

/// Тестовый хук: задержка перед `sync_data`, миллисекунды (M1-AC14).
///
/// Появляется только со сборкой feature `test-hooks` и существует ради одного теста:
/// писатель журнала замедляется, а ответ анализатору всё равно уходит сразу. В обычной
/// сборке поля нет, и писатель не может замедлиться из-за теста.
#[cfg(feature = "test-hooks")]
pub static TEST_SYNC_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// Сколько раз хук [`TEST_SYNC_DELAY_MS`] уже задержал фиксацию.
///
/// Тест ждёт первое срабатывание, а не гадает по времени: так проверка не зависит от
/// того, когда планировщик разбудил задачу-писателя.
#[cfg(feature = "test-hooks")]
pub static TEST_SYNC_DELAYS: AtomicU64 = AtomicU64::new(0);

/// Тестовый хук: после стольких событий писатель отказывает, как сломанный диск.
///
/// `u64::MAX` — хук выключен. Считаются события, дошедшие до пачки; заголовок `session`
/// пишется синхронно и не считается. Нужен тесту остановки сессии при отказе журнала
/// (ADR 0001, «Ошибка записи»).
#[cfg(feature = "test-hooks")]
pub static TEST_FAIL_AFTER_EVENTS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Сколько событий хук [`TEST_FAIL_AFTER_EVENTS`] уже пропустил в пачки.
#[cfg(feature = "test-hooks")]
static TEST_EVENTS_IN_BATCHES: AtomicU64 = AtomicU64::new(0);

/// Задержка перед фиксацией пачки, если тест её запросил.
#[cfg(feature = "test-hooks")]
fn test_hook_delay() {
    let millis = TEST_SYNC_DELAY_MS.load(Ordering::Relaxed);
    if millis > 0 {
        TEST_SYNC_DELAYS.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(millis));
    }
}

#[cfg(not(feature = "test-hooks"))]
fn test_hook_delay() {}

/// Отказ записи, запрошенный хуком [`TEST_FAIL_AFTER_EVENTS`].
#[cfg(feature = "test-hooks")]
fn test_hook_failure(events: usize) -> Option<io::Error> {
    let limit = TEST_FAIL_AFTER_EVENTS.load(Ordering::Relaxed);
    if limit == u64::MAX {
        return None;
    }
    let events = u64::try_from(events).unwrap_or(u64::MAX);
    let total = TEST_EVENTS_IN_BATCHES
        .fetch_add(events, Ordering::Relaxed)
        .saturating_add(events);
    (total > limit).then(|| io::Error::other("test hook: the journal device refused the write"))
}

#[cfg(not(feature = "test-hooks"))]
fn test_hook_failure(_events: usize) -> Option<io::Error> {
    None
}

/// Дескриптор писателя журнала.
///
/// Клонируется: событие можно писать из нескольких задач одной сессии.
#[derive(Clone)]
pub struct Journal {
    tx: mpsc::UnboundedSender<Command>,
    path: Arc<PathBuf>,
    failure: watch::Sender<Option<JournalFailureReason>>,
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
        if let Err(err) = lock_exclusive(&file).and_then(|()| write_header(&file, &session)) {
            // Созданный пустой файл без заголовка не остаётся: иначе он выглядел бы
            // журналом сессии, а `verify` отказывал бы на первой же строке (ADR 0001, п. 9).
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(err);
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let (failure, _) = watch::channel(None);
        let pending = Arc::new(AtomicUsize::new(0));
        let task = Arc::new(Mutex::new(None));
        let journal = Self {
            tx,
            path: Arc::new(path),
            failure: failure.clone(),
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

    /// Причина отказа писателя, если он уже остановился после ошибки записи.
    ///
    /// `record` после отказа возвращает `false`, а сессия берёт причину отсюда и
    /// останавливается (ADR 0001, «Ошибка записи»).
    #[must_use]
    pub fn failure(&self) -> Option<JournalFailureReason> {
        self.failure.borrow().as_ref().cloned()
    }

    /// Подписка на отказ писателя: `Some(причина)`, как только запись стала невозможна.
    ///
    /// Нужна сессии, чтобы остановиться сразу, а не ждать следующего события: если
    /// анализатор замолчал ровно в момент отказа, `record` больше никто не вызовет,
    /// и без этого сигнала сессия осталась бы висеть живой без журнала (ADR 0001,
    /// «Ошибка записи»).
    #[must_use]
    pub fn failed(&self) -> watch::Receiver<Option<JournalFailureReason>> {
        self.failure.subscribe()
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
            .failure()
            .unwrap_or_else(|| JournalFailureReason::from_message("писатель журнала остановлен"));
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

/// Причина предупреждения о папке журналов: код для интерфейса (FR-19).
///
/// Интерфейс переводит код, а не текст: сообщение ядра английское и для журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogDirWarningKind {
    /// Папка внутри OneDrive.
    Onedrive,
    /// Сетевой путь: UNC или сетевой диск.
    Network,
}

impl LogDirWarningKind {
    /// Код из контракта моста: `onedrive` | `network`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Onedrive => "onedrive",
            Self::Network => "network",
        }
    }
}

/// Причина предупреждения о папке журналов, если она уезжает за пределы машины.
///
/// Сетевой путь распознаётся по UNC-записи (`\\server\share`, `\\?\UNC\server\share`),
/// на Windows ещё и по типу тома: буква диска, подключённого по сети (`GetDriveTypeW`
/// == `DRIVE_REMOTE`). Расширенные записи `\\?\C:` и `\\.\` — локальные.
/// OneDrive ищется по компонентам пути (`OneDrive`, `OneDrive - Contoso`).
#[must_use]
pub fn log_dir_warning_kind(path: impl AsRef<Path>) -> Option<LogDirWarningKind> {
    let path = warning_path(path.as_ref());
    let path = path.as_path();
    if is_remote_path(path) {
        return Some(LogDirWarningKind::Network);
    }
    // Путь проверяется по тексту: на любой ОС разделители `/` и `\\` разбираются одинаково,
    // а Windows-путь, записанный на Unix, остаётся одной компонентой.
    let in_onedrive = path
        .to_string_lossy()
        .split(['/', '\\'])
        .any(|segment| segment.to_lowercase().starts_with("onedrive"));
    in_onedrive.then_some(LogDirWarningKind::Onedrive)
}

/// Предупреждение о папке журналов текстом для журнала и CLI.
///
/// `None` — папка обычная. Иначе текст для инженера: журналы могут содержать данные
/// пациентов и не должны синхронизироваться в OneDrive или лежать на сетевом диске.
#[must_use]
pub fn log_dir_warning(path: impl AsRef<Path>) -> Option<String> {
    let path = warning_path(path.as_ref());
    let shown = path.display();
    match log_dir_warning_kind(&path)? {
        LogDirWarningKind::Network => Some(format!(
            "папка журналов {shown} — сетевой путь: журналы с данными пациентов останутся на другом компьютере"
        )),
        LogDirWarningKind::Onedrive => Some(format!(
            "папка журналов {shown} внутри OneDrive: синхронизация может отправить журналы с данными пациентов в облако"
        )),
    }
}

/// Путь, по которому проверяется предупреждение о папке журналов.
///
/// Относительный путь сначала приводится к абсолютному: `--log-dir logs` или `.` при
/// работе из папки OneDrive должны давать предупреждение, а по компонентам `logs` его
/// не видно. Windows-записи (UNC, `\\?\`, буква диска) разбираются по тексту и остаются
/// как есть: на Unix такой путь выглядит относительным, и `absolute` приклеил бы к нему
/// текущую папку, сломав распознавание сети.
fn warning_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    let drive_letter = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let windows_style = text.starts_with(r"\\")
        || text.starts_with("//")
        || drive_letter
        || matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        );
    if windows_style {
        return path.to_path_buf();
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `true`, если папка лежит на другом компьютере.
///
/// `\\?\` и `\\.\` — локальные записи (`\\?\C:`, устройство); `\\?\UNC\` и `\\server\`
/// — сетевые. Проверка на тип тома нужна для буквы диска: `Z:` может быть сетевой.
fn is_remote_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let lower = text.to_lowercase();
    if lower.starts_with(r"\\?\unc\") || lower.starts_with("//?/unc/") {
        return true;
    }
    if text.starts_with(r"\\?\")
        || text.starts_with("//?/")
        || text.starts_with(r"\\.\")
        || text.starts_with("//./")
    {
        return false;
    }
    if text.starts_with(r"\\") || text.starts_with("//") {
        return true;
    }
    drive_is_remote(path)
}

/// `DRIVE_REMOTE` из `GetDriveTypeW`: том подключён по сети.
#[cfg(windows)]
const DRIVE_REMOTE: u32 = 4;

/// Буква диска в пути принадлежит сетевому тому (`GetDriveTypeW`).
#[cfg(windows)]
fn drive_is_remote(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt as _;

    // `GetDriveTypeW` хочет корень тома (`C:\`), а не полный путь.
    let Some(std::path::Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
        return false;
    }
    let mut root: Vec<u16> = prefix.as_os_str().encode_wide().collect();
    root.extend([u16::from(b'\\'), 0]);
    // SAFETY: строка с завершающим NUL живёт до конца вызова, других требований у функции нет.
    let kind = unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) };
    kind == DRIVE_REMOTE
}

#[cfg(not(windows))]
fn drive_is_remote(_path: &Path) -> bool {
    false
}

/// Дописывает событие `note` в конец готового журнала, не переписывая начало файла (FR-23).
///
/// Файл должен существовать и не должен быть занят идущей сессией: писатель не отдаёт
/// файл на запись до остановки. Обрезанный хвост (последняя строка без `\n`, обычно
/// после аварийного завершения) — отказ: комментарий не дописывается и файл не меняется,
/// иначе неполная строка слилась бы с комментарием и журнал перестал читаться.
pub fn append_note(path: impl AsRef<Path>, note: &Note) -> io::Result<()> {
    let mut file = open_for_note(path.as_ref())?;

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

/// Открывает журнал на дописывание и убеждается, что его не пишет идущая сессия.
///
/// Unix: сессия держит `flock`, и `try_lock` отвечает `WouldBlock`. Windows: сессия открыла
/// файл без `FILE_SHARE_WRITE`, поэтому открыть его на запись нельзя, и система отвечает
/// `ERROR_SHARING_VIOLATION` — по правилам Windows это и значит «файл занят на запись».
fn open_for_note(path: &Path) -> io::Result<File> {
    let file = match OpenOptions::new().append(true).read(true).open(path) {
        Ok(file) => file,
        Err(err) if is_sharing_violation(&err) => return Err(journal_busy_error()),
        Err(err) => return Err(err),
    };
    #[cfg(unix)]
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(journal_busy_error()),
        Err(TryLockError::Error(err)) => return Err(err),
    }
    Ok(file)
}

/// Ошибка «журнал занят»: `WouldBlock` — на обеих ОС.
///
/// Так `note` отвечает и на идущую сессию, и на файл, который держит другая программа:
/// для инженера это одно и то же — сейчас дописать нельзя.
fn journal_busy_error() -> io::Error {
    io::Error::new(
        ErrorKind::WouldBlock,
        "файл занят: идёт сессия или его держит другая программа",
    )
}

/// `ERROR_SHARING_VIOLATION` (32): файл открыт другой программой без `FILE_SHARE_WRITE`.
#[cfg(windows)]
fn is_sharing_violation(err: &io::Error) -> bool {
    err.raw_os_error() == Some(32)
}

#[cfg(not(windows))]
fn is_sharing_violation(_err: &io::Error) -> bool {
    false
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
    let mut options = OpenOptions::new();
    options.create_new(true).append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};

        // Другие процессы читают журнал на ходу — `verify`, Блокнот, будущий FR-29.
        // `FILE_SHARE_WRITE` не запрашивается, поэтому открыть файл на запись больше
        // нельзя, и это заменяет `LockFileEx`: тот на Windows запрещал бы и чтение,
        // то есть журнал не прочитать при живой сессии (ADR 0001, п. 7).
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    }
    options.open(path)
}

/// Запрещает вторую запись в файл журнала на всё время сессии.
///
/// Unix: рекомендательная `flock` — чтению она не мешает, а `append_note` получает
/// `WouldBlock`. Windows: блокировки нет, её роль играет [`create_new`] с `share_mode`
/// без `FILE_SHARE_WRITE` (ADR 0001, п. 7).
#[cfg(unix)]
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

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> io::Result<()> {
    Ok(())
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
    failure: watch::Sender<Option<JournalFailureReason>>,
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
            // в эту же пачку. `recv` после `close` доходит до конца очереди: `try_recv` здесь
            // не годится, он может остановиться раньше, чем очередь опустеет.
            rx.close();
            while let Some(command) = rx.recv().await {
                batch.push(command);
            }
        }

        let events = batch.events;
        // Ошибка сериализации одного события не выбрасывает всю пачку: остальные строки
        // пишутся, а причина запоминается и уходит ожидающим.
        let mut outcome: io::Result<()> = Ok(());
        if let Some(err) = batch.error.take() {
            remember(&failure, &err);
            outcome = Err(err);
        }
        let mut fatal = false;
        if let Some(err) = test_hook_failure(events) {
            remember(&failure, &err);
            outcome = Err(err);
            fatal = true;
        }
        if !fatal && !batch.bytes.is_empty() {
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
///
/// С feature `test-hooks` перед фиксацией может спать [`TEST_SYNC_DELAY_MS`]: так
/// проверяется, что пересылка ответа не ждёт журнал (M1-AC14).
async fn append(mut file: File, bytes: Vec<u8>) -> io::Result<File> {
    tokio::task::spawn_blocking(move || {
        file.write_all(&bytes)?;
        file.flush()?;
        test_hook_delay();
        file.sync_data()?;
        Ok::<File, io::Error>(file)
    })
    .await
    .map_err(|err| io::Error::other(format!("задача записи журнала упала: {err}")))?
}

/// Запоминает причину отказа и будит подписчиков [`Journal::failed`].
///
/// Первая причина остаётся: последующие отказы — следствие того же сбоя.
fn remember(failure: &watch::Sender<Option<JournalFailureReason>>, err: &io::Error) {
    failure.send_if_modified(|current| {
        if current.is_some() {
            false
        } else {
            *current = Some(JournalFailureReason::from_error(err));
            true
        }
    });
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

    /// Дописывает `note` в журнал после остановки сессии.
    ///
    /// На Windows дескриптор журнала освобождается вместе с задачей-писателем, но антивирус
    /// или индексатор могут задержать это на доли секунды. До пяти повторов по 100 мс
    /// проверку не ослабляют: отказ «файл занят» по-прежнему виден сразу, а ждём
    /// мы только освобождения файла после `shutdown`.
    fn append_note_after_stop(path: &Path, text: &str) {
        for attempt in 0..5 {
            match append_note(path, &note(text)) {
                Ok(()) => return,
                Err(err) if err.kind() == ErrorKind::WouldBlock && attempt < 4 => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => panic!("note после остановки: {err}"),
            }
        }
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
        journal.flush().await.unwrap();

        let err = append_note(&path, &note("во время сессии")).unwrap_err();
        assert!(err.to_string().contains("файл занят"), "{err}");

        // Читать журнал идущей сессии можно: на этом стоят `verify`, Блокнот и будущий FR-29.
        let outcome = read_all(&path).expect("журнал читается на ходу");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.lines.len(), 2, "заголовок и записанное событие");

        journal.shutdown().await.unwrap();
        append_note_after_stop(&path, "после остановки");

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
    fn log_dir_warning_kind_is_a_code_for_the_interface() {
        assert_eq!(log_dir_warning_kind("/home/user/Documents/bytetap"), None);
        assert_eq!(
            log_dir_warning_kind(r"C:\Users\user\OneDrive\Documents\bytetap"),
            Some(LogDirWarningKind::Onedrive)
        );
        assert_eq!(
            log_dir_warning_kind(r"\\fileserver\share\bytetap"),
            Some(LogDirWarningKind::Network)
        );
        assert_eq!(LogDirWarningKind::Onedrive.as_str(), "onedrive");
        assert_eq!(LogDirWarningKind::Network.as_str(), "network");
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

    #[test]
    fn log_dir_warning_tells_extended_paths_from_unc() {
        // `\\?\` и `\\.\` — расширенные записи этого же компьютера, а не сеть.
        assert!(log_dir_warning(r"\\?\C:\Users\user\Documents\bytetap").is_none());
        assert!(log_dir_warning(r"\\.\C:\Users\user\Documents\bytetap").is_none());
        // `\\?\UNC\` и `\\server\` — сетевые, в обеих записях разделителей.
        for path in [
            r"\\?\UNC\fileserver\share\bytetap",
            r"\\fileserver\share\bytetap",
            "//fileserver/share/bytetap",
        ] {
            let warning = log_dir_warning(path).unwrap_or_else(|| panic!("{path} — сетевой путь"));
            assert!(warning.contains("сетевой"), "{path}: {warning}");
        }
    }

    /// Текущая папка возвращается на место, даже если проверка упала.
    struct CwdGuard(PathBuf);

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn a_relative_log_dir_is_checked_after_resolving_it() {
        let base = temp_dir();
        let onedrive = base.path().join("OneDrive").join("bytetap");
        std::fs::create_dir_all(&onedrive).unwrap();
        let _guard = CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(&onedrive).unwrap();

        // `.` сама по себе не содержит компоненты OneDrive: предупреждение видно только
        // потому, что путь сначала приводится к абсолютному.
        log_dir_warning(".").expect("`.` внутри OneDrive — это папка в OneDrive");
    }

    #[test]
    fn a_windows_path_is_not_resolved_against_the_current_folder() {
        // На Unix `C:\…` и `\\server\…` выглядят относительными: их нельзя приклеивать
        // к текущей папке, иначе путь перестанет распознаваться по тексту.
        for path in [r"\\fileserver\share\bytetap", r"C:\Users\user\Documents"] {
            assert_eq!(warning_path(Path::new(path)), PathBuf::from(path));
        }
        let relative = Path::new("Documents").join("bytetap");
        assert_eq!(
            warning_path(&relative),
            std::path::absolute(&relative).unwrap()
        );
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
