//! Эмулятор анализатора-сервера для проверки TCP-клиента bytetap (T-2, M3-AC1).
//!
//! Обычный [`crate::emulator::AnalyzerEmulator`] подключается к bytetap как анализатор.
//! Здесь наоборот: bytetap — клиент, а эмулятор слушает порт. На каждое соединение
//! сценарий отправляет свои сообщения MLLP, ждёт ACK на каждое и закрывает соединение;
//! следующее подключение получает следующий сценарий. Так проверяются переподключение,
//! сквозная нумерация `conn` и пауза между попытками.

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::emulator::{DEFAULT_TIMEOUT, Fragmentation, fragment_bounds, read_frame};

/// Что эмулятор-сервер увидел за прогон.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerReport {
    /// Сколько соединений принял сервер.
    pub connections: usize,
    /// ACK по соединениям и сообщениям: внешний индекс — соединение, внутренний — сообщение.
    pub acks: Vec<Vec<Vec<u8>>>,
    /// Моменты приёма соединений: по разнице видно паузу переподключения.
    pub accepted_at: Vec<Instant>,
}

/// Эмулятор анализатора как TCP-сервер.
#[derive(Debug)]
pub struct AnalyzerServer {
    listener: TcpListener,
    scripts: Vec<Vec<Vec<u8>>>,
    fragmentation: Fragmentation,
    timeout: Duration,
}

impl AnalyzerServer {
    /// Сервер на адресе; порт `0` — любой свободный.
    ///
    /// # Errors
    ///
    /// Адрес занят или недоступен.
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            scripts: Vec::new(),
            fragmentation: Fragmentation::Whole,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Фактический адрес сервера.
    ///
    /// # Errors
    ///
    /// Сокет уже закрыт.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Сценарий одного соединения: сообщения, на каждое ждём ACK.
    #[must_use]
    pub fn with_connection(mut self, messages: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.scripts.push(messages.into_iter().collect());
        self
    }

    /// Как резать сообщения на записи в сокет.
    #[must_use]
    pub fn with_fragmentation(mut self, fragmentation: Fragmentation) -> Self {
        self.fragmentation = fragmentation;
        self
    }

    /// Таймаут ожидания одного ACK.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Принимает соединения по сценариям и собирает ACK.
    ///
    /// # Errors
    ///
    /// Ошибка сокета, закрытие до ACK или истечение таймаута ожидания.
    pub async fn run(self) -> io::Result<ServerReport> {
        let mut report = ServerReport::default();
        for (index, messages) in self.scripts.iter().enumerate() {
            let (mut stream, _) = self.listener.accept().await?;
            report.accepted_at.push(Instant::now());
            report.connections += 1;
            let mut acks = Vec::with_capacity(messages.len());
            for (message_index, message) in messages.iter().enumerate() {
                for (start, end) in fragment_bounds(message.len(), self.fragmentation) {
                    stream.write_all(&message[start..end]).await?;
                }
                let ack = timeout(self.timeout, read_frame(&mut stream))
                    .await
                    .map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "ACK не пришёл за {:?}: соединение {}, сообщение {}",
                                self.timeout,
                                index + 1,
                                message_index + 1
                            ),
                        )
                    })??;
                acks.push(ack);
            }
            report.acks.push(acks);
            // Анализатор закрывает соединение: bytetap должен это увидеть и подключиться снова.
            stream.shutdown().await?;
            drop(stream);
        }
        Ok(report)
    }
}
