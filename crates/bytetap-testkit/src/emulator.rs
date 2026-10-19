//! Эмулятор анализатора HL7/MLLP для интеграционных тестов на loopback (T-2).
//!
//! Эмулятор подключается к bytetap как анализатор, отправляет набор сообщений MLLP
//! и ждёт на каждое ответный ACK. Поток режется на записи по-разному
//! ([`Fragmentation`]): целиком, побайтно и случайными кусками по seed — так проверяется,
//! что сессия собирает сообщение из любого разбиения. [`Reconnect`] проверяет
//! переподключения анализатора посреди сессии (FR-1).
//!
//! Сообщения — только синтетические (`ANALYZER`, `LIS`, `PATIENT^TEST`) из
//! `tests/fixtures/hl7` (M1-AC16). Эмулятор не разбирает HL7: он возвращает полученные
//! ACK целиком, а [`ack_msa`] достаёт из них `MSA-1` и `MSA-2`.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Начало рамки MLLP — `VT`.
pub const SB: u8 = 0x0B;

/// Конец рамки MLLP — `FS`; за ним идёт `CR`.
pub const EB: u8 = 0x1C;

/// Завершение рамки MLLP — `CR`.
pub const CR: u8 = 0x0D;

/// Сколько ждёт эмулятор ACK, если таймаут не задан.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Как эмулятор режет поток на записи в сокет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fragmentation {
    /// Одно сообщение — один `write_all`.
    Whole,
    /// По одному байту на запись.
    ByteByByte,
    /// Случайные куски не длиннее `max` байт; при одном `seed` нарезка повторяется.
    Random {
        /// Зерно генератора: одинаковое зерно — одинаковая нарезка.
        seed: u64,
        /// Наибольший размер куска в байтах.
        max: usize,
    },
}

/// Переподключаться ли между сообщениями.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconnect {
    /// Все сообщения в одном соединении.
    Never,
    /// После каждого сообщения соединение закрывается и открывается заново.
    BetweenMessages,
}

/// Что эмулятор получил за сессию.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmulatorReport {
    /// Полученные ACK целиком, в рамке MLLP, в порядке сообщений.
    pub acks: Vec<Vec<u8>>,
    /// Сколько TCP-соединений открыл эмулятор.
    pub connections: usize,
}

/// Эмулятор анализатора HL7/MLLP.
#[derive(Debug, Clone)]
pub struct AnalyzerEmulator {
    addr: SocketAddr,
    messages: Vec<Vec<u8>>,
    fragmentation: Fragmentation,
    reconnect: Reconnect,
    timeout: Duration,
}

impl AnalyzerEmulator {
    /// Эмулятор для адреса bytetap.
    #[must_use]
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            messages: Vec::new(),
            fragmentation: Fragmentation::Whole,
            reconnect: Reconnect::Never,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Набор сообщений MLLP, на каждое эмулятор ждёт ACK.
    #[must_use]
    pub fn with_messages(mut self, messages: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.messages = messages.into_iter().collect();
        self
    }

    /// Добавляет одно сообщение MLLP.
    #[must_use]
    pub fn push_message(mut self, message: impl Into<Vec<u8>>) -> Self {
        self.messages.push(message.into());
        self
    }

    /// Как резать поток на записи.
    #[must_use]
    pub fn with_fragmentation(mut self, fragmentation: Fragmentation) -> Self {
        self.fragmentation = fragmentation;
        self
    }

    /// Переподключаться между сообщениями.
    #[must_use]
    pub fn reconnect_between_messages(mut self) -> Self {
        self.reconnect = Reconnect::BetweenMessages;
        self
    }

    /// Таймаут ожидания одного ACK.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Отправляет сообщения и собирает ACK.
    ///
    /// # Errors
    ///
    /// Ошибка соединения, закрытие сокета до ACK или истечение таймаута ожидания.
    pub async fn run(self) -> io::Result<EmulatorReport> {
        let mut report = EmulatorReport::default();
        let mut stream: Option<TcpStream> = None;
        for (index, message) in self.messages.iter().enumerate() {
            let reconnect =
                self.reconnect == Reconnect::BetweenMessages && index > 0 && stream.is_some();
            if stream.is_none() || reconnect {
                if let Some(mut old) = stream.take() {
                    let _ = old.shutdown().await;
                }
                stream = Some(TcpStream::connect(self.addr).await?);
                report.connections += 1;
            }
            let stream = stream.as_mut().expect("соединение только что открыто");
            for (start, end) in fragment_bounds(message.len(), self.fragmentation) {
                stream.write_all(&message[start..end]).await?;
            }
            let ack = timeout(self.timeout, read_frame(stream))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "ACK на сообщение {} не пришёл за {:?}",
                            index + 1,
                            self.timeout
                        ),
                    )
                })??;
            report.acks.push(ack);
        }
        if let Some(mut stream) = stream {
            let _ = stream.shutdown().await;
        }
        Ok(report)
    }
}

/// Границы фрагментов, на которые эмулятор режет сообщение длиной `len`.
///
/// Куски идут подряд и покрывают всё сообщение: нарезка не меняет отправленные байты,
/// а только их разбиение по записям.
#[must_use]
pub fn fragment_bounds(len: usize, fragmentation: Fragmentation) -> Vec<(usize, usize)> {
    if len == 0 {
        return Vec::new();
    }
    match fragmentation {
        Fragmentation::Whole => vec![(0, len)],
        Fragmentation::ByteByByte => (0..len).map(|at| (at, at + 1)).collect(),
        Fragmentation::Random { seed, max } => {
            let max = max.max(1);
            let mut rng = Rng::new(seed);
            let mut bounds = Vec::new();
            let mut offset = 0;
            while offset < len {
                let take = (rng.next() as usize % max) + 1;
                let end = (offset + take).min(len);
                bounds.push((offset, end));
                offset = end;
            }
            bounds
        }
    }
}

/// Оборачивает сообщение HL7 в рамку MLLP.
#[must_use]
pub fn frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 3);
    out.push(SB);
    out.extend_from_slice(message);
    out.push(EB);
    out.push(CR);
    out
}

/// `MSA-1` и `MSA-2` из ACK в рамке MLLP (рамку можно не снимать).
///
/// `None` — это не ACK или в нём нет сегмента `MSA`.
#[must_use]
pub fn ack_msa(ack: &[u8]) -> Option<(String, String)> {
    let body = ack.strip_prefix(&[SB]).unwrap_or(ack);
    let body = body.strip_suffix(&[EB, CR]).unwrap_or(body);
    // Разделитель полей ACK — его собственный `MSH-1`.
    let field_separator = *body.get(3)?;
    let segment = body
        .split(|&byte| byte == CR)
        .find(|segment| segment.starts_with(b"MSA"))?;
    let fields: Vec<&[u8]> = segment.split(|&byte| byte == field_separator).collect();
    let [_, code, id, ..] = fields.as_slice() else {
        return None;
    };
    Some((
        String::from_utf8(code.to_vec()).ok()?,
        String::from_utf8(id.to_vec()).ok()?,
    ))
}

/// Читает один ответ в рамке MLLP.
async fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if buffer.ends_with(&[EB, CR]) {
            return Ok(buffer);
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "соединение закрыто до ACK",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Воспроизводимый генератор: xorshift64, без зависимостей.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Ноль — неподвижная точка xorshift, поэтому зерно 0 заменяется константой.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    const ACK: &[u8] = b"MSH|^~\\&|LIS|LAB|ANALYZER|LAB|20261019101530+0300||ACK^R01|BT261019101530000001|P|2.3.1\r\
                         MSA|AA|MSG0001\r";
    const MESSAGE: &[u8] =
        b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0001|P|2.3.1\r\
                            PID|1||PATIENT^TEST\r";

    #[test]
    fn every_fragmentation_covers_the_message_without_gaps() {
        let stream = frame(MESSAGE);
        let modes = [
            Fragmentation::Whole,
            Fragmentation::ByteByByte,
            Fragmentation::Random { seed: 7, max: 5 },
            Fragmentation::Random {
                seed: 123_456,
                max: 4096,
            },
        ];
        for mode in modes {
            let bounds = fragment_bounds(stream.len(), mode);
            assert_eq!(bounds.first().map(|&(start, _)| start), Some(0), "{mode:?}");
            assert_eq!(
                bounds.last().map(|&(_, end)| end),
                Some(stream.len()),
                "{mode:?}"
            );
            let mut offset = 0;
            for &(start, end) in &bounds {
                assert_eq!(start, offset, "фрагменты подряд: {mode:?}");
                assert!(end > start, "пустых фрагментов нет: {mode:?}");
                offset = end;
            }
            if let Fragmentation::Random { max, .. } = mode {
                assert!(offset == stream.len(), "сообщение покрыто целиком");
                assert_eq!(fragment_bounds(stream.len(), mode).len(), bounds.len());
                let biggest = bounds.iter().map(|(start, end)| end - start).max();
                assert!(biggest.unwrap_or_default() <= max.max(1));
            }
        }
        assert!(fragment_bounds(0, Fragmentation::Whole).is_empty());
    }

    #[test]
    fn the_same_seed_gives_the_same_cuts() {
        let mode = Fragmentation::Random { seed: 42, max: 9 };
        assert_eq!(fragment_bounds(1000, mode), fragment_bounds(1000, mode));
        assert_ne!(
            fragment_bounds(1000, mode),
            fragment_bounds(1000, Fragmentation::Random { seed: 43, max: 9 })
        );
    }

    #[test]
    fn msa_is_read_from_a_framed_ack() {
        assert_eq!(
            ack_msa(&frame(ACK)),
            Some(("AA".to_owned(), "MSG0001".to_owned()))
        );
        assert_eq!(ack_msa(ACK).map(|(code, _)| code), Some("AA".to_owned()));
        assert_eq!(ack_msa(b"not an ack"), None);
    }

    #[tokio::test]
    async fn a_byte_by_byte_message_is_delivered_and_the_ack_is_read() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = frame(MESSAGE);
        let expected = sent.clone();
        let for_server = expected.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 64];
            while received.len() < for_server.len() {
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                received.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(&frame(ACK)).await.unwrap();
            received
        });

        let report = AnalyzerEmulator::new(addr)
            .with_messages([sent])
            .with_fragmentation(Fragmentation::ByteByByte)
            .run()
            .await
            .expect("обмен");
        assert_eq!(report.connections, 1);
        assert_eq!(report.acks.len(), 1);
        assert_eq!(
            ack_msa(&report.acks[0]),
            Some(("AA".to_owned(), "MSG0001".to_owned()))
        );
        assert_eq!(server.await.unwrap(), expected, "байты дошли целиком");
    }

    #[tokio::test]
    async fn a_missing_ack_is_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
        });
        let error = AnalyzerEmulator::new(addr)
            .with_messages([frame(MESSAGE)])
            .with_timeout(Duration::from_millis(50))
            .run()
            .await
            .expect_err("ACK не приходит");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        server.await.unwrap();
    }
}
