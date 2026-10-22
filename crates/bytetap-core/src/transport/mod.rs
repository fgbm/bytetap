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
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
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
    },
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
            } => {
                // Цель стартом не проверяется: недоступная ЛИС — событие `error`,
                // а не отказ старта (M3-AC1, M4-AC6).
                let server = TcpServer::bind(*listen).await?;
                let local = server.local_addr()?.to_string();
                Ok(OpenedSource {
                    source: Box::new(ProxySource {
                        accepted: TcpServerSource::new(server),
                        target: target.clone(),
                        role: *role,
                        retry: *retry,
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
            Self::Proxy { target, role, .. } => serde_json::json!({
                "listen": local,
                "target": target,
                "role": role.as_str(),
            }),
        }
    }
}

/// Сколько ждёт попытка подключения к цели прокси, прежде чем стать `proxy_connect_timeout`.
///
/// Тот же срок, что у TCP-клиента: `connect` без срока висит минутами, а ждать дольше
/// десяти секунд анализатору или ЛИС уже незачем — входящее соединение всё это время
/// стоит без `open` (M4-AC6).
const PROXY_CONNECT_TIMEOUT: Duration = CONNECT_TIMEOUT;

/// Прокси: принимает сторону и на каждое входящее соединение подключается к цели (FR-5).
///
/// Пара появляется только целиком: сначала принятый сокет, затем исходящий. Неудачное
/// подключение к цели не создаёт соединения — принятый сокет закрывается, а сессия
/// получает [`Next::Retry`]: `open` без второго сокета не пишется (M4-AC6).
///
/// Слушатель ведёт [`TcpServerSource`]: ошибка `accept` повторяется с той же растущей
/// паузой, что у TCP-сервера, а `TCP_NODELAY` ставится на обоих сокетах пары (NFR-6).
struct ProxySource {
    accepted: TcpServerSource,
    target: String,
    role: ProxyRole,
    retry: Duration,
}

impl ConnectionSource for ProxySource {
    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>> {
        Box::pin(async move {
            let after = reconnect_pause(self.retry);
            let accepted = match self.accepted.next().await {
                Next::Connected(connection) => connection,
                Next::Retry { error, after } => return Next::Retry { error, after },
            };
            let linked = timeout(
                PROXY_CONNECT_TIMEOUT,
                TcpStream::connect(self.target.as_str()),
            )
            .await;
            match linked {
                Ok(Ok(stream)) => {
                    // Пересылка не должна ждать Nagle на обоих сокетах пары (NFR-6).
                    let _ = stream.set_nodelay(true);
                    let link_peer = stream
                        .peer_addr()
                        .map_or_else(|_| self.target.clone(), |addr| addr.to_string());
                    let link_local = stream
                        .local_addr()
                        .map_or_else(|_| String::new(), |addr| addr.to_string());
                    Next::Connected(Connection {
                        transport: accepted.transport,
                        peer: accepted.peer,
                        local: accepted.local,
                        link: Some(Link {
                            transport: Box::new(stream),
                            role: self.role.target_role(),
                            peer: link_peer,
                            local: link_local,
                        }),
                    })
                }
                // Принятый сокет выгружается здесь же: соединения из одного сокета не бывает.
                Ok(Err(error)) => Next::Retry { error, after },
                Err(_elapsed) => Next::Retry {
                    error: io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "connect to {} timed out after {} s",
                            self.target,
                            PROXY_CONNECT_TIMEOUT.as_secs()
                        ),
                    ),
                    after,
                },
            }
        })
    }
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
                    })
                }
                Ok(Err(error)) => Next::Retry { error, after },
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
