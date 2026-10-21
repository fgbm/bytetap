//! Запись без моста: прямые вызовы `bytetap_core::session` (U1-AC13).
//!
//! Перенос логики моста Tauri-версии (`app/src-tauri/src/{bridge,batch,feed,error}.rs`)
//! без IPC: старт сессии с кодами ошибок у полей формы, приёмник событий в кольцевой
//! буфер ленты, комментарий, остановка с итогами и отказ журнала по коду.
//!
//! Сессия живёт в runtime `tokio` приложения. Приёмник — задача того же runtime: он
//! забирает всё, что накопилось в подписке `broadcast`, кладёт в общий буфер под одним
//! замком и просит перерисовку не чаще [`FRAME`], сколько бы фрагментов ни пришло.
//! Отставший приёмник (`Lagged`) теряет события ленты, но не счётчики: их окно берёт
//! из [`SessionCounters`], а потерянное показывает как «пропущено в ленте: N».

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytetap_core::journal::{JournalFailureReason, OsFamily, Stats};
use bytetap_core::session::{
    Session, SessionConfig, SessionCounters, SessionEvent, SessionHandle, journal_failure_reason,
};
use bytetap_core::transport::Source;
use bytetap_core::transport::serial::SerialSettings;
use eframe::egui;
use tokio::sync::{broadcast, watch};

use crate::errors::{ErrorCode, StartError};
use crate::feed::{FeedEvent, FeedRing};

/// Не чаще одной перерисовки на столько: ~30 кадров в секунду во время записи.
pub const FRAME: Duration = Duration::from_millis(33);

/// Предел ожидания остановки при закрытии окна (NFR-5): у писателя журнала свой
/// предел в 10 с, 15 с — запас на очередь и медленную машину.
pub const STOP_WAIT: Duration = Duration::from_secs(15);

/// Общий кольцевой буфер ленты: приёмник пишет, окно читает только новое.
pub type SharedFeed = Arc<Mutex<FeedRing>>;

pub fn lock(feed: &SharedFeed) -> std::sync::MutexGuard<'_, FeedRing> {
    feed.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Код отказа журнала для строки под шапкой: вид отказа, а не текст системы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalFailureCode {
    DiskFull,
    PermissionDenied,
    NotFound,
    Network,
    Busy,
    Timeout,
    Io,
}

impl JournalFailureCode {
    /// Вид отказа по причине от ядра: сначала код ОС (таблица по семейству ОС — 5 в
    /// Windows и в Linux значит разное), затем вид ошибки.
    pub fn from_failure(reason: &JournalFailureReason) -> Self {
        match reason.raw_os_error() {
            Some(code) => Self::from_os_code(reason, code),
            None => Self::from_kind(reason.kind()),
        }
    }

    fn from_kind(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::StorageFull => Self::DiskFull,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::TimedOut => Self::Timeout,
            _ => Self::Io,
        }
    }

    fn from_os_code(reason: &JournalFailureReason, code: i32) -> Self {
        match reason.os() {
            OsFamily::Windows => match code {
                // ERROR_DISK_FULL и ERROR_HANDLE_DISK_FULL.
                112 | 39 => Self::DiskFull,
                // ERROR_ACCESS_DENIED.
                5 => Self::PermissionDenied,
                // ERROR_FILE_NOT_FOUND и ERROR_PATH_NOT_FOUND.
                2 | 3 => Self::NotFound,
                // ERROR_BAD_NETPATH, ERROR_DEV_NOT_EXIST, ERROR_UNEXP_NET_ERR, ERROR_NETNAME_DELETED.
                53 | 55 | 59 | 64 => Self::Network,
                // ERROR_SHARING_VIOLATION и ERROR_LOCK_VIOLATION.
                32 | 33 => Self::Busy,
                _ => Self::from_kind(reason.kind()),
            },
            OsFamily::Unix => match code {
                // ENOSPC.
                28 => Self::DiskFull,
                // EACCES и EPERM.
                13 | 1 => Self::PermissionDenied,
                // ENOENT.
                2 => Self::NotFound,
                _ => Self::from_kind(reason.kind()),
            },
        }
    }
}

/// Отказ журнала: код для строки под шапкой и текст для журнала запуска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalFailure {
    pub code: JournalFailureCode,
    pub message: String,
}

impl JournalFailure {
    pub fn new(reason: &JournalFailureReason) -> Self {
        Self {
            code: JournalFailureCode::from_failure(reason),
            message: reason.to_string(),
        }
    }
}

/// Код ошибки старта по ошибке ядра.
///
/// Ядро сначала занимает адрес или открывает порт, потом создаёт журнал, поэтому у
/// TCP-сервера всё, что не про адрес, — про журнал.
pub fn start_error(err: &io::Error, source: &Source) -> StartError {
    match source {
        Source::TcpServer { listen } => {
            let port = listen.port().to_string();
            match ErrorCode::from_io_kind(err.kind()) {
                ErrorCode::PortInUse => {
                    StartError::new(ErrorCode::PortInUse, err.to_string()).with_param("port", port)
                }
                ErrorCode::Io => StartError::new(ErrorCode::Journal, err.to_string()),
                code => StartError::new(code, err.to_string()).with_param("port", port),
            }
        }
        Source::TcpClient { .. } => StartError::new(ErrorCode::Io, err.to_string()),
        Source::Serial { settings, .. } => serial_error(err, settings, port_listed),
    }
}

/// Ошибка старта COM-порта: нет порта, занят, нет прав, неверные настройки.
///
/// `serialport` отдаёт `NotFound` и для отсутствующего порта, и для занятого: различие —
/// по списку портов (`listed`).
pub fn serial_error(
    err: &io::Error,
    settings: &SerialSettings,
    listed: impl Fn(&str) -> bool,
) -> StartError {
    let code = match err.kind() {
        io::ErrorKind::InvalidInput => ErrorCode::PortSettings,
        io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        io::ErrorKind::NotFound if listed(&settings.port) => ErrorCode::PortBusy,
        io::ErrorKind::NotFound => ErrorCode::PortNotFound,
        _ => ErrorCode::Serial,
    };
    StartError::new(code, err.to_string()).with_param("port", &settings.port)
}

fn port_listed(port: &str) -> bool {
    bytetap_core::transport::serial::list_ports().is_ok_and(|ports| {
        ports
            .iter()
            .any(|info| info.name.eq_ignore_ascii_case(port))
    })
}

/// Идущая сессия: дескриптор ядра, приёмник ленты и общий буфер.
pub struct LiveSession {
    handle: SessionHandle,
    pump: tokio::task::JoinHandle<()>,
    feed: SharedFeed,
    journal_path: PathBuf,
    local_addr: String,
}

impl std::fmt::Debug for LiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSession")
            .field("journal_path", &self.journal_path)
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

/// Запускает сессию и приёмник событий.
///
/// Папка журналов создаётся до старта: недоступная папка — ошибка `log_dir`, а не отказ
/// журнала посреди работы (FR-19). Ошибка источника (занятый порт, нет COM-порта) —
/// ошибка старта до создания файла журнала (U1-AC11).
///
/// # Errors
///
/// Код ошибки и подстановки для текста под полем формы.
pub async fn start(config: SessionConfig, ctx: egui::Context) -> Result<LiveSession, StartError> {
    if let Some(dir) = &config.log_dir {
        std::fs::create_dir_all(dir).map_err(|err| {
            StartError::new(ErrorCode::LogDir, err.to_string())
                .with_param("dir", dir.to_string_lossy())
        })?;
    }
    let source = config.source.clone();
    let (handle, events) = Session::start_with_feed(config)
        .await
        .map_err(|err| start_error(&err, &source))?;
    let feed = SharedFeed::default();
    let pump = tokio::spawn(pump(events, Arc::clone(&feed), ctx));
    Ok(LiveSession {
        journal_path: handle.journal_path().to_path_buf(),
        local_addr: handle.local_addr().to_owned(),
        handle,
        pump,
        feed,
    })
}

/// Приёмник ленты: всё, что накопилось в подписке, — в буфер под одним замком.
///
/// Заканчивается, когда сессия выгрузила отправителя событий (остановка, отказ журнала).
///
/// Пачка ограничена [`crate::feed::FEED_CAPACITY`]: буфер ленты столько и вмещает, а
/// лишнее всё равно вытеснило бы старое. Ограничение не даёт приёмнику разобрать всю
/// очередь подписчика, если она вдруг окажется большой.
///
/// `Lagged` считает все события подписки, а лента без заголовка сессии и итогов: на
/// число пропущенных это влияет в пределах двух событий (U1-AC13).
pub async fn pump(
    mut events: broadcast::Receiver<SessionEvent>,
    feed: SharedFeed,
    ctx: egui::Context,
) {
    let mut batch = Vec::new();
    loop {
        let mut lagged = 0;
        let mut closed = false;
        match events.recv().await {
            Ok(event) => batch.extend(FeedEvent::from_session(&event)),
            Err(broadcast::error::RecvError::Lagged(count)) => lagged += count,
            Err(broadcast::error::RecvError::Closed) => closed = true,
        }
        while !closed && batch.len() < crate::feed::FEED_CAPACITY {
            match events.try_recv() {
                Ok(event) => batch.extend(FeedEvent::from_session(&event)),
                Err(broadcast::error::TryRecvError::Lagged(count)) => lagged += count,
                Err(broadcast::error::TryRecvError::Closed) => closed = true,
                Err(broadcast::error::TryRecvError::Empty) => break,
            }
        }
        if !batch.is_empty() || lagged > 0 {
            // `Arc` создаётся до взятия замка: под замком только кладём готовые события.
            let ready: Vec<Arc<FeedEvent>> = batch.drain(..).map(Arc::new).collect();
            let mut ring = lock(&feed);
            ring.lagged(lagged);
            for event in ready {
                ring.push(event);
            }
            drop(ring);
            ctx.request_repaint_after(FRAME);
        }
        if closed {
            ctx.request_repaint();
            return;
        }
    }
}

/// Итоги остановки: статистика ядра или отказ журнала.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopOutcome {
    pub stats: Option<Stats>,
    pub failure: Option<JournalFailure>,
    /// Прочая ошибка остановки: текст для журнала запуска.
    pub error: Option<String>,
    pub journal_path: PathBuf,
}

impl LiveSession {
    pub fn feed(&self) -> SharedFeed {
        Arc::clone(&self.feed)
    }

    pub fn counters(&self) -> SessionCounters {
        self.handle.counters()
    }

    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    /// Для TCP-сервера — фактический адрес слушателя (при порте 0 выбран системой).
    pub fn local_addr(&self) -> &str {
        &self.local_addr
    }

    pub fn failure_watch(&self) -> watch::Receiver<Option<JournalFailureReason>> {
        self.handle.journal_failure()
    }

    /// Сессия ядра закончилась сама: отказ журнала, падение задачи или иное завершение.
    ///
    /// Окно замечает это и останавливает запись: слот освобождается без «Стоп», как в
    /// UI v2 (слот занятой завершившейся сессии освобождается — `app/src-tauri/src/bridge.rs`).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Комментарий в идущую сессию (FR-23): событие `note` в журнал и ленту.
    ///
    /// # Errors
    ///
    /// Сессия уже остановлена.
    pub fn note(&self, text: &str) -> io::Result<()> {
        self.handle.note(text.trim())
    }

    /// Останавливает сессию: журнал получает `session_end`, лента — последние события.
    ///
    /// Сначала ядро закрывает журнал, потом приёмник дописывает хвост ленты: итоги не
    /// приходят раньше событий.
    pub async fn stop(self) -> StopOutcome {
        let Self {
            handle,
            pump,
            journal_path,
            ..
        } = self;
        let failure_watch = handle.journal_failure();
        let result = handle.stop().await;
        let _ = pump.await;
        let mut outcome = StopOutcome {
            stats: None,
            failure: None,
            error: None,
            journal_path,
        };
        match result {
            Ok(stats) => outcome.stats = Some(stats),
            Err(err) => match journal_failure_reason(&err) {
                Some(reason) => outcome.failure = Some(JournalFailure::new(reason)),
                None => outcome.error = Some(err.to_string()),
            },
        }
        if outcome.failure.is_none()
            && let Some(reason) = failure_watch.borrow().as_ref()
        {
            outcome.failure = Some(JournalFailure::new(reason));
        }
        outcome
    }
}

/// Фаза записи — как `app/src/session/reducer.ts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Записи нет.
    #[default]
    Idle,
    Starting,
    Recording,
    Stopping,
    /// Журнал отказал: запись остановлена, под шапкой строка с причиной.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    StartRequested,
    Started,
    StartFailed,
    StopRequested,
    Stopped,
    /// Остановка завершилась ошибкой без отказа журнала: видно на стартовом экране.
    StopFailed,
    JournalFailed,
    FailureDismissed,
}

/// Переход фазы; недопустимое действие оставляет фазу как есть.
pub fn reduce(phase: Phase, action: Action) -> Phase {
    use Action as A;
    use Phase as P;
    match (phase, action) {
        (P::Idle | P::Failed, A::StartRequested) => P::Starting,
        (P::Starting, A::Started) => P::Recording,
        (P::Starting, A::StartFailed) => P::Idle,
        (P::Recording, A::StopRequested) => P::Stopping,
        (P::Stopping, A::Stopped) => P::Idle,
        (P::Recording | P::Stopping, A::StopFailed) => P::Failed,
        (P::Recording | P::Stopping, A::JournalFailed) => P::Failed,
        (P::Failed, A::FailureDismissed) => P::Idle,
        (phase, _) => phase,
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use bytetap_core::transport::serial::SerialFormat;

    #[test]
    fn the_phase_follows_the_session() {
        use Action as A;
        use Phase as P;
        assert_eq!(reduce(P::Idle, A::StartRequested), P::Starting);
        assert_eq!(reduce(P::Starting, A::Started), P::Recording);
        assert_eq!(reduce(P::Starting, A::StartFailed), P::Idle);
        assert_eq!(reduce(P::Recording, A::StopRequested), P::Stopping);
        assert_eq!(reduce(P::Stopping, A::Stopped), P::Idle);
        assert_eq!(reduce(P::Recording, A::JournalFailed), P::Failed);
        assert_eq!(reduce(P::Stopping, A::JournalFailed), P::Failed);
        assert_eq!(reduce(P::Failed, A::FailureDismissed), P::Idle);
        assert_eq!(reduce(P::Failed, A::StartRequested), P::Starting);
        // Недопустимое — без изменений: второй старт во время записи, стоп без записи.
        assert_eq!(reduce(P::Recording, A::StartRequested), P::Recording);
        assert_eq!(reduce(P::Stopping, A::StartRequested), P::Stopping);
        assert_eq!(reduce(P::Idle, A::StopRequested), P::Idle);
        assert_eq!(reduce(P::Idle, A::Stopped), P::Idle);
        assert_eq!(reduce(P::Idle, A::JournalFailed), P::Idle);
    }

    fn server() -> Source {
        Source::TcpServer {
            listen: "0.0.0.0:5000".parse::<SocketAddr>().unwrap(),
        }
    }

    #[test]
    fn core_errors_get_their_codes() {
        let error = start_error(&io::Error::from(io::ErrorKind::AddrInUse), &server());
        assert_eq!(error.code, ErrorCode::PortInUse);
        assert_eq!(error.params.get("port").map(String::as_str), Some("5000"));
        assert_eq!(
            start_error(&io::Error::from(io::ErrorKind::AddrNotAvailable), &server()).code,
            ErrorCode::AddressUnavailable
        );
        assert_eq!(
            start_error(&io::Error::from(io::ErrorKind::PermissionDenied), &server()).code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            start_error(&io::Error::other("journal not created"), &server()).code,
            ErrorCode::Journal,
            "у сервера остальное — журнал"
        );
        let client = Source::TcpClient {
            addr: "192.168.1.50:5000".into(),
            reconnect: Duration::from_secs(5),
        };
        assert_eq!(
            start_error(&io::Error::from(io::ErrorKind::AddrInUse), &client).code,
            ErrorCode::Io
        );

        let settings = SerialSettings::new("COM5", 9600, SerialFormat::EIGHT_N_ONE);
        let serial =
            |kind, listed: bool| serial_error(&io::Error::from(kind), &settings, |_| listed);
        assert_eq!(
            serial(io::ErrorKind::InvalidInput, false).code,
            ErrorCode::PortSettings
        );
        assert_eq!(
            serial(io::ErrorKind::PermissionDenied, false).code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            serial(io::ErrorKind::NotFound, true).code,
            ErrorCode::PortBusy
        );
        assert_eq!(
            serial(io::ErrorKind::NotFound, false).code,
            ErrorCode::PortNotFound
        );
        let other = serial(io::ErrorKind::Other, false);
        assert_eq!(other.code, ErrorCode::Serial);
        assert_eq!(other.params.get("port").map(String::as_str), Some("COM5"));
    }

    fn reason(os: OsFamily, code: i32) -> JournalFailureReason {
        JournalFailureReason::from_code(io::ErrorKind::Other, Some(code), os)
    }

    #[test]
    fn a_journal_failure_reason_becomes_a_code() {
        let windows = |code| JournalFailureCode::from_failure(&reason(OsFamily::Windows, code));
        assert_eq!(windows(112), JournalFailureCode::DiskFull);
        assert_eq!(windows(39), JournalFailureCode::DiskFull);
        assert_eq!(windows(5), JournalFailureCode::PermissionDenied);
        assert_eq!(windows(2), JournalFailureCode::NotFound);
        assert_eq!(windows(3), JournalFailureCode::NotFound);
        for code in [53, 55, 59, 64] {
            assert_eq!(windows(code), JournalFailureCode::Network);
        }
        assert_eq!(windows(32), JournalFailureCode::Busy);
        assert_eq!(windows(33), JournalFailureCode::Busy);
        let unix = |code| JournalFailureCode::from_failure(&reason(OsFamily::Unix, code));
        assert_eq!(unix(28), JournalFailureCode::DiskFull);
        assert_eq!(unix(13), JournalFailureCode::PermissionDenied);
        assert_eq!(unix(1), JournalFailureCode::PermissionDenied);
        assert_eq!(unix(2), JournalFailureCode::NotFound);
        assert_eq!(unix(5), JournalFailureCode::Io, "EIO, а не «нет доступа»");
        let sem_timeout =
            JournalFailureReason::from_code(io::ErrorKind::TimedOut, Some(121), OsFamily::Windows);
        assert_eq!(
            JournalFailureCode::from_failure(&sem_timeout),
            JournalFailureCode::Timeout
        );
        let timeout = JournalFailureReason::from_error(&io::Error::new(
            io::ErrorKind::TimedOut,
            "journal not flushed",
        ));
        assert_eq!(
            JournalFailureCode::from_failure(&timeout),
            JournalFailureCode::Timeout
        );
        let other = JournalFailureReason::from_message("device refused the write");
        assert_eq!(
            JournalFailureCode::from_failure(&other),
            JournalFailureCode::Io
        );
    }

    fn note(index: usize) -> SessionEvent {
        Arc::new(bytetap_core::journal::Event::Note(
            bytetap_core::journal::Note {
                t: "2026-10-19T10:15:03.000+03:00".into(),
                mono_us: Some(index as u64),
                text: index.to_string(),
            },
        ))
    }

    /// 50 000 событий через подписку с маленькой очередью: буфер не растёт сверх
    /// предела, а всё, что не дошло, посчитано как пропущенное (U1-AC13).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lagging_pump_counts_what_it_missed() {
        let (sender, receiver) = broadcast::channel(64);
        let feed = SharedFeed::default();
        let pump = tokio::spawn(pump(receiver, Arc::clone(&feed), egui::Context::default()));
        const TOTAL: usize = 50_000;
        for index in 0..TOTAL {
            sender.send(note(index)).unwrap();
            if index % 5_000 == 0 {
                tokio::task::yield_now().await;
            }
        }
        drop(sender);
        pump.await.unwrap();
        let ring = lock(&feed);
        assert!(ring.len() <= crate::feed::FEED_CAPACITY);
        assert_eq!(ring.next_seq() + ring.dropped(), TOTAL as u64);
        assert!(ring.dropped() > 0, "очередь 64 при 50 000 событий отстаёт");
        assert!(
            matches!(&ring.iter().last().unwrap().kind, crate::feed::FeedKind::Note { text } if *text == (TOTAL - 1).to_string()),
            "последнее событие дошло"
        );
    }
}
