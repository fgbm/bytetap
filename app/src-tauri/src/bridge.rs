//! Мост к ядру: старт и стоп сессии, состояние приложения и поток событий (FR-25, NFR-5).
//!
//! Всё, что можно проверить без окна, живёт здесь: [`Sessions`] хранит одну активную
//! сессию, [`start_session`] поднимает её на заданном адресе, а [`batch::pump`] пересылает
//! события в приёмник. Обработчики Tauri-команд в [`crate::commands`] — тонкие обёртки:
//! `Channel` в них превращается в [`FeedSink`], поэтому тесты подставляют простой приёмник.

use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytetap_core::journal::{Autoresponder, Stats, default_log_dir};
use bytetap_core::session::{
    Session, SessionConfig as CoreConfig, SessionHandle, journal_failure_reason,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::api::{SessionConfig, SessionStats, SessionSummary, StartInfo};
use crate::batch::{self, FLUSH_INTERVAL, FeedSink, PumpEnd};
use crate::error::{CommandError, ErrorCode};

/// Настройки запуска после проверки полей формы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Адрес и порт прослушивания; порт `0` — любой свободный.
    pub listen: SocketAddr,
    /// Профиль автоответчика.
    pub responder: Autoresponder,
    /// Папка журналов.
    pub log_dir: PathBuf,
    /// Модель анализатора для заголовка журнала.
    pub analyzer: Option<String>,
    /// Комментарий инженера для заголовка журнала.
    pub note: Option<String>,
}

impl SessionConfig {
    /// Проверяет поля формы и превращает их в настройки моста.
    ///
    /// Нереализованные режимы и автоответчики отвергаются здесь, до создания файла журнала:
    /// интерфейс может прислать их из заглушки, но сессия не должна начаться наполовину.
    pub fn into_bridge(self) -> Result<BridgeConfig, CommandError> {
        if self.mode != "tcp_server" {
            return Err(CommandError::new(
                ErrorCode::Io,
                format!(
                    "режим `{}` появится в следующих версиях: сейчас доступен только TCP-сервер",
                    self.mode
                ),
            ));
        }
        let host = self.listen_host.trim();
        let ip: Ipv4Addr = if host.is_empty() {
            Ipv4Addr::UNSPECIFIED
        } else {
            host.parse().map_err(|_| {
                CommandError::new(
                    ErrorCode::AddressUnavailable,
                    format!("`{host}` — не IPv4-адрес этого компьютера"),
                )
                .with_param("addr", host)
            })?
        };
        let responder = match self.responder.trim() {
            "hl7" => Autoresponder::Hl7Mllp,
            "none" => Autoresponder::None,
            other => {
                return Err(CommandError::new(
                    ErrorCode::Io,
                    format!("автоответчик `{other}` появится в следующих версиях"),
                ));
            }
        };
        let log_dir = match self.log_dir.trim() {
            "" => default_log_dir().ok_or_else(|| {
                CommandError::new(
                    ErrorCode::LogDir,
                    "не удалось определить папку журналов: укажите каталог явно",
                )
            })?,
            dir => PathBuf::from(dir),
        };
        Ok(BridgeConfig {
            listen: SocketAddr::from((ip, self.listen_port)),
            responder,
            log_dir,
            analyzer: non_empty(self.analyzer),
            note: non_empty(self.note),
        })
    }
}

/// Строка формы: пустая или из пробелов — «не указано».
fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Код ошибки старта по ошибке ядра.
///
/// Ядро сначала занимает адрес, потом создаёт журнал, поэтому всё, что не про адрес, —
/// про журнал: папка недоступна или файл не создан.
fn start_error(err: &std::io::Error, port: u16) -> CommandError {
    let code = CommandError::code_for(err.kind());
    match code {
        ErrorCode::PortInUse => {
            CommandError::new(code, err.to_string()).with_param("port", port.to_string())
        }
        ErrorCode::Io => CommandError::new(ErrorCode::Journal, err.to_string()),
        _ => CommandError::new(code, err.to_string()),
    }
}

/// Начало сессии из заголовка, который ядро уже записало в журнал.
fn started_at_of(path: &Path) -> Option<String> {
    let mut line = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_string(&mut line)
        .ok()?;
    let first = line.lines().next()?;
    let header: serde_json::Value = serde_json::from_str(first).ok()?;
    Some(header.get("started_at")?.as_str()?.to_owned())
}

/// Запущенная сессия вместе с задачей пересылки событий.
pub struct ActiveSession {
    handle: SessionHandle,
    pump: JoinHandle<PumpEnd>,
    sink: Arc<dyn FeedSink>,
    journal_path: PathBuf,
    /// То, что мост вернул на старт: переживает перезагрузку страницы (M2-AC1).
    info: StartInfo,
}

impl ActiveSession {
    /// Комментарий в идущую сессию (FR-23).
    fn note(&self, text: String) -> Result<(), CommandError> {
        self.handle
            .note(text)
            .map_err(|err| CommandError::from_io_with(ErrorCode::Journal, &err))
    }

    /// Останавливает сессию: журнал получает `session_end`, лента — `stopped`.
    ///
    /// Сначала ядро закрывает журнал, потом лента дописывает последние события и только
    /// затем уходит `stopped`: интерфейс не должен увидеть остановку раньше её событий.
    /// Отказ на финальной фиксации заканчивает ленту той же причиной, что и ошибка команды:
    /// интерфейс покажет баннер AC15, а не молча потеряет хвост журнала (NFR-5).
    async fn stop(self) -> Result<SessionSummary, CommandError> {
        let Self {
            handle,
            pump,
            sink,
            journal_path,
            ..
        } = self;
        let path = journal_path.to_string_lossy().into_owned();
        let result = handle.stop().await;
        // Задача пересылки заканчивается сама: сессия выгрузила отправителя событий.
        let end = pump.await.unwrap_or(PumpEnd::Closed);
        if end == PumpEnd::Closed {
            match result.as_ref().err().and_then(journal_failure_reason) {
                Some(reason) => sink.send(batch::journal_failed_batch(reason)),
                None => sink.send(batch::user_stopped_batch()),
            }
        }
        let stats = result.map_err(|err| match journal_failure_reason(&err) {
            Some(reason) => CommandError::new(ErrorCode::Journal, reason.to_owned()).with_param(
                "failure",
                crate::feed::JournalFailureCode::from_reason(reason).as_str(),
            ),
            None => CommandError::from_io(&err),
        })?;
        Ok(SessionSummary {
            stats: stats.into(),
            journal_path: path,
        })
    }
}

impl From<Stats> for SessionStats {
    fn from(stats: Stats) -> Self {
        Self {
            connections: stats.connections,
            bytes_analyzer_to_lis: stats.bytes_analyzer_to_lis,
            bytes_lis_to_analyzer: stats.bytes_lis_to_analyzer,
        }
    }
}

/// Запускает сессию на настроенном адресе и начинает пересылку событий.
///
/// Папка журналов создаётся до старта: недоступная папка — ошибка `log_dir`, а не
/// непонятный отказ журнала посреди работы (FR-19).
pub async fn start_session(
    config: BridgeConfig,
    sink: Arc<dyn FeedSink>,
    flush: Duration,
) -> Result<(ActiveSession, StartInfo), CommandError> {
    std::fs::create_dir_all(&config.log_dir).map_err(|err| {
        CommandError::from_io_with(ErrorCode::LogDir, &err)
            .with_param("dir", config.log_dir.to_string_lossy())
    })?;
    let mut core = CoreConfig::new(config.listen)
        .with_responder(config.responder)
        .with_log_dir(&config.log_dir);
    if let Some(analyzer) = config.analyzer.clone() {
        core = core.with_analyzer(analyzer);
    }
    if let Some(note) = config.note.clone() {
        core = core.with_note(note);
    }

    let (handle, feed) = Session::start_with_feed(core)
        .await
        .map_err(|err| start_error(&err, config.listen.port()))?;
    let journal_path = handle.journal_path().to_path_buf();
    let started_at =
        started_at_of(&journal_path).unwrap_or_else(bytetap_core::journal::now_rfc3339);
    let failure = handle.journal_failure();
    let pump = tokio::spawn(batch::pump(
        feed,
        failure,
        handle.counters(),
        Arc::clone(&sink),
        flush,
    ));
    let info = StartInfo {
        journal_path: journal_path.to_string_lossy().into_owned(),
        local_addr: handle.local_addr().to_string(),
        started_at,
    };
    Ok((
        ActiveSession {
            handle,
            pump,
            sink,
            journal_path,
            info: info.clone(),
        },
        info,
    ))
}

/// Что сейчас с записью.
#[derive(Default)]
enum Slot {
    /// Сессии нет.
    #[default]
    Idle,
    /// Сессия запускается или останавливается: второй старт уже запрещён.
    Busy,
    /// Сессия идёт.
    Running(Box<ActiveSession>),
}

/// Предел ожидания остановки при закрытии окна (NFR-5).
///
/// У писателя журнала свой предел в десять секунд; пятнадцать секунд здесь — запас на
/// очередь и на медленную машину. По истечении окно закрывается принудительно, но
/// пользователь об этом узнаёт: журнал мог быть дописан не полностью.
pub const STOP_WAIT: Duration = Duration::from_secs(15);

/// Одна активная сессия приложения.
///
/// Замок здесь обычный, [`std::sync::Mutex`]: он не держится через `await`, поэтому
/// команда и обработчик закрытия окна не могут заклинить друг друга. Пока сессия
/// запускается или останавливается, слот занят: повторный старт получает
/// [`ErrorCode::AlreadyRunning`], а не второй журнал. После отказа журнала мост
/// останавливает сессию сам и освобождает слот (NFR-5).
#[derive(Default, Clone)]
pub struct Sessions {
    slot: Arc<Mutex<Slot>>,
}

/// Сброс `Busy` при панике: замок не должен остаться занятым навсегда.
struct BusyGuard<'a> {
    slot: &'a Mutex<Slot>,
    armed: bool,
}

impl<'a> BusyGuard<'a> {
    fn new(slot: &'a Mutex<Slot>) -> Self {
        Self { slot, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Slot::Idle;
        }
    }
}

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Идёт ли запись. `true` и пока сессия останавливается: закрывать окно до конца
    /// остановки нельзя, иначе журнал не получит `session_end` (NFR-5).
    pub fn is_running(&self) -> bool {
        !matches!(*self.lock(), Slot::Idle)
    }

    /// Идущая сессия: то, что вернул её старт. `None` — записи нет.
    ///
    /// Нужно перезагруженной странице: она не получала `StartInfo`, но может предложить
    /// «Остановить» и не оставить сессию сиротой (M2-AC1).
    #[must_use]
    pub fn status(&self) -> Option<StartInfo> {
        match &*self.lock() {
            Slot::Running(session) => Some(session.info.clone()),
            _ => None,
        }
    }

    /// Запускает сессию, если другой нет.
    ///
    /// Слот, занятый завершившейся сессией (отказ журнала), освобождается: ядро уже
    /// ничего не пишет, и новый старт не должен получать `already_running`.
    ///
    /// # Errors
    ///
    /// `already_running`, если запись идёт или останавливается; ошибки старта ядра.
    pub async fn start(
        &self,
        config: SessionConfig,
        sink: Arc<dyn FeedSink>,
    ) -> Result<StartInfo, CommandError> {
        let stale = {
            let mut slot = self.lock();
            match std::mem::replace(&mut *slot, Slot::Busy) {
                Slot::Idle => None,
                Slot::Running(session) if session.handle.is_finished() => Some(session),
                other => {
                    *slot = other;
                    return Err(already_running());
                }
            }
        };
        let mut guard = BusyGuard::new(&self.slot);
        if let Some(session) = stale {
            // Дожидаемся ленты старой сессии в фоне: `stopped` должна уйти в её канал.
            tokio::spawn(async move {
                let _ = session.stop().await;
            });
        }
        let started = match config.into_bridge() {
            Ok(config) => start_session(config, sink, FLUSH_INTERVAL).await,
            Err(err) => Err(err),
        };
        let mut slot = self.lock();
        match started {
            Ok((session, info)) => {
                let failure = session.handle.journal_failure();
                *slot = Slot::Running(Box::new(session));
                guard.disarm();
                drop(slot);
                self.stop_after_failure(failure);
                Ok(info)
            }
            Err(err) => {
                *slot = Slot::Idle;
                guard.disarm();
                Err(err)
            }
        }
    }

    /// Останавливает сессию сама, как только писатель журнала откажет (NFR-5).
    ///
    /// Слот освобождается без участия интерфейса: перезагруженная страница не должна
    /// оставлять мост занятым. Вызов `stop_session` из интерфейса после этого безвреден.
    fn stop_after_failure(&self, mut failure: watch::Receiver<Option<String>>) {
        let sessions = self.clone();
        tokio::spawn(async move {
            loop {
                if failure.borrow_and_update().is_some() {
                    break;
                }
                if failure.changed().await.is_err() {
                    // Сессия закончилась штатно: останавливать нечего.
                    return;
                }
            }
            let _ = sessions.stop().await;
        });
    }

    /// Останавливает сессию и возвращает итоги.
    ///
    /// # Errors
    ///
    /// `journal`, если журнал отказал: записанное до сбоя сохранено, но `session_end`
    /// в файле может не быть. `io` — если записи не было.
    pub async fn stop(&self) -> Result<SessionSummary, CommandError> {
        let (session, mut guard) = {
            let mut slot = self.lock();
            match std::mem::replace(&mut *slot, Slot::Busy) {
                Slot::Running(session) => (session, BusyGuard::new(&self.slot)),
                other => {
                    let error = match other {
                        Slot::Busy => {
                            CommandError::new(ErrorCode::Io, "сессия ещё запускается: повторите")
                        }
                        _ => CommandError::new(ErrorCode::Io, "сессия не запущена"),
                    };
                    *slot = match other {
                        Slot::Busy => Slot::Busy,
                        _ => Slot::Idle,
                    };
                    return Err(error);
                }
            }
        };
        let result = session.stop().await;
        *self.lock() = Slot::Idle;
        guard.disarm();
        result
    }

    /// Останавливает сессию, если она идёт: закрытие окна во время записи (NFR-5).
    ///
    /// Если остановка уже начата командой «Стоп», окно ждёт её окончания, а не выходит
    /// посреди записи журнала. Дольше [`STOP_WAIT`] не ждёт: зависшая фиксация не должна
    /// держать окно вечно, и вызывающий закрывает окно принудительно с предупреждением.
    ///
    /// # Errors
    ///
    /// `io`: остановка не уложилась в [`STOP_WAIT`].
    pub async fn stop_if_running(&self) -> Result<(), CommandError> {
        let deadline = tokio::time::Instant::now() + STOP_WAIT;
        loop {
            let state = match &*self.lock() {
                Slot::Idle => 0,
                Slot::Running(_) => 1,
                Slot::Busy => 2,
            };
            match state {
                0 => return Ok(()),
                1 => {
                    let _ = self.stop().await;
                    return Ok(());
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CommandError::new(
                    ErrorCode::Io,
                    "сессия не остановилась за отведённое время",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Комментарий в идущую сессию (FR-23).
    ///
    /// # Errors
    ///
    /// `journal`, если сессия уже остановлена или журнал отказал.
    pub fn note(&self, text: impl Into<String>) -> Result<(), CommandError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(CommandError::new(
                ErrorCode::Io,
                "пустой комментарий не записывается",
            ));
        }
        match &*self.lock() {
            Slot::Running(session) => session.note(text),
            _ => Err(CommandError::new(
                ErrorCode::Io,
                "сессия не идёт: комментарий некуда добавить",
            )),
        }
    }
}

/// Ошибка повторного старта.
fn already_running() -> CommandError {
    CommandError::new(
        ErrorCode::AlreadyRunning,
        "запись уже идёт: сначала остановите текущую сессию",
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use bytetap_core::journal::{Event, read_all};
    use bytetap_testkit::emulator::AnalyzerEmulator;
    use bytetap_testkit::fixtures_dir;
    use tokio::io::AsyncWriteExt as _;

    use super::*;
    use crate::batch::test_sink::TestSink;
    use crate::feed::{FeedDir, FeedEvent, StopReason};
    use crate::journal::{JournalCache, open, read_page};

    fn config(dir: &Path, port: u16) -> SessionConfig {
        SessionConfig {
            mode: "tcp_server".to_owned(),
            listen_host: "127.0.0.1".to_owned(),
            listen_port: port,
            responder: "none".to_owned(),
            analyzer: " ANALYZER ".to_owned(),
            note: String::new(),
            log_dir: dir.to_string_lossy().into_owned(),
        }
    }

    async fn wait_for(mut ready: impl FnMut() -> bool) {
        for _ in 0..200 {
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("событие не дошло до интерфейса за две секунды");
    }

    #[tokio::test]
    async fn start_and_stop_record_a_session_and_end_the_feed() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес");
        assert_ne!(addr.port(), 0, "порт 0 заменён фактическим");
        assert!(Path::new(&info.journal_path).exists(), "журнал создан");
        assert!(sessions.is_running());

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("подключение анализатора");
        client
            .write_all(b"\x0bMSH|^~\\&\r\x1c\r")
            .await
            .expect("фрагмент");
        drop(client);
        wait_for(|| {
            sink.events()
                .iter()
                .any(|event| matches!(event, FeedEvent::Data { conn: 1, .. }))
        })
        .await;

        sessions.note("прогнали три пробы").expect("комментарий");
        wait_for(|| {
            sink.events()
                .iter()
                .any(|event| matches!(event, FeedEvent::Note { .. }))
        })
        .await;
        let summary = sessions.stop().await.expect("остановка");

        assert_eq!(summary.stats.connections, 1);
        assert_eq!(summary.stats.bytes_analyzer_to_lis, 12);
        assert_eq!(summary.stats.bytes_lis_to_analyzer, 0);
        assert!(!sessions.is_running());

        let events = sink.events();
        assert_eq!(
            events.last(),
            Some(&FeedEvent::Stopped {
                reason: StopReason::User,
                code: None,
                message: None,
            }),
            "лента заканчивается штатной остановкой"
        );

        let outcome = read_all(&summary.journal_path).expect("чтение журнала");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let kinds: Vec<&str> = outcome.lines.iter().map(|line| line.event.kind()).collect();
        assert_eq!(kinds.first(), Some(&"session"));
        assert_eq!(kinds.last(), Some(&"session_end"));
        assert!(kinds.contains(&"note"), "{kinds:?}");
        assert!(kinds.contains(&"open") && kinds.contains(&"data") && kinds.contains(&"close"));
        let Event::Session(header) = &outcome.lines[0].event else {
            panic!("первая строка — заголовок");
        };
        assert_eq!(header.mode.as_str(), "tcp_server");
        assert_eq!(header.analyzer.as_deref(), Some("ANALYZER"));
        assert_eq!(
            header.settings["listen"].as_str(),
            Some(info.local_addr.as_str())
        );

        // После остановки запись можно начать снова: слот свободен.
        let again = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("повторный старт");
        sessions.stop().await.expect("повторная остановка");
        assert_ne!(again.journal_path, info.journal_path, "новый файл журнала");
    }

    #[tokio::test]
    async fn a_second_start_while_running_is_refused() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");

        let error = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect_err("второй старт запрещён");
        assert_eq!(error.code, ErrorCode::AlreadyRunning);

        sessions.stop().await.expect("остановка");
        assert!(!sessions.is_running());
    }

    #[tokio::test]
    async fn a_busy_port_is_reported_as_port_in_use() {
        let _serial = serial().await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("занятый порт");
        let port = listener.local_addr().expect("адрес").port();
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let error = sessions
            .start(config(dir.path(), port), sink.clone())
            .await
            .expect_err("порт занят");
        assert_eq!(error.code, ErrorCode::PortInUse);
        assert_eq!(
            error.params.as_ref().and_then(|params| params.get("port")),
            Some(&port.to_string())
        );
        assert!(
            std::fs::read_dir(dir.path())
                .expect("папка журналов")
                .next()
                .is_none(),
            "файл журнала не создан"
        );

        // Неудачный старт не занимает слот: следующая попытка проходит.
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("свободный порт");
        assert!(!info.local_addr.is_empty());
        sessions.stop().await.expect("остановка");
    }

    #[tokio::test]
    async fn an_unimplemented_responder_does_not_start_a_session() {
        let _serial = serial().await;
        let sessions = Sessions::default();
        let mut cfg = config(Path::new("/tmp"), 0);
        cfg.responder = "astm".to_owned();
        let error = sessions
            .start(cfg, Arc::new(TestSink::default()))
            .await
            .expect_err("профиль M3");
        assert_eq!(error.code, ErrorCode::Io);
        assert!(!sessions.is_running(), "слот освобождён после отказа");
    }

    #[test]
    fn form_fields_are_validated_before_start() {
        let valid = config(Path::new("/tmp/bytetap"), 5000)
            .into_bridge()
            .expect("поля");
        assert_eq!(valid.listen, "127.0.0.1:5000".parse().unwrap());
        assert_eq!(valid.responder, Autoresponder::None);
        assert_eq!(valid.analyzer.as_deref(), Some("ANALYZER"));
        assert_eq!(valid.note, None);

        let mut all = config(Path::new("/tmp/bytetap"), 5000);
        all.listen_host = String::new();
        all.responder = "hl7".to_owned();
        let all = all.into_bridge().expect("все интерфейсы");
        assert_eq!(all.listen, "0.0.0.0:5000".parse().unwrap());
        assert_eq!(all.responder, Autoresponder::Hl7Mllp);

        let mut wrong_host = config(Path::new("/tmp/bytetap"), 5000);
        wrong_host.listen_host = "localhost".to_owned();
        assert_eq!(
            wrong_host.into_bridge().expect_err("не IPv4").code,
            ErrorCode::AddressUnavailable
        );

        let mut proxy = config(Path::new("/tmp/bytetap"), 5000);
        proxy.mode = "proxy".to_owned();
        assert!(proxy.into_bridge().is_err(), "режим M4 не стартует");
    }

    #[tokio::test]
    async fn closing_the_window_stops_a_running_session() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");

        sessions
            .stop_if_running()
            .await
            .expect("остановка при закрытии");

        assert!(!sessions.is_running());
        let outcome = read_all(&info.journal_path).expect("чтение журнала");
        assert_eq!(
            outcome.lines.last().map(|line| line.event.kind()),
            Some("session_end"),
            "журнал закрыт штатно, а не оборван"
        );
        sessions
            .stop_if_running()
            .await
            .expect("без сессии ничего не делает");
    }

    /// Сквозная проверка M2: запись через мост, затем чтение того же журнала командами.
    ///
    /// Эмулятор анализатора шлёт синтетические HL7/MLLP-сообщения, лента получает `open`,
    /// `data` обоих направлений (ответ автоответчика — `synthetic`), `close` и `stopped`,
    /// а `open_journal` с `read_journal` возвращают те же события из файла.
    #[tokio::test]
    async fn a_recorded_session_is_read_back_through_the_journal_commands() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let mut cfg = config(dir.path(), 0);
        cfg.responder = "hl7".to_owned();
        cfg.analyzer = "ANALYZER".to_owned();
        cfg.note = "три пробы".to_owned();

        let info = sessions.start(cfg, sink.clone()).await.expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес слушателя");
        let messages: Vec<Vec<u8>> = ["analyzer_oru_r01.hl7", "analyzer_qry_q02.hl7"]
            .iter()
            .map(|name| {
                let path = fixtures_dir().join("hl7").join(name);
                std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            })
            .collect();
        let report = AnalyzerEmulator::new(addr)
            .with_messages(messages)
            .run()
            .await
            .expect("обмен с анализатором");
        assert_eq!(
            report.acks.len(),
            2,
            "автоответчик подтвердил оба сообщения"
        );

        let summary = sessions.stop().await.expect("остановка");
        assert_eq!(summary.stats.connections, 1);
        assert!(summary.stats.bytes_analyzer_to_lis > 0);
        assert!(summary.stats.bytes_lis_to_analyzer > 0);

        let events = sink.events();
        let count = |predicate: fn(&FeedEvent) -> bool| {
            events.iter().filter(|event| predicate(event)).count()
        };
        assert_eq!(count(|event| matches!(event, FeedEvent::Open { .. })), 1);
        assert!(
            count(|event| matches!(
                event,
                FeedEvent::Data {
                    dir: FeedDir::A2l,
                    synthetic: false,
                    ..
                }
            )) >= 2
        );
        assert!(
            count(|event| matches!(
                event,
                FeedEvent::Data {
                    dir: FeedDir::L2a,
                    synthetic: true,
                    ..
                }
            )) >= 2
        );
        assert_eq!(count(|event| matches!(event, FeedEvent::Close { .. })), 1);
        assert_eq!(
            events.last(),
            Some(&FeedEvent::Stopped {
                reason: StopReason::User,
                code: None,
                message: None,
            })
        );

        let path = Path::new(&summary.journal_path);
        let cache = JournalCache::default();
        let journal = open(path, &cache).await.expect("сводка журнала");
        assert_eq!(journal.header["analyzer"].as_str(), Some("ANALYZER"));
        assert_eq!(journal.header["note"].as_str(), Some("три пробы"));
        assert_eq!(
            journal.stats, summary.stats,
            "итоги журнала и сессии совпали"
        );
        assert_eq!(journal.density.a2l.len(), crate::journal::DENSITY_BUCKETS);

        let page = read_page(path, 0, 10_000, &cache)
            .await
            .expect("страница журнала");
        assert_eq!(page.len(), journal.events, "журнал читается целиком");
        assert_eq!(
            page.iter()
                .filter(|event| matches!(event, FeedEvent::Data { .. }))
                .count(),
            count(|event| matches!(event, FeedEvent::Data { .. })),
            "лента и журнал содержат одни и те же фрагменты"
        );
        assert!(matches!(page.first(), Some(FeedEvent::Open { .. })));
        assert!(matches!(page.last(), Some(FeedEvent::Close { .. })));
    }

    #[test]
    fn started_at_is_read_from_the_header() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("empty.jsonl");
        let mut file = std::fs::File::create(&path).expect("файл");
        file.write_all(b"not a journal\n").expect("запись");
        assert!(started_at_of(&path).is_none());
        assert!(started_at_of(&dir.path().join("missing.jsonl")).is_none());
    }

    /// Хуки ядра — глобальные статики: тесты моста идут по одному, включая те,
    /// что хуки включают. Иначе отказ журнала одного теста ломает соседний.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        SERIAL.lock().await
    }

    /// Отказ журнала освобождает слот без участия интерфейса (NFR-5).
    ///
    /// `stop_session` после отказа возвращает ошибку, но и без него новый старт проходит:
    /// мост сам остановил сессию в фоне и освободил слот.
    #[tokio::test]
    async fn a_journal_failure_frees_the_slot_and_a_new_start_passes() {
        use std::sync::atomic::Ordering;

        use bytetap_core::journal::writer::TEST_FAIL_AFTER_EVENTS;

        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес");

        // Писатель отказывает на первой же пачке событий.
        TEST_FAIL_AFTER_EVENTS.store(0, Ordering::Relaxed);
        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("анализатор");
        client
            .write_all(b"\x0bMSH|^~\\&\r\x1c\r")
            .await
            .expect("фрагмент");
        drop(client);

        wait_for(|| {
            sink.events().iter().any(|event| {
                matches!(
                    event,
                    FeedEvent::Stopped {
                        reason: StopReason::JournalFailure,
                        ..
                    }
                )
            })
        })
        .await;
        for _ in 0..200 {
            if !sessions.is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);

        assert!(!sessions.is_running(), "мост освободил слот сам");
        assert!(sessions.status().is_none());
        // Вызов интерфейса после отказа безвреден: ошибка ожидаема.
        assert!(sessions.stop().await.is_err(), "останавливать уже нечего");

        let again = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("новый старт после отказа");
        assert_ne!(again.journal_path, info.journal_path, "новый файл журнала");
        sessions.stop().await.expect("остановка");
    }

    /// Отказ на финальной фиксации: лента заканчивается `journal_failure`, команда — ошибкой.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_flush_at_the_stop_ends_the_feed_with_a_journal_failure() {
        use std::sync::atomic::Ordering;

        use bytetap_core::journal::writer::{TEST_SYNC_DELAY_MS, TEST_SYNC_DELAYS};
        use bytetap_core::session::TEST_JOURNAL_SHUTDOWN_MS;

        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let mut cfg = config(dir.path(), 0);
        cfg.responder = "hl7".to_owned();
        let info = sessions.start(cfg, sink.clone()).await.expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес");
        let message = std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7"))
            .expect("фикстура analyzer_oru_r01.hl7");

        TEST_SYNC_DELAYS.store(0, Ordering::Relaxed);
        TEST_SYNC_DELAY_MS.store(1_000, Ordering::Relaxed);
        TEST_JOURNAL_SHUTDOWN_MS.store(50, Ordering::Relaxed);

        AnalyzerEmulator::new(addr)
            .with_messages([message])
            .run()
            .await
            .expect("обмен с анализатором");
        let waiting = std::time::Instant::now();
        while TEST_SYNC_DELAYS.load(Ordering::Relaxed) == 0 {
            assert!(
                waiting.elapsed() < Duration::from_secs(5),
                "писатель журнала так и не был замедлен"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let error = sessions
            .stop()
            .await
            .expect_err("остановка не дождалась журнала");
        TEST_SYNC_DELAY_MS.store(0, Ordering::Relaxed);
        TEST_JOURNAL_SHUTDOWN_MS.store(0, Ordering::Relaxed);

        assert_eq!(error.code, ErrorCode::Journal);
        assert_eq!(
            error
                .params
                .as_ref()
                .and_then(|params| params.get("failure")),
            Some(&"timeout".to_owned())
        );
        assert!(!sessions.is_running(), "слот освобождён");
        let events = sink.events();
        assert!(
            matches!(
                events.last(),
                Some(FeedEvent::Stopped {
                    reason: StopReason::JournalFailure,
                    code: Some(crate::feed::JournalFailureCode::Timeout),
                    ..
                })
            ),
            "лента закончилась отказом журнала: {:?}",
            events.last()
        );
    }
}
