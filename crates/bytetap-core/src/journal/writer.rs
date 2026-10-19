//! Писатель журнала: отдельная задача, неограниченный канал и групповая фиксация (FR-20).
//!
//! Решение и его обоснование — ADR `docs/dev/adr/0001-journal-writer.md`:
//!
//! - событие уходит в неограниченный [`mpsc`]-канал и не ждёт записи на диск: медленный диск
//!   не задерживает пересылку байтов (NFR-6);
//! - задача-писатель забирает всё накопленное (`recv` + `try_recv` в цикле), пишет строки,
//!   делает один `flush` и один `sync_data` на пачку (FR-20);
//! - пачка ограничена сверху [`MAX_BATCH_BYTES`], чтобы журнал не отставал на большую очередь;
//! - после аварийного завершения в худшем случае теряется последняя неполная строка:
//!   записанные пачки уже прошли `sync_data`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use tokio::sync::{mpsc, oneshot};

use crate::journal::event::{Event, Mode, Note, Session};

/// Верхняя граница одной пачки.
///
/// Медленный диск и большой поток событий не должны растягивать пачку на мегабайты:
/// остаток канала забирается следующей пачкой, потому что канал не теряет порядок.
pub const MAX_BATCH_BYTES: usize = 256 * 1024;

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
    /// Вызывается внутри runtime `tokio`. Родительские папки создаются, существующий файл
    /// не перезаписывается: имя сессии должно быть свободно.
    pub fn create(path: impl AsRef<Path>, session: Session) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;

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
        if !journal.record(Event::Session(session)) {
            return Err(io::Error::other("писатель журнала не запустился"));
        }
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
        let sent = self.tx.send(Command::Record(event)).is_ok();
        if sent {
            self.pending.fetch_add(1, Ordering::Relaxed);
        }
        sent
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
    /// Повторный вызов возвращает ошибку: писатель уже остановлен.
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
#[must_use]
pub fn session_file_name(started: DateTime<Local>, mode: Mode) -> String {
    format!(
        "bytetap-{}-{}.jsonl",
        started.format("%Y%m%d-%H%M%S"),
        mode.as_str()
    )
}

/// Папка журналов по умолчанию: `Documents/bytetap` (FR-19).
///
/// На Windows это `%USERPROFILE%\Documents\bytetap`.
#[must_use]
pub fn default_log_dir() -> Option<PathBuf> {
    dirs::document_dir().map(|documents| documents.join("bytetap"))
}

/// Дописывает событие `note` в конец готового журнала, не переписывая начало файла (FR-23).
///
/// Файл должен существовать: комментарий дописывается к журналу сессии, а не создаёт новый.
pub fn append_note(path: impl AsRef<Path>, note: &Note) -> io::Result<()> {
    let mut file = OpenOptions::new().append(true).open(path)?;
    serde_json::to_writer(&mut file, &Event::Note(note.clone())).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.flush()?;
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

        let events = batch.events;
        let result = if let Some(err) = batch.error.take() {
            Err(err)
        } else if batch.bytes.is_empty() {
            Ok(())
        } else {
            let writer = file.take().expect("файл принадлежит задаче-писателю");
            match append(writer, std::mem::take(&mut batch.bytes)).await {
                Ok(writer) => {
                    file = Some(writer);
                    Ok(())
                }
                Err(err) => {
                    if let Ok(mut failure) = failure.lock() {
                        *failure = Some(err.to_string());
                    }
                    Err(err)
                }
            }
        };

        if result.is_ok() {
            pending.fetch_sub(events, Ordering::Relaxed);
        }
        for waiter in batch.waiters {
            let _ = waiter.send(result.as_ref().map(|&()| ()).map_err(clone_error));
        }
        if result.is_err() || batch.stop {
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

fn clone_error(err: &io::Error) -> io::Error {
    io::Error::new(err.kind(), err.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
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
        append_note(
            &path,
            &Note {
                t: "2026-10-19T10:20:00.000000+03:00".to_owned(),
                mono_us: None,
                text: "прогнали три пробы".to_owned(),
            },
        )
        .unwrap();
        let after = std::fs::read(&path).unwrap();

        assert!(after.starts_with(&before), "начало файла не переписано");
        assert_eq!(after[before.len()..].last(), Some(&b'\n'));
        let outcome = read_all(&path).unwrap();
        let last = outcome.lines.last().unwrap();
        assert_eq!(last.event.kind(), "note");
    }

    #[test]
    fn append_note_refuses_a_missing_file() {
        let dir = temp_dir();
        let path = dir.path().join("nope.jsonl");
        let note = Note {
            t: "t".to_owned(),
            mono_us: None,
            text: "x".to_owned(),
        };
        assert!(append_note(&path, &note).is_err());
        assert!(!path.exists(), "новый файл не создаётся");
    }

    #[tokio::test]
    async fn existing_file_is_not_overwritten() {
        let dir = temp_dir();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "старый журнал\n").unwrap();
        assert!(Journal::create(&path, session()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "старый журнал\n");
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
    fn default_log_dir_ends_with_bytetap() {
        let Some(dir) = default_log_dir() else {
            return;
        };
        assert_eq!(dir.file_name().unwrap(), "bytetap");
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
}
