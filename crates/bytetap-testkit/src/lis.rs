//! Эмулятор ЛИС для интеграционных тестов прокси на loopback (T-2, M4-AC11).
//!
//! В топологии «анализатор — клиент» ЛИС ждёт подключения прокси, в топологии
//! «анализатор — сервер» сама подключается к порту bytetap — отсюда два конструктора,
//! [`LisEmulator::listen`] и [`LisEmulator::connect`]. На каждое соединение эмулятор
//! отвечает по своему сценарию ([`LisBehaviour`]):
//!
//! - [`LisBehaviour::Echo`] возвращает те же байты: так проверяется побайтовая пересылка
//!   в обе стороны без разбора протокола;
//! - [`LisBehaviour::Hl7Ack`] подтверждает каждое сообщение MLLP готовым ACK;
//! - [`LisBehaviour::AstmAck`] подтверждает `ENQ` и каждый кадр E1381 и завершает передачу
//!   по `EOT`;
//! - [`LisBehaviour::EchoAfterEof`] отвечает всем принятым только после `FIN` собеседника:
//!   так проверяется полузакрытие, где ЛИС отвечает после конца запроса (FR-7);
//! - [`LisBehaviour::HoldAfterEof`] после `FIN` молчит и держит соединение открытым: так
//!   проверяется срок `PROXY_LINGER`, по которому пару закрывает bytetap.
//!
//! Ответы режутся на записи так же, как у эмулятора анализатора ([`Fragmentation`]).
//! Данные — только синтетические (`ANALYZER`, `LIS`, `PATIENT^TEST`).

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::astm::{ACK as ASTM_ACK, ENQ, LF, STX};
use crate::emulator::{CR, DEFAULT_TIMEOUT, EB, Fragmentation, SB, fragment_bounds};

/// Готовый ACK ЛИС в рамке MLLP: на любое сообщение — `MSA|AA|MSG0001`.
#[must_use]
pub fn hl7_ack() -> Vec<u8> {
    let body = b"MSH|^~\\&|LIS|LAB|ANALYZER|LAB|20261022101530+0300||ACK^R01|\
                 BT261022101530000001|P|2.3.1\rMSA|AA|MSG0001\r";
    let mut ack = Vec::with_capacity(body.len() + 3);
    ack.push(SB);
    ack.extend_from_slice(body);
    ack.push(EB);
    ack.push(CR);
    ack
}

/// Что эмулятор ЛИС делает с принятыми байтами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LisBehaviour {
    /// Вернуть те же байты.
    Echo,
    /// Подтверждать каждый кадр MLLP.
    Hl7Ack,
    /// Подтверждать `ENQ` и каждый кадр ASTM E1381, конец — по `EOT`.
    AstmAck,
    /// Молчать до `FIN` собеседника, затем вернуть всё принятое и закрыть соединение.
    EchoAfterEof,
    /// Молчать до `FIN` собеседника и держать соединение открытым до таймаута.
    HoldAfterEof,
}

/// Кто подключается: ЛИС ждёт прокси или сама идёт на порт bytetap.
#[derive(Debug)]
enum LisRole {
    Server(TcpListener),
    Client(SocketAddr),
}

/// Что эмулятор увидел за прогон.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LisReport {
    /// Принятые байты по соединениям, в порядке соединений.
    pub received: Vec<Vec<u8>>,
    /// Сколько соединений обслужил эмулятор.
    pub connections: usize,
    /// Сколько ответов отправлено.
    pub answers: usize,
}

impl LisReport {
    /// Всё принятое одним потоком: у одного соединения — его байты.
    #[must_use]
    pub fn all_received(&self) -> Vec<u8> {
        self.received.concat()
    }
}

/// Эмулятор ЛИС.
#[derive(Debug)]
pub struct LisEmulator {
    role: LisRole,
    connections: usize,
    behaviour: LisBehaviour,
    fragmentation: Fragmentation,
    timeout: Duration,
}

impl LisEmulator {
    /// ЛИС ждёт подключения на адресе; порт `0` — любой свободный.
    ///
    /// # Errors
    ///
    /// Адрес занят или недоступен.
    pub async fn listen(addr: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            role: LisRole::Server(listener),
            connections: 1,
            behaviour: LisBehaviour::Echo,
            fragmentation: Fragmentation::Whole,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// ЛИС сама подключается к адресу bytetap.
    #[must_use]
    pub fn connect(addr: SocketAddr) -> Self {
        Self {
            role: LisRole::Client(addr),
            connections: 1,
            behaviour: LisBehaviour::Echo,
            fragmentation: Fragmentation::Whole,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Фактический адрес слушателя.
    ///
    /// # Errors
    ///
    /// Сокет уже закрыт или эмулятор — клиент.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match &self.role {
            LisRole::Server(listener) => listener.local_addr(),
            LisRole::Client(addr) => Ok(*addr),
        }
    }

    /// Сколько соединений обслужить: по одному сценарию на соединение.
    #[must_use]
    pub fn with_connections(mut self, connections: usize) -> Self {
        self.connections = connections.max(1);
        self
    }

    /// Что отвечать на принятые байты.
    #[must_use]
    pub fn with_behaviour(mut self, behaviour: LisBehaviour) -> Self {
        self.behaviour = behaviour;
        self
    }

    /// Как резать ответы на записи в сокет.
    #[must_use]
    pub fn with_fragmentation(mut self, fragmentation: Fragmentation) -> Self {
        self.fragmentation = fragmentation;
        self
    }

    /// Таймаут ожидания данных на соединении.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Обслуживает соединения по сценарию и возвращает принятое.
    ///
    /// # Errors
    ///
    /// Ошибка сокета или молчание дольше [`LisEmulator::with_timeout`].
    pub async fn run(self) -> io::Result<LisReport> {
        let mut report = LisReport::default();
        for _ in 0..self.connections {
            let mut stream =
                match &self.role {
                    LisRole::Server(listener) => {
                        let (stream, _) =
                            timeout(self.timeout, listener.accept())
                                .await
                                .map_err(|_| {
                                    io::Error::new(io::ErrorKind::TimedOut, "никто не пришёл")
                                })??;
                        stream
                    }
                    LisRole::Client(addr) => timeout(self.timeout, TcpStream::connect(addr))
                        .await
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "нет соединения"))??,
                };
            report.connections += 1;
            let (received, answers) = self.serve(&mut stream).await?;
            report.received.push(received);
            report.answers += answers;
        }
        Ok(report)
    }

    /// Один поток: читает, отвечает по сценарию, возвращает принятое и число ответов.
    async fn serve(&self, stream: &mut TcpStream) -> io::Result<(Vec<u8>, usize)> {
        let mut received = Vec::new();
        // Незаконченное сообщение MLLP: ACK отправляется на целую рамку, а не на фрагмент.
        let mut pending = Vec::new();
        // Кадр ASTM собирается до `LF`: только тогда он принят целиком.
        let mut in_frame = false;
        let mut answers = 0;
        let mut buffer = [0u8; 4096];
        let mut eof = false;
        loop {
            let read = timeout(self.timeout, stream.read(&mut buffer))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ЛИС не дождалась данных"))?;
            let read = match read {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(read) => read,
                // Сброс соединения — тоже конец обмена.
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => break,
                Err(error) => return Err(error),
            };
            let chunk = &buffer[..read];
            received.extend_from_slice(chunk);
            match self.behaviour {
                LisBehaviour::Echo => {
                    self.send(stream, chunk).await?;
                    answers += 1;
                }
                LisBehaviour::Hl7Ack => {
                    pending.extend_from_slice(chunk);
                    while let Some(at) = mllp_end(&pending) {
                        pending.drain(..at);
                        self.send(stream, &hl7_ack()).await?;
                        answers += 1;
                    }
                }
                LisBehaviour::AstmAck => {
                    for &byte in chunk {
                        match byte {
                            ENQ => {
                                self.send(stream, &[ASTM_ACK]).await?;
                                answers += 1;
                            }
                            STX => in_frame = true,
                            LF if in_frame => {
                                in_frame = false;
                                self.send(stream, &[ASTM_ACK]).await?;
                                answers += 1;
                            }
                            _ => {}
                        }
                    }
                }
                // Ответ после `FIN` собеседника: до конца запроса эмулятор молчит.
                LisBehaviour::EchoAfterEof | LisBehaviour::HoldAfterEof => {}
            }
        }
        if eof {
            match self.behaviour {
                LisBehaviour::EchoAfterEof => {
                    self.send(stream, &received).await?;
                    answers += 1;
                }
                // Соединение не закрывается: bytetap закрывает пару по своему сроку.
                LisBehaviour::HoldAfterEof => tokio::time::sleep(self.timeout).await,
                LisBehaviour::Echo | LisBehaviour::Hl7Ack | LisBehaviour::AstmAck => {}
            }
        }
        Ok((received, answers))
    }

    /// Отправляет ответ по нарезке соединения.
    async fn send(&self, stream: &mut TcpStream, payload: &[u8]) -> io::Result<()> {
        for (start, end) in fragment_bounds(payload.len(), self.fragmentation) {
            stream.write_all(&payload[start..end]).await?;
        }
        Ok(())
    }
}

/// Позиция сразу за первым сообщением MLLP в накопленном потоке.
fn mllp_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|pair| pair == [EB, CR])
        .map(|at| at + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &[u8] =
        b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261022101500||ORU^R01|MSG0001|P|2.3.1\r\
          PID|1||PATIENT^TEST\r";

    #[tokio::test]
    async fn echo_returns_the_same_bytes() {
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let addr = lis.local_addr().unwrap();
        let server = tokio::spawn(lis.run());

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut answer = [0u8; 4];
        stream.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"ping");
        drop(stream);

        let report = server.await.unwrap().unwrap();
        assert_eq!(report.connections, 1);
        assert_eq!(report.all_received(), b"ping");
    }

    #[tokio::test]
    async fn an_hl7_message_is_acknowledged_once() {
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap()
            .with_behaviour(LisBehaviour::Hl7Ack);
        let addr = lis.local_addr().unwrap();
        let server = tokio::spawn(lis.run());

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let mut framed = vec![SB];
        framed.extend_from_slice(MESSAGE);
        framed.extend_from_slice(&[EB, CR]);
        stream.write_all(&framed).await.unwrap();
        let ack = crate::emulator::read_frame(&mut stream).await.unwrap();
        assert_eq!(
            crate::emulator::ack_msa(&ack).map(|(code, _)| code),
            Some("AA".to_owned())
        );
        drop(stream);

        let report = server.await.unwrap().unwrap();
        assert_eq!(report.answers, 1);
        assert_eq!(report.all_received(), framed);
    }

    #[tokio::test]
    async fn echo_after_eof_answers_only_at_the_end() {
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap()
            .with_behaviour(LisBehaviour::EchoAfterEof);
        let addr = lis.local_addr().unwrap();
        let server = tokio::spawn(lis.run());

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"request").await.unwrap();
        let mut early = [0u8; 1];
        assert!(
            timeout(Duration::from_millis(200), stream.read(&mut early))
                .await
                .is_err(),
            "до `FIN` эмулятор молчит"
        );
        stream.shutdown().await.unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.unwrap();
        assert_eq!(answer, b"request", "ответ пришёл после `FIN`");
        drop(stream);

        let report = server.await.unwrap().unwrap();
        assert_eq!(report.answers, 1);
        assert_eq!(report.all_received(), b"request");
    }

    #[tokio::test]
    async fn hold_after_eof_keeps_the_connection_open() {
        let hold = Duration::from_millis(300);
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap()
            .with_behaviour(LisBehaviour::HoldAfterEof)
            .with_timeout(hold);
        let addr = lis.local_addr().unwrap();
        let server = tokio::spawn(lis.run());

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"request").await.unwrap();
        stream.shutdown().await.unwrap();
        let started = std::time::Instant::now();
        let mut buffer = [0u8; 8];
        let read = stream.read(&mut buffer).await.unwrap();
        assert_eq!(read, 0, "конец соединения");
        assert!(
            started.elapsed() >= hold / 2,
            "соединение держалось открытым {:?}",
            started.elapsed()
        );
        drop(stream);
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_astm_exchange_is_acknowledged() {
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap()
            .with_behaviour(LisBehaviour::AstmAck);
        let addr = lis.local_addr().unwrap();
        let server = tokio::spawn(lis.run());

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let frame = crate::astm::frame(b'1', b"H|\\^&|||ANALYZER\r", crate::astm::ETX, false);
        let mut exchange = vec![ENQ];
        exchange.extend_from_slice(&frame);
        stream.write_all(&exchange).await.unwrap();
        let mut answers = [0u8; 2];
        stream.read_exact(&mut answers).await.unwrap();
        assert_eq!(answers, [ASTM_ACK, ASTM_ACK], "ACK на ENQ и на кадр");
        stream.write_all(&[crate::astm::EOT]).await.unwrap();
        drop(stream);

        let report = server.await.unwrap().unwrap();
        assert_eq!(report.answers, 2);
    }
}
