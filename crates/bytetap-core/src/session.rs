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
//! Отказ журнала останавливает сессию: [`Journal::record`], вернувший `false`, значит, что
//! события больше не попадают на диск. Сессия рассылает подписчикам `error` с причиной,
//! перестаёт отвечать анализатору (ни одного ACK после отказа), закрывает соединения и
//! завершается без `session_end`; [`SessionHandle::stop`] возвращает ошибку с причиной
//! (ADR 0001, «Ошибка записи»).
//!
//! Автоответчик у каждого соединения свой ([`Responder`]), а источник `MSH-10` — общий на
//! сессию ([`ControlIdSource`]): номера ответов не повторяются между соединениями.

use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Local, SecondsFormat};
use serde_json::json;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

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

/// Сколько ждёт запись ответа в сокет, прежде чем соединение закроется с причиной `timeout`.
///
/// Клиент, который перестал читать, не должен держать сессию вечно: ответ на сообщение
/// анализатора уходит в буфер сокета за микросекунды, а пять секунд — уже заведомо сбой
/// приёма, а не медленный диск (журнал в пересылку не входит, NFR-6).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Сколько `run` ждёт задачи соединений после остановки, прежде чем прервать их.
const CONNECTIONS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Первая пауза после ошибки `accept`.
const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(10);

/// Предел паузы после ошибки `accept`: порт, занятый чужой программой, не должен
/// превращаться в поток событий `error` по тысяче в секунду (NFR-5).
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

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
    ///
    /// # Errors
    ///
    /// Порт занят, папка журналов недоступна, журнал не создан.
    pub async fn start(config: SessionConfig) -> io::Result<SessionHandle> {
        Self::start_with_feed(config)
            .await
            .map(|(handle, _)| handle)
    }

    /// То же, что [`Session::start`], но подписка на ленту создаётся до запуска задачи.
    ///
    /// Нужна `bytetap-cli capture`: иначе первые события (подключение анализатора сразу
    /// после старта) могли бы разойтись до подписки и пропасть из ленты.
    ///
    /// # Errors
    ///
    /// Те же, что у [`Session::start`].
    pub async fn start_with_feed(
        config: SessionConfig,
    ) -> io::Result<(SessionHandle, broadcast::Receiver<SessionEvent>)> {
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
        let feed = events.subscribe();
        let (stop_tx, stop_rx) = watch::channel(false);
        let (failure_tx, failure_rx) = watch::channel(None);
        let shared = Arc::new(Shared {
            responder: config.responder,
            journal,
            clock: MonoClock::new(),
            events,
            ids: ControlIdSource::new(),
            counters: Arc::new(Counters::default()),
            failure: failure_tx,
            open: Mutex::new(HashSet::new()),
        });
        let task = tokio::spawn(run(server, Arc::clone(&shared), stop_rx, failure_rx));

        let handle = SessionHandle {
            local_addr,
            path,
            warning,
            events: shared.events.clone(),
            failure: shared.failure.subscribe(),
            stop: stop_tx,
            task: Some(task),
        };
        Ok((handle, feed))
    }
}

/// Дескриптор запущенной сессии.
///
/// Живёт до [`SessionHandle::stop`]. Без явной остановки при выгрузке значения
/// отправляется только сигнал: задача сессии живёт в runtime, и если runtime к этому
/// моменту уже остановлен, сигнал до неё не дойдёт, а `session_end` не запишется.
/// Поэтому останавливать сессию нужно явно, пока runtime жив.
pub struct SessionHandle {
    local_addr: SocketAddr,
    path: PathBuf,
    warning: Option<String>,
    events: broadcast::Sender<SessionEvent>,
    failure: watch::Receiver<Option<String>>,
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

    /// Отказ журнала: `Some(причина)`, если писатель перестал принимать события.
    ///
    /// По этой метке `capture` заканчивает ленту, не дожидаясь Ctrl+C: сессия уже
    /// остановлена, и [`SessionHandle::stop`] вернёт ошибку с той же причиной.
    #[must_use]
    pub fn journal_failure(&self) -> watch::Receiver<Option<String>> {
        self.failure.clone()
    }

    /// Останавливает сессию штатно и возвращает её статистику (FR-22).
    ///
    /// Слушатель закрывается, соединения закрываются с событием `close`, в журнал пишется
    /// `session_end`, писатель журнала дописывает накопленное и фиксирует файл.
    ///
    /// # Errors
    ///
    /// Журнал отказал ([`journal_failure_reason`] даёт причину) или задача сессии упала.
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
        // Без `stop` сессия всё равно получает сигнал остановки: слушатель закрывается,
        // журнал дописывается, а ждать итогов некому. Сигнал дойдёт, только если runtime
        // ещё жив: на `Drop` нельзя полагаться как на штатную остановку.
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

/// Отказ журнала: сессия остановлена, потому что события больше не записываются.
#[derive(Debug)]
struct JournalFailure {
    reason: String,
}

impl std::fmt::Display for JournalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "запись в журнал прервана: {}", self.reason)
    }
}

impl std::error::Error for JournalFailure {}

/// Ошибка сессии с причиной отказа журнала.
fn journal_failure_error(reason: String) -> io::Error {
    io::Error::other(JournalFailure { reason })
}

/// Причина отказа журнала, если [`SessionHandle::stop`] вернул ошибку из-за него.
///
/// Нужна `bytetap-cli capture`, чтобы напечатать своё сообщение об остановке сессии,
/// а не общую ошибку ввода-вывода.
#[must_use]
pub fn journal_failure_reason(err: &io::Error) -> Option<&str> {
    err.get_ref()?
        .downcast_ref::<JournalFailure>()
        .map(|failure| failure.reason.as_str())
}

/// То, что нужно каждому соединению: журнал, часы, автоответчик, подписчики и счётчики.
struct Shared {
    responder: Autoresponder,
    journal: Journal,
    clock: MonoClock,
    events: broadcast::Sender<SessionEvent>,
    ids: ControlIdSource,
    counters: Arc<Counters>,
    /// Причина отказа журнала: как только она появилась, сессия перестаёт отвечать.
    failure: watch::Sender<Option<String>>,
    /// Номера открытых соединений: `close` для них пишется даже после abort.
    open: Mutex<HashSet<u32>>,
}

impl Shared {
    /// Кладёт событие в журнал и раздаёт его подписчикам.
    ///
    /// Оба действия не ждут: канал журнала неограниченный, канал подписчиков теряет
    /// старые события у отставших. Порядок строк журнала — порядок вызовов.
    /// `record`, вернувший `false`, означает отказ журнала: сессия останавливается.
    fn publish(&self, event: Event) {
        let recorded = self.journal.record(event.clone());
        // Ошибка означает лишь отсутствие подписчиков: сессия работает и без них.
        let _ = self.events.send(Arc::new(event));
        if !recorded {
            self.fail_journal();
        }
    }

    /// Ошибка и замечание уровня сессии или соединения (NFR-5).
    ///
    /// Сообщения в журнале — английские, независимо от источника: их читает и человек,
    /// и поиск по журналу (`docs/reference/session-format.md`). Русский — только интерфейс.
    fn error(&self, conn: Option<u32>, message: impl Into<String>) {
        self.publish(Event::Error(ErrorEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            message: message.into(),
        }));
    }

    fn info(&self, conn: Option<u32>, message: impl Into<String>) {
        self.publish(Event::Info(InfoEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            message: message.into(),
        }));
    }

    /// Первый `false` от писателя: сообщает подписчикам причину и будит соединения.
    ///
    /// В журнал `error` уже не попадёт — он и есть отказ, — поэтому событие уходит только
    /// в ленту подписчиков.
    fn fail_journal(&self) {
        let reason = self
            .journal
            .failure()
            .unwrap_or_else(|| "journal writer stopped".to_owned());
        let first = self.failure.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(reason.clone());
                true
            }
        });
        if !first {
            return;
        }
        let _ = self.events.send(Arc::new(Event::Error(ErrorEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn: None,
            message: format!("journal write failed: {reason}; no ACKs are sent after the failure"),
        })));
    }

    /// Пишет `close` ровно один раз: номер убирается из открытых под тем же замком.
    fn close_conn(&self, conn: u32, by: ClosedBy, reason: &str) {
        let Ok(mut open) = self.open.lock() else {
            // Замок отравлен: терять `close` всё равно нельзя.
            self.publish_close(conn, by, reason);
            return;
        };
        if !open.remove(&conn) {
            return;
        }
        self.publish_close(conn, by, reason);
    }

    /// Закрывает соединения, которые не успели закрыться сами после остановки.
    ///
    /// Вызывается после abort задач: `close` с причиной `stop` пишется всё равно, чтобы
    /// в журнале не осталось `open` без пары.
    fn close_leftover(&self) {
        let leftover: Vec<u32> = match self.open.lock() {
            Ok(mut open) => open.drain().collect(),
            Err(_) => Vec::new(),
        };
        for conn in leftover {
            self.publish_close(conn, ClosedBy::Bytetap, "stop");
        }
    }

    fn publish_close(&self, conn: u32, by: ClosedBy, reason: &str) {
        self.publish(Event::Close(Close {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            by,
            reason: reason.to_owned(),
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

/// Подряд идущие ошибки `accept`.
///
/// Одинаковые ошибки схлопываются: в журнал попадает одно событие `error`, а при
/// восстановлении — одно `info` со счётчиком повторов. Иначе занятый порт давал бы
/// две строки в секунду на весь журнал.
#[derive(Debug, Default)]
struct AcceptErrors {
    current: Option<(String, u32)>,
}

/// Что записать о новой ошибке `accept`.
#[derive(Debug)]
enum AcceptReport {
    /// Ошибка новая: нужен `error`.
    First(String),
    /// Ошибка сменилась: `info` о повторах прежней и `error` о новой.
    Changed {
        previous: String,
        repeats: u32,
        message: String,
    },
    /// Та же ошибка продолжается: писать нечего.
    Repeated,
}

impl AcceptErrors {
    /// Учитывает ошибку и говорит, что о ней писать.
    fn failed(&mut self, message: String) -> AcceptReport {
        match self.current.take() {
            Some((previous, repeats)) if previous == message => {
                self.current = Some((previous, repeats + 1));
                AcceptReport::Repeated
            }
            Some((previous, repeats)) => {
                self.current = Some((message.clone(), 0));
                AcceptReport::Changed {
                    previous,
                    repeats,
                    message,
                }
            }
            None => {
                self.current = Some((message.clone(), 0));
                AcceptReport::First(message)
            }
        }
    }

    /// Приём восстановился: отчёт о схлопнутых повторах, если они были.
    fn recovered(&mut self) -> Option<String> {
        self.current.take().and_then(|(previous, repeats)| {
            (repeats > 0)
                .then(|| format!("accept recovered after {repeats} repeated failures: {previous}"))
        })
    }
}

/// Следующая пауза после ошибки `accept`: удвоение до [`ACCEPT_BACKOFF_MAX`].
fn next_backoff(previous: Duration) -> Duration {
    (previous * 2).min(ACCEPT_BACKOFF_MAX)
}

/// Ждёт остановки сессии (или выгрузки её дескриптора).
async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow_and_update() {
        return;
    }
    // Ошибка `changed` значит, что отправитель выгружен: сессии больше нет.
    let _ = stop.changed().await;
}

/// Ждёт отказа журнала (или выгрузки дескриптора сессии).
async fn wait_for_failure(failure: &mut watch::Receiver<Option<String>>) {
    if failure.borrow_and_update().is_some() {
        return;
    }
    let _ = failure.changed().await;
}

/// Задача сессии: принимает соединения, пока не придёт остановка или отказ журнала.
async fn run(
    server: TcpServer,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
    mut failure: watch::Receiver<Option<String>>,
) -> io::Result<Stats> {
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut accept_errors = AcceptErrors::default();
    let mut backoff = ACCEPT_BACKOFF_MIN;
    loop {
        tokio::select! {
            accepted = server.accept() => match accepted {
                Ok(connection) => {
                    if let Some(note) = accept_errors.recovered() {
                        shared.info(None, note);
                    }
                    backoff = ACCEPT_BACKOFF_MIN;
                    let conn = shared.counters.next_conn();
                    if let Ok(mut open) = shared.open.lock() {
                        open.insert(conn);
                    }
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
                    let mut conn_failure = failure.clone();
                    tasks.spawn(async move {
                        serve(connection, conn, &shared, &mut conn_stop, &mut conn_failure).await;
                    });
                }
                Err(err) => {
                    // Ошибка приёма не роняет сессию (NFR-5): пишем её и слушаем дальше.
                    let message = format!("accept failed: {err}");
                    match accept_errors.failed(message) {
                        AcceptReport::Repeated => {}
                        AcceptReport::First(message) => shared.error(None, message),
                        AcceptReport::Changed { previous, repeats, message } => {
                            shared.info(
                                None,
                                format!(
                                    "accept error changed after {repeats} repeats of the previous one: {previous}"
                                ),
                            );
                            shared.error(None, message);
                        }
                    }
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {}
                        _ = wait_for_stop(&mut stop) => break,
                        _ = wait_for_failure(&mut failure) => break,
                    }
                    backoff = next_backoff(backoff);
                }
            },
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let _ = joined;
            },
            _ = wait_for_stop(&mut stop) => break,
            _ = wait_for_failure(&mut failure) => break,
        }
    }

    // Слушатель закрыт вместе с `server`; соединения видят остановку и пишут `close`.
    drop(server);
    let all_closed = timeout(CONNECTIONS_SHUTDOWN_TIMEOUT, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    if all_closed.is_err() {
        // Соединение не закрылось за отведённое время: задачи прерываются, `close` для
        // них пишется здесь, чтобы в журнале не осталось `open` без пары.
        tasks.shutdown().await;
        shared.close_leftover();
    }

    let stats = shared.counters.snapshot();
    let failed = failure
        .borrow()
        .clone()
        .or_else(|| shared.failure.borrow().clone());
    if let Some(reason) = failed {
        // `session_end` не пишется: журнал уже не принимает строк.
        let _ = shared.journal.shutdown().await;
        return Err(journal_failure_error(reason));
    }

    shared.publish(Event::SessionEnd(SessionEnd {
        t: now_rfc3339(),
        mono_us: shared.clock.now_us(),
        stats: stats.clone(),
    }));
    if let Err(err) = shared.journal.shutdown().await {
        // Журнал отказал на последней пачке: `session_end` мог не записаться.
        let reason = shared.journal.failure().unwrap_or_else(|| err.to_string());
        return Err(journal_failure_error(reason));
    }
    Ok(stats)
}

/// Обслуживает одно соединение до закрытия.
async fn serve(
    mut connection: TcpConnection,
    conn: u32,
    shared: &Shared,
    stop: &mut watch::Receiver<bool>,
    failure: &mut watch::Receiver<Option<String>>,
) {
    let mut responder = make_responder(shared.responder, &shared.ids);
    let mut buffer = vec![0u8; READ_BUFFER];
    let (by, reason) = loop {
        if failure.borrow().is_some() {
            break (ClosedBy::Bytetap, "stop");
        }
        tokio::select! {
            biased;
            _ = wait_for_stop(stop) => break (ClosedBy::Bytetap, "stop"),
            _ = wait_for_failure(failure) => break (ClosedBy::Bytetap, "stop"),
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
                    if let Some((by, reason)) = apply(
                        shared,
                        conn,
                        &mut connection,
                        responder.feed(payload, now),
                        stop,
                        failure,
                    )
                    .await
                    {
                        break (by, reason);
                    }
                }
                Err(err) => {
                    let (by, reason) = close_reason(&err);
                    shared.error(Some(conn), format!("connection read failed: {err}"));
                    break (by, reason);
                }
            },
        }
    };

    // Автоответчик не теряет недособранное состояние молча: отчёт идёт до `close`.
    let actions = responder.finish(timestamp().1);
    let _ = apply(shared, conn, &mut connection, actions, stop, failure).await;
    shared.close_conn(conn, by, reason);
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
/// только для того, что действительно отправлено. Запись в сокет идёт в `select!` с
/// остановкой, отказом журнала и таймаутом [`WRITE_TIMEOUT`]: клиент, который перестал
/// читать, не держит сессию. Возвращает причину закрытия, если ответ отправить не удалось.
async fn apply(
    shared: &Shared,
    conn: u32,
    connection: &mut TcpConnection,
    actions: Vec<Action>,
    stop: &mut watch::Receiver<bool>,
    failure: &mut watch::Receiver<Option<String>>,
) -> Option<(ClosedBy, &'static str)> {
    for action in actions {
        match action {
            Action::Send(payload) => {
                // После отказа журнала ответов нет: анализатор должен узнать о сбое
                // по таймауту, а не получить ACK, которого нет в журнале.
                if failure.borrow().is_some() {
                    return Some((ClosedBy::Bytetap, "stop"));
                }
                let write = async { timeout(WRITE_TIMEOUT, connection.write_all(&payload)).await };
                tokio::select! {
                    biased;
                    _ = wait_for_stop(stop) => return Some((ClosedBy::Bytetap, "stop")),
                    _ = wait_for_failure(failure) => return Some((ClosedBy::Bytetap, "stop")),
                    outcome = write => match outcome {
                        Err(_elapsed) => {
                            shared.error(
                                Some(conn),
                                format!(
                                    "answer write timed out after {} s",
                                    WRITE_TIMEOUT.as_secs()
                                ),
                            );
                            return Some((ClosedBy::Bytetap, "timeout"));
                        }
                        Ok(Err(err)) => {
                            let (by, reason) = close_reason(&err);
                            shared.error(Some(conn), format!("answer write failed: {err}"));
                            return Some((by, reason));
                        }
                        Ok(Ok(())) => {}
                    }
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
                if failure.borrow().is_some() {
                    return Some((ClosedBy::Bytetap, "stop"));
                }
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

    #[test]
    fn accept_backoff_grows_to_one_second() {
        let mut pause = ACCEPT_BACKOFF_MIN;
        assert_eq!(pause, Duration::from_millis(10), "первая пауза — 10 мс");
        pause = next_backoff(pause);
        assert_eq!(pause, Duration::from_millis(20));
        pause = next_backoff(pause);
        assert_eq!(pause, Duration::from_millis(40));
        for _ in 0..10 {
            pause = next_backoff(pause);
        }
        assert_eq!(pause, ACCEPT_BACKOFF_MAX, "предел паузы — 1 с");
    }

    #[test]
    fn identical_accept_errors_collapse_into_one_event_and_a_counter() {
        let mut errors = AcceptErrors::default();
        match errors.failed("accept failed: нет мест".to_owned()) {
            AcceptReport::First(message) => assert!(message.contains("нет мест"), "{message}"),
            other => panic!("первая ошибка должна попасть в журнал: {other:?}"),
        }
        assert!(
            matches!(
                errors.failed("accept failed: нет мест".to_owned()),
                AcceptReport::Repeated
            ),
            "повтор не пишется"
        );
        assert!(matches!(
            errors.failed("accept failed: нет мест".to_owned()),
            AcceptReport::Repeated
        ));
        assert_eq!(
            errors.recovered().as_deref(),
            Some("accept recovered after 2 repeated failures: accept failed: нет мест")
        );
        assert_eq!(
            errors.recovered(),
            None,
            "восстановление сообщается один раз"
        );
    }

    #[test]
    fn a_different_accept_error_is_reported_with_the_previous_count() {
        let mut errors = AcceptErrors::default();
        let _ = errors.failed("first".to_owned());
        let _ = errors.failed("first".to_owned());
        match errors.failed("second".to_owned()) {
            AcceptReport::Changed {
                previous,
                repeats,
                message,
            } => {
                assert_eq!(previous, "first");
                assert_eq!(repeats, 1);
                assert_eq!(message, "second");
            }
            other => panic!("смена ошибки не сообщена: {other:?}"),
        }
        assert_eq!(errors.recovered(), None, "у новой ошибки повторов не было");
    }

    #[test]
    fn journal_failure_error_keeps_its_reason() {
        let err = journal_failure_error("диск недоступен".to_owned());
        assert_eq!(journal_failure_reason(&err), Some("диск недоступен"));
        assert!(err.to_string().contains("запись в журнал прервана"));
        assert_eq!(
            journal_failure_reason(&io::Error::other("другая ошибка")),
            None
        );
    }
}
