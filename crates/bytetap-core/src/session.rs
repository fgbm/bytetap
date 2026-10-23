//! Сессия записи: слушает TCP, ведёт соединения, пишет журнал и рассылает события (FR-1…FR-5).
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
//! Режим прокси — тот же код сессии с другим источником: пара сокетов приходит одним
//! [`Connection`] с заполненным [`Connection::link`], обслуживает её [`serve_proxy`] рядом
//! с [`serve`]. Нумерация `conn`, счётчики, журнал, схлопывание ошибок, комментарий
//! и остановка общие (M4-AC16).
//!
//! Соединения нумеруются с 1 сквозным счётчиком сессии. Параллельные соединения
//! обслуживаются независимо, последовательные получают новый номер. Ошибка в соединении
//! закрывает только его (NFR-5): сессия пишет `error`, `close` и продолжает слушать.
//!
//! Отказ журнала останавливает сессию: [`Journal::record`], вернувший `false`, значит, что
//! события больше не попадают на диск. Сессия рассылает подписчикам `error` с причиной,
//! перестаёт отвечать анализатору (ни одного ACK после отказа) и в прокси перестаёт
//! пересылать байты, закрывает соединения и завершается без `session_end`;
//! [`SessionHandle::stop`] возвращает ошибку с причиной (ADR 0001, «Ошибка записи»).
//!
//! Автоответчик у каждого соединения свой ([`Responder`]), а источник `MSH-10` — общий на
//! сессию ([`ControlIdSource`]): номера ответов не повторяются между соединениями.
//! В прокси автоответчика нет вообще: байты идут без изменений (FR-6).

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Local, SecondsFormat};
use tokio::io::{AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::journal::writer::max_pending_bytes;
use crate::journal::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, Host, InfoEvent, Journal,
    JournalFailureReason, LinkRole, Mode, MonoClock, Note, Open, Session as SessionHeader,
    SessionEnd, Stats, default_log_dir, io_message, log_dir_warning, now_rfc3339,
    session_file_name,
};
use crate::responder::{Action, ControlIdSource, Now, Responder, ResponderEvent, ResponderKind};
use crate::transport::{
    Connection, ConnectionSource, Link, Next, ProxyRole, ResetSocket, Source, Stage, Transport,
    is_proxy_peer_rejected, is_proxy_target_is_self, rejected_peer,
};

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

/// Сколько пара прокси доживает после `FIN` одной стороны, прежде чем закрыться сама (FR-7).
///
/// Протоколы, где сторона полузакрывается после запроса и ждёт ответа, не должны рваться
/// там, где без прокси работали: обратное направление доживает до своего `FIN`, но не
/// дольше пяти секунд — иначе зависшая сторона держала бы пару вечно (M4-В7).
///
/// Открыт наружу: интерфейс называет тот же срок в строке закрытия
/// (`feed.closedPairAppTimeout`), а второй копии числа быть не должно.
pub const PROXY_LINGER: Duration = Duration::from_secs(5);

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

/// Сколько событий дошло до публикации за всё время процесса.
///
/// Тест ставит предел хука от этого значения («ещё N событий»): счётчик общий на
/// процесс, и предел, отсчитанный от нуля, сработал бы сразу после соседнего теста.
#[cfg(feature = "test-hooks")]
pub fn test_published_events() -> u64 {
    PUBLISHED_EVENTS.load(Ordering::Relaxed)
}

/// Сбрасывает счётчик хуков: его возвращает в исходное состояние гард теста.
#[cfg(feature = "test-hooks")]
pub fn reset_test_published_events() {
    PUBLISHED_EVENTS.store(0, Ordering::Relaxed);
}

/// Тестовый хук: задержка выхода насоса пары после `FIN`, миллисекунды.
///
/// Появляется только со сборкой feature `test-hooks`. Выход насоса и так идёт после
/// `shutdown` обратной стороны, и на загруженном раннере задача первого закрывшегося
/// насоса возвращается не первой; задержка делает этот порядок проверяемым (M4-AC5).
#[cfg(feature = "test-hooks")]
pub static TEST_PUMP_EOF_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// Тестовый хук: чья сторона пары задерживается на [`TEST_PUMP_EOF_DELAY_MS`].
///
/// `1` — сторона анализатора, `2` — сторона ЛИС, `0` — хук выключен: задержка нужна той
/// стороне, которая закрывает пару, иначе вторая сторона закрыла бы её раньше (M4-AC5).
#[cfg(feature = "test-hooks")]
pub static TEST_PUMP_EOF_DELAY_SIDE: AtomicU64 = AtomicU64::new(0);

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

    /// Сессия прокси: слушать одну сторону и подключаться к другой (FR-5).
    ///
    /// `role` — кто приходит на порт bytetap: анализатор (`AnalyzerClient`) или ЛИС
    /// (`AnalyzerServer`). `retry` — пауза после неудачного подключения к цели.
    ///
    /// Автоответчика здесь нет и быть не может (FR-6): байты идут без изменений, а прибор
    /// получает ответы от ЛИС. Поэтому профиль — [`Autoresponder::None`], и [`SessionConfig::with_responder`]
    /// на прокси не действует: заголовок журнала и автомат остаются «без ответов».
    #[must_use]
    pub fn proxy(
        listen: SocketAddr,
        target: impl Into<String>,
        role: ProxyRole,
        retry: Duration,
    ) -> Self {
        Self {
            source: Source::Proxy {
                listen,
                target: target.into(),
                role,
                retry,
                allow_from: None,
            },
            responder: Autoresponder::None,
            astm_nak_on_bad_checksum: false,
            log_dir: None,
            analyzer: None,
            note: None,
        }
    }

    /// Сессия прослушки двух COM-портов (FR-10).
    ///
    /// `analyzer` — порт стороны анализатора, `lis` — порт стороны ЛИС; `retry` — пауза
    /// между попытками открыть оба порта после разрыва.
    ///
    /// Автоответчика здесь нет и быть не может: байты идут без изменений, ответы приходят
    /// от ЛИС (FR-10, M5-AC4). Поэтому профиль — [`Autoresponder::None`], и
    /// [`SessionConfig::with_responder`] на прослушку не действует.
    #[must_use]
    pub fn com_monitor(
        analyzer: crate::transport::serial::SerialSettings,
        lis: crate::transport::serial::SerialSettings,
        retry: Duration,
    ) -> Self {
        Self {
            source: Source::ComMonitor {
                analyzer,
                lis,
                retry,
            },
            responder: Autoresponder::None,
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

    /// Прокси принимает входящие только с адреса `allow_from`; `None` — с любого (FR-8).
    ///
    /// Действует только на прокси: у других источников такого поля нет, и вызов ничего
    /// не меняет. Входящее с чужого адреса закрывается до подключения к цели.
    #[must_use]
    pub fn with_allow_from(mut self, allow_from: Option<IpAddr>) -> Self {
        if let Source::Proxy {
            allow_from: field, ..
        } = &mut self.source
        {
            *field = allow_from;
        }
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
    ///
    /// В прокси и в прослушке автоответчика нет всегда, какой бы профиль ни был задан
    /// настройками (FR-6, FR-10): второй автомат отвечал бы прибору вместо ЛИС.
    fn responder_kind(&self) -> ResponderKind {
        if matches!(
            self.source,
            Source::Proxy { .. } | Source::ComMonitor { .. }
        ) {
            return ResponderKind::None;
        }
        match self.responder {
            Autoresponder::Auto => ResponderKind::Auto,
            Autoresponder::Hl7Mllp => ResponderKind::Hl7,
            Autoresponder::AstmE1381 => ResponderKind::Astm {
                nak_on_bad_checksum: self.astm_nak_on_bad_checksum,
            },
            Autoresponder::None => ResponderKind::None,
        }
    }

    /// Профиль автоответчика для заголовка журнала.
    ///
    /// У прокси и прослушки он всегда `none`: поле заголовка должно совпадать с тем,
    /// что делает сессия.
    #[must_use]
    pub const fn header_responder(&self) -> Autoresponder {
        if matches!(
            self.source,
            Source::Proxy { .. } | Source::ComMonitor { .. }
        ) {
            return Autoresponder::None;
        }
        self.responder
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
            config.header_responder(),
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
            mode,
            responder: config.responder_kind(),
            journal,
            clock: MonoClock::new(),
            events,
            ids: ControlIdSource::new(),
            counters: Arc::new(Counters::default()),
            failure: failure_tx,
            open: Mutex::new(HashSet::new()),
            backlog_warned: AtomicBool::new(false),
            ends: AtomicU64::new(0),
        });
        // Замечания источника идут раньше первого соединения: инженер должен узнать про
        // разрешение таймера и про линии управления до обмена (M5-AC5, М5-В3).
        for note in opened.notes {
            shared.info_with_code(None, Some(note.code), note.message);
        }
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
    mode: Mode,
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
    /// Предупреждение о растущей очереди писателя уже отправлено (M4-AC12).
    backlog_warned: AtomicBool,
    /// Номера концов пар: по ним `close` называет сторону, закончившуюся первой (M4-AC5).
    ends: AtomicU64,
}

impl Shared {
    /// Номер конца стороны пары: меньшее значение — эту сторону увидели раньше.
    ///
    /// Номер берётся в момент события, а не на выходе из задачи насоса: выход ждёт
    /// `shutdown` обратной стороны, и первым возвращается тот насос, кого отпустил
    /// планировщик, а не тот, кто увидел конец первым (M4-AC5).
    fn end_stamp(&self) -> u64 {
        self.ends.fetch_add(1, Ordering::Relaxed)
    }

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
        self.warn_backlog();
        #[cfg(feature = "test-hooks")]
        PUBLISHED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }

    /// Очередь писателя дошла до половины предела: одно `info` за сессию (M4-AC12).
    ///
    /// Предупреждение не останавливает запись: это ещё не отказ, а сигнал, что журнал
    /// отстаёт от пересылки. Предел — [`max_pending_bytes`]; перейдя его, писатель
    /// отказывает, и сессия останавливается уже как при сломанном диске.
    fn warn_backlog(&self) {
        let limit = max_pending_bytes();
        let queued = self.journal.pending_bytes();
        if queued < limit / 2 || self.backlog_warned.swap(true, Ordering::Relaxed) {
            return;
        }
        self.info_with_code(
            None,
            Some("journal_backlog_warning"),
            format!(
                "journal backlog: {queued} bytes are queued for the writer, half of the {limit}-byte limit; the journal is falling behind"
            ),
        );
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
            peer: None,
            message: message.into(),
        }));
    }

    /// Ошибка с адресом отвергнутой стороны (FR-8) или с портом отказавшей стороны
    /// прослушки (M5-AC2).
    ///
    /// Сторона уходит отдельным полем события: та же строка ленты должна называть её, не
    /// разбирая английский `message`. У прокси `peer` — адрес отвергнутого, у прослушки —
    /// имя порта, у которого пропал обмен.
    fn error_with_peer(
        &self,
        conn: Option<u32>,
        code: Option<&str>,
        peer: Option<String>,
        message: impl Into<String>,
    ) {
        self.publish(Event::Error(ErrorEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            code: code.map(str::to_owned),
            peer,
            message: message.into(),
        }));
    }

    fn info_with_code(&self, conn: Option<u32>, code: Option<&str>, message: impl Into<String>) {
        self.publish(Event::Info(InfoEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            code: code.map(str::to_owned),
            repeats: None,
            message: message.into(),
        }));
    }

    /// Замечание о серии ошибок источника: код и число схлопнутых повторов отдельным
    /// полем, чтобы интерфейс подставил число в свою фразу (формат `/2`).
    fn info_repeating(
        &self,
        conn: Option<u32>,
        code: &'static str,
        repeats: u32,
        message: impl Into<String>,
    ) {
        self.publish(Event::Info(InfoEvent {
            t: now_rfc3339(),
            mono_us: self.clock.now_us(),
            conn,
            code: Some(code.to_owned()),
            repeats: Some(repeats),
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
            code: Some(reason.code().unwrap_or("journal_failure").to_owned()),
            peer: None,
            message: format!("journal write failed: {reason}{}", after_failure(self.mode)),
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
///
/// Серия помнит свой шаг (`accept`, `connect`): у прокси отказ приёма и отказ цели —
/// разные коды, и сообщение о восстановлении должно называть тот, что был в серии.
#[derive(Debug, Default)]
struct SourceErrors {
    current: Option<SourceSeries>,
}

/// Серия одинаковых ошибок источника: шаг, текст последней и число повторов.
#[derive(Debug)]
struct SourceSeries {
    prefix: &'static str,
    /// Серия отказов фильтра адресов: считается по коду, а не по тексту (FR-8).
    by_code: bool,
    message: String,
    repeats: u32,
}

/// Что записать о новой ошибке источника.
#[derive(Debug)]
enum SourceReport {
    /// Ошибка новая: нужен `error`.
    First(String),
    /// Ошибка сменилась: `error` о новой, а `info` о повторах прежней — только если
    /// повторы были (поле `repeats` схемы `/2` начинается с единицы).
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
    fn failed(&mut self, prefix: &'static str, message: String) -> SourceReport {
        self.record(prefix, message, false)
    }

    /// Учитывает отказ фильтра адресов (`proxy_peer_rejected`): одна серия на код.
    ///
    /// Текст отказа называет адрес отвергнутого, поэтому по тексту каждый чужой адрес
    /// начинал бы новую серию, и перебор адресов забил бы ленту и журнал (FR-8). Серия
    /// считается по коду, а `message` обновляется на последний отказ: адрес последнего
    /// отвергнутого называет замечание о серии.
    fn rejected(&mut self, prefix: &'static str, message: String) -> SourceReport {
        self.record(prefix, message, true)
    }

    /// Учитывает ошибку: `by_code` — сравнивать серию по коду, а не по тексту.
    fn record(&mut self, prefix: &'static str, message: String, by_code: bool) -> SourceReport {
        let same = |series: &SourceSeries| (by_code && series.by_code) || series.message == message;
        match self.current.take() {
            Some(series) if same(&series) => {
                self.current = Some(SourceSeries {
                    // Последний отказ серии: его адрес называет замечание о ней.
                    message,
                    repeats: series.repeats.saturating_add(1),
                    ..series
                });
                SourceReport::Repeated
            }
            Some(series) => {
                self.current = Some(SourceSeries {
                    prefix,
                    by_code,
                    message: message.clone(),
                    repeats: 0,
                });
                SourceReport::Changed {
                    previous: series.message,
                    repeats: series.repeats,
                    message,
                }
            }
            None => {
                self.current = Some(SourceSeries {
                    prefix,
                    by_code,
                    message: message.clone(),
                    repeats: 0,
                });
                SourceReport::First(message)
            }
        }
    }

    /// Замечание о повторах прежней ошибки при её смене.
    ///
    /// `None`, когда повторов не было: ошибки сменились сразу, и записывать в `info`
    /// нечего. Поле `repeats` схемы `/2` начинается с единицы — замечание с нулём
    /// `verify` забраковал бы (M4-AC6).
    fn changed_note(previous: String, repeats: u32) -> Option<SourceNote> {
        (repeats > 0).then(|| SourceNote {
            code: "source_retry_repeats",
            repeats,
            text: format!(
                "source error changed after {repeats} repeats of the previous one: {previous}"
            ),
        })
    }

    /// Приём восстановился: отчёт о схлопнутых повторах, если они были.
    fn recovered(&mut self) -> Option<SourceNote> {
        self.current.take().and_then(|series| {
            (series.repeats > 0).then(|| SourceNote {
                code: "source_recovered",
                repeats: series.repeats,
                text: format!(
                    "{} recovered after {} repeated failures: {}",
                    series.prefix, series.repeats, series.message
                ),
            })
        })
    }

    /// Серия не закончилась к остановке: отчёт о повторах перед `session_end`.
    ///
    /// Без него последняя серия ошибок выглядела бы в журнале одной ошибкой:
    /// повторы схлопнуты, а восстановления, о котором сообщает `info`, уже не будет.
    fn stopped(&mut self) -> Option<SourceNote> {
        self.current.take().and_then(|series| {
            (series.repeats > 0).then(|| SourceNote {
                code: "source_failing_at_stop",
                repeats: series.repeats,
                text: format!(
                    "{} still failing after {} repeated failures at the stop: {}",
                    series.prefix, series.repeats, series.message
                ),
            })
        })
    }
}

/// Замечание о серии ошибок источника: код для интерфейса, число повторов и текст.
///
/// Число повторов уходит в отдельном поле события `info` (формат `/2`), поэтому
/// интерфейс подставляет его в свою фразу, а не выковыривает из английского текста
/// журнала.
#[derive(Debug, PartialEq, Eq)]
struct SourceNote {
    code: &'static str,
    repeats: u32,
    text: String,
}

/// Как назвать ошибку источника в журнале и какой код дать интерфейсу.
///
/// Название зависит от шага, а не только от режима: у прокси ошибка `accept` — это сбой
/// приёма, а не недоступная цель, и попадать под код отказа подключения она не должна
/// (M4-AC6).
fn source_failure(mode: Mode, stage: Stage) -> (&'static str, &'static str) {
    match (stage, mode) {
        (Stage::Accept, _) => ("accept failed", "accept_failed"),
        (Stage::Open, _) => ("serial port open failed", "serial_open_failed"),
        (Stage::Connect, Mode::Proxy) => ("connect failed", "proxy_connect_failed"),
        (Stage::Connect, _) => ("connect failed", "connect_failed"),
    }
}

/// Что обмен получает после отказа журнала: у прокси и прослушки автоответчика нет.
///
/// Текст события — английский, для журнала и поиска (`session-format.md`); русский
/// остаётся интерфейсу.
fn after_failure(mode: Mode) -> &'static str {
    if matches!(mode, Mode::Proxy | Mode::ComMonitor) {
        "; no bytes are forwarded after the failure"
    } else {
        "; no ACKs are sent after the failure"
    }
}

/// Код ошибки источника с поправкой на её вид.
///
/// У прокси отказ цели, её молчание и петля на адрес прослушивания — разные коды
/// (M4-AC6): инженеру важно отличить «ЛИС не отвечает» от «ЛИС отказала в соединении»
/// и от ошибки настройки.
fn source_failure_code(mode: Mode, code: &'static str, error: &io::Error) -> &'static str {
    if is_proxy_target_is_self(error) {
        return "proxy_target_is_self";
    }
    // Входящее с неразрешённого адреса — не сломанный приём: слушатель в порядке,
    // а отказано тому, кто пришёл (FR-8).
    if is_proxy_peer_rejected(error) {
        return "proxy_peer_rejected";
    }
    match (mode, error.kind()) {
        (Mode::Proxy, io::ErrorKind::TimedOut) => "proxy_connect_timeout",
        _ => code,
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
    // Задача соединения говорит, кто закрыл пару: источник прослушки начинает следующую
    // попытку с отказавшей стороны (M5-AC5).
    let mut tasks: JoinSet<Option<ClosedBy>> = JoinSet::new();
    let mut source_errors = SourceErrors::default();
    let sequential = source.sequential();
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
                    if let Some(note) = source_errors.recovered() {
                        shared.info_repeating(None, note.code, note.repeats, note.text);
                    }
                    let conn = shared.counters.next_conn();
                    if let Ok(mut open) = shared.open.lock() {
                        open.insert(conn);
                    }
                    // `open` пишется здесь, а не в задаче соединения: так порядок строк
                    // совпадает с порядком номеров, и открытие не может оказаться после
                    // первых фрагментов этого же соединения.
                    let Connection {
                        transport,
                        peer,
                        local,
                        link,
                        reset,
                    } = connection;
                    // Прослушка: имена обоих портов нужны ошибке обмена — она называет
                    // отказавший порт (M5-AC2). Снимаются до `open`: `peer` уходит в него.
                    let ports = (mode == Mode::ComMonitor).then(|| MonitorPorts {
                        analyzer: peer.clone(),
                        lis: link
                            .as_ref()
                            .map(|link| link.peer.clone())
                            .unwrap_or_default(),
                    });
                    shared.publish(Event::Open(Open {
                        t: now_rfc3339(),
                        mono_us: shared.clock.now_us(),
                        conn,
                        peer,
                        local,
                        // У прокси `open` несёт обе стороны пары (формат `/2`).
                        link: link.as_ref().map(|link| crate::journal::Link {
                            role: link.role,
                            peer: link.peer.clone(),
                            local: link.local.clone(),
                        }),
                    }));
                    let shared = Arc::clone(&shared);
                    let mut conn_stop = stop.clone();
                    let mut conn_failure = failure.clone();
                    tasks.spawn(async move {
                        match link {
                            Some(link) => {
                                Some(
                                    serve_pair(
                                        transport,
                                        reset,
                                        link,
                                        conn,
                                        shared,
                                        &mut conn_stop,
                                        &mut conn_failure,
                                        close_policy(mode),
                                        ports,
                                    )
                                    .await,
                                )
                            }
                            None => {
                                let mut transport = transport;
                                serve(
                                    transport.as_mut(),
                                    conn,
                                    &shared,
                                    &mut conn_stop,
                                    &mut conn_failure,
                                )
                                .await;
                                None
                            }
                        }
                    });
                }
                Next::Retry {
                    error,
                    after,
                    stage,
                } => {
                    // Неудачное подключение не роняет сессию (NFR-5, M3-AC1, M3-AC2,
                    // M4-AC6): одно `error`, повторы схлопываются, ожидание прерывается
                    // «Стопом».
                    let (prefix, base_code) = source_failure(mode, stage);
                    let message = format!("{prefix}: {}", io_message(&error));
                    let code = source_failure_code(mode, base_code, &error);
                    // Отвергнутая сторона остаётся в событии адресом: интерфейс называет
                    // его в ленте, а не разбирает английский текст журнала (FR-8).
                    let peer = rejected_peer(&error).map(|ip| ip.to_string());
                    // Отказ фильтра схлопывается по коду, а не по тексту: текст несёт
                    // адрес отвергнутого, и по тексту каждый чужой адрес начинал бы
                    // новую серию (FR-8).
                    let report = if is_proxy_peer_rejected(&error) {
                        source_errors.rejected(prefix, message)
                    } else {
                        source_errors.failed(prefix, message)
                    };
                    match report {
                        SourceReport::Repeated => {}
                        SourceReport::First(message) => {
                            shared.error_with_peer(None, Some(code), peer, message);
                        }
                        SourceReport::Changed { previous, repeats, message } => {
                            if let Some(note) = SourceErrors::changed_note(previous, repeats) {
                                shared.info_repeating(None, note.code, note.repeats, note.text);
                            }
                            shared.error_with_peer(None, Some(code), peer, message);
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
                if let Some(Ok(Some(by))) = joined {
                    source.connection_closed(by);
                }
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
    if let Some(note) = source_errors.stopped() {
        shared.info_repeating(None, note.code, note.repeats, note.text);
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

/// Чем закончился насос одного направления пары прокси.
enum Pump {
    /// Сторона закрыла свою половину (`FIN`): запись в обратную сторону полузакрыта.
    Eof(LinkRole),
    /// Обмен прерван ошибкой сокета: сброс, обрыв, ошибка записи.
    Broken {
        /// Сторона, из которой читал насос.
        role: LinkRole,
        /// Что именно случилось.
        error: io::Error,
        /// `true` — ошибка на записи, `false` — на чтении.
        writing: bool,
    },
    /// Сессия остановлена или журнал отказал: пересылка прекращается.
    Stopped,
}

/// Итог насоса и номер, под которым он увидел конец своей стороны.
///
/// Номер проставляется в момент события, а не на выходе из задачи: задача ждёт `shutdown`
/// обратной стороны, поэтому порядок возврата — порядок планировщика, а не порядок событий.
/// По номеру [`serve_pair`] называет в `close` сторону, которая закончилась первой (M4-AC5).
struct PumpEnd {
    /// Порядок наблюдения: меньшее значение — эта сторона закончилась раньше.
    seq: u64,
    /// Что именно закончилось.
    outcome: Pump,
}

/// Как закрывается пара: когда прекращать пересылку второй стороны (FR-7, M5-AC2).
///
/// У TCP-сокета есть полузакрытие: `FIN` одной стороны не значит, что обмен кончился,
/// и обратное направление доживает до своего `FIN` ([`ClosePolicy::Linger`]). У COM-порта
/// полузакрытия нет — есть только «байт нет» и «порт пропал», поэтому первая же ошибка
/// чтения или записи закрывает оба порта ([`ClosePolicy::Together`], M5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosePolicy {
    /// Дожитие обратного направления, но не дольше срока (прокси, FR-7).
    Linger(Duration),
    /// Обе стороны закрываются сразу, по первой ошибке или концу данных (прослушка).
    Together,
}

/// Политика закрытия пары по режиму сессии: прокси доживает, прослушка закрывает обе.
fn close_policy(mode: Mode) -> ClosePolicy {
    if mode == Mode::ComMonitor {
        ClosePolicy::Together
    } else {
        ClosePolicy::Linger(PROXY_LINGER)
    }
}

/// Обслуживает пару до закрытия: прокси (FR-5, FR-6, FR-7) и прослушку (FR-10).
///
/// Пара — одно соединение журнала с одним `conn`: `open` уже написан сессией. Внутри два
/// насоса, по одному на направление: каждый читает в свой буфер [`READ_BUFFER`], собирает
/// событие `data` по времени чтения, пишет прочитанное в другой транспорт и потом кладёт
/// событие в журнал — с фактически переданными байтами. Журнал в пути пересылки не стоит:
/// `record` только кладёт событие в канал писателя (NFR-6, FR-20).
///
/// Закрытие задаёт [`ClosePolicy`]: у прокси первая закончившаяся сторона определяет `by`
/// и `reason` одного `close`, `FIN` пересылается полузакрытием, обратное направление
/// доживает до своего `FIN` не дольше [`PROXY_LINGER`], а истёк срок — пара закрывается
/// как `bytetap/timeout` (FR-7). У прослушки полузакрытия нет: первая ошибка или конец
/// данных закрывает обе стороны сразу, `close` тоже один (M5-AC2). Сброс закрывает обе
/// стороны сразу и в прокси, «Стоп» и отказ журнала — тоже.
///
/// Второй насос останавливается сигналом, а не `abort`: прерванная запись теряла бы
/// отправленные байты, а они уже дошли до адресата. Насос после сигнала дописывает
/// отправленное в журнал и возвращается сам.
#[allow(clippy::too_many_arguments)]
async fn serve_pair(
    accepted: Box<dyn Transport>,
    accepted_reset: Option<ResetSocket>,
    link: Link,
    conn: u32,
    shared: Arc<Shared>,
    stop: &mut watch::Receiver<bool>,
    failure: &mut watch::Receiver<Option<JournalFailureReason>>,
    policy: ClosePolicy,
    ports: Option<MonitorPorts>,
) -> ClosedBy {
    // Принятая сторона — противоположная роли дальнего конца пары (FR-5).
    let accepted_role = link.role.opposite();
    let Link {
        transport,
        role,
        peer: _,
        local: _,
        reset: link_reset,
    } = link;
    let (hangup, hangup_rx) = watch::channel(false);
    let (accepted_read, accepted_write) = tokio::io::split(accepted);
    let (link_read, link_write) = tokio::io::split(transport);

    let mut pumps: JoinSet<PumpEnd> = JoinSet::new();
    pumps.spawn(pump(
        accepted_read,
        link_write,
        accepted_role,
        conn,
        Arc::clone(&shared),
        PumpSignals {
            stop: stop.clone(),
            failure: failure.clone(),
            hangup: hangup_rx.clone(),
        },
    ));
    pumps.spawn(pump(
        link_read,
        accepted_write,
        role,
        conn,
        Arc::clone(&shared),
        PumpSignals {
            stop: stop.clone(),
            failure: failure.clone(),
            hangup: hangup_rx,
        },
    ));

    let mut close = match pumps.join_next().await {
        Some(Ok(ended)) => PairClose::new(&shared, conn, &ended, ports.as_ref()),
        // Насосов не осталось или задача упала: пары больше нет.
        None | Some(Err(_)) => PairClose {
            seq: None,
            by: ClosedBy::Bytetap,
            reason: "error",
            linger: Duration::ZERO,
        },
    };
    // Прослушка не доживает обратное направление: у порта нет `FIN`, и закрытая пара —
    // это закрытые оба порта (M5-AC2).
    let linger = match policy {
        ClosePolicy::Together => Duration::ZERO,
        ClosePolicy::Linger(limit) => close.linger.min(limit),
    };
    // Первая закончившаяся сторона решает, как закрывается пара; сброс второй стороны
    // во время дожития решается отдельно — `close` от него не меняется (FR-7).
    let mut reset_pair = close.reason == "reset";

    if linger.is_zero() {
        // Вторая сторона закрывается сразу: `FIN` уже не пересылать, `RST`, «Стоп» или
        // отказ журнала.
        let _ = hangup.send(true);
        if let Some(ended) = settle(&mut pumps).await {
            // Конец второй стороны — тоже событие журнала, а `close` называет ту сторону,
            // которая закончилась раньше по времени (M4-AC5).
            close.absorb(&shared, conn, &ended, ports.as_ref());
            reset_pair = reset_pair || is_reset_outcome(&ended.outcome);
        }
    } else {
        // `FIN` одной стороны: обратное направление доживает до своего `FIN`;
        // истёк срок — закрывает bytetap, иначе зависшая сторона держала бы пару вечно.
        match timeout(linger, pumps.join_next()).await {
            Ok(Some(Ok(ended))) => {
                // Вторая сторона закончилась сама; её ошибка — тоже событие журнала.
                close.absorb(&shared, conn, &ended, ports.as_ref());
                // Сброс во время дожития: собеседник второй стороны уже сбросил
                // соединение, и ответа он не ждёт. Обе стороны закрываются `RST`,
                // как при сбросе первой: иначе оставшаяся сторона увидела бы `FIN`
                // там, где собеседник видел сброс.
                reset_pair = reset_pair || is_reset_outcome(&ended.outcome);
            }
            // Насосов не осталось или задача упала: закрывать уже нечего.
            Ok(_) => {}
            Err(_elapsed) => {
                // Срок истёк: пару закрывает bytetap, итог второго насоса его не меняет.
                close.by = ClosedBy::Bytetap;
                close.reason = "timeout";
                close.seq = None;
                let _ = hangup.send(true);
                // Но ошибка обмена второго насоса — по-прежнему событие журнала.
                if let Some(ended) = settle(&mut pumps).await {
                    let _ = pair_end(&shared, conn, &ended.outcome, ports.as_ref());
                }
            }
        }
    }

    // Сброс: вторая сторона тоже получает `RST`, а не `FIN`. Зависший собеседник иначе
    // ждал бы закрытия полуоткрытого соединения (FR-7). Ручек нет — стороны закроются
    // обычным `FIN`.
    if reset_pair {
        if let Some(reset) = &accepted_reset {
            reset.reset();
        }
        if let Some(reset) = &link_reset {
            reset.reset();
        }
    }
    // Половинки сокетов уже выгружены насосами: закрытие уходит с выставленным `SO_LINGER`.
    drop(pumps);
    drop(accepted_reset);
    drop(link_reset);
    shared.close_conn(conn, close.by, close.reason);
    close.by
}

/// Конец пары для `close`: сторона, причина, срок дожития и номер наблюдения.
///
/// Пара закрывается той стороной, которая закончилась раньше по времени, а не тем насосом,
/// чья задача вернулась первой: выход из насоса ждёт `shutdown` обратной стороны, и первым
/// возвращается тот, кого отпустил планировщик. Поэтому итог второй стороны только уточняет
/// `close`, если её конец увидели раньше (M4-AC5, FR-7).
struct PairClose {
    /// Номер конца выбранной стороны; `None` — насосов не осталось или задача упала.
    seq: Option<u64>,
    /// Кто закрыл пару.
    by: ClosedBy,
    /// Почему закрылась пара.
    reason: &'static str,
    /// Сколько доживает обратное направление.
    linger: Duration,
}

impl PairClose {
    /// Конец пары по итогу первого вернувшегося насоса.
    fn new(shared: &Shared, conn: u32, ended: &PumpEnd, ports: Option<&MonitorPorts>) -> Self {
        let (by, reason, linger) = pair_end(shared, conn, &ended.outcome, ports);
        Self {
            seq: Some(ended.seq),
            by,
            reason,
            linger,
        }
    }

    /// Учитывает конец второй стороны: `close` называет сторону, закончившуюся раньше.
    ///
    /// Итог второй стороны пишется в журнал всегда — ошибка обмена относится к паре, — но
    /// `close` меняется, только если её конец увидели раньше первого (M4-AC5).
    fn absorb(
        &mut self,
        shared: &Shared,
        conn: u32,
        ended: &PumpEnd,
        ports: Option<&MonitorPorts>,
    ) {
        let (by, reason, _) = pair_end(shared, conn, &ended.outcome, ports);
        if earlier(self.seq, ended) {
            (self.by, self.reason, self.seq) = (by, reason, Some(ended.seq));
        }
    }
}

/// Конец второй стороны решает `close`: её номер меньше уже выбранного.
///
/// `None` — насосов не осталось или задача упала: это `bytetap/error`, и конец уцелевшей
/// стороны его не перекрывает, иначе паника насоса выглядела бы штатным закрытием (M4-AC5).
fn earlier(chosen: Option<u64>, ended: &PumpEnd) -> bool {
    chosen.is_some_and(|seq| ended.seq < seq)
}

/// Имена портов прослушки: сторона анализатора и сторона ЛИС (M5-AC2).
///
/// Пара портов — одно соединение журнала, и ошибка обмена называет отказавший порт, чтобы
/// лента говорила «COM4 пропал», а не «ошибка чтения соединения» (M5-В8). У прокси портов
/// нет, поэтому там `None`.
#[derive(Debug)]
struct MonitorPorts {
    /// Порт, к которому подключён анализатор: имя принятой стороны пары.
    analyzer: String,
    /// Порт стороны ЛИС: `link.peer` журнала.
    lis: String,
}

impl MonitorPorts {
    /// Имя порта стороны роли в журнале.
    fn name(&self, role: LinkRole) -> Option<&str> {
        let name = match role {
            LinkRole::Analyzer => &self.analyzer,
            LinkRole::Lis => &self.lis,
        };
        (!name.is_empty()).then_some(name.as_str())
    }
}

/// Итог насоса: кто закрыл пару, с какой причиной и сколько доживает обратное направление.
///
/// Ошибку транспорта функция пишет в журнал: она относится к паре, а не к решению о закрытии,
/// поэтому `close` от этого не меняется (FR-7, M5-AC2).
fn pair_end(
    shared: &Shared,
    conn: u32,
    outcome: &Pump,
    ports: Option<&MonitorPorts>,
) -> (ClosedBy, &'static str, Duration) {
    match outcome {
        Pump::Eof(role) => (role.closed_by(), "eof", PROXY_LINGER),
        Pump::Broken {
            role,
            error,
            writing,
        } => {
            let (by, reason) = pair_failure(shared.mode, *role, error, *writing);
            let monitor = shared.mode == Mode::ComMonitor;
            // Прослушка называет в ошибке отказавший порт: чтение — порт своей стороны,
            // запись — противоположной. По нему интерфейс говорит «COM4 пропал» (M5-В8).
            let peer = ports.and_then(|ports| {
                let failed = if *writing { role.opposite() } else { *role };
                ports.name(failed).map(str::to_owned)
            });
            let (code, what) = if *writing {
                (
                    "connection_write_failed",
                    if monitor {
                        "monitor write failed"
                    } else {
                        "proxy write failed"
                    },
                )
            } else {
                (
                    "connection_read_failed",
                    if monitor {
                        "monitor read failed"
                    } else {
                        "proxy read failed"
                    },
                )
            };
            shared.error_with_peer(
                Some(conn),
                Some(code),
                peer,
                format!("{what}: {}", io_message(error)),
            );
            (by, reason, Duration::ZERO)
        }
        Pump::Stopped => (ClosedBy::Bytetap, "stop", Duration::ZERO),
    }
}

/// Дожидается второго насоса после сигнала: он вернётся сам, записав отправленное.
///
/// `abort` здесь не годится: прерванная посреди записи задача не успела бы записать
/// в журнал байты, которые адресат получил (M4-AC10). Предел — тот же, что у остановки
/// сессии; дольше ждать незачем, и тогда задачи прерываются. Итог второго насоса решает
/// вызывающий: он уточняет `close`, если её сторона закончилась раньше (M4-AC5).
async fn settle(pumps: &mut JoinSet<PumpEnd>) -> Option<PumpEnd> {
    match timeout(CONNECTIONS_SHUTDOWN_TIMEOUT, pumps.join_next()).await {
        Ok(Some(Ok(ended))) => Some(ended),
        Ok(_) => None,
        Err(_elapsed) => {
            pumps.shutdown().await;
            None
        }
    }
}

/// Что слушает насос пары: остановку сессии, отказ журнала и закрытие пары.
struct PumpSignals {
    /// «Стоп» сессии.
    stop: watch::Receiver<bool>,
    /// Отказ журнала: пересылка прекращается.
    failure: watch::Receiver<Option<JournalFailureReason>>,
    /// Сигнал пары: вторая сторона закрыла или истёк срок дожития.
    hangup: watch::Receiver<bool>,
}

/// Один насос пары: читает из `from` и пишет прочитанное в `to` (FR-6, NFR-6).
///
/// Событие `data` собирается до записи в сокет, а публикуется после неё: так `mono_us`
/// относится к моменту чтения, и по журналу видно порядок направлений (FR-22). В журнал
/// попадают фактически отправленные байты, включая обрыв на середине: их адресат получил.
///
/// Запись в сокет идёт в `select!` с остановкой, отказом журнала и сигналом пары: адресат,
/// переставший читать, не держит пару, а отмена записи не теряет отправленный кусок.
async fn pump(
    mut from: tokio::io::ReadHalf<Box<dyn Transport>>,
    mut to: tokio::io::WriteHalf<Box<dyn Transport>>,
    role: LinkRole,
    conn: u32,
    shared: Arc<Shared>,
    signals: PumpSignals,
) -> PumpEnd {
    let PumpSignals {
        mut stop,
        mut failure,
        mut hangup,
    } = signals;
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        // Отказ журнала останавливает пересылку: обмен, которого нет в записи, дальше
        // не идёт (M4-AC10).
        if failure.borrow().is_some() {
            return PumpEnd {
                seq: shared.end_stamp(),
                outcome: Pump::Stopped,
            };
        }
        let read = tokio::select! {
            biased;
            () = wait_for_stop(&mut stop) => return PumpEnd {
                seq: shared.end_stamp(),
                outcome: Pump::Stopped,
            },
            () = wait_for_failure(&mut failure) => return PumpEnd {
                seq: shared.end_stamp(),
                outcome: Pump::Stopped,
            },
            () = wait_for_hangup(&mut hangup) => return PumpEnd {
                seq: shared.end_stamp(),
                outcome: Pump::Stopped,
            },
            read = from.read(&mut buffer) => read,
        };
        match read {
            Ok(0) => {
                // `FIN` этой стороны — полузакрытие записи в другую (FR-7). Номер берётся
                // до `shutdown`: пару закрыла эта сторона, даже если задача вернётся позже
                // задачи второй стороны (M4-AC5).
                let ended = PumpEnd {
                    seq: shared.end_stamp(),
                    outcome: Pump::Eof(role),
                };
                let _ = to.shutdown().await;
                test_hook_pump_eof_delay(role).await;
                return ended;
            }
            Ok(len) => {
                let payload = &buffer[..len];
                let (t, mono_us) = (timestamp().0, shared.clock.now_us());
                let sent = AtomicUsize::new(0);
                let write = write_counted(&mut to, payload, &sent);
                tokio::pin!(write);
                let outcome = tokio::select! {
                    biased;
                    () = wait_for_stop(&mut stop) => None,
                    () = wait_for_failure(&mut failure) => None,
                    () = wait_for_hangup(&mut hangup) => None,
                    result = &mut write => Some(result),
                };
                let sent = sent.load(Ordering::Relaxed).min(payload.len());
                shared.counters.add(role.dir(), sent as u64);
                let forwarded = Data::new(t, mono_us, conn, role.dir(), false, &payload[..sent]);
                if sent > 0 {
                    shared.publish(Event::Data(forwarded));
                }
                match outcome {
                    Some(Ok(())) => {}
                    Some(Err(error)) => {
                        return PumpEnd {
                            seq: shared.end_stamp(),
                            outcome: Pump::Broken {
                                role,
                                error,
                                writing: true,
                            },
                        };
                    }
                    // Запись прервана: отправленное записано, пересылка кончается.
                    None => {
                        return PumpEnd {
                            seq: shared.end_stamp(),
                            outcome: Pump::Stopped,
                        };
                    }
                }
            }
            Err(error) => {
                return PumpEnd {
                    seq: shared.end_stamp(),
                    outcome: Pump::Broken {
                        role,
                        error,
                        writing: false,
                    },
                };
            }
        }
    }
}

/// Ждёт сигнала пары о закрытии (или выгрузки отправителя).
async fn wait_for_hangup(hangup: &mut watch::Receiver<bool>) {
    if *hangup.borrow_and_update() {
        return;
    }
    let _ = hangup.changed().await;
}

/// Задержка выхода насоса после `FIN` на стороне из хука [`TEST_PUMP_EOF_DELAY_SIDE`].
///
/// Задержка ставится после `shutdown`: `FIN` уходит второй стороне сразу, а позже
/// возвращается только задача своего насоса (M4-AC5).
#[cfg(feature = "test-hooks")]
async fn test_hook_pump_eof_delay(role: LinkRole) {
    let side = TEST_PUMP_EOF_DELAY_SIDE.load(Ordering::Relaxed);
    let ours = (side == 1 && role == LinkRole::Analyzer) || (side == 2 && role == LinkRole::Lis);
    let millis = TEST_PUMP_EOF_DELAY_MS.load(Ordering::Relaxed);
    if ours && millis > 0 {
        tokio::time::sleep(Duration::from_millis(millis)).await;
    }
}

#[cfg(not(feature = "test-hooks"))]
async fn test_hook_pump_eof_delay(_role: LinkRole) {}

/// Сброс ли это: ошибка сокета, после которой пару закрывают `RST`, а не `FIN` (FR-7).
fn is_reset_kind(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof
    )
}

/// Сброс второй стороны пары во время дожития (FR-7).
fn is_reset_outcome(outcome: &Pump) -> bool {
    matches!(outcome, Pump::Broken { error, .. } if is_reset_kind(error))
}

/// Кто закрыл пару по ошибке транспорта и почему.
///
/// У прокси ошибка чтения — сторону сбросила та сторона, из которой читали; ошибка записи —
/// та, в которую писали, то есть противоположная. Всё остальное bytetap считает своей
/// ошибкой. У прослушки сброса нет: ошибка — это пропавший порт, и `close` называет его
/// сторону, потому что её адаптер и отключили (M5-AC2).
fn pair_failure(
    mode: Mode,
    role: LinkRole,
    error: &io::Error,
    writing: bool,
) -> (ClosedBy, &'static str) {
    if mode == Mode::ComMonitor {
        let failed = if writing { role.opposite() } else { role };
        return (failed.closed_by(), "error");
    }
    proxy_broken(role, error, writing)
}

/// Кто закрыл пару прокси по ошибке сокета и почему (FR-7).
fn proxy_broken(role: LinkRole, error: &io::Error, writing: bool) -> (ClosedBy, &'static str) {
    if is_reset_kind(error) {
        let closer = if writing { role.opposite() } else { role };
        (closer.closed_by(), "reset")
    } else {
        (ClosedBy::Bytetap, "error")
    }
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
                        peer: None,
                        message,
                    }),
                    ResponderEvent::Info { message, code } => Event::Info(InfoEvent {
                        t,
                        mono_us,
                        conn: Some(conn),
                        code: code.map(|code| code.as_str().to_owned()),
                        repeats: None,
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
///
/// Подходит и полузакрытой записи пары прокси (`WriteHalf`), поэтому ограничение —
/// [`AsyncWrite`], а не транспорт соединения.
async fn write_counted<W: AsyncWrite + Unpin + ?Sized>(
    transport: &mut W,
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
    fn accepts_and_connections_are_named_by_stage() {
        assert_eq!(
            source_failure(Mode::TcpServer, Stage::Accept),
            ("accept failed", "accept_failed")
        );
        assert_eq!(
            source_failure(Mode::TcpClient, Stage::Connect),
            ("connect failed", "connect_failed")
        );
        assert_eq!(
            source_failure(Mode::Serial, Stage::Open),
            ("serial port open failed", "serial_open_failed")
        );
        assert_eq!(
            source_failure(Mode::Proxy, Stage::Connect),
            ("connect failed", "proxy_connect_failed")
        );
        // Отказ `accept` у прокси — не недоступная цель (M4-AC6).
        assert_eq!(
            source_failure(Mode::Proxy, Stage::Accept),
            ("accept failed", "accept_failed")
        );
    }

    #[test]
    fn a_pair_looping_on_itself_gets_its_own_code() {
        let error = io::Error::new(
            io::ErrorKind::InvalidInput,
            crate::transport::ProxyTargetIsSelf {
                addr: "127.0.0.1:50025".parse().unwrap(),
            },
        );
        assert!(is_proxy_target_is_self(&error));
        assert_eq!(
            source_failure_code(Mode::Proxy, "proxy_connect_failed", &error),
            "proxy_target_is_self"
        );
        assert_eq!(
            source_failure_code(Mode::Proxy, "proxy_connect_failed", &io::Error::other("x")),
            "proxy_connect_failed",
            "чужая ошибка `InvalidInput` кода петли не получает"
        );
    }

    #[test]
    fn the_failure_text_of_a_proxy_does_not_promise_acks() {
        assert_eq!(
            after_failure(Mode::TcpServer),
            "; no ACKs are sent after the failure"
        );
        assert_eq!(
            after_failure(Mode::Proxy),
            "; no bytes are forwarded after the failure"
        );
    }

    #[test]
    fn an_unreachable_proxy_target_is_told_from_a_silent_one() {
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(
            source_failure_code(Mode::Proxy, "proxy_connect_failed", &refused),
            "proxy_connect_failed"
        );
        let timed_out = io::Error::from(io::ErrorKind::TimedOut);
        assert_eq!(
            source_failure_code(Mode::Proxy, "proxy_connect_failed", &timed_out),
            "proxy_connect_timeout"
        );
        assert_eq!(
            source_failure_code(Mode::TcpClient, "connect_failed", &timed_out),
            "connect_failed",
            "у TCP-клиента таймаут своего кода не получает"
        );
    }

    #[test]
    fn a_proxy_session_has_no_responder_at_all() {
        let config = SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:5000",
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_responder(Autoresponder::AstmE1381)
        .with_astm_nak(true);
        assert_eq!(
            config.header_responder(),
            Autoresponder::None,
            "в прокси автоответчика нет: байты идут без изменений (FR-6)"
        );
        assert_eq!(config.responder_kind(), ResponderKind::None);
        assert_eq!(config.source.mode(), Mode::Proxy);
    }

    #[test]
    fn a_failed_pump_end_is_not_overridden_by_the_surviving_side() {
        let ended = |seq| PumpEnd {
            seq,
            outcome: Pump::Eof(LinkRole::Analyzer),
        };
        // Насосов не осталось или задача упала: `bytetap/error` остаётся (M4-AC5).
        assert!(!earlier(None, &ended(0)));
        // Конец второй стороны увидели раньше — он и закрывает пару.
        assert!(earlier(Some(5), &ended(4)));
        assert!(!earlier(Some(5), &ended(6)));
        assert!(
            !earlier(Some(5), &ended(5)),
            "одновременный конец первого не перекрывает"
        );
    }

    #[test]
    fn a_pair_is_closed_by_the_side_that_broke_it() {
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        // Чтение из сокета анализатора: сбросил анализатор.
        assert_eq!(
            proxy_broken(LinkRole::Analyzer, &reset, false),
            (ClosedBy::Analyzer, "reset")
        );
        // Запись в сокет ЛИС: сбросила ЛИС, а не тот, из кого читали.
        assert_eq!(
            proxy_broken(LinkRole::Analyzer, &reset, true),
            (ClosedBy::Lis, "reset")
        );
        assert_eq!(
            proxy_broken(LinkRole::Analyzer, &io::Error::other("странно"), false),
            (ClosedBy::Bytetap, "error")
        );
    }

    #[test]
    fn identical_source_errors_collapse_into_one_event_and_a_counter() {
        let mut errors = SourceErrors::default();
        match errors.failed("accept", "accept failed: нет мест".to_owned()) {
            SourceReport::First(message) => assert!(message.contains("нет мест"), "{message}"),
            other => panic!("первая ошибка должна попасть в журнал: {other:?}"),
        }
        assert!(
            matches!(
                errors.failed("accept", "accept failed: нет мест".to_owned()),
                SourceReport::Repeated
            ),
            "повтор не пишется"
        );
        assert!(matches!(
            errors.failed("accept", "accept failed: нет мест".to_owned()),
            SourceReport::Repeated
        ));
        assert_eq!(
            errors.recovered(),
            Some(SourceNote {
                code: "source_recovered",
                repeats: 2,
                text: "accept recovered after 2 repeated failures: accept failed: нет мест".into(),
            })
        );
        assert_eq!(
            errors.recovered(),
            None,
            "восстановление сообщается один раз"
        );
    }

    /// Отказы фильтра адресов схлопываются по коду: адрес в тексте не начинает новую
    /// серию, а замечание о ней называет последний отвергнутый адрес (FR-8).
    #[test]
    fn rejections_of_different_peers_are_one_series() {
        let mut errors = SourceErrors::default();
        match errors.rejected(
            "accept",
            "accept failed: peer 192.0.2.7 is not allowed".to_owned(),
        ) {
            SourceReport::First(message) => assert!(message.contains("192.0.2.7"), "{message}"),
            other => panic!("первый отказ должен попасть в журнал: {other:?}"),
        }
        assert!(
            matches!(
                errors.rejected(
                    "accept",
                    "accept failed: peer 192.0.2.8 is not allowed".to_owned()
                ),
                SourceReport::Repeated
            ),
            "другой адрес продолжает серию, а не начинает новую"
        );
        assert_eq!(
            errors.stopped(),
            Some(SourceNote {
                code: "source_failing_at_stop",
                repeats: 1,
                text: "accept still failing after 1 repeated failures at the stop: accept failed: peer 192.0.2.8 is not allowed".into(),
            }),
            "замечание называет последний отвергнутый адрес"
        );
    }

    /// Обычные ошибки по-прежнему сравниваются по тексту: другой текст — новая серия.
    #[test]
    fn ordinary_errors_with_different_text_are_a_new_series() {
        let mut errors = SourceErrors::default();
        let _ = errors.failed("accept", "accept failed: first".to_owned());
        assert!(matches!(
            errors.failed("accept", "accept failed: second".to_owned()),
            SourceReport::Changed { repeats: 0, .. }
        ));
        // Отказ фильтра в серии с обычной ошибкой приёма — тоже смена.
        assert!(matches!(
            errors.rejected(
                "accept",
                "accept failed: peer 192.0.2.7 is not allowed".to_owned()
            ),
            SourceReport::Changed { .. }
        ));
    }

    #[test]
    fn a_series_unfinished_at_the_stop_is_reported_with_its_count() {
        let mut errors = SourceErrors::default();
        let _ = errors.failed("accept", "accept failed: port taken".to_owned());
        let _ = errors.failed("accept", "accept failed: port taken".to_owned());

        // Серия не восстановилась до остановки: счётчик повторов не теряется.
        assert_eq!(
            errors.stopped(),
            Some(SourceNote {
                code: "source_failing_at_stop",
                repeats: 1,
                text: "accept still failing after 1 repeated failures at the stop: accept failed: port taken".into(),
            })
        );
        assert_eq!(errors.stopped(), None, "серия сообщается один раз");

        // Одиночная ошибка уже в журнале: отчёт о нуле повторов не нужен.
        let mut single = SourceErrors::default();
        let _ = single.failed("accept", "accept failed: port taken".to_owned());
        assert_eq!(single.stopped(), None);
    }

    #[test]
    fn a_different_source_error_is_reported_with_the_previous_count() {
        let mut errors = SourceErrors::default();
        let _ = errors.failed("accept", "first".to_owned());
        let _ = errors.failed("accept", "first".to_owned());
        match errors.failed("connect", "second".to_owned()) {
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
        assert_eq!(errors.recovered(), None, "у новой ошибки повторов не было");
    }

    /// Ошибка сменилась сразу, без единого повтора: `info` не пишется, потому что поле
    /// `repeats` схемы `/2` начинается с единицы и `verify` забраковал бы журнал.
    #[test]
    fn an_error_change_without_repeats_gives_no_note() {
        let mut errors = SourceErrors::default();
        let _ = errors.failed("accept", "first".to_owned());
        match errors.failed("connect", "second".to_owned()) {
            SourceReport::Changed {
                previous, repeats, ..
            } => {
                assert_eq!(previous, "first");
                assert_eq!(repeats, 0);
                assert_eq!(SourceErrors::changed_note(previous, repeats), None);
            }
            other => panic!("смена ошибки не сообщена: {other:?}"),
        }
        // С повторами замечание есть: число уходит в своё поле, а не в текст.
        let note = SourceErrors::changed_note("first".to_owned(), 2).expect("повторы были");
        assert_eq!(note.code, "source_retry_repeats");
        assert_eq!(note.repeats, 2);
        assert!(note.text.contains("after 2 repeats"), "{}", note.text);
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
