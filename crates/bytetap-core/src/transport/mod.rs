//! Транспорты сессии: источник соединений и байтовый поток (FR-1, FR-2, FR-9).
//!
//! Источник знает, откуда берутся соединения: `TcpServer` принимает анализатор,
//! `TcpClient` сам подключается к анализатору-серверу, `Serial` открывает COM-порт.
//! Дальше всё одинаково: [`Connection`] отдаёт [`Transport`] — `AsyncRead + AsyncWrite`,
//! а нумерация `conn`, автоответчик и журнал остаются делом сессии ([`crate::session`]).
//!
//! Источник не нумерует соединения и ничего не пишет в журнал. Ошибка подключения или
//! открытия порта возвращается как [`Next::Retry`]: сколько ждать до повтора, решает
//! источник, а сессия схлопывает повторы в одно `error` и ждёт в `select!` вместе
//! с остановкой и отказом журнала (NFR-5).
//!
//! Прокси TCP появится в M4 и станет ещё одним источником.

pub mod serial;
pub mod tcp;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use self::serial::{SerialConnection, SerialSettings};
use self::tcp::{TcpServer, TcpServerSource};

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
        }
    }

    /// Режим журнала, которому соответствует источник.
    #[must_use]
    pub const fn mode(&self) -> crate::journal::Mode {
        match self {
            Self::TcpServer { .. } => crate::journal::Mode::TcpServer,
            Self::TcpClient { .. } => crate::journal::Mode::TcpClient,
            Self::Serial { .. } => crate::journal::Mode::Serial,
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
        }
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
        self.reconnect
    }

    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>> {
        Box::pin(async move {
            match TcpStream::connect(self.addr.as_str()).await {
                Ok(stream) => {
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
                    })
                }
                Err(error) => Next::Retry {
                    error,
                    after: self.reconnect,
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
        })
    }
}

impl ConnectionSource for SerialSource {
    fn sequential(&self) -> bool {
        true
    }

    fn pause_between_connections(&self) -> Duration {
        self.reconnect
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
                    after: self.reconnect,
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

    #[tokio::test]
    async fn a_client_starts_without_an_analyzer_and_retries_with_the_configured_pause() {
        // Порт заведомо свободен: подключение откажет, но старт источника — нет.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
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
