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
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Local, SecondsFormat};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::journal::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, Host, InfoEvent, Journal,
    JournalFailureReason, Mode, MonoClock, Note, Open, Session as SessionHeader, SessionEnd, Stats,
    default_log_dir, io_message, log_dir_warning, now_rfc3339, session_file_name,
};
use crate::responder::{Action, ControlIdSource, Now, Responder, ResponderEvent, ResponderKind};
use crate::transport::{Connection, ConnectionSource, Next, Source, Transport};

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
///
/// Клиент, переставший читать, не должен задерживать остановку: ответ ему уже не нужен,
/// а журнал должен получить `close` и `session_end`. Две секунды — с запасом на медленную
/// машину, но меньше, чем анализатор ждёт ответа.
const CONNECTIONS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Сколько остановка ждёт, пока писатель журнала допишет пачку и зафиксирует файл.
///
/// Медленный диск, забитый буфер записи или сетевой путь могут держать `sync_data`
/// неопределённо долго. `stop` в таком случае возвращает ошибку «journal not flushed»,
/// а не висит вечно: журнал уже не изменится, и инженеру важнее получить управление.
const JOURNAL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Тестовый хук: предел ожидания записи журнала при остановке, миллисекунды.
///
/// Появляется только со сборкой feature `test-hooks`: проверка «остановка не ждёт
/// зависшую фиксацию» должна занимать миллисекунды, а не десять секунд.
#[cfg(feature = "test-hooks")]
pub static TEST_JOURNAL_SHUTDOWN_MS: AtomicU64 = AtomicU64::new(0);

/// Тестовый хук: сессия заканчивается сама после стольких событий.
///
/// `u64::MAX` — хук выключен. Нужен тесту интерфейса: сессия, закончившаяся сама без
/// отказа журнала, должна освобождать слот и останавливать таймер (U1-AC17).
#[cfg(feature = "test-hooks")]
pub static TEST_END_AFTER_EVENTS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Тестовый хук: задача сессии падает после стольких событий.
///
/// `u64::MAX` — хук выключен. Падение задачи — не отказ журнала: остановка вернёт
/// ошибку без причины журнала, и интерфейс покажет её как ошибку остановки (U1-AC17).
#[cfg(feature = "test-hooks")]
pub static TEST_PANIC_AFTER_EVENTS: AtomicU64 = AtomicU64::new(u64::MAX);

/// События, дошедшие до публикации: счётчик хуков [`TEST_END_AFTER_EVENTS`] и
/// [`TEST_PANIC_AFTER_EVENTS`].
#[cfg(feature = "test-hooks")]
static PUBLISHED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Пора закончить сессию самой: хук [`TEST_END_AFTER_EVENTS`] сработал.
#[cfg(feature = "test-hooks")]
fn test_hook_end() -> bool {
    let limit = TEST_END_AFTER_EVENTS.load(Ordering::Relaxed);
    limit != u64::MAX && PUBLISHED_EVENTS.load(Ordering::Relaxed) > limit
}

#[cfg(not(feature = "test-hooks"))]
fn test_hook_end() -> bool {
    false
}

/// Задача сессии должна упасть: хук [`TEST_PANIC_AFTER_EVENTS`] сработал.
#[cfg(feature = "test-hooks")]
fn test_hook_panic() -> bool {
    let limit = TEST_PANIC_AFTER_EVENTS.load(Ordering::Relaxed);
    limit != u64::MAX && PUBLISHED_EVENTS.load(Ordering::Relaxed) > limit
}

#[cfg(not(feature = "test-hooks"))]
fn test_hook_panic() -> bool {
    false
}

/// Настройки сессии: источник соединений, автоответчик, папка журналов, поля заголовка (FR-21).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Откуда берутся соединения: TCP-сервер, TCP-клиент или COM-порт.
    pub source: Source,
    /// Профиль автоответчика (FR-14).
    pub responder: Autoresponder,
    /// Отвечать `NAK` на неверную контрольную сумму ASTM (FR-17, M3-AC7).
    pub astm_nak_on_bad_checksum: bool,
    /// Папка журналов; `None` — [`default_log_dir`].
    pub log_dir: Option<PathBuf>,
    /// Модель анализатора для заголовка журнала.
    pub analyzer: Option<String>,
    /// Комментарий инженера для заголовка журнала.
    pub note: Option<String>,
}

impl SessionConfig {
    /// Сессия TCP-сервера: ждать анализатор на адресе с автоответчиком HL7/MLLP.
    ///
    /// Порт `0` — любой свободный (FR-1).
    #[must_use]
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            source: Source::TcpServer { listen },
            responder: Autoresponder::Hl7Mllp,
            astm_nak_on_bad_checksum: false,
            log_dir: None,
            analyzer: None,
            note: None,
        }
    }

    /// Сессия TCP-клиента: подключаться к анализатору-серверу (FR-2).
    ///
    /// `reconnect` — пауза между попытками; старт не требует доступного анализатора.
    #[must_use]
    pub fn tcp_client(addr: impl Into<String>, reconnect: Duration) -> Self {
        Self {
            source: Source::TcpClient {
                addr: addr.into(),
                reconnect,
            },
            responder: Autoresponder::Hl7Mllp,
            astm_nak_on_bad_checksum: false,
            log_dir: None,
            analyzer: None,
            note: None,
        }
    }

    /// Сессия COM-порта «вместо ЛИС» (FR-9).
    ///
    /// `reconnect` — пауза между попытками открыть порт после разрыва.
    #[must_use]
    pub fn serial(settings: crate::transport::serial::SerialSettings, reconnect: Duration) -> Self {
        Self {
            source: Source::Serial {
                settings,
                reconnect,
            },
            responder: Autoresponder::Hl7Mllp,
            astm_nak_on_bad_checksum: false,
            log_dir: None,
            analyzer: None,
            note: None,
        }
    }

    /// Источник соединений вместо адреса прослушивания.
    #[must_use]
    pub fn with_source(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    /// Профиль автоответчика (FR-14).
    #[must_use]
    pub fn with_responder(mut self, responder: Autoresponder) -> Self {
        self.responder = responder;
        self
    }

    /// Отвечать `NAK` на неверную контрольную сумму ASTM (FR-17).
    #[must_use]
    pub fn with_astm_nak(mut self, nak_on_bad_checksum: bool) -> Self {
        self.astm_nak_on_bad_checksum = nak_on_bad_checksum;
        self
    }

    /// Профиль автоответчика в виде, из которого строится автомат соединения (FR-14).
    fn responder_kind(&self) -> ResponderKind {
        match self.responder {
            Autoresponder::Auto => ResponderKind::Auto,
            Autoresponder::Hl7Mllp => ResponderKind::Hl7,
            Autoresponder::AstmE1381 => ResponderKind::Astm {
                nak_on_bad_checksum: self.astm_nak_on_bad_checksum,
            },
            Autoresponder::None => ResponderKind::None,
        }
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
    /// Ошибка источника (занятый порт, настройки или открытие COM-порта) — ошибка старта
    /// до создания файла. TCP-клиент стартует и без доступного анализатора (M3-AC1).
    ///
    /// # Errors
    ///
    /// Порт занят, настройки порта неверны, папка журналов недоступна, журнал не создан.
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

        // Источник готовится до журнала: его отказ не должен оставлять пустой файл.
        let mode = config.source.mode();
        let opened = config.source.open().await?;
        let local = opened.local;

        let started = Local::now();
        let mut header = SessionHeader::at(
            started,
            mode,
            config.responder,
            // `settings` — фактические настройки режима: у TCP-сервера это адрес, выбранный
            // системой при порте 0, у клиента — адрес анализатора и интервал, у COM — порт
            // и параметры (M3-AC16).
            config.source.settings(&local),
            Host::current(),
        );
        header.analyzer = config.analyzer.clone();
        header.note = config.note.clone();

        let path = log_dir.join(session_file_name(started, mode));
        let journal = Journal::create(&path, header)?;
        // Подписка до запуска задачи: отказ, случившийся до неё, не должен потеряться.
        let journal_failed = journal.failed();
        let path = journal.path().to_path_buf();

        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let feed = events.subscribe();
        let (stop_tx, stop_rx) = watch::channel(false);
        let (failure_tx, failure_rx) = watch::channel(None);
        let (notes_tx, notes_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            responder: config.responder_kind(),
            journal,
            clock: MonoClock::new(),
            events,
            ids: ControlIdSource::new(),
            counters: Arc::new(Counters::default()),
            failure: failure_tx,
            open: Mutex::new(HashSet::new()),
        });
        let task = tokio::spawn(run(
            opened.source,
            mode,
            Arc::clone(&shared),
            stop_rx,
            failure_rx,
            journal_failed,
            notes_rx,
        ));

        let handle = SessionHandle {
            local,
            path,
            warning,
            events: shared.events.clone(),
            failure: shared.failure.subscribe(),
            counters: SessionCounters(Arc::clone(&shared.counters)),
            stop: stop_tx,
            notes: notes_tx,
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
    local: String,
    path: PathBuf,
    warning: Option<String>,
    events: broadcast::Sender<SessionEvent>,
    failure: watch::Receiver<Option<JournalFailureReason>>,
    counters: SessionCounters,
    stop: watch::Sender<bool>,
    notes: mpsc::UnboundedSender<String>,
    task: Option<tokio::task::JoinHandle<io::Result<Stats>>>,
}

/// Снимок счётчиков идущей сессии.
///
/// Живёт отдельно от [`SessionHandle`], потому что ленте нужен только он: мост кладёт
/// счётчики в каждую пачку, и строка состояния не считает байты по событиям, которые
/// могли быть отброшены (M2-AC16).
#[derive(Debug, Clone)]
pub struct SessionCounters(Arc<Counters>);

impl SessionCounters {
    /// Текущие соединения и байты по направлениям.
    #[must_use]
    pub fn snapshot(&self) -> Stats {
        self.0.snapshot()
    }

    /// Пустые счётчики: тесты ленты и команды без запущенной сессии.
    #[must_use]
    pub fn detached() -> Self {
        Self(Arc::new(Counters::default()))
    }
}

impl SessionHandle {
    /// Что показать в строке состояния.
    ///
    /// Для TCP-сервера — фактический адрес слушателя (при порте `0` выбран системой),
    /// для TCP-клиента — адрес анализатора, для COM — имя порта (M3-AC16, контракт моста).
    #[must_use]
    pub fn local_addr(&self) -> &str {
        &self.local
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
    pub fn journal_failure(&self) -> watch::Receiver<Option<JournalFailureReason>> {
        self.failure.clone()
    }

    /// Счётчики сессии: соединения и байты по направлениям (FR-25).
    #[must_use]
    pub fn counters(&self) -> SessionCounters {
        self.counters.clone()
    }

    /// Задача сессии уже завершилась.
    ///
    /// `true` после отказа журнала и после [`SessionHandle::stop`]: мост по этой метке
    /// освобождает слот, если сессия остановилась сама (NFR-5).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    /// Комментарий инженера в идущую сессию: событие `note` в журнал и ленту (FR-23).
    ///
    /// Порядок строк журнала — порядок вызовов: комментарий стоит между событиями,
    /// которые шли до и после него. Поле `mono_us` у такого комментария есть: сессия
    /// ещё идёт.
    ///
    /// # Errors
    ///
    /// Сессия уже остановлена: класть событие некуда.
    pub fn note(&self, text: impl Into<String>) -> io::Result<()> {
        self.notes.send(text.into()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "сессия уже остановлена: комментарий не записан",
            )
        })
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
            .field("local_addr", &self.local)
            .field("path", &self.path)
            .field("stopped", &self.task.is_none())
            .finish()
    }
}

/// Отказ журнала: сессия остановлена, потому что события больше не записываются.
#[derive(Debug)]
struct JournalFailure {
    reason: JournalFailureReason,
}

impl std::fmt::Display for JournalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "запись в журнал прервана: {}", self.reason)
    }
}

impl std::error::Error for JournalFailure {}

/// Ошибка сессии с причиной отказа журнала.
fn journal_failure_error(reason: JournalFailureReason) -> io::Error {
    io::Error::other(JournalFailure { reason })
}

/// Причина отказа журнала, если [`SessionHandle::stop`] вернул ошибку из-за него.
///
/// Нужна `bytetap-cli capture`, чтобы напечатать своё сообщение об остановке сессии,
/// а не общую ошибку ввода-вывода.
#[must_use]
pub fn journal_failure_reason(err: &io::Error) -> Option<&JournalFailureReason> {
    err.get_ref()?
        .downcast_ref::<JournalFailure>()
        .map(|failure| &failure.reason)
}

/// То, что нужно каждому соединению: журнал, часы, автоответчик, подписчики и счётчики.
struct Shared {
    responder: ResponderKind,
    journal: Journal,
    clock: MonoClock,
    events: broadcast::Sender<SessionEvent>,
    ids: ControlIdSource,
    counters: Arc<Counters>,
    /// Причина отказа журнала: как только она появилась, сессия перестаёт отвечать.
    failure: watch::Sender<Option<JournalFailureReason>>,
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
        #[cfg(feature = "test-hooks")]
        PUBLISHED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }

    /// Ошибка и замечание уровня сессии или соединения (NFR-5).
    ///
    /// Сообщения в журнале — английские, независимо от источника: их читает и человек,
    /// и поиск по журналу (`docs/reference/session-format.md`). Русский — только интерфейс.
    /// Ошибка с кодом для интерфейса: `code` остаётся `None`, если перевода нет.
    fn error_with_code(&self, conn: Option<u32>, code: Option<&str>, message: impl Into<String>) {
        self.publish(Event::Error(ErrorEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            code: code.map(str::to_owned),
            message: message.into(),
        }));
    }

    fn info(&self, conn: Option<u32>, message: impl Into<String>) {
        self.info_with_code(conn, None, message);
    }

    fn info_with_code(&self, conn: Option<u32>, code: Option<&str>, message: impl Into<String>) {
        self.publish(Event::Info(InfoEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            code: code.map(str::to_owned),
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
            .unwrap_or_else(|| JournalFailureReason::from_message("journal writer stopped"));
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
            code: Some("journal_failure".to_owned()),
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

/// Подряд идущие ошибки источника: `accept`, подключения TCP-клиента, открытия порта.
///
/// Одинаковые ошибки схлопываются: в журнал попадает одно событие `error`, а при
/// восстановлении — одно `info` со счётчиком повторов. Иначе недоступный анализатор
/// давал бы строку в журнал каждые несколько секунд на весь журнал (M3-AC1, M3-AC2).
#[derive(Debug, Default)]
struct SourceErrors {
    current: Option<(String, u32)>,
}

/// Что записать о новой ошибке источника.
#[derive(Debug)]
enum SourceReport {
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

impl SourceErrors {
    /// Учитывает ошибку и говорит, что о ней писать.
    fn failed(&mut self, message: String) -> SourceReport {
        match self.current.take() {
            Some((previous, repeats)) if previous == message => {
                self.current = Some((previous, repeats.saturating_add(1)));
                SourceReport::Repeated
            }
            Some((previous, repeats)) => {
                self.current = Some((message.clone(), 0));
                SourceReport::Changed {
                    previous,
                    repeats,
                    message,
                }
            }
            None => {
                self.current = Some((message.clone(), 0));
                SourceReport::First(message)
            }
        }
    }

    /// Приём восстановился: отчёт о схлопнутых повторах, если они были.
    fn recovered(&mut self, prefix: &str) -> Option<String> {
        self.current.take().and_then(|(previous, repeats)| {
            (repeats > 0).then(|| {
                format!("{prefix} recovered after {repeats} repeated failures: {previous}")
            })
        })
    }

    /// Серия не закончилась к остановке: отчёт о повторах перед `session_end`.
    ///
    /// Без него последняя серия ошибок выглядела бы в журнале одной ошибкой:
    /// повторы схлопнуты, а восстановления, о котором сообщает `info`, уже не будет.
    fn stopped(&mut self, prefix: &str) -> Option<String> {
        self.current.take().and_then(|(previous, repeats)| {
            (repeats > 0).then(|| {
                format!(
                    "{prefix} still failing after {repeats} repeated failures at the stop: {previous}"
                )
            })
        })
    }
}

/// Как назвать ошибку источника в журнале и какой код дать интерфейсу.
fn source_failure(mode: Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::TcpServer => ("accept failed", "accept_failed"),
        Mode::TcpClient => ("connect failed", "connect_failed"),
        Mode::Serial => ("serial port open failed", "serial_open_failed"),
        Mode::Proxy | Mode::ComMonitor | Mode::ComTcp => ("source failed", "source_failed"),
    }
}

/// Ждёт остановки сессии (или выгрузки её дескриптора).
async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow_and_update() {
        return;
    }
    // Ошибка `changed` значит, что отправитель выгружен: сессии больше нет.
    let _ = stop.changed().await;
}

/// Срок автомата от текущего момента; `None` — автомат ждёт только данных.
fn responder_deadline(responder: &mut (impl Responder + ?Sized)) -> Option<tokio::time::Instant> {
    responder
        .poll_timeout(timestamp().1)
        .map(|after| tokio::time::Instant::now() + after)
}

/// Ждёт срок автомата; без срока не наступает никогда.
async fn wait_for_deadline(deadline: &mut Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(*at).await,
        None => std::future::pending().await,
    }
}

/// Ждёт отказа журнала (или выгрузки дескриптора сессии).
async fn wait_for_failure(failure: &mut watch::Receiver<Option<JournalFailureReason>>) {
    if failure.borrow_and_update().is_some() {
        return;
    }
    let _ = failure.changed().await;
}

/// Задача сессии: берёт соединения из источника, пока не придёт остановка или отказ журнала.
async fn run(
    mut source: Box<dyn ConnectionSource>,
    mode: Mode,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
    mut failure: watch::Receiver<Option<JournalFailureReason>>,
    mut journal_failed: watch::Receiver<Option<JournalFailureReason>>,
    mut notes: mpsc::UnboundedReceiver<String>,
) -> io::Result<Stats> {
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut source_errors = SourceErrors::default();
    let sequential = source.sequential();
    let (prefix, code) = source_failure(mode);
    let mut wait_before_next = false;
    loop {
        // Тестовый хук: сессия заканчивается сама. Обычной сборки здесь нет — хук
        // живёт только в сборке с `test-hooks`.
        if test_hook_end() {
            break;
        }
        if test_hook_panic() {
            panic!("test hook: the session task failed");
        }
        // TCP-клиент и COM-порт не открывают второе соединение, пока живо первое,
        // и выдерживают интервал после разрыва (M3-AC1, M3-AC2).
        if wait_before_next {
            wait_before_next = false;
            let pause = source.pause_between_connections();
            if !pause.is_zero() {
                tokio::select! {
                    () = tokio::time::sleep(pause) => {}
                    _ = wait_for_stop(&mut stop) => break,
                    _ = wait_for_failure(&mut failure) => break,
                    _ = wait_for_failure(&mut journal_failed) => {
                        shared.fail_journal();
                        break;
                    }
                }
            }
        }
        tokio::select! {
            next = source.next(), if !sequential || tasks.is_empty() => match next {
                Next::Connected(connection) => {
                    if let Some(note) = source_errors.recovered(prefix) {
                        shared.info(None, note);
                    }
                    let conn = shared.counters.next_conn();
                    if let Ok(mut open) = shared.open.lock() {
                        open.insert(conn);
                    }
                    // `open` пишется здесь, а не в задаче соединения: так порядок строк
                    // совпадает с порядком номеров, и открытие не может оказаться после
                    // первых фрагментов этого же соединения.
                    let Connection { transport, peer, local } = connection;
                    shared.publish(Event::Open(Open {
                        t: now_rfc3339(),
                        mono_us: shared.clock.now_us(),
                        conn,
                        peer,
                        local,
                    }));
                    let shared = Arc::clone(&shared);
                    let mut conn_stop = stop.clone();
                    let mut conn_failure = failure.clone();
                    tasks.spawn(async move {
                        let mut transport = transport;
                        serve(transport.as_mut(), conn, &shared, &mut conn_stop, &mut conn_failure)
                            .await;
                    });
                }
                Next::Retry { error, after } => {
                    // Неудачное подключение не роняет сессию (NFR-5, M3-AC1, M3-AC2):
                    // одно `error`, повторы схлопываются, ожидание прерывается «Стопом».
                    let message = format!("{prefix}: {}", io_message(&error));
                    match source_errors.failed(message) {
                        SourceReport::Repeated => {}
                        SourceReport::First(message) => {
                            shared.error_with_code(None, Some(code), message);
                        }
                        SourceReport::Changed { previous, repeats, message } => {
                            shared.info(
                                None,
                                format!(
                                    "source error changed after {repeats} repeats of the previous one: {previous}"
                                ),
                            );
                            shared.error_with_code(None, Some(code), message);
                        }
                    }
                    tokio::select! {
                        () = tokio::time::sleep(after) => {}
                        _ = wait_for_stop(&mut stop) => break,
                        _ = wait_for_failure(&mut failure) => break,
                        _ = wait_for_failure(&mut journal_failed) => {
                            shared.fail_journal();
                            break;
                        }
                    }
                }
            },
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let _ = joined;
                if tasks.is_empty() && sequential {
                    wait_before_next = true;
                }
            },
            Some(text) = notes.recv() => {
                // Комментарий идёт в журнал и ленту как обычное событие сессии (FR-23).
                shared.publish(Event::Note(Note {
                    t: now_rfc3339(),
                    mono_us: Some(shared.clock.now_us()),
                    text,
                }));
            },
            _ = wait_for_stop(&mut stop) => break,
            _ = wait_for_failure(&mut failure) => break,
            // Отказ писателя виден без следующего события: анализатор мог замолчать.
            _ = wait_for_failure(&mut journal_failed) => {
                shared.fail_journal();
                break;
            }
        }
    }

    // Источник закрыт: у TCP-сервера вместе с ним закрывается слушатель.
    drop(source);
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
        let _ = shutdown_journal(&shared.journal).await;
        return Err(journal_failure_error(reason));
    }

    // Серия ошибок источника, не закончившаяся к остановке: повторы не должны пропасть
    // из журнала только потому, что восстановления не было (NFR-5).
    if let Some(note) = source_errors.stopped(prefix) {
        shared.info(None, note);
    }
    // Комментарий, отправленный перед «Стоп», не теряется: очередь дочитывается
    // до `session_end` (FR-23). Без этого `note` мог остаться в канале и пропасть.
    while let Ok(text) = notes.try_recv() {
        shared.publish(Event::Note(Note {
            t: now_rfc3339(),
            mono_us: Some(shared.clock.now_us()),
            text,
        }));
    }
    shared.publish(Event::SessionEnd(SessionEnd {
        t: now_rfc3339(),
        mono_us: shared.clock.now_us(),
        stats: stats.clone(),
    }));
    if let Err(err) = shutdown_journal(&shared.journal).await {
        // Журнал отказал на последней пачке: `session_end` мог не записаться.
        let reason = shared
            .journal
            .failure()
            .unwrap_or_else(|| JournalFailureReason::from_error(&err));
        return Err(journal_failure_error(reason));
    }
    Ok(stats)
}

/// Предел ожидания остановки писателя: без него зависшая фиксация держала бы `stop` вечно.
#[cfg(feature = "test-hooks")]
fn journal_shutdown_timeout() -> Duration {
    let millis = TEST_JOURNAL_SHUTDOWN_MS.load(Ordering::Relaxed);
    if millis > 0 {
        Duration::from_millis(millis)
    } else {
        JOURNAL_SHUTDOWN_TIMEOUT
    }
}

#[cfg(not(feature = "test-hooks"))]
fn journal_shutdown_timeout() -> Duration {
    JOURNAL_SHUTDOWN_TIMEOUT
}

/// Останавливает писателя журнала не дольше [`journal_shutdown_timeout`].
///
/// По истечении предела возвращается ошибка «journal not flushed»: данные, не дошедшие
/// до диска, уже не появятся, а вызывающий должен узнать, что журнал не полон.
async fn shutdown_journal(journal: &Journal) -> io::Result<()> {
    let pause = journal_shutdown_timeout();
    match timeout(pause, journal.shutdown()).await {
        Ok(result) => result,
        Err(_elapsed) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("journal not flushed: the writer did not stop within {pause:?}"),
        )),
    }
}

/// Обслуживает одно соединение до закрытия.
///
/// Функция не знает, TCP это или COM-порт: транспорт — `AsyncRead + AsyncWrite`
/// (M3-AC12). Всё остальное — нумерация, журнал, автоответчик — общее для всех режимов.
async fn serve(
    transport: &mut dyn Transport,
    conn: u32,
    shared: &Shared,
    stop: &mut watch::Receiver<bool>,
    failure: &mut watch::Receiver<Option<JournalFailureReason>>,
) {
    let mut responder = shared.responder.build(shared.ids.clone());
    let mut buffer = vec![0u8; READ_BUFFER];
    // Срок автомата: ASTM сообщает по нему о недособранном кадре (FR-17).
    let mut deadline = responder_deadline(responder.as_mut());
    let (by, reason) = loop {
        if failure.borrow().is_some() {
            break (ClosedBy::Bytetap, "stop");
        }
        tokio::select! {
            biased;
            _ = wait_for_stop(stop) => break (ClosedBy::Bytetap, "stop"),
            _ = wait_for_failure(failure) => break (ClosedBy::Bytetap, "stop"),
            read = transport.read(&mut buffer) => match read {
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
                    let actions = responder.feed(payload, now);
                    deadline = responder_deadline(responder.as_mut());
                    if let Some((by, reason)) =
                        apply(shared, conn, transport, actions, stop, failure).await
                    {
                        break (by, reason);
                    }
                }
                Err(err) => {
                    let (by, reason) = close_reason(&err);
                    shared.error_with_code(
                        Some(conn),
                        Some("connection_read_failed"),
                        format!("connection read failed: {}", io_message(&err)),
                    );
                    break (by, reason);
                }
            },
            // Срок автомата истёк: пустой фрагмент будит его, отчёт уходит в журнал.
            _ = wait_for_deadline(&mut deadline) => {
                let actions = responder.feed(&[], timestamp().1);
                deadline = responder_deadline(responder.as_mut());
                if let Some((by, reason)) =
                    apply(shared, conn, transport, actions, stop, failure).await
                {
                    break (by, reason);
                }
            },
        }
    };

    // Автоответчик не теряет недособранное состояние молча: отчёт идёт до `close`.
    let actions = responder.finish(timestamp().1);
    let _ = apply(shared, conn, transport, actions, stop, failure).await;
    shared.close_conn(conn, by, reason);
    let _ = transport.shutdown().await;
}

/// Выполняет действия автоответчика по порядку: ответ — в сокет, затем в журнал.
///
/// Ответ уходит в сокет до записи в журнал: событие `data` с `synthetic: true` появляется
/// только для того, что действительно отправлено. Если ответ оборвался на середине
/// (таймаут, сброс), в журнал попадает отправленная часть — анализатор её получил.
/// Запись в сокет идёт в `select!` с остановкой, отказом журнала и таймаутом
/// [`WRITE_TIMEOUT`]: клиент, который перестал читать, не держит сессию. Возвращает
/// причину закрытия, если ответ отправить не удалось.
async fn apply(
    shared: &Shared,
    conn: u32,
    transport: &mut dyn Transport,
    actions: Vec<Action>,
    stop: &mut watch::Receiver<bool>,
    failure: &mut watch::Receiver<Option<JournalFailureReason>>,
) -> Option<(ClosedBy, &'static str)> {
    for action in actions {
        match action {
            Action::Send(payload) => {
                // После отказа журнала ответов нет: анализатор должен узнать о сбое
                // по таймауту, а не получить ACK, которого нет в журнале.
                if failure.borrow().is_some() {
                    return Some((ClosedBy::Bytetap, "stop"));
                }
                let sent = AtomicUsize::new(0);
                let write = write_counted(transport, &payload, &sent);
                let outcome = tokio::select! {
                    biased;
                    _ = wait_for_stop(stop) => {
                        // Остановка посреди ответа: отправленное уже не отменить.
                        record_answer(shared, conn, &payload, sent.load(Ordering::Relaxed));
                        return Some((ClosedBy::Bytetap, "stop"));
                    }
                    _ = wait_for_failure(failure) => return Some((ClosedBy::Bytetap, "stop")),
                    outcome = timeout(WRITE_TIMEOUT, write) => outcome,
                };
                match outcome {
                    Err(_elapsed) => {
                        record_answer(shared, conn, &payload, sent.load(Ordering::Relaxed));
                        shared.error_with_code(
                            Some(conn),
                            Some("answer_timeout"),
                            format!("answer write timed out after {} s", WRITE_TIMEOUT.as_secs()),
                        );
                        return Some((ClosedBy::Bytetap, "timeout"));
                    }
                    Ok(Err(err)) => {
                        record_answer(shared, conn, &payload, sent.load(Ordering::Relaxed));
                        let (by, reason) = close_reason(&err);
                        shared.error_with_code(
                            Some(conn),
                            Some("connection_write_failed"),
                            format!("answer write failed: {}", io_message(&err)),
                        );
                        return Some((by, reason));
                    }
                    Ok(Ok(())) => record_answer(shared, conn, &payload, payload.len()),
                }
                if failure.borrow().is_some() {
                    return Some((ClosedBy::Bytetap, "stop"));
                }
            }
            Action::Event(event) => {
                let (t, _) = timestamp();
                let mono_us = shared.clock.now_us();
                let event = match event {
                    ResponderEvent::Error { message, code } => Event::Error(ErrorEvent {
                        t,
                        mono_us,
                        conn: Some(conn),
                        code: code.map(|code| code.as_str().to_owned()),
                        message,
                    }),
                    ResponderEvent::Info { message, code } => Event::Info(InfoEvent {
                        t,
                        mono_us,
                        conn: Some(conn),
                        code: code.map(|code| code.as_str().to_owned()),
                        message,
                    }),
                };
                shared.publish(event);
            }
        }
    }
    None
}

/// Пишет ответ в сокет и считает фактически отправленные байты.
///
/// `write_all` не говорит, сколько байт успело уйти. Счётчик [`AtomicUsize`] общий
/// с вызывающим: если запись прервёт таймаут или остановка, отправленная часть
/// останется в счётчике и попадёт в журнал — анализатор её получил.
async fn write_counted(
    transport: &mut dyn Transport,
    payload: &[u8],
    sent: &AtomicUsize,
) -> io::Result<()> {
    loop {
        let written = sent.load(Ordering::Relaxed);
        if written >= payload.len() {
            return Ok(());
        }
        let accepted = transport.write(&payload[written..]).await?;
        if accepted == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the socket accepted no bytes of the answer",
            ));
        }
        sent.fetch_add(accepted, Ordering::Relaxed);
    }
}

/// Записывает фактически отправленную часть ответа: байты, статистику и событие `data`.
///
/// Пустая часть не пишется: ответа, которого не было, в журнале быть не должно.
fn record_answer(shared: &Shared, conn: u32, payload: &[u8], sent: usize) {
    let sent = sent.min(payload.len());
    if sent == 0 {
        return;
    }
    shared.counters.add(Dir::LisToAnalyzer, sent as u64);
    shared.publish(Event::Data(Data::new(
        timestamp().0,
        shared.clock.now_us(),
        conn,
        Dir::LisToAnalyzer,
        true,
        &payload[..sent],
    )));
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
    use crate::journal::read_all;
    use crate::journal::writer::append_note_after_stop;
    use chrono::{DateTime, FixedOffset};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("временная папка")
    }

    #[tokio::test]
    async fn every_responder_profile_starts_and_is_written_to_the_header() {
        for responder in [
            Autoresponder::Hl7Mllp,
            Autoresponder::AstmE1381,
            Autoresponder::Auto,
            Autoresponder::None,
        ] {
            let dir = temp_dir();
            let config = SessionConfig::new("127.0.0.1:0".parse().unwrap())
                .with_responder(responder)
                .with_log_dir(dir.path());
            let handle = Session::start(config)
                .await
                .unwrap_or_else(|err| panic!("{responder}: {err}"));
            let path = handle.journal_path().to_path_buf();
            handle.stop().await.expect("остановка");
            let outcome = read_all(&path).expect("чтение журнала");
            let Event::Session(header) = &outcome.lines[0].event else {
                panic!("первая строка — заголовок");
            };
            assert_eq!(header.autoresponder, responder);
        }
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
        let addr = handle.local_addr().to_owned();
        assert_ne!(addr.parse::<SocketAddr>().unwrap().port(), 0);
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
        // Дескриптор писателя закрывается вместе с задачей, а не с ответом на остановку,
        // и `flock` снимается на доли миллисекунды позже. Ждём освобождения, как в тестах
        // писателя: отказ «файл занят» при живой сессии по-прежнему виден сразу.
        append_note_after_stop(&path, "после остановки");
    }

    #[tokio::test]
    async fn a_note_of_a_running_session_goes_to_the_journal_and_the_feed() {
        let dir = temp_dir();
        let (handle, mut feed) = Session::start_with_feed(
            SessionConfig::new("127.0.0.1:0".parse().unwrap())
                .with_responder(Autoresponder::None)
                .with_log_dir(dir.path()),
        )
        .await
        .expect("сессия");
        let path = handle.journal_path().to_path_buf();

        handle.note("прогнали три пробы").expect("комментарий");
        let event = tokio::time::timeout(Duration::from_secs(1), feed.recv())
            .await
            .expect("событие в ленте")
            .expect("лента не закрыта");
        let Event::Note(note) = event.as_ref() else {
            panic!("ожидался note, получено {}", event.kind());
        };
        assert_eq!(note.text, "прогнали три пробы");
        assert!(
            note.mono_us.is_some(),
            "у комментария идущей сессии есть mono_us"
        );

        handle.stop().await.expect("остановка");
        let outcome = read_all(&path).expect("чтение журнала");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let kinds: Vec<&str> = outcome.lines.iter().map(|line| line.event.kind()).collect();
        assert_eq!(kinds, ["session", "note", "session_end"]);
    }

    /// Комментарий, поставленный в очередь прямо перед «Стоп», не теряется.
    #[tokio::test]
    async fn a_note_sent_right_before_the_stop_reaches_the_journal() {
        let dir = temp_dir();
        let handle = Session::start(
            SessionConfig::new("127.0.0.1:0".parse().unwrap())
                .with_responder(Autoresponder::None)
                .with_log_dir(dir.path()),
        )
        .await
        .expect("сессия");
        let path = handle.journal_path().to_path_buf();

        handle.note("перед остановкой").expect("комментарий");
        handle.stop().await.expect("остановка");

        let outcome = read_all(&path).expect("чтение журнала");
        let kinds: Vec<&str> = outcome.lines.iter().map(|line| line.event.kind()).collect();
        assert_eq!(
            kinds,
            ["session", "note", "session_end"],
            "очередь комментариев дочитана до session_end"
        );
        let Event::Note(note) = &outcome.lines[1].event else {
            panic!("вторая строка — комментарий");
        };
        assert_eq!(note.text, "перед остановкой");
    }

    /// Счётчики доступны ленте, пока сессия идёт: строка состояния не считает по событиям.
    #[tokio::test]
    async fn counters_are_visible_to_the_feed() {
        let dir = temp_dir();
        let handle = Session::start(
            SessionConfig::new("127.0.0.1:0".parse().unwrap())
                .with_responder(Autoresponder::None)
                .with_log_dir(dir.path()),
        )
        .await
        .expect("сессия");
        let counters = handle.counters();
        assert_eq!(counters.snapshot().connections, 0);

        let mut client = tokio::net::TcpStream::connect(handle.local_addr())
            .await
            .expect("анализатор");
        tokio::io::AsyncWriteExt::write_all(&mut client, b"MSH|^~\\&\r")
            .await
            .expect("фрагмент");
        tokio::io::AsyncWriteExt::shutdown(&mut client)
            .await
            .expect("закрытие");

        for _ in 0..100 {
            if counters.snapshot().bytes_analyzer_to_lis > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let stats = counters.snapshot();
        handle.stop().await.expect("остановка");
        assert_eq!(stats.connections, 1);
        assert_eq!(stats.bytes_analyzer_to_lis, 9);
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
    fn accepts_and_connections_are_named_by_mode() {
        assert_eq!(
            source_failure(Mode::TcpServer),
            ("accept failed", "accept_failed")
        );
        assert_eq!(
            source_failure(Mode::TcpClient),
            ("connect failed", "connect_failed")
        );
        assert_eq!(
            source_failure(Mode::Serial),
            ("serial port open failed", "serial_open_failed")
        );
    }

    #[test]
    fn identical_source_errors_collapse_into_one_event_and_a_counter() {
        let mut errors = SourceErrors::default();
        match errors.failed("accept failed: нет мест".to_owned()) {
            SourceReport::First(message) => assert!(message.contains("нет мест"), "{message}"),
            other => panic!("первая ошибка должна попасть в журнал: {other:?}"),
        }
        assert!(
            matches!(
                errors.failed("accept failed: нет мест".to_owned()),
                SourceReport::Repeated
            ),
            "повтор не пишется"
        );
        assert!(matches!(
            errors.failed("accept failed: нет мест".to_owned()),
            SourceReport::Repeated
        ));
        assert_eq!(
            errors.recovered("accept").as_deref(),
            Some("accept recovered after 2 repeated failures: accept failed: нет мест")
        );
        assert_eq!(
            errors.recovered("accept"),
            None,
            "восстановление сообщается один раз"
        );
    }

    #[test]
    fn a_series_unfinished_at_the_stop_is_reported_with_its_count() {
        let mut errors = SourceErrors::default();
        let _ = errors.failed("accept failed: port taken".to_owned());
        let _ = errors.failed("accept failed: port taken".to_owned());

        // Серия не восстановилась до остановки: счётчик повторов не теряется.
        assert_eq!(
            errors.stopped("accept").as_deref(),
            Some(
                "accept still failing after 1 repeated failures at the stop: accept failed: port taken"
            )
        );
        assert_eq!(errors.stopped("accept"), None, "серия сообщается один раз");

        // Одиночная ошибка уже в журнале: отчёт о нуле повторов не нужен.
        let mut single = SourceErrors::default();
        let _ = single.failed("accept failed: port taken".to_owned());
        assert_eq!(single.stopped("accept"), None);
    }

    #[test]
    fn a_different_source_error_is_reported_with_the_previous_count() {
        let mut errors = SourceErrors::default();
        let _ = errors.failed("first".to_owned());
        let _ = errors.failed("first".to_owned());
        match errors.failed("second".to_owned()) {
            SourceReport::Changed {
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
        assert_eq!(
            errors.recovered("accept"),
            None,
            "у новой ошибки повторов не было"
        );
    }

    #[test]
    fn journal_failure_error_keeps_its_reason() {
        let err = journal_failure_error(JournalFailureReason::from_message("диск недоступен"));
        assert_eq!(
            journal_failure_reason(&err).map(JournalFailureReason::message),
            Some("диск недоступен")
        );
        assert!(err.to_string().contains("запись в журнал прервана"));
        assert_eq!(
            journal_failure_reason(&io::Error::other("другая ошибка")),
            None
        );
    }
}
