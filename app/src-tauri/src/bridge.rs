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
    async fn stop(self) -> Result<SessionSummary, CommandError> {
        let Self {
            handle,
            pump,
            sink,
            journal_path,
        } = self;
        let path = journal_path.to_string_lossy().into_owned();
        let result = handle.stop().await;
        // Задача пересылки заканчивается сама: сессия выгрузила отправителя событий.
        let end = pump.await.unwrap_or(PumpEnd::Closed);
        if end == PumpEnd::Closed {
            sink.send(batch::user_stopped_batch());
        }
        let stats = result.map_err(|err| match journal_failure_reason(&err) {
            Some(reason) => CommandError::new(ErrorCode::Journal, reason.to_owned()),
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
    let pump = tokio::spawn(batch::pump(feed, failure, Arc::clone(&sink), flush));
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

/// Одна активная сессия приложения.
///
/// Замок здесь обычный, [`std::sync::Mutex`]: он не держится через `await`, поэтому
/// команда и обработчик закрытия окна не могут заклинить друг друга. Пока сессия
/// запускается или останавливается, слот занят: повторный старт получает
/// [`ErrorCode::AlreadyRunning`], а не второй журнал.
#[derive(Default)]
pub struct Sessions {
    slot: Mutex<Slot>,
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

    /// Запускает сессию, если другой нет.
    ///
    /// # Errors
    ///
    /// `already_running`, если запись уже идёт или останавливается; ошибки старта ядра.
    pub async fn start(
        &self,
        config: SessionConfig,
        sink: Arc<dyn FeedSink>,
    ) -> Result<StartInfo, CommandError> {
        {
            let mut slot = self.lock();
            if !matches!(*slot, Slot::Idle) {
                return Err(already_running());
            }
            *slot = Slot::Busy;
        }
        let started = match config.into_bridge() {
            Ok(config) => start_session(config, sink, FLUSH_INTERVAL).await,
            Err(err) => Err(err),
        };
        let mut slot = self.lock();
        match started {
            Ok((session, info)) => {
                *slot = Slot::Running(Box::new(session));
                Ok(info)
            }
            Err(err) => {
                *slot = Slot::Idle;
                Err(err)
            }
        }
    }

    /// Останавливает сессию и возвращает итоги.
    ///
    /// # Errors
    ///
    /// `journal`, если журнал отказал: записанное до сбоя сохранено, но `session_end`
    /// в файле может не быть. `io` — если записи не было.
    pub async fn stop(&self) -> Result<SessionSummary, CommandError> {
        let session = {
            let mut slot = self.lock();
            match std::mem::replace(&mut *slot, Slot::Busy) {
                Slot::Running(session) => session,
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
        result
    }

    /// Останавливает сессию, если она идёт: закрытие окна во время записи (NFR-5).
    ///
    /// Если остановка уже начата командой «Стоп», окно ждёт её окончания, а не выходит
    /// посреди записи журнала. Дольше пятнадцати секунд не ждём: зависшая фиксация не
    /// должна держать окно вечно, у писателя журнала свой предел в десять секунд.
    pub async fn stop_if_running(&self) {
        for _ in 0..1500 {
            let running = match &*self.lock() {
                Slot::Idle => Some(false),
                Slot::Busy => None,
                Slot::Running(_) => Some(true),
            };
            match running {
                Some(false) => return,
                Some(true) => {
                    let _ = self.stop().await;
                    return;
                }
                None => tokio::time::sleep(Duration::from_millis(10)).await,
            }
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
    use tokio::io::AsyncWriteExt as _;

    use super::*;
    use crate::batch::test_sink::TestSink;
    use crate::feed::{FeedEvent, StopReason};

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
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");

        sessions.stop_if_running().await;

        assert!(!sessions.is_running());
        let outcome = read_all(&info.journal_path).expect("чтение журнала");
        assert_eq!(
            outcome.lines.last().map(|line| line.event.kind()),
            Some("session_end"),
            "журнал закрыт штатно, а не оборван"
        );
        sessions.stop_if_running().await; // без сессии ничего не делает
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
}
