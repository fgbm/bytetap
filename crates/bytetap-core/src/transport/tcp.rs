//! TCP-сервер режима «вместо ЛИС» (FR-1): анализатор подключается как клиент.
//!
//! Слушатель и одно принятое соединение. [`TcpServer::accept`] отдаёт [`TcpConnection`]
//! с адресами сторон: они нужны журналу (`open` по FR-22) и ленте. Модуль не нумерует
//! соединения, не знает про автоответчик и не пишет журнал — это делает сессия.
//!
//! `TCP_NODELAY` включён: подтверждение в один пакет не должно ждать, пока Nagle соберёт
//! ещё данные, иначе таймаут протокола у анализатора может сработать раньше ответа (NFR-6).
//!
//! Ошибка `accept` не роняет источник: [`TcpServerSource`] отдаёт её сессии вместе
//! с паузой до повтора, удваивая паузу до [`ACCEPT_BACKOFF_MAX`]. Занятый порт на старте —
//! по-прежнему ошибка [`TcpServer::bind`].

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

use super::{Connection, ConnectionSource, Next};

/// Первая пауза после ошибки `accept`.
pub(crate) const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(10);

/// Предел паузы после ошибки `accept`: порт, занятый чужой программой, не должен
/// превращаться в поток событий `error` по тысяче в секунду (NFR-5).
pub(crate) const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

/// Следующая пауза после ошибки `accept`: удвоение до [`ACCEPT_BACKOFF_MAX`].
pub(crate) fn next_backoff(previous: Duration) -> Duration {
    (previous * 2).min(ACCEPT_BACKOFF_MAX)
}

/// Слушающий сокет TCP-сервера.
#[derive(Debug)]
pub struct TcpServer {
    listener: TcpListener,
}

impl TcpServer {
    /// Занимает адрес и начинает слушать.
    ///
    /// Порт `0` означает «любой свободный»: фактический адрес даёт [`TcpServer::local_addr`].
    ///
    /// # Errors
    ///
    /// Адрес занят или недоступен.
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener })
    }

    /// Фактический адрес и порт слушателя.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Принимает очередное соединение.
    ///
    /// Метод отменяемый: если `accept` прервать, соединение не теряется.
    pub async fn accept(&self) -> io::Result<TcpConnection> {
        let (stream, peer) = self.listener.accept().await?;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?;
        Ok(TcpConnection {
            stream,
            peer,
            local,
        })
    }
}

/// Источник соединений TCP-сервера.
///
/// Пауза `accept` живёт здесь, а не в сессии: источник решает, через сколько повторять,
/// а сессия ждёт в `select!` вместе с остановкой и отказом журнала (NFR-5).
pub struct TcpServerSource {
    server: TcpServer,
    backoff: Duration,
}

impl TcpServerSource {
    /// Источник поверх занятого адреса.
    #[must_use]
    pub fn new(server: TcpServer) -> Self {
        Self {
            server,
            backoff: ACCEPT_BACKOFF_MIN,
        }
    }
}

impl ConnectionSource for TcpServerSource {
    fn next(&mut self) -> Pin<Box<dyn Future<Output = Next> + Send + '_>> {
        Box::pin(async move {
            match self.server.accept().await {
                Ok(connection) => {
                    self.backoff = ACCEPT_BACKOFF_MIN;
                    let peer = connection.peer().to_string();
                    let local = connection.local().to_string();
                    Next::Connected(Connection {
                        transport: Box::new(connection),
                        peer,
                        local,
                    })
                }
                Err(error) => {
                    let after = self.backoff;
                    self.backoff = next_backoff(after);
                    Next::Retry { error, after }
                }
            }
        })
    }
}

/// Принятое TCP-соединение.
#[derive(Debug)]
pub struct TcpConnection {
    stream: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
}

impl TcpConnection {
    /// Адрес и порт анализатора.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Локальный адрес и порт, на который пришло соединение.
    #[must_use]
    pub const fn local(&self) -> SocketAddr {
        self.local
    }
}

impl AsyncRead for TcpConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;

    use super::*;

    #[tokio::test]
    async fn two_connections_are_accepted_with_their_addresses() {
        let server = TcpServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();
        assert_ne!(addr.port(), 0, "порт выбран операционной системой");

        let mut first_client = TcpStream::connect(addr).await.unwrap();
        let mut first = server.accept().await.unwrap();
        assert_eq!(first.local(), addr);
        assert_eq!(
            first.peer().port(),
            first_client.local_addr().unwrap().port()
        );

        let mut second_client = TcpStream::connect(addr).await.unwrap();
        let mut second = server.accept().await.unwrap();
        assert_eq!(second.local(), addr);
        assert_ne!(second.peer(), first.peer(), "два разных соединения");

        first.write_all(b"ping").await.unwrap();
        let mut buffer = [0u8; 4];
        let read = first_client.read(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..read], b"ping");

        second_client.write_all(b"pong").await.unwrap();
        let read = second.read(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..read], b"pong");
        drop(first_client);
    }

    #[tokio::test]
    async fn a_closed_peer_reads_as_zero() {
        let server = TcpServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let client = TcpStream::connect(server.local_addr().unwrap())
            .await
            .unwrap();
        let mut connection = server.accept().await.unwrap();
        drop(client);
        let mut buffer = [0u8; 8];
        assert_eq!(connection.read(&mut buffer).await.unwrap(), 0, "EOF");
    }

    #[test]
    fn the_accept_backoff_grows_to_one_second() {
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
}
