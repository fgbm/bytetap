//! Сессия записи: слушает TCP, ведёт соединения, пишет журнал и рассылает события (FR-1…FR-4).
//!
//! Сессия — актор: [`Session::start`] занимает порт, создаёт файл журнала и запускает задачу,
//! которая принимает соединения и обслуживает их. Всё остальное общается с ней через
//! [`SessionHandle`]: [`SessionHandle::stop`] останавливает штатно, [`SessionHandle::subscribe`]
//! даёт ленту событий для CLI (M1) и интерфейса (M2).
//!
//! Горячий путь одного соединения — «прочитали → записали в журнал → отдали автоответчику →
//! отправили ответ → записали ответ в журнал». Порядок событий одного соединения в журнале
//! сохраняется: `data` входящий идёт раньше `data` ответа. Журнал при этом не блокирует
//! пересылку: [`Journal::record`] только кладёт событие в канал (NFR-6, ADR 0001).
//!
//! Соединения нумеруются с 1 сквозным счётчиком сессии. Параллельные соединения
//! обслуживаются независимо, последовательные получают новый номер. Ошибка в соединении
//! закрывает только его (NFR-5): сессия пишет `error`, `close` и продолжает слушать.
//!
//! Автоответчик у каждого соединения свой ([`Responder`]), а источник `MSH-10` — общий на
//! сессию ([`ControlIdSource`]): номера ответов не повторяются между соединениями.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use chrono::{Local, SecondsFormat};
use serde_json::json;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

use crate::journal::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, Host, InfoEvent, Journal, Mode,
    MonoClock, Open, Session as SessionHeader, SessionEnd, Stats, default_log_dir, log_dir_warning,
    now_rfc3339, session_file_name,
};
use crate::responder::{
    Action, ControlIdSource, Hl7Mllp, NoResponder, Now, Responder, ResponderEvent,
};
use crate::transport::tcp::{TcpConnection, TcpServer};

/// Сколько событий держат каналы подписчиков.
///
/// Лента не должна стоить памяти: отставший подписчик теряет старые события
/// ([`broadcast::error::RecvError::Lagged`]), а журнал и пересылка идут дальше.
const EVENT_BUFFER: usize = 1024;

/// Размер буфера чтения сокета: один вызов чтения — один фрагмент журнала.
const READ_BUFFER: usize = 16 * 1024;

/// Настройки сессии: адрес, автоответчик, папка журналов, поля заголовка (FR-21).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Адрес и порт прослушивания; порт `0` — любой свободный.
    pub listen: SocketAddr,
    /// Профиль автоответчика: в M1 доступны `hl7_mllp` и `none`.
    pub responder: Autoresponder,
    /// Папка журналов; `None` — [`default_log_dir`].
    pub log_dir: Option<PathBuf>,
    /// Модель анализатора для заголовка журнала.
    pub analyzer: Option<String>,
    /// Комментарий инженера для заголовка журнала.
    pub note: Option<String>,
}

impl SessionConfig {
    /// Настройки сессии на адресе с автоответчиком HL7/MLLP.
    #[must_use]
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            responder: Autoresponder::Hl7Mllp,
            log_dir: None,
            analyzer: None,
            note: None,
        }
    }

    /// Профиль автоответчика (FR-14).
    #[must_use]
    pub fn with_responder(mut self, responder: Autoresponder) -> Self {
        self.responder = responder;
        self
    }

    /// Папка журналов вместо папки по умолчанию (FR-19).
    #[must_use]
    pub fn with_log_dir(mut self, log_dir: impl Into<PathBuf>) -> Self {
        self.log_dir = Some(log_dir.into());
        self
    }

    /// Модель анализатора для заголовка журнала (FR-23).
    #[must_use]
    pub fn with_analyzer(mut self, analyzer: impl Into<String>) -> Self {
        self.analyzer = Some(analyzer.into());
        self
    }

    /// Комментарий инженера для заголовка журнала (FR-23).
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Событие сессии для подписчиков ([`SessionHandle::subscribe`]).
///
/// Это то же событие, что уходит в журнал, в [`Arc`]: подписчиков может быть несколько,
/// а байты фрагмента не копируются на каждого. Лента строится из тех же полей, что и файл,
/// поэтому в ней видны `t`, `conn`, `dir`, `synthetic` и `text` (FR-22, FR-26).
pub type SessionEvent = Arc<Event>;

/// Актор сессии: точка входа [`Session::start`].
pub struct Session;

impl Session {
    /// Занимает адрес, создаёт журнал и начинает принимать соединения.
    ///
    /// Заголовок `session` пишется и фиксируется до возврата: путь к журналу уже существует,
    /// а ошибка открытия файла — ошибка старта. Вызывается внутри runtime `tokio`.
    ///
    /// Профили, которых нет в M1 (`astm_e1381`, `auto`), отвергаются до создания файла.
    pub async fn start(config: SessionConfig) -> io::Result<SessionHandle> {
        if !matches!(
            config.responder,
            Autoresponder::Hl7Mllp | Autoresponder::None
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "автоответчик `{}` появится в M3: в M1 доступны `hl7_mllp` и `none`",
                    config.responder
                ),
            ));
        }
        let log_dir = match config.log_dir.clone().or_else(default_log_dir) {
            Some(log_dir) => log_dir,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "не удалось определить папку журналов: укажите каталог явно",
                ));
            }
        };
        let warning = log_dir_warning(&log_dir);

        let server = TcpServer::bind(config.listen).await?;
        let local_addr = server.local_addr()?;

        let started = Local::now();
        let mut header = SessionHeader::at(
            started,
            Mode::TcpServer,
            config.responder,
            // `settings` — фактический адрес слушателя: при порте 0 он выбран системой,
            // и записанное в журнал значение совпадает с тем, куда подключался анализатор.
            json!({"listen": local_addr.to_string()}),
            Host::current(),
        );
        header.analyzer = config.analyzer.clone();
        header.note = config.note.clone();

        let path = log_dir.join(session_file_name(started, Mode::TcpServer));
        let journal = Journal::create(&path, header)?;
        let path = journal.path().to_path_buf();

        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (stop_tx, stop_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            responder: config.responder,
            journal,
            clock: MonoClock::new(),
            events,
            ids: ControlIdSource::new(),
            counters: Arc::new(Counters::default()),
        });
        let task = tokio::spawn(run(server, Arc::clone(&shared), stop_rx));

        Ok(SessionHandle {
            local_addr,
            path,
            warning,
            events: shared.events.clone(),
            stop: stop_tx,
            task: Some(task),
        })
    }
}

/// Дескриптор запущенной сессии.
///
/// Живёт до [`SessionHandle::stop`] (или до выгрузки значения: тогда сессия тоже
/// останавливается, но без ожидания итогов).
pub struct SessionHandle {
    local_addr: SocketAddr,
    path: PathBuf,
    warning: Option<String>,
    events: broadcast::Sender<SessionEvent>,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<io::Result<Stats>>>,
}

impl SessionHandle {
    /// Фактический адрес и порт слушателя (при порте `0` выбран системой).
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Путь к файлу журнала.
    #[must_use]
    pub fn journal_path(&self) -> &Path {
        &self.path
    }

    /// Предупреждение о папке журналов (OneDrive, сетевой путь), если оно есть (FR-19).
    #[must_use]
    pub fn log_dir_warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// Подписка на ленту событий сессии.
    ///
    /// Сессия не ждёт подписчиков: при заполнении канала получатель теряет старые события
    /// и получает [`broadcast::error::RecvError::Lagged`], а журнал и пересылка не меняются.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// Останавливает сессию штатно и возвращает её статистику (FR-22).
    ///
    /// Слушатель закрывается, соединения закрываются с событием `close`, в журнал пишется
    /// `session_end`, писатель журнала дописывает накопленное и фиксирует файл.
    pub async fn stop(mut self) -> io::Result<Stats> {
        self.request_stop();
        let task = self
            .task
            .take()
            .expect("задача сессии принадлежит дескриптору");
        match task.await {
            Ok(result) => result,
            Err(err) => Err(io::Error::other(format!("задача сессии упала: {err}"))),
        }
    }

    fn request_stop(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        // Без `stop` сессия всё равно останавливается: слушатель закрывается, журнал
        // дописывается, а ждать итогов некому.
        self.request_stop();
    }
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("local_addr", &self.local_addr)
            .field("path", &self.path)
            .field("stopped", &self.task.is_none())
            .finish()
    }
}

/// То, что нужно каждому соединению: журнал, часы, автоответчик, подписчики и счётчики.
struct Shared {
    responder: Autoresponder,
    journal: Journal,
    clock: MonoClock,
    events: broadcast::Sender<SessionEvent>,
    ids: ControlIdSource,
    counters: Arc<Counters>,
}

impl Shared {
    /// Кладёт событие в журнал и раздаёт его подписчикам.
    ///
    /// Оба действия не ждут: канал журнала неограниченный, канал подписчиков теряет
    /// старые события у отставших. Порядок строк журнала — порядок вызовов.
    fn publish(&self, event: Event) {
        self.journal.record(event.clone());
        // Ошибка означает лишь отсутствие подписчиков: сессия работает и без них.
        let _ = self.events.send(Arc::new(event));
    }

    /// Ошибка уровня соединения: она закрывает только это соединение (NFR-5).
    fn error(&self, conn: Option<u32>, message: impl Into<String>) {
        self.publish(Event::Error(ErrorEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            message: message.into(),
        }));
    }
}

/// Счётчики сессии: номер соединения и байты по направлениям.
///
/// Счётчик соединений одновременно выдаёт номера, поэтому `conn` не повторяется и
/// `stats.connections` совпадает с числом событий `open`.
#[derive(Debug, Default)]
struct Counters {
    connections: AtomicU32,
    bytes_analyzer_to_lis: AtomicU64,
    bytes_lis_to_analyzer: AtomicU64,
}

impl Counters {
    fn next_conn(&self) -> u32 {
        self.connections.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn add(&self, dir: Dir, len: u64) {
        let counter = match dir {
            Dir::AnalyzerToLis => &self.bytes_analyzer_to_lis,
            Dir::LisToAnalyzer => &self.bytes_lis_to_analyzer,
        };
        counter.fetch_add(len, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Stats {
        Stats {
            connections: self.connections.load(Ordering::Relaxed),
            bytes_analyzer_to_lis: self.bytes_analyzer_to_lis.load(Ordering::Relaxed),
            bytes_lis_to_analyzer: self.bytes_lis_to_analyzer.load(Ordering::Relaxed),
        }
    }
}

/// Задача сессии: принимает соединения, пока не придёт остановка.
async fn run(
    server: TcpServer,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) -> io::Result<Stats> {
    let mut tasks: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            accepted = server.accept() => match accepted {
                Ok(connection) => {
                    let conn = shared.counters.next_conn();
                    // `open` пишется здесь, а не в задаче соединения: так порядок строк
                    // совпадает с порядком номеров, и открытие не может оказаться после
                    // первых фрагментов этого же соединения.
                    shared.publish(Event::Open(Open {
                        t: now_rfc3339(),
                        mono_us: shared.clock.now_us(),
                        conn,
                        peer: connection.peer().to_string(),
                        local: connection.local().to_string(),
                    }));
                    let shared = Arc::clone(&shared);
                    let mut conn_stop = stop.clone();
                    tasks.spawn(async move {
                        serve(connection, conn, &shared, &mut conn_stop).await;
                    });
                }
                Err(err) => {
                    // Ошибка приёма не роняет сессию (NFR-5): пишем её и слушаем дальше.
                    shared.error(None, format!("не удалось принять соединение: {err}"));
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let _ = joined;
            },
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow_and_update() {
                    break;
                }
            }
        }
    }

    // Слушатель закрыт вместе с `server`; соединения видят остановку и пишут `close`.
    drop(server);
    while tasks.join_next().await.is_some() {}

    let stats = shared.counters.snapshot();
    shared.publish(Event::SessionEnd(SessionEnd {
        t: now_rfc3339(),
        mono_us: shared.clock.now_us(),
        stats: stats.clone(),
    }));
    shared.journal.shutdown().await?;
    Ok(stats)
}

/// Обслуживает одно соединение до закрытия.
async fn serve(
    mut connection: TcpConnection,
    conn: u32,
    shared: &Shared,
    stop: &mut watch::Receiver<bool>,
) {
    let mut responder = make_responder(shared.responder, &shared.ids);
    let mut buffer = vec![0u8; READ_BUFFER];
    let (by, reason) = loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                let _ = changed;
                break (ClosedBy::Bytetap, "stop");
            }
            read = connection.read(&mut buffer) => match read {
                Ok(0) => break (ClosedBy::Analyzer, "eof"),
                Ok(len) => {
                    let payload = &buffer[..len];
                    let (t, now) = timestamp();
                    let mono_us = shared.clock.now_us();
                    shared.counters.add(Dir::AnalyzerToLis, len as u64);
                    shared.publish(Event::Data(Data::new(
                        t,
                        mono_us,
                        conn,
                        Dir::AnalyzerToLis,
                        false,
                        payload,
                    )));
                    if let Some((by, reason)) =
                        apply(shared, conn, &mut connection, responder.feed(payload, now)).await
                    {
                        break (by, reason);
                    }
                }
                Err(err) => {
                    let (by, reason) = close_reason(&err);
                    shared.error(Some(conn), format!("чтение из соединения: {err}"));
                    break (by, reason);
                }
            },
        }
    };

    // Автоответчик не теряет недособранное состояние молча: отчёт идёт до `close`.
    let actions = responder.finish(timestamp().1);
    let _ = apply(shared, conn, &mut connection, actions).await;
    shared.publish(Event::Close(Close {
        t: now_rfc3339(),
        mono_us: shared.clock.now_us(),
        conn,
        by,
        reason: reason.to_owned(),
    }));
    connection.shutdown().await;
}

/// Автоответчик соединения по профилю. Профиль уже проверен при старте сессии.
fn make_responder(kind: Autoresponder, ids: &ControlIdSource) -> Box<dyn Responder + Send> {
    match kind {
        Autoresponder::Hl7Mllp => Box::new(Hl7Mllp::new(ids.clone())),
        _ => Box::new(NoResponder),
    }
}

/// Выполняет действия автоответчика по порядку: ответ — в сокет, затем в журнал.
///
/// Ответ уходит в сокет до записи в журнал: событие `data` с `synthetic: true` появляется
/// только для того, что действительно отправлено. Ошибка отправки закрывает соединение.
/// Возвращает причину закрытия, если ответ отправить не удалось.
async fn apply(
    shared: &Shared,
    conn: u32,
    connection: &mut TcpConnection,
    actions: Vec<Action>,
) -> Option<(ClosedBy, &'static str)> {
    for action in actions {
        match action {
            Action::Send(payload) => {
                if let Err(err) = connection.write_all(&payload).await {
                    let (by, reason) = close_reason(&err);
                    shared.error(Some(conn), format!("отправка ответа: {err}"));
                    return Some((by, reason));
                }
                let (t, _) = timestamp();
                shared
                    .counters
                    .add(Dir::LisToAnalyzer, payload.len() as u64);
                shared.publish(Event::Data(Data::new(
                    t,
                    shared.clock.now_us(),
                    conn,
                    Dir::LisToAnalyzer,
                    true,
                    &payload,
                )));
            }
            Action::Event(event) => {
                let (t, _) = timestamp();
                let mono_us = shared.clock.now_us();
                let event = match event {
                    ResponderEvent::Error { message } => Event::Error(ErrorEvent {
                        t,
                        mono_us,
                        conn: Some(conn),
                        message,
                    }),
                    ResponderEvent::Info { message } => Event::Info(InfoEvent {
                        t,
                        mono_us,
                        conn: Some(conn),
                        message,
                    }),
                };
                shared.publish(event);
            }
        }
    }
    None
}

/// Одна метка времени на фрагмент: `t` для журнала и `now` для `MSH-7` ответа (FR-22).
///
/// Системные часы читаются один раз, поэтому время строки журнала и время ACK относятся
/// к одному моменту, а не расходятся на два чтения.
fn timestamp() -> (String, Now) {
    let now = Local::now();
    (
        now.to_rfc3339_opts(SecondsFormat::Micros, false),
        now.fixed_offset(),
    )
}

/// Кто закрыл соединение по ошибке сокета и почему.
fn close_reason(err: &io::Error) -> (ClosedBy, &'static str) {
    match err.kind() {
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => (ClosedBy::Analyzer, "reset"),
        _ => (ClosedBy::Bytetap, "error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Note, append_note, read_all};
    use chrono::{DateTime, FixedOffset};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("временная папка")
    }

    #[tokio::test]
    async fn an_unimplemented_responder_is_rejected_before_the_file() {
        let dir = temp_dir();
        let config = SessionConfig::new("127.0.0.1:0".parse().unwrap())
            .with_responder(Autoresponder::Auto)
            .with_log_dir(dir.path());
        let err = Session::start(config).await.expect_err("профиль M3");
        assert!(err.to_string().contains("M3"), "{err}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "файл журнала не создан"
        );
    }

    #[tokio::test]
    async fn start_writes_the_header_and_stop_writes_session_end() {
        let dir = temp_dir();
        let config = SessionConfig::new("127.0.0.1:0".parse().unwrap())
            .with_responder(Autoresponder::None)
            .with_log_dir(dir.path())
            .with_analyzer("ANALYZER")
            .with_note("синтетическая сессия");
        let handle = Session::start(config).await.expect("сессия");
        let addr = handle.local_addr();
        assert_ne!(addr.port(), 0);
        assert!(handle.journal_path().exists(), "заголовок уже на диске");
        assert!(handle.log_dir_warning().is_none(), "{:?}", handle.warning);
        let path = handle.journal_path().to_path_buf();

        let stats = handle.stop().await.expect("остановка");
        assert_eq!(stats.connections, 0);

        let outcome = read_all(&path).expect("чтение журнала");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.lines.len(), 2, "заголовок и итоги");
        assert_eq!(outcome.lines[0].event.kind(), "session");
        assert_eq!(outcome.lines[1].event.kind(), "session_end");
        let Event::Session(header) = &outcome.lines[0].event else {
            panic!("первая строка — заголовок");
        };
        assert_eq!(header.mode, Mode::TcpServer);
        assert_eq!(header.autoresponder, Autoresponder::None);
        assert_eq!(header.analyzer.as_deref(), Some("ANALYZER"));
        assert_eq!(header.note.as_deref(), Some("синтетическая сессия"));
        assert_eq!(header.settings["listen"], addr.to_string());
        let Event::SessionEnd(end) = &outcome.lines[1].event else {
            panic!("последняя строка — итоги");
        };
        assert_eq!(end.stats.connections, 0);
        assert_eq!(end.stats.bytes_analyzer_to_lis, 0);
        assert_eq!(end.stats.bytes_lis_to_analyzer, 0);
    }

    #[tokio::test]
    async fn a_stopped_session_keeps_its_journal_unlocked() {
        let dir = temp_dir();
        let handle = Session::start(
            SessionConfig::new("127.0.0.1:0".parse().unwrap())
                .with_responder(Autoresponder::None)
                .with_log_dir(dir.path()),
        )
        .await
        .expect("сессия");
        let path = handle.journal_path().to_path_buf();
        handle.stop().await.expect("остановка");
        append_note(
            &path,
            &Note {
                t: now_rfc3339(),
                mono_us: None,
                text: "после остановки".to_owned(),
            },
        )
        .expect("журнал после остановки не заблокирован");
    }

    #[test]
    fn socket_reset_and_other_errors_are_told_apart() {
        assert_eq!(
            close_reason(&io::Error::from(io::ErrorKind::ConnectionReset)),
            (ClosedBy::Analyzer, "reset")
        );
        assert_eq!(
            close_reason(&io::Error::from(io::ErrorKind::BrokenPipe)),
            (ClosedBy::Analyzer, "reset")
        );
        assert_eq!(
            close_reason(&io::Error::from(io::ErrorKind::Other)),
            (ClosedBy::Bytetap, "error")
        );
    }

    #[test]
    fn timestamps_from_one_call_agree() {
        let (t, now) = timestamp();
        let parsed: DateTime<FixedOffset> = DateTime::parse_from_rfc3339(&t).unwrap();
        // `now` — тот же момент, что и `t`, с точностью до микросекунд.
        let difference = (parsed - now).num_microseconds().unwrap_or_default().abs();
        assert!(difference <= 1_000, "{t} и {now}");
        assert_eq!(t.len(), "2026-10-19T10:15:00.123456+03:00".len());
    }
}
