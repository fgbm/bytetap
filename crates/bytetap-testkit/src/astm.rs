//! Эмулятор анализатора ASTM E1381 для интеграционных тестов на loopback (T-2, M3-AC10).
//!
//! Эмулятор ведёт себя как анализатор: `ENQ`, затем кадры записей с контрольной суммой,
//! затем `EOT`. На `ENQ` и на каждый кадр он ждёт ответ — `ACK` по умолчанию, `NAK` в
//! режиме «неверная сумма» при настройке `NAK` у автоответчика; не тот ответ или молчание
//! дольше таймаута — ошибка, на которой падает тест.
//!
//! Поток режется на записи так же, как у HL7-эмулятора ([`Fragmentation`]): целиком,
//! побайтно и случайными кусками по seed. Длинная запись режется на кадры-продолжения с
//! `ETB`, как это делает прибор: `FN` растёт 0…7, последний кадр записи заканчивается `ETX`.
//!
//! Записи — только синтетические (`ANALYZER`, `LIS`, `PATIENT^TEST`) из `tests/fixtures/astm`.
//! Эмулятор не разбирает записи: ему всё равно, `H`, `P`, `Q` или `R` в кадре — как и
//! автоответчику, который подтверждает кадр целиком (FR-18).

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::emulator::{Fragmentation, Reconnect, fragment_bounds};

/// `ENQ` — анализатор просит разрешения передавать.
pub const ENQ: u8 = 0x05;

/// `ACK` — подтверждение приёма.
pub const ACK: u8 = 0x06;

/// `NAK` — отказ: кадр не принят.
pub const NAK: u8 = 0x15;

/// `STX` — начало кадра.
pub const STX: u8 = 0x02;

/// `ETX` — кадр заканчивает запись.
pub const ETX: u8 = 0x03;

/// `ETB` — кадр заканчивает блок записи, продолжение — следующим кадром.
pub const ETB: u8 = 0x17;

/// `EOT` — передача закончена.
pub const EOT: u8 = 0x04;

/// `CR` — конец сегмента и второй с конца байт кадра.
pub const CR: u8 = 0x0D;

/// `LF` — последний байт кадра.
pub const LF: u8 = 0x0A;

/// Сколько байт записи эмулятор кладёт в один кадр по умолчанию.
///
/// E1381 разрешает 247 байт от `FN` до `ETX`/`ETB` включительно; 240 байт текста плюс номер
/// и терминатор в предел укладываются.
pub const DEFAULT_CHUNK: usize = 240;

/// Сколько ждёт эмулятор ответа, если таймаут не задан.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Какой ответ эмулятор ждёт: настройка автоответчика решает, кто прав.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expected {
    /// `ACK` на `ENQ` и на каждый кадр — обычный обмен.
    Ack,
    /// `ACK` на `ENQ`, `NAK` на кадр — автоответчик в режиме «NAK на неверную сумму».
    Nak,
}

/// Что эмулятор получил за сессию.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AstmReport {
    /// Ответы автоответчика в порядке прихода: по одному байту на `ENQ` и на каждый кадр.
    pub responses: Vec<u8>,
    /// Сколько кадров отправил эмулятор, включая продолжения с `ETB`.
    pub frames: usize,
    /// Сколько TCP-соединений открыл эмулятор.
    pub connections: usize,
}

impl AstmReport {
    /// Сколько пришло `ACK`.
    #[must_use]
    pub fn acks(&self) -> usize {
        self.responses.iter().filter(|&&byte| byte == ACK).count()
    }

    /// Сколько пришло `NAK`.
    #[must_use]
    pub fn naks(&self) -> usize {
        self.responses.iter().filter(|&&byte| byte == NAK).count()
    }
}

/// Эмулятор анализатора ASTM E1381.
#[derive(Debug, Clone)]
pub struct AstmEmulator {
    addr: SocketAddr,
    records: Vec<Vec<u8>>,
    fragmentation: Fragmentation,
    reconnect: Reconnect,
    timeout: Duration,
    bad_checksum: bool,
    expected: Expected,
    chunk: usize,
}

impl AstmEmulator {
    /// Эмулятор для адреса bytetap.
    #[must_use]
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            records: Vec::new(),
            fragmentation: Fragmentation::Whole,
            reconnect: Reconnect::Never,
            timeout: DEFAULT_TIMEOUT,
            bad_checksum: false,
            expected: Expected::Ack,
            chunk: DEFAULT_CHUNK,
        }
    }

    /// Записи, которые эмулятор передаст: каждая — текст кадра без рамки и суммы.
    #[must_use]
    pub fn with_records(mut self, records: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.records = records.into_iter().collect();
        self
    }

    /// Добавляет одну запись.
    #[must_use]
    pub fn push_record(mut self, record: impl Into<Vec<u8>>) -> Self {
        self.records.push(record.into());
        self
    }

    /// Как резать поток на записи в сокет.
    #[must_use]
    pub fn with_fragmentation(mut self, fragmentation: Fragmentation) -> Self {
        self.fragmentation = fragmentation;
        self
    }

    /// Переподключаться между записями: каждая запись — своё соединение и свой `ENQ`.
    #[must_use]
    pub fn reconnect_between_messages(mut self) -> Self {
        self.reconnect = Reconnect::BetweenMessages;
        self
    }

    /// Таймаут ожидания одного ответа.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Портить контрольную сумму каждого кадра: на единицу больше верной.
    #[must_use]
    pub fn with_bad_checksum(mut self) -> Self {
        self.bad_checksum = true;
        self
    }

    /// Ждать `NAK` на кадр вместо `ACK` (автоответчик в режиме «NAK на неверную сумму»).
    #[must_use]
    pub fn expect_nak(mut self) -> Self {
        self.expected = Expected::Nak;
        self
    }

    /// Сколько байт записи класть в один кадр: меньше [`DEFAULT_CHUNK`] — больше `ETB`-кадров.
    #[must_use]
    pub fn with_chunk(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }

    /// Отправляет записи и собирает ответы.
    ///
    /// # Errors
    ///
    /// Ошибка соединения, закрытие сокета до ответа, истечение таймаута или ответ,
    /// которого эмулятор не ждал: тест с неверным ответом должен падать, а не проходить.
    pub async fn run(self) -> io::Result<AstmReport> {
        let mut report = AstmReport::default();
        let mut stream: Option<TcpStream> = None;
        // Номер кадра идёт по кругу всю передачу: `EOT` (новое соединение) начинает с 0.
        let mut number = 0u8;
        for (index, record) in self.records.iter().enumerate() {
            let reconnect =
                self.reconnect == Reconnect::BetweenMessages && index > 0 && stream.is_some();
            if stream.is_none() || reconnect {
                if let Some(mut old) = stream.take() {
                    // `EOT` закрывает передачу; ответа на него нет (FR-17).
                    let _ = old.write_all(&[EOT]).await;
                    let _ = old.shutdown().await;
                }
                stream = Some(TcpStream::connect(self.addr).await?);
                report.connections += 1;
                number = 0;
                let stream = stream.as_mut().expect("соединение только что открыто");
                stream.write_all(&[ENQ]).await?;
                report
                    .responses
                    .push(self.read_answer(stream, "ACK на ENQ").await?);
            }
            let stream = stream.as_mut().expect("соединение только что открыто");
            let frames = record_frames(record, self.chunk, number, self.bad_checksum);
            number = number.wrapping_add(frames.len() as u8) & 0x07;
            for frame in frames {
                for (start, end) in fragment_bounds(frame.len(), self.fragmentation) {
                    stream.write_all(&frame[start..end]).await?;
                }
                let answer = self
                    .read_answer(stream, &format!("ответ на кадр записи {}", index + 1))
                    .await?;
                let wanted = match self.expected {
                    Expected::Ack => ACK,
                    Expected::Nak => NAK,
                };
                if answer != wanted {
                    return Err(io::Error::other(format!(
                        "неверный ответ на кадр записи {}: получен 0x{answer:02X}, ожидался 0x{wanted:02X}",
                        index + 1
                    )));
                }
                report.responses.push(answer);
                report.frames += 1;
            }
        }
        if let Some(mut stream) = stream {
            stream.write_all(&[EOT]).await?;
            let _ = stream.shutdown().await;
        }
        Ok(report)
    }

    /// Ждёт один байт ответа не дольше таймаута.
    async fn read_answer(&self, stream: &mut TcpStream, what: &str) -> io::Result<u8> {
        let mut byte = [0u8; 1];
        timeout(self.timeout, stream.read_exact(&mut byte))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{what}: ответ не пришёл за {:?}", self.timeout),
                )
            })?
            .map_err(|err| {
                if err.kind() == io::ErrorKind::UnexpectedEof {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("{what}: соединение закрыто до ответа"),
                    )
                } else {
                    err
                }
            })?;
        Ok(byte[0])
    }
}

/// Кадры одной записи так, как их отправит эмулятор.
///
/// Запись режется на куски по `chunk` байт: все кадры, кроме последнего, заканчиваются
/// `ETB`, последний — `ETX`. Номер кадра растёт от `start_number` по кругу 0…7.
#[must_use]
pub fn record_frames(
    record: &[u8],
    chunk: usize,
    start_number: u8,
    bad_checksum: bool,
) -> Vec<Vec<u8>> {
    let chunk = chunk.max(1);
    let mut frames = Vec::new();
    let mut number = start_number & 0x07;
    let mut offset = 0;
    loop {
        let end = (offset + chunk).min(record.len());
        let last = end >= record.len();
        let terminator = if last { ETX } else { ETB };
        frames.push(frame(
            number,
            &record[offset..end],
            terminator,
            bad_checksum,
        ));
        number = number.wrapping_add(1) & 0x07;
        offset = end;
        if last {
            return frames;
        }
    }
}

/// Кадр ASTM: `STX`, номер, текст, терминатор, сумма, `CR LF`.
///
/// `bad_checksum` добавляет к сумме единицу — так проверяется ошибка суммы на приёме.
#[must_use]
pub fn frame(number: u8, text: &[u8], terminator: u8, bad_checksum: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(text.len() + 2);
    body.push(number);
    body.extend_from_slice(text);
    body.push(terminator);
    let mut checksum = body.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    if bad_checksum {
        checksum = checksum.wrapping_add(1);
    }
    let mut frame = Vec::with_capacity(body.len() + 6);
    frame.push(STX);
    frame.extend_from_slice(&body);
    frame.extend_from_slice(format!("{checksum:02X}").as_bytes());
    frame.push(CR);
    frame.push(LF);
    frame
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    const RECORD: &[u8] = b"H|\\^&|||ANALYZER\rP|1|PATIENT^TEST\rL|1|N\r";

    /// Сервер-автоответчик для проверк эмулятора без ядра: `ACK` на `ENQ`, `NAK` на кадр
    /// с неверной суммой, конец — по `EOT`.
    async fn serve_astm(listener: TcpListener, refuse_bad_checksum: bool) -> Vec<u8> {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        let mut buffer = [0u8; 32];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            for &byte in &buffer[..read] {
                received.push(byte);
                match byte {
                    ENQ => stream.write_all(&[ACK]).await.unwrap(),
                    EOT => return received,
                    _ => {}
                }
            }
            // Кадр принят целиком, когда пришёл `LF`.
            if buffer[read - 1] == LF {
                let answer = match checksum_is_valid(&received) {
                    Some(true) => ACK,
                    Some(false) if refuse_bad_checksum => NAK,
                    Some(false) => ACK,
                    None => NAK,
                };
                stream.write_all(&[answer]).await.unwrap();
            }
        }
        received
    }

    /// Проверка суммы последнего кадра в принятом потоке: `None` — кадр не разобран.
    fn checksum_is_valid(received: &[u8]) -> Option<bool> {
        let after = received.iter().rposition(|&byte| byte == LF)?;
        let start = received[..after].iter().rposition(|&byte| byte == STX)?;
        // Кадр: `STX`, тело, два знака суммы, `CR`, `LF`.
        let body = &received[start + 1..after - 3];
        let digits = std::str::from_utf8(&received[after - 3..after - 1]).ok()?;
        let got = u8::from_str_radix(digits, 16).ok()?;
        let expected = body.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
        Some(got == expected)
    }

    #[test]
    fn a_long_record_is_split_into_etb_frames() {
        let frames = record_frames(RECORD, 8, 0, false);
        assert!(frames.len() > 1, "запись не влезла в один кадр");
        let mut text = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let last = index == frames.len() - 1;
            assert_eq!(frame[0], STX, "кадр начинается с STX");
            assert_eq!(frame[1], index as u8, "номер кадра растёт с 0");
            assert_eq!(&frame[frame.len() - 2..], &[CR, LF], "кадр кончается CR LF");
            let terminator = frame[frame.len() - 5];
            assert_eq!(
                terminator,
                if last { ETX } else { ETB },
                "продолжение — ETB"
            );
            let body = &frame[1..frame.len() - 4];
            let got = u8::from_str_radix(
                std::str::from_utf8(&frame[frame.len() - 4..frame.len() - 2]).unwrap(),
                16,
            )
            .unwrap();
            let expected = body.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
            assert_eq!(got, expected, "сумма кадра верна");
            assert!(body.len() - 2 <= 8, "кусок записи не больше chunk");
            text.extend_from_slice(&body[1..body.len() - 1]);
        }
        assert_eq!(text, RECORD, "продолжения собирают запись целиком");
    }

    #[test]
    fn a_bad_checksum_frame_differs_by_one() {
        let good = frame(0, b"P|1\r", ETX, false);
        let bad = frame(0, b"P|1\r", ETX, true);
        assert_eq!(good.len(), bad.len());
        assert_ne!(good, bad);
        let mut stream = vec![ENQ];
        stream.extend_from_slice(&good);
        assert_eq!(checksum_is_valid(&stream), Some(true));
        let mut stream = vec![ENQ];
        stream.extend_from_slice(&bad);
        assert_eq!(checksum_is_valid(&stream), Some(false));
    }

    #[tokio::test]
    async fn records_are_delivered_frame_by_frame_and_acknowledged() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_astm(listener, false));

        let report = AstmEmulator::new(addr)
            .push_record(RECORD)
            .with_fragmentation(Fragmentation::ByteByByte)
            .run()
            .await
            .expect("обмен");
        assert_eq!(report.connections, 1);
        assert_eq!(
            report.frames,
            record_frames(RECORD, DEFAULT_CHUNK, 0, false).len()
        );
        assert_eq!(report.acks(), report.frames + 1, "ACK на ENQ и на кадр");
        assert_eq!(report.naks(), 0);

        let received = server.await.unwrap();
        assert_eq!(received.first(), Some(&ENQ), "передача начинается с ENQ");
        assert_eq!(received.last(), Some(&EOT), "и заканчивается EOT");
    }

    #[tokio::test]
    async fn a_bad_checksum_is_nakked_when_the_answerer_says_so() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_astm(listener, true));

        let report = AstmEmulator::new(addr)
            .push_record(RECORD)
            .with_bad_checksum()
            .expect_nak()
            .run()
            .await
            .expect("NAK получен");
        assert_eq!(report.naks(), report.frames);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unexpected_answer_fails_the_run() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_astm(listener, false));

        // Автоответчик подтвердил кадр, а эмулятор ждал NAK: тест должен упасть.
        let error = AstmEmulator::new(addr)
            .push_record(RECORD)
            .with_bad_checksum()
            .expect_nak()
            .run()
            .await
            .expect_err("ответ не тот");
        assert!(error.to_string().contains("неверный ответ"), "{error}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_missing_answer_is_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
        });
        let error = AstmEmulator::new(addr)
            .push_record(RECORD)
            .with_timeout(Duration::from_millis(50))
            .run()
            .await
            .expect_err("ответа нет");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnecting_opens_a_new_connection_and_starts_a_new_enq() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut connections = 0;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                connections += 1;
                serve_astm_stream(&mut stream).await;
            }
            connections
        });
        let report = AstmEmulator::new(addr)
            .push_record(b"P|1\r".to_vec())
            .push_record(b"P|2\r".to_vec())
            .reconnect_between_messages()
            .run()
            .await
            .expect("два соединения");
        assert_eq!(report.connections, 2);
        assert_eq!(server.await.unwrap(), 2);
    }

    /// Та же логика `ACK`, что выше, для уже принятого соединения.
    async fn serve_astm_stream(stream: &mut TcpStream) {
        let mut received = Vec::new();
        let mut buffer = [0u8; 32];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            for &byte in &buffer[..read] {
                received.push(byte);
                match byte {
                    ENQ => stream.write_all(&[ACK]).await.unwrap(),
                    EOT => return,
                    _ => {}
                }
            }
            if buffer[read - 1] == LF {
                stream.write_all(&[ACK]).await.unwrap();
            }
        }
    }
}
