//! Транспорты сессии: источник соединений и байтовый поток (FR-1, FR-2, FR-5, FR-9).
//!
//! Источник знает, откуда берутся соединения: `TcpServer` принимает анализатор,
//! `TcpClient` сам подключается к анализатору-серверу, `Serial` открывает COM-порт,
//! `Proxy` встаёт в разрыв и на каждое входящее соединение подключается к цели (FR-5).
//! Дальше всё одинаково: [`Connection`] отдаёт [`Transport`] — `AsyncRead + AsyncWrite`,
//! а нумерация `conn`, автоответчик и журнал остаются делом сессии ([`crate::session`]).
//!
//! Источник не нумерует соединения и ничего не пишет в журнал. Ошибка подключения или
//! открытия порта возвращается как [`Next::Retry`]: сколько ждать до повтора, решает
//! источник, а сессия схлопывает повторы в одно `error` и ждёт в `select!` вместе
//! с остановкой и отказом журнала (NFR-5).
//!
//! Прокси — не вторая сессия, а четвёртый источник: пара соединений приходит одним
//! [`Connection`], у которого заполнено поле [`Connection::link`].

pub mod serial;
pub mod tcp;

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "test-hooks")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::timeout;

use self::serial::{SerialConnection, SerialSettings};
use self::tcp::{TcpServer, TcpServerSource};

pub use crate::journal::LinkRole;

/// Наименьшая пауза переподключения.
///
/// `--reconnect-sec 0` не должен превращать недоступный анализатор в цикл попыток без
/// паузы: сто миллисекунд — уже не «сразу», но ещё и не заметно инженеру.
const MIN_RECONNECT: Duration = Duration::from_millis(100);

/// Сколько ждёт попытка TCP-подключения: `connect` без срока висит минутами.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Пауза переподключения с нижней границей [`MIN_RECONNECT`].
fn reconnect_pause(pause: Duration) -> Duration {
    pause.max(MIN_RECONNECT)
}

/// Байтовый поток одного соединения.
///
/// Сессия работает только с этими трейтами: ей всё равно, сокет это или COM-порт.
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> Transport for T where T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized {}

/// Соединение, которое выдал источник.
///
/// `peer` и `local` — строки для события `open`: у TCP это адреса сторон, у COM-порта
/// `peer` — имя порта, а `local` пустая (адресов у порта нет, см.
/// `docs/reference/session-format.md`).
pub struct Connection {
    /// Байтовый поток соединения.
    pub transport: Box<dyn Transport>,
    /// Кто на другом конце: адрес анализатора или имя порта.
    pub peer: String,
    /// Локальная сторона: адрес сокета или пустая строка у порта.
    pub local: String,
    /// Исходящая сторона пары прокси (FR-5); у остальных источников её нет.
    pub link: Option<Link>,
    /// Второй дескриптор сокета, если пару закрывают сбросом (FR-7).
    ///
    /// Заполняется только источником прокси: сброс одной стороны должен уйти сбросом
    /// и второй, иначе зависший собеседник ждал бы `FIN`.
    pub reset: Option<ResetSocket>,
}

/// Исходящая сторона пары прокси: сокет, который bytetap открыл к цели.
///
/// Входящая сторона лежит в самом [`Connection`]: вместе они и есть одна пара, одно
/// соединение журнала. `role` — кто на дальнем конце: направление фрагментов и сторона
/// закрытия считаются по роли, а не по тому, кто инициировал соединение (FR-5, FR-7).
pub struct Link {
    /// Байтовый поток исходящего сокета.
    pub transport: Box<dyn Transport>,
    /// Кто на дальнем конце: анализатор или ЛИС.
    pub role: LinkRole,
    /// Адрес цели, к которой подключился bytetap.
    pub peer: String,
    /// Локальный адрес исходящего сокета.
    pub local: String,
    /// Второй дескриптор исходящего сокета для закрытия сбросом (FR-7).
    pub reset: Option<ResetSocket>,
}

/// Сокет пары, который можно закрыть сбросом (`RST`), а не `FIN` (FR-7).
///
/// Держит второй дескриптор того же сокета: `SO_LINGER 0` ставится перед тем, как рабочие
/// половины пары выгружаются, и собеседник получает сброс сразу, а не ждёт закрытия
/// полуоткрытого соединения. Ошибка копирования дескриптора пару не роняет: без ручки
/// сторона закроется обычным `FIN`.
///
/// Дескриптор не отдаётся реактору: `SO_LINGER` — настройка сокета, а не ввод-вывод.
/// Копия нужна только на закрытии пары, а `Connection` и `Link` едут на каждом
/// соединении, поэтому лишняя регистрация в реакторе была бы платой ни за что.
#[derive(Debug)]
pub struct ResetSocket(ResetHandle);

/// Платформенная ручка второго дескриптора: владеющая, но не зарегистрированная.
#[cfg(unix)]
type ResetHandle = std::os::fd::OwnedFd;
#[cfg(windows)]
type ResetHandle = std::os::windows::io::OwnedSocket;
#[cfg(not(any(unix, windows)))]
type ResetHandle = std::net::TcpStream;

impl ResetSocket {
    /// Второй дескриптор того же сокета; `None` — копия не получилась.
    #[must_use]
    pub fn of(stream: &TcpStream) -> Option<Self> {
        duplicate(stream).ok().map(Self)
    }

    /// Дальнейшие закрытия этого сокета уходят сбросом, а не `FIN`.
    ///
    /// Ошибка не обрабатывается: без `SO_LINGER` сторона просто закроется `FIN`, и это
    /// не повод терять уже принятое решение о закрытии пары.
    pub fn reset(&self) {
        let _ = zero_linger(&self.0);
    }
}

/// Копия дескриптора сокета: второй дескриптор того же соединения.
///
/// Платформенная ручка берётся у tokio-сокета как владеющая копия и не заворачивается
/// обратно в [`TcpStream`]: `from_std` зарегистрировал бы её в реакторе.
#[cfg(any(unix, windows))]
fn duplicate(stream: &TcpStream) -> io::Result<ResetHandle> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;

        stream.as_fd().try_clone_to_owned()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsSocket as _;

        stream.as_socket().try_clone_to_owned()
    }
}

#[cfg(not(any(unix, windows)))]
fn duplicate(_stream: &TcpStream) -> io::Result<ResetHandle> {
    Err(io::Error::other("копия сокета не поддерживается"))
}

/// `SO_LINGER 0` на второй ручке: закрытие уходит сбросом (FR-7).
///
/// Настройка сокета, а не ввод-вывод: `socket2::SockRef` берёт дескриптор на время
/// вызова и обратно его не отдаёт.
#[cfg(any(unix, windows))]
fn zero_linger(handle: &ResetHandle) -> io::Result<()> {
    socket2::SockRef::from(handle).set_linger(Some(Duration::ZERO))
}

#[cfg(not(any(unix, windows)))]
fn zero_linger(_handle: &ResetHandle) -> io::Result<()> {
    Err(io::Error::other("SO_LINGER не поддерживается"))
}

/// Топология прокси: кто из двух сторон подключается к bytetap (FR-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyRole {
    /// Анализатор — клиент: bytetap слушает порт для анализатора и подключается к ЛИС.
    AnalyzerClient,
    /// Анализатор — сервер: bytetap слушает порт для ЛИС и подключается к анализатору.
    AnalyzerServer,
}

impl ProxyRole {
    /// Значение роли в поле `settings.role` заголовка журнала.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnalyzerClient => "analyzer_client",
            Self::AnalyzerServer => "analyzer_server",
        }
    }

    /// Кто стоит на дальнем конце пары: цель подключения.
    #[must_use]
    pub const fn target_role(self) -> LinkRole {
        match self {
            // Анализатор приходит сам, bytetap идёт к ЛИС.
            Self::AnalyzerClient => LinkRole::Lis,
            // ЛИС приходит сама, bytetap идёт к анализатору.
            Self::AnalyzerServer => LinkRole::Analyzer,
        }
    }
}

impl std::fmt::Display for ProxyRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Что источник вернул на очередной запрос.
pub enum Next {
    /// Соединение готово: можно писать `open` и обслуживать его.
    Connected(Connection),
    /// Соединения нет; `after` — сколько подождать до повтора.
    ///
    /// Ошибка уже описана для журнала, а пауза живёт в сессии: так «Стоп» и отказ
    /// журнала прерывают ожидание, а недоступный анализатор не забивает журнал.
    Retry {
        /// Причина неудачи.
        error: io::Error,
        /// Пауза до следующей попытки.
        after: Duration,
        /// На каком шаге случилась неудача: от этого зависит код в журнале.
        stage: Stage,
    },
}

/// Шаг источника, на котором случилась ошибка.
///
/// Ошибка приёма и ошибка подключения к цели — разные события для инженера: у прокси
/// недоступная ЛИС и сломанный `accept` не должны попадать под один код (M4-AC6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Приём входящего соединения: TCP-сервер и прокси.
    Accept,
    /// Исходящее подключение: TCP-клиент и цель прокси.
    Connect,
    /// Открытие COM-порта.
    Open,
}

/// Пара прокси замкнулась на адрес прослушивания: цель и слушатель — одно и то же.
///
/// Отдельный тип нужен, чтобы сессия дала этой ошибке свой код (`proxy_target_is_self`),
/// а не общий код отказа подключения: причина не в доступности цели, а в настройке.
#[derive(Debug)]
pub struct ProxyTargetIsSelf {
    /// Адрес, который оказался и целью, и прослушиванием.
    pub addr: SocketAddr,
}

impl std::fmt::Display for ProxyTargetIsSelf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "proxy target {} is the listening address itself: the proxy would connect to itself",
            self.addr
        )
    }
}

impl std::error::Error for ProxyTargetIsSelf {}

/// Ошибка «цель совпадает с адресом прослушивания» в обёртке [`io::Error`].
fn target_is_self_error(addr: SocketAddr) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, ProxyTargetIsSelf { addr })
}

/// `true`, если ошибка старта или источника — «цель совпадает с прослушиванием».
#[must_use]
pub fn is_proxy_target_is_self(err: &io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<ProxyTargetIsSelf>())
}

/// Входящее соединение пришло с адреса, который не разрешён настройкой `allow_from`.
///
/// Отдельный тип нужен, чтобы сессия дала отказу свой код (`proxy_peer_rejected`),
/// а не общий код отказа приёма: причина не в слушателе, а в том, кто пришёл.
#[derive(Debug)]
pub struct ProxyPeerRejected {
    /// Адрес отвергнутой стороны без порта: порт входящего выбирает клиент, и в тексте
    /// отказа он не нужен — одинаковые отказы должны схлопываться в одно событие.
    pub peer: Option<IpAddr>,
    /// Адрес, с которого принимать разрешено.
    pub allowed: IpAddr,
}

impl std::fmt::Display for ProxyPeerRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let peer = self
            .peer
            .map_or_else(|| "an unrecognised address".to_owned(), |ip| ip.to_string());
        write!(
            f,
            "proxy peer {peer} is not allowed: only {} may connect, the connection is closed without reaching the target",
            self.allowed
        )
    }
}

impl std::error::Error for ProxyPeerRejected {}

/// `true`, если ошибка источника — «входящее с неразрешённого адреса».
#[must_use]
pub fn is_proxy_peer_rejected(err: &io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<ProxyPeerRejected>())
}

/// Ошибка «входящее с неразрешённого адреса» в обёртке [`io::Error`].
///
/// Вид ошибки — `PermissionDenied`: отказ в доступе по настройке, а не сбой сокета.
fn peer_rejected_error(peer: Option<IpAddr>, allowed: IpAddr) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        ProxyPeerRejected { peer, allowed },
    )
}

/// IP-адрес принятой стороны; порт входящего для фильтра не важен.
fn peer_ip(peer: &str) -> Option<IpAddr> {
    peer.parse::<SocketAddr>().ok().map(|addr| addr.ip())
}

/// Один и тот же адрес без IPv4-mapped префикса.
fn same_ip(a: IpAddr, b: IpAddr) -> bool {
    fn plain(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            IpAddr::V4(_) => ip,
        }
    }
    plain(a) == plain(b)
}

/// Источник соединений сессии.
///
/// `next` — единственный метод: источник либо отдаёт соединение, либо говорит, через
/// сколько повторить. Состояние источника (очередь принятых сокетов, пауза `accept`,
/// уже открытый порт) живёт внутри.
pub trait ConnectionSource: Send {
    /// Ждёт следующее соединение или указывает паузу до повтора.
    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>>;

    /// Источник ведёт одно соединение за раз.
    ///
    /// TCP-сервер принимает анализаторы параллельно, а у TCP-клиента и COM-порта
    /// соединение одно: следующее открывается только после закрытия предыдущего.
    fn sequential(&self) -> bool {
        false
    }

    /// Пауза между закрытием соединения и следующей попыткой.
    ///
    /// Для TCP-клиента и COM-порта это интервал переподключения (FR-2, FR-9): после
    /// разрыва анализатор может быть ещё не готов. Сессия выдерживает паузу в `select!`
    /// вместе с остановкой, поэтому «Стоп» её не ждёт.
    fn pause_between_connections(&self) -> Duration {
        Duration::ZERO
    }
}

/// Источник соединений из настроек сессии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Ждать анализатор на адресе; порт `0` — любой свободный.
    TcpServer {
        /// Адрес и порт прослушивания.
        listen: SocketAddr,
    },
    /// Подключаться к анализатору-серверу.
    TcpClient {
        /// Адрес анализатора `HOST:PORT`; имя узла разрешается при каждой попытке.
        addr: String,
        /// Пауза между попытками подключения.
        reconnect: Duration,
    },
    /// Открывать COM-порт вместо ЛИС.
    Serial {
        /// Параметры порта (FR-11).
        settings: SerialSettings,
        /// Пауза между попытками открыть порт.
        reconnect: Duration,
    },
    /// Встать в разрыв между анализатором и ЛИС (FR-5).
    Proxy {
        /// Адрес и порт, на который приходит одна из сторон; порт `0` — любой свободный.
        listen: SocketAddr,
        /// Цель, к которой bytetap подключается на каждое входящее соединение.
        target: String,
        /// Кто подключается к bytetap: анализатор или ЛИС.
        role: ProxyRole,
        /// Пауза после неудачного подключения к цели.
        retry: Duration,
        /// Принимать входящие только с этого адреса; `None` — с любого (FR-5, FR-8).
        allow_from: Option<IpAddr>,
    },
}

/// Готовый источник: соединения и строка для строки состояния.
pub struct OpenedSource {
    /// Источник, из которого сессия берёт соединения.
    pub source: Box<dyn ConnectionSource>,
    /// Что показать в строке состояния: адрес слушателя, адрес анализатора или имя порта.
    pub local: String,
}

impl Source {
    /// Готовит источник: занимает адрес, проверяет настройки порта и открывает его.
    ///
    /// Всё, что может отказать до создания журнала, отказывает здесь: занятый порт,
    /// неверные настройки COM, отсутствующий или занятый порт (FR-11, M3-AC3).
    /// TCP-клиент не требует доступного анализатора: первая неудачная попытка станет
    /// событием `error`, а не отказом старта (M3-AC1).
    ///
    /// # Errors
    ///
    /// Порт занят, настройки порта неверны, порт не найден или недоступен.
    pub async fn open(&self) -> io::Result<OpenedSource> {
        match self {
            Self::TcpServer { listen } => {
                let server = TcpServer::bind(*listen).await?;
                let local = server.local_addr()?.to_string();
                Ok(OpenedSource {
                    source: Box::new(TcpServerSource::new(server)),
                    local,
                })
            }
            Self::TcpClient { addr, reconnect } => Ok(OpenedSource {
                source: Box::new(TcpClientSource {
                    addr: addr.clone(),
                    reconnect: *reconnect,
                }),
                // До первого соединения локального адреса нет: показываем, куда идём.
                local: addr.clone(),
            }),
            Self::Serial {
                settings,
                reconnect,
            } => {
                settings.validate()?;
                let connection = serial::open(settings).await?;
                Ok(OpenedSource {
                    source: Box::new(SerialSource {
                        settings: settings.clone(),
                        reconnect: *reconnect,
                        pending: Some(connection),
                    }),
                    local: settings.port.clone(),
                })
            }
            Self::Proxy {
                listen,
                target,
                role,
                retry,
                allow_from,
            } => {
                // Цель стартом не проверяется: недоступная ЛИС — событие `error`,
                // а не отказ старта (M3-AC1, M4-AC6). Проверяется только петля на себя:
                // прокси, подключённый к своему же порту, — ошибка настройки, и ждать
                // её событием в журнале незачем.
                let server = TcpServer::bind(*listen).await?;
                let bound = server.local_addr()?;
                // Имя узла не разбирается здесь: проверка по адресу, а не по DNS.
                if let Ok(target_addr) = target.parse::<SocketAddr>()
                    && loops_on_itself(bound, target_addr)
                {
                    return Err(target_is_self_error(target_addr));
                }
                let local = bound.to_string();
                Ok(OpenedSource {
                    source: Box::new(ProxySource {
                        accepted: TcpServerSource::new(server).with_reset(),
                        target: target.clone(),
                        role: *role,
                        retry: *retry,
                        allow_from: *allow_from,
                        connecting: JoinSet::new(),
                        listen: bound,
                        self_loop: Arc::new(AtomicBool::new(false)),
                    }),
                    local,
                })
            }
        }
    }

    /// Режим журнала, которому соответствует источник.
    #[must_use]
    pub const fn mode(&self) -> crate::journal::Mode {
        match self {
            Self::TcpServer { .. } => crate::journal::Mode::TcpServer,
            Self::TcpClient { .. } => crate::journal::Mode::TcpClient,
            Self::Serial { .. } => crate::journal::Mode::Serial,
            Self::Proxy { .. } => crate::journal::Mode::Proxy,
        }
    }

    /// Фактические настройки источника для заголовка журнала.
    ///
    /// `local` — то, что вернул [`Source::open`]: у TCP-сервера это адрес, выбранный
    /// системой при порте `0`, поэтому журнал совпадает с тем, куда подключался анализатор.
    #[must_use]
    pub fn settings(&self, local: &str) -> serde_json::Value {
        match self {
            Self::TcpServer { .. } => serde_json::json!({"listen": local}),
            Self::TcpClient { addr, reconnect } => serde_json::json!({
                "connect": addr,
                "reconnect_sec": reconnect.as_secs(),
            }),
            Self::Serial { settings, .. } => serde_json::json!({
                "port": settings.port,
                "baud": settings.baud,
                "format": settings.format().to_string(),
                "flow": settings.flow.as_str(),
                "dtr": settings.dtr,
                "rts": settings.rts,
            }),
            Self::Proxy {
                target,
                role,
                allow_from,
                ..
            } => {
                // Фильтр попадает в журнал, только если он включён: отсутствие поля
                // значит «принимаю с любого адреса», как и до фильтра (M4-AC17).
                let mut settings = serde_json::json!({
                    "listen": local,
                    "target": target,
                    "role": role.as_str(),
                });
                if let Some(allowed) = allow_from
                    && let Some(map) = settings.as_object_mut()
                {
                    map.insert(
                        "allow_from".to_owned(),
                        serde_json::json!(allowed.to_string()),
                    );
                }
                settings
            }
        }
    }
}

/// Прокси: принимает сторону и на каждое входящее соединение подключается к цели (FR-5).
///
/// Пара появляется только целиком: сначала принятый сокет, затем исходящий. Неудачное
/// подключение к цели не создаёт соединения — принятый сокет закрывается, а сессия
/// получает [`Next::Retry`]: `open` без второго сокета не пишется (M4-AC6).
///
/// Подключения к цели идут в [`JoinSet`] внутри источника. Сессия вызывает `next` в
/// `select!` вместе с остановкой, отказом журнала, комментарием и закрытием соседней пары,
/// поэтому `next` отменяется в произвольный момент: принятое соединение и начатое
/// подключение лежат в задаче, а не в будущем `next`, и отмена их не теряет — результат
/// заберёт следующий вызов. Заодно подключения идут параллельно: пара, ждущая молчащую
/// цель, не задерживает следующую (M4-AC16).
///
/// Слушатель ведёт [`TcpServerSource`]: ошибка `accept` повторяется с той же растущей
/// паузой, что у TCP-сервера, а `TCP_NODELAY` ставится на обоих сокетах пары (NFR-6).
///
/// Адрес прослушивания известен только после старта (порт `0` выбирает система), а цель
/// приходит строкой — именем узла, чужим адресом или адресом прослушивания. Поэтому петля
/// на себя ловится не настройкой, а после подключения: соединение с собственным
/// слушателем узнаётся по адресам его концов. Найденная петля поднимает общий флаг,
/// и следующие входящие закрываются без подключения — серия схлопывается в одно `error`
/// (M4-AC6).
struct ProxySource {
    accepted: TcpServerSource,
    target: String,
    role: ProxyRole,
    retry: Duration,
    /// Принимать входящие только с этого адреса; `None` — с любого.
    ///
    /// Фильтр стоит до подключения к цели: любой узел сети, достучавшийся до порта,
    /// иначе говорил бы с ЛИС от имени анализатора (FR-8).
    allow_from: Option<IpAddr>,
    /// Подключения к цели, которые уже идут, и их результаты для сессии.
    connecting: JoinSet<Next>,
    /// Адрес прослушивания: по нему узнаётся петля на себя.
    listen: SocketAddr,
    /// Петля на себя уже найдена: пара не создаётся ни на одном следующем входящем.
    self_loop: Arc<AtomicBool>,
}

/// Сколько подключений к цели идёт одновременно.
///
/// Входящие приходят быстрее, чем цель отвечает: без предела приём забирал бы их сотнями,
/// а каждая задача держала бы принятый сокет и полный срок подключения. Предел
/// останавливает приём, пока не освободится место, — очередь ждёт у слушателя, а не
/// в памяти bytetap (M4-AC16).
const MAX_CONNECTING: usize = 64;

impl ProxySource {
    /// Запускает подключение к цели отдельной задачей.
    ///
    /// Принятый сокет переезжает в задачу целиком: пока идёт подключение, входящая сторона
    /// жива, и отмена `next` ничего не рвёт посреди пути.
    fn spawn_connect(&mut self, accepted: Connection, after: Duration) {
        let target = self.target.clone();
        let role = self.role.target_role();
        let listen = self.listen;
        let self_loop = Arc::clone(&self.self_loop);
        self.connecting.spawn(async move {
            connect_target(accepted, target, role, listen, self_loop, after).await
        });
    }
}

impl ConnectionSource for ProxySource {
    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>> {
        Box::pin(async move {
            let after = reconnect_pause(self.retry);
            loop {
                tokio::select! {
                    // Готовое подключение: пара уходит сессии. Отмена `join_next` задачу
                    // не прерывает — результат останется в наборе до следующего вызова.
                    joined = self.connecting.join_next(), if !self.connecting.is_empty() => {
                        match joined {
                            Some(Ok(next)) => return next,
                            Some(Err(err)) => {
                                return Next::Retry {
                                    error: io::Error::other(format!(
                                        "proxy connect task failed: {err}"
                                    )),
                                    after,
                                    stage: Stage::Connect,
                                };
                            }
                            // Набор опустел между проверкой и вызовом: ждём приём.
                            None => {}
                        }
                    }
                    accepted = self.accepted.next(), if self.connecting.len() < MAX_CONNECTING => {
                        match accepted {
                            Next::Connected(connection) => {
                                // Чужой адрес: соединение закрывается сразу, к цели
                                // bytetap не подключается, а отказ схлопывается с
                                // остальными ошибками источника (FR-8).
                                if let Some(allowed) = self.allow_from {
                                    let peer = peer_ip(&connection.peer);
                                    if !peer.is_some_and(|peer| same_ip(peer, allowed)) {
                                        drop(connection);
                                        return Next::Retry {
                                            error: peer_rejected_error(peer, allowed),
                                            after,
                                            stage: Stage::Accept,
                                        };
                                    }
                                }
                                // Петля уже найдена: пара не создаётся, входящее
                                // закрывается, а серия схлопывается в одно `error`.
                                if self.self_loop.load(Ordering::Acquire) {
                                    drop(connection);
                                    return Next::Retry {
                                        error: target_is_self_error(self.listen),
                                        after,
                                        stage: Stage::Connect,
                                    };
                                }
                                self.spawn_connect(connection, after);
                            }
                            Next::Retry { error, after, stage } => {
                                return Next::Retry { error, after, stage };
                            }
                        }
                    }
                }
            }
        })
    }
}

/// Подключается к цели и собирает пару; отмена этой задачи ничего не теряет.
///
/// Срок подключения — [`PROXY_CONNECT_TIMEOUT`] вместе с тестовой задержкой: она в окне
/// срока, потому что изображает цель, которая принимает `SYN` и не отвечает.
///
/// Успешное подключение ещё не пара: если цель оказалась адресом прослушивания самого
/// bytetap, обе стороны были бы концами одного соединения. Такая петля поднимает флаг
/// [`ProxySource::self_loop`] и возвращает отказ: принятый сокет выгружается, `open`
/// не пишется (M4-AC6).
async fn connect_target(
    accepted: Connection,
    target: String,
    role: LinkRole,
    listen: SocketAddr,
    self_loop: Arc<AtomicBool>,
    after: Duration,
) -> Next {
    let linked = timeout(proxy_connect_timeout(), async {
        test_connect_delay().await;
        TcpStream::connect(target.as_str()).await
    })
    .await;
    match linked {
        Ok(Ok(stream)) => {
            // Пересылка не должна ждать Nagle на обоих сокетах пары (NFR-6).
            let _ = stream.set_nodelay(true);
            let peer = stream.peer_addr();
            let local = stream.local_addr();
            if let (Ok(peer), Ok(local)) = (&peer, &local)
                && connects_to_itself(listen, *peer, *local)
            {
                // Адрес в ошибке — адрес прослушивания: он же и петля, а одинаковый текст
                // повтора нужен, чтобы серия схлопнулась в одно `error`.
                self_loop.store(true, Ordering::Release);
                return Next::Retry {
                    error: target_is_self_error(listen),
                    after,
                    stage: Stage::Connect,
                };
            }
            let link_peer = peer.map_or_else(|_| target.clone(), |addr| addr.to_string());
            let link_local = local.map_or_else(|_| String::new(), |addr| addr.to_string());
            Next::Connected(Connection {
                transport: accepted.transport,
                peer: accepted.peer,
                local: accepted.local,
                reset: accepted.reset,
                link: Some(Link {
                    reset: ResetSocket::of(&stream),
                    transport: Box::new(stream),
                    role,
                    peer: link_peer,
                    local: link_local,
                }),
            })
        }
        // Принятый сокет выгружается вместе с задачей: соединения из одного сокета не бывает.
        Ok(Err(error)) => Next::Retry {
            error,
            after,
            stage: Stage::Connect,
        },
        Err(_elapsed) => Next::Retry {
            error: io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "connect to {target} timed out after {} s",
                    proxy_connect_timeout().as_secs()
                ),
            ),
            after,
            stage: Stage::Connect,
        },
    }
}

/// Соединение с собственным слушателем: прокси подключился сам к себе.
///
/// Сравниваются адреса концов уже открытого соединения, а не настройка: цель могла прийти
/// именем узла (`localhost`, имя машины) или портом, который выбрал вместо нуля сам
/// источник. Признак петли — адрес цели совпал с локальным адресом исходящего сокета
/// (подключение с машины на неё же идёт с того же адреса), порт цели — порт
/// прослушивания, а слушатель этот адрес покрывает: `0.0.0.0` и `::` слушают все адреса
/// машины.
fn connects_to_itself(listen: SocketAddr, peer: SocketAddr, local: SocketAddr) -> bool {
    let (listen, peer, local) = (unmap(listen), unmap(peer), unmap(local));
    if listen.port() != peer.port() || peer.ip() != local.ip() {
        return false;
    }
    match (listen, peer) {
        // `::` на двойном стеке принимает и IPv4: семейства разные, а слушатель один.
        (SocketAddr::V6(listen), _) if listen.ip().is_unspecified() => true,
        (SocketAddr::V4(listen), SocketAddr::V4(peer)) => {
            listen.ip().is_unspecified() || listen.ip() == peer.ip()
        }
        _ => false,
    }
}

/// Адрес без IPv4-mapped префикса: `::ffff:127.0.0.1` — это `127.0.0.1`.
///
/// Двойной стек отдаёт адреса IPv4 в виде IPv6, и без нормализации сравнение не увидело
/// бы петлю.
fn unmap(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(addr) => match addr.ip().to_ipv4_mapped() {
            Some(ip) => SocketAddr::new(ip.into(), addr.port()),
            None => SocketAddr::V6(addr),
        },
        SocketAddr::V4(_) => addr,
    }
}

/// Цель совпадает с адресом прослушивания: прокси замкнулся бы сам на себя.
///
/// Проверка старта для цели, заданной адресом: порт `0` известен только после занятия
/// порта, поэтому цель строкой здесь не разобрать — имя узла и чужой адрес ловит
/// [`connects_to_itself`] после подключения. `0.0.0.0` и `::` слушают все адреса машины,
/// поэтому петлёй считается и любой петлевой адрес (`127.0.0.0/8`, `::1`): до остальных
/// своих адресов проверка не достаёт без перечисления интерфейсов.
fn loops_on_itself(listen: SocketAddr, target: SocketAddr) -> bool {
    if listen.port() != target.port() {
        return false;
    }
    match (listen, target) {
        (listen, target) if listen.is_ipv4() != target.is_ipv4() => false,
        (listen, target) => match (listen.ip(), target.ip()) {
            (listen_ip, target_ip) if listen_ip == target_ip => true,
            (listen_ip, target_ip) if listen_ip.is_unspecified() => {
                target_ip.is_loopback() || target_ip.is_unspecified()
            }
            (_, target_ip) => target_ip.is_unspecified(),
        },
    }
}

/// Сколько ждёт попытка подключения к цели, прежде чем стать `proxy_connect_timeout`.
///
/// Тот же срок, что у TCP-клиента: `connect` без срока висит минутами, а ждать дольше
/// десяти секунд анализатору или ЛИС уже незачем — входящее соединение всё это время
/// стоит без `open` (M4-AC6).
const PROXY_CONNECT_TIMEOUT: Duration = CONNECT_TIMEOUT;

/// Тестовый хук: задержка в окне подключения к цели, миллисекунды.
///
/// Появляется только со сборкой feature `test-hooks`: проверка «отмена `next` не теряет
/// начатое подключение» должна открыть окно на десятки миллисекунд, а не ждать десять
/// секунд. Задержка стоит внутри срока подключения: с [`TEST_PROXY_CONNECT_TIMEOUT_MS`]
/// она изображает цель, которая принимает `SYN` и молчит.
#[cfg(feature = "test-hooks")]
pub static TEST_PROXY_CONNECT_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// Тестовый хук: срок подключения к цели вместо [`PROXY_CONNECT_TIMEOUT`], миллисекунды.
///
/// `0` — хук выключен.
#[cfg(feature = "test-hooks")]
pub static TEST_PROXY_CONNECT_TIMEOUT_MS: AtomicU64 = AtomicU64::new(0);

/// Задержка подключения, если тест её запросил.
#[cfg(feature = "test-hooks")]
async fn test_connect_delay() {
    let millis = TEST_PROXY_CONNECT_DELAY_MS.load(Ordering::Relaxed);
    if millis > 0 {
        tokio::time::sleep(Duration::from_millis(millis)).await;
    }
}

#[cfg(not(feature = "test-hooks"))]
async fn test_connect_delay() {}

/// Срок подключения к цели с поправкой на тестовый хук.
fn proxy_connect_timeout() -> Duration {
    #[cfg(feature = "test-hooks")]
    {
        let millis = TEST_PROXY_CONNECT_TIMEOUT_MS.load(Ordering::Relaxed);
        if millis > 0 {
            return Duration::from_millis(millis);
        }
    }
    PROXY_CONNECT_TIMEOUT
}

/// TCP-клиент: подключается к анализатору-серверу (FR-2).
struct TcpClientSource {
    addr: String,
    reconnect: Duration,
}
impl ConnectionSource for TcpClientSource {
    fn sequential(&self) -> bool {
        true
    }

    fn pause_between_connections(&self) -> Duration {
        reconnect_pause(self.reconnect)
    }

    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>> {
        Box::pin(async move {
            let after = reconnect_pause(self.reconnect);
            let connected =
                tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(self.addr.as_str())).await;
            match connected {
                Ok(Ok(stream)) => {
                    // Подтверждение должно уйти одним пакетом, а не ждать Nagle (NFR-6).
                    let _ = stream.set_nodelay(true);
                    let peer = stream
                        .peer_addr()
                        .map_or_else(|_| self.addr.clone(), |addr| addr.to_string());
                    let local = stream
                        .local_addr()
                        .map_or_else(|_| String::new(), |addr| addr.to_string());
                    Next::Connected(Connection {
                        transport: Box::new(stream),
                        peer,
                        local,
                        link: None,
                        reset: None,
                    })
                }
                Ok(Err(error)) => Next::Retry {
                    error,
                    after,
                    stage: Stage::Connect,
                },
                Err(_elapsed) => Next::Retry {
                    error: io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "connect to {} timed out after {} s",
                            self.addr,
                            CONNECT_TIMEOUT.as_secs()
                        ),
                    ),
                    after,
                    stage: Stage::Connect,
                },
            }
        })
    }
}

/// COM-порт: каждое открытие порта — новое соединение (FR-9).
///
/// Порт, открытый при старте, уже лежит в `pending`: первое соединение не открывает его
/// второй раз. Ошибка открытия после разрыва не роняет сессию — она возвращается как
/// [`Next::Retry`], и порт переоткрывается с тем же интервалом.
struct SerialSource {
    settings: SerialSettings,
    reconnect: Duration,
    pending: Option<SerialConnection>,
}

impl SerialSource {
    fn connected_connection(&mut self, connection: SerialConnection) -> Next {
        Next::Connected(Connection {
            transport: Box::new(connection),
            peer: self.settings.port.clone(),
            // У порта нет адресов: правило записано в `session-format.md` (M3-AC2).
            local: String::new(),
            link: None,
            reset: None,
        })
    }
}

impl ConnectionSource for SerialSource {
    fn sequential(&self) -> bool {
        true
    }

    fn pause_between_connections(&self) -> Duration {
        reconnect_pause(self.reconnect)
    }

    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>> {
        Box::pin(async move {
            if let Some(connection) = self.pending.take() {
                return self.connected_connection(connection);
            }
            match serial::open(&self.settings).await {
                Ok(connection) => self.connected_connection(connection),
                Err(error) => Next::Retry {
                    error,
                    after: reconnect_pause(self.reconnect),
                    stage: Stage::Open,
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_reports_its_journal_mode() {
        let server = Source::TcpServer {
            listen: "127.0.0.1:0".parse().unwrap(),
        };
        assert_eq!(server.mode(), crate::journal::Mode::TcpServer);
        assert_eq!(
            server.settings("127.0.0.1:41234")["listen"],
            "127.0.0.1:41234"
        );

        let client = Source::TcpClient {
            addr: "127.0.0.1:5000".to_owned(),
            reconnect: Duration::from_secs(5),
        };
        assert_eq!(client.mode(), crate::journal::Mode::TcpClient);
        assert_eq!(
            client.settings("127.0.0.1:5000"),
            serde_json::json!({"connect": "127.0.0.1:5000", "reconnect_sec": 5})
        );
    }

    #[test]
    fn a_reconnect_pause_is_never_shorter_than_a_hundred_milliseconds() {
        assert_eq!(reconnect_pause(Duration::ZERO), MIN_RECONNECT);
        assert_eq!(
            reconnect_pause(Duration::from_millis(50)),
            MIN_RECONNECT,
            "нулевой интервал не крутит попытки в цикле"
        );
        assert_eq!(
            reconnect_pause(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn a_proxy_source_reports_its_settings_and_the_target_role() {
        let proxy = Source::Proxy {
            listen: "0.0.0.0:50025".parse().unwrap(),
            target: "10.0.0.5:5000".to_owned(),
            role: ProxyRole::AnalyzerClient,
            retry: Duration::from_millis(100),
            allow_from: None,
        };
        assert_eq!(proxy.mode(), crate::journal::Mode::Proxy);
        assert_eq!(
            proxy.settings("0.0.0.0:50025"),
            serde_json::json!({
                "listen": "0.0.0.0:50025",
                "target": "10.0.0.5:5000",
                "role": "analyzer_client",
            })
        );
        assert_eq!(ProxyRole::AnalyzerClient.target_role(), LinkRole::Lis);
        assert_eq!(ProxyRole::AnalyzerServer.target_role(), LinkRole::Analyzer);
        assert_eq!(ProxyRole::AnalyzerServer.as_str(), "analyzer_server");
        assert_eq!(ProxyRole::AnalyzerClient.to_string(), "analyzer_client");
    }

    #[tokio::test]
    async fn a_proxy_binds_its_port_and_does_not_require_the_target() {
        // Порт цели занят сокетом без `listen`: подключение откажет, но старт — нет.
        let socket = tokio::net::TcpSocket::new_v4().expect("сокет");
        socket
            .bind("127.0.0.1:0".parse().unwrap())
            .expect("порт занят без прослушивания");
        let target = socket.local_addr().expect("адрес цели");
        let source = Source::Proxy {
            listen: "127.0.0.1:0".parse().unwrap(),
            target: target.to_string(),
            role: ProxyRole::AnalyzerClient,
            retry: Duration::from_millis(100),
            allow_from: None,
        };
        let mut opened = source.open().await.expect("старт без цели");
        assert_ne!(
            opened.local.parse::<SocketAddr>().expect("адрес").port(),
            0,
            "порт выбран системой"
        );
        let analyzer =
            tokio::net::TcpStream::connect(opened.local.parse::<SocketAddr>().expect("адрес"))
                .await
                .expect("анализатор пришёл");
        match opened.source.next().await {
            Next::Retry { after, .. } => {
                assert_eq!(after, Duration::from_millis(100), "пауза источника")
            }
            Next::Connected(_) => panic!("цели нет"),
        }
        drop(analyzer);
        drop(socket);
    }

    #[test]
    fn a_target_on_the_listening_address_is_a_loop() {
        let listen: SocketAddr = "0.0.0.0:50025".parse().unwrap();
        assert!(loops_on_itself(listen, "0.0.0.0:50025".parse().unwrap()));
        assert!(loops_on_itself(listen, "127.0.0.1:50025".parse().unwrap()));
        assert!(loops_on_itself(listen, "127.0.0.2:50025".parse().unwrap()));
        assert!(
            !loops_on_itself(listen, "192.168.1.10:50025".parse().unwrap()),
            "адреса интерфейсов без перечисления не проверяются"
        );
        assert!(
            !loops_on_itself(listen, "127.0.0.1:5000".parse().unwrap()),
            "порт решает"
        );
        let loopback: SocketAddr = "127.0.0.1:50025".parse().unwrap();
        assert!(loops_on_itself(loopback, loopback));
        assert!(loops_on_itself(loopback, "0.0.0.0:50025".parse().unwrap()));
        assert!(
            !loops_on_itself(loopback, "192.168.1.10:50025".parse().unwrap()),
            "чужой адрес"
        );
        assert!(
            !loops_on_itself(
                "127.0.0.1:50025".parse().unwrap(),
                "[::1]:50025".parse().unwrap()
            ),
            "разные семейства адресов"
        );
    }

    #[test]
    fn a_connection_to_its_own_listening_address_is_recognised() {
        // Подключение с машины на неё же идёт с того же адреса: цель и локальный адрес
        // исходящего сокета совпадают, а порт цели — порт прослушивания.
        let listen: SocketAddr = "127.0.0.1:50025".parse().unwrap();
        let peer: SocketAddr = "127.0.0.1:50025".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:50231".parse().unwrap();
        assert!(connects_to_itself(listen, peer, local));
        // Слушатель на всех адресах покрывает и петлевой адрес, и адрес интерфейса.
        let any: SocketAddr = "0.0.0.0:50025".parse().unwrap();
        assert!(connects_to_itself(any, peer, local));
        assert!(connects_to_itself(
            any,
            "192.168.1.10:50025".parse().unwrap(),
            "192.168.1.10:50231".parse().unwrap()
        ));
        // Двойной стек: `::` принимает IPv4, а `::ffff:` — тот же адрес IPv4.
        assert!(connects_to_itself(
            "[::]:50025".parse().unwrap(),
            peer,
            local
        ));
        assert!(connects_to_itself(
            "[::]:50025".parse().unwrap(),
            "[::ffff:127.0.0.1]:50025".parse().unwrap(),
            "[::ffff:127.0.0.1]:50231".parse().unwrap()
        ));
        // Чужая цель: другой адрес, другой порт или другой порт слушателя.
        assert!(!connects_to_itself(
            listen,
            "192.168.1.10:50025".parse().unwrap(),
            "192.168.1.10:50231".parse().unwrap()
        ));
        assert!(!connects_to_itself(
            listen,
            "127.0.0.1:5000".parse().unwrap(),
            local
        ));
        // Исходящий сокет на другом адресе: цель — другой узел с тем же портом.
        assert!(!connects_to_itself(
            listen,
            peer,
            "192.168.1.10:50231".parse().unwrap()
        ));
        // Слушатель только на петлевом адресе не покрывает адрес интерфейса.
        let loopback: SocketAddr = "127.0.0.1:50025".parse().unwrap();
        assert!(!connects_to_itself(
            loopback,
            "192.168.1.10:50025".parse().unwrap(),
            "192.168.1.10:50231".parse().unwrap()
        ));
    }

    /// Источник прокси на уже занятом адресе: порт известен тесту.
    async fn proxy_on(listen: SocketAddr) -> io::Result<(ProxySource, SocketAddr)> {
        let server = TcpServer::bind(listen).await?;
        let bound = server.local_addr()?;
        Ok((
            ProxySource {
                accepted: TcpServerSource::new(server).with_reset(),
                target: String::new(),
                role: ProxyRole::AnalyzerClient,
                retry: Duration::from_millis(100),
                allow_from: None,
                connecting: JoinSet::new(),
                listen: bound,
                self_loop: Arc::new(AtomicBool::new(false)),
            },
            bound,
        ))
    }

    /// Одна петля: входящее закрывается, пара не создаётся, отказ — `proxy_target_is_self`.
    async fn assert_one_loop(source: &mut ProxySource, come_to: SocketAddr, case: &str) {
        let _first = TcpStream::connect(come_to)
            .await
            .unwrap_or_else(|error| panic!("{case}: сторона пришла: {error}"));
        let error = match source.next().await {
            Next::Retry { error, .. } => error,
            Next::Connected(_) => panic!("{case}: петля создала пару"),
        };
        assert!(
            is_proxy_target_is_self(&error),
            "{case}: код петли виден снаружи: {error}"
        );
        assert!(
            error.to_string().contains("connect to itself"),
            "{case}: {error}"
        );
        // Флаг поднят: следующее входящее закрывается без подключения, а текст отказа
        // тот же — иначе серия не схлопнулась бы в одно `error`.
        let _second = TcpStream::connect(come_to)
            .await
            .unwrap_or_else(|error| panic!("{case}: вторая сторона пришла: {error}"));
        match source.next().await {
            Next::Retry { error: repeat, .. } => {
                assert_eq!(repeat.to_string(), error.to_string(), "{case}");
            }
            Next::Connected(_) => panic!("{case}: петля создала вторую пару"),
        }
    }

    #[tokio::test]
    async fn a_proxy_catches_a_loop_to_its_own_listening_address() {
        // Порт выбирает система, а цель приходит строкой: проверка старта такую петлю
        // не видит, её ловит подключение (M4-AC6).
        let (mut source, bound) = proxy_on("127.0.0.1:0".parse().unwrap())
            .await
            .expect("слушатель");
        source.target = format!("localhost:{}", bound.port());
        assert_one_loop(&mut source, bound, "localhost").await;

        // Слушатель на всех адресах, цель — петлевой адрес.
        let (mut source, bound) = proxy_on("0.0.0.0:0".parse().unwrap())
            .await
            .expect("слушатель");
        source.target = format!("127.0.0.1:{}", bound.port());
        let come_to = SocketAddr::new("127.0.0.1".parse().unwrap(), bound.port());
        assert_one_loop(&mut source, come_to, "0.0.0.0").await;

        // `::` на двойном стеке: слушатель один, а семейства адресов разные. Окружение
        // без IPv6 или с выключенным двойным стеком случай пропускает.
        let Ok((mut source, bound)) = proxy_on("[::]:0".parse().unwrap()).await else {
            return;
        };
        source.target = format!("127.0.0.1:{}", bound.port());
        let Ok(_first) = TcpStream::connect(("127.0.0.1", bound.port())).await else {
            return;
        };
        match source.next().await {
            Next::Retry { error, .. } => assert!(is_proxy_target_is_self(&error), "{error}"),
            Next::Connected(_) => panic!("[::] и 127.0.0.1 — одна петля"),
        }
    }

    #[tokio::test]
    async fn a_proxy_refuses_a_target_that_is_its_own_listening_address() {
        // Свободный порт занимается и отпускается: проверка старта сравнивает адрес цели
        // с адресом, который занял сам источник.
        let socket = tokio::net::TcpSocket::new_v4().expect("сокет");
        socket
            .bind("127.0.0.1:0".parse().unwrap())
            .expect("свободный порт");
        let addr = socket.local_addr().expect("адрес");
        drop(socket);

        let source = Source::Proxy {
            listen: addr,
            target: addr.to_string(),
            role: ProxyRole::AnalyzerClient,
            retry: Duration::from_millis(100),
            allow_from: None,
        };
        let err = match source.open().await {
            Ok(_) => panic!("цель — свой же порт"),
            Err(err) => err,
        };
        assert!(
            is_proxy_target_is_self(&err),
            "код петли виден снаружи: {err}"
        );
        assert!(err.to_string().contains("connect to itself"), "{err}");
    }

    #[tokio::test]
    async fn a_reset_socket_is_a_second_descriptor_of_the_same_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("слушатель");
        let addr = listener.local_addr().expect("адрес");
        let client = TcpStream::connect(addr).await.expect("клиент");
        let (server, _) = listener.accept().await.expect("приём");
        let reset = ResetSocket::of(&client).expect("второй дескриптор");
        // Сброс выставлен на сокет пары, а не на копию: последнее закрытие уходит `RST`.
        reset.reset();
        drop(client);
        drop(reset);
        let mut buffer = [0u8; 8];
        let error = tokio::time::timeout(Duration::from_secs(5), server.peek(&mut buffer))
            .await
            .expect("сброс дошёл")
            .expect_err("соединение сброшено, а не закрыто");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }

    #[tokio::test]
    async fn a_client_starts_without_an_analyzer_and_retries_with_the_configured_pause() {
        // Порт занят сокетом без `listen`: подключение откажет, но старт источника — нет.
        // Сокет держится до конца проверки: иначе порт достался бы другому тесту (флак M4).
        let socket = tokio::net::TcpSocket::new_v4().expect("сокет");
        socket
            .bind("127.0.0.1:0".parse().unwrap())
            .expect("порт занят без прослушивания");
        let addr = socket.local_addr().expect("адрес");
        let source = Source::TcpClient {
            addr: addr.to_string(),
            reconnect: Duration::from_secs(7),
        };
        let mut opened = source.open().await.expect("старт без анализатора");
        match opened.source.next().await {
            Next::Retry { after, .. } => assert_eq!(after, Duration::from_secs(7)),
            Next::Connected(_) => panic!("анализатора нет"),
        }
        assert_eq!(opened.local, addr.to_string());
    }
}
