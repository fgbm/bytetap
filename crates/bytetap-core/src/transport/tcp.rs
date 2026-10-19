//! TCP-сервер режима «вместо ЛИС» (FR-1): анализатор подключается как клиент.
//!
//! Слушатель и одно принятое соединение. [`TcpServer::accept`] отдаёт [`TcpConnection`]
//! с адресами сторон: они нужны журналу (`open` по FR-22) и ленте. Модуль не нумерует
//! соединения, не знает про автоответчик и не пишет журнал — это делает сессия.
//!
//! `TCP_NODELAY` включён: подтверждение в один пакет не должно ждать, пока Nagle соберёт
//! ещё данные, иначе таймаут протокола у анализатора может сработать раньше ответа (NFR-6).

use std::io;
use std::net::SocketAddr;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// Слушающий сокет TCP-сервера.
#[derive(Debug)]
pub struct TcpServer {
    listener: TcpListener,
}

impl TcpServer {
    /// Занимает адрес и начинает слушать.
    ///
    /// Порт `0` означает «любой свободный»: фактический адрес даёт [`TcpServer::local_addr`].
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

    /// Читает очередной фрагмент. `Ok(0)` — собеседник закрыл соединение.
    ///
    /// Метод отменяемый, если прервать его до готовности данных: прочитанное не теряется.
    pub async fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer).await
    }

    /// Отправляет ответ целиком.
    pub async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }

    /// Отправляет часть ответа и возвращает число байт, принятых сокетом.
    ///
    /// Нужен, чтобы после обрыва посреди ответа в журнал попала именно отправленная
    /// часть: [`AsyncWriteExt::write_all`] об этом не говорит.
    pub async fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.write(bytes).await
    }

    /// Закрывает соединение со своей стороны.
    ///
    /// Ошибка не возвращается: к моменту закрытия собеседник мог уже уйти.
    pub async fn shutdown(&mut self) {
        let _ = self.stream.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
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
}
