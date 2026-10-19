//! ASTM E1381: низкий уровень обмена — `ENQ`, кадры и `EOT` (FR-17, FR-18).
//!
//! Автомат понимает только транспортный уровень: он не разбирает записи H/P/Q и не
//! формирует ответных данных. Всё, что приходит в кадре, подтверждается `ACK` или
//! отклоняется `NAK` — этого достаточно, чтобы анализатор не повторял передачу (FR-18).
//!
//! Обмен по E1381:
//!
//! - `ENQ` (`0x05`) — анализатор просит разрешения передавать; ответ [`ACK`] (`0x06`).
//! - Кадр — `STX` (`0x02`), номер кадра `FN`, текст записи, `ETX` (`0x03`) или `ETB` (`0x17`),
//!   два знака контрольной суммы, `CR` `LF`. Ответ — [`ACK`], при неверной сумме [`NAK`]
//!   (`0x15`), если так настроено.
//! - `EOT` (`0x04`) — передача закончена, ответа нет; следующий кадр снова первый (`FN` = 0).
//!
//! Контрольная сумма — сумма байтов от `FN` до `ETX`/`ETB` включительно по модулю 256, две
//! hex-цифры. На приём допускается нижний регистр, в сообщении о неверной сумме — верхний.
//!
//! Правила, которые не ломают обмен, а только попадают в журнал:
//!
//! - Номер кадра — 0…7 циклически. Нарушение последовательности — [`ResponderEvent::Info`],
//!   кадр всё равно подтверждается: данные важнее нумерации.
//! - Кадр длиннее 247 байт от `FN` до `ETX`/`ETB` включительно — [`ResponderEvent::Info`]:
//!   E1381 такого не разрешает, но приборы шлют. Принимается до [`MAX_FRAME_BYTES`];
//!   дальше кадр сбрасывается с [`ResponderEvent::Error`], иначе буфер рос бы без предела
//!   (NFR-5).
//! - Мусор вне кадра не ломает автомат: о блоке мусора пишется одно `Info`, когда блок
//!   заканчивается. `CR` и `LF` вне кадра мусором не считаются — это конец кадра.
//! - Недособранный кадр при закрытии соединения — [`ResponderEvent::Error`]: иначе он
//!   пропал бы из журнала молча.
//!
//! Таймаут (E1381: 15 с ожидания кадра) нужен и «вместо ЛИС»: анализатор, замолчавший
//! посреди кадра, иначе оставил бы автомат с недособранным кадром до закрытия соединения,
//! а оно может не закрываться часами. Сессия берёт срок из [`Responder::poll_timeout`] и по
//! истечении будит автомат пустым `feed(&[], now)`; если сессия этого не делает, кадр
//! сбрасывается при следующем фрагменте.

use std::time::Duration;

use crate::responder::{Action, Now, Responder, ResponderEvent};

/// `ENQ` — анализатор просит разрешения передавать.
pub const ENQ: u8 = 0x05;

/// `ACK` — кадр (или `ENQ`) принят.
pub const ACK: u8 = 0x06;

/// `NAK` — кадр не принят: неверная сумма или негодная рамка.
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

/// Предел длины кадра по E1381: от `FN` до `ETX`/`ETB` включительно 247 байт.
pub const E1381_FRAME_MAX: usize = 247;

/// Предел кадра, который принимает автомат.
///
/// Больше приборы не шлют, а буфер незакрытого кадра не должен расти без предела (NFR-5).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Сколько автомат ждёт продолжения кадра, прежде чем сообщить о потере: E1381 — 15 с.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(15);

/// Где автомат находится в обмене.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Между кадрами: ждём `ENQ` или первый `STX`.
    Idle,
    /// Внутри кадра: байты от `FN` до `ETX`/`ETB` уже сложены в буфер.
    Frame,
    /// Конец кадра принят, ждём первый знак контрольной суммы.
    ChecksumHigh,
    /// Первый знак принят, ждём второй.
    ChecksumLow,
}

/// Автоответчик ASTM E1381 (FR-17): профиль `astm_e1381`.
#[derive(Debug, Clone)]
pub struct Astm {
    state: State,
    /// Байты кадра от `FN` до `ETX`/`ETB` включительно — по ним же считается сумма.
    frame: Vec<u8>,
    /// Первый знак контрольной суммы, принятый в [`State::ChecksumHigh`].
    high: u8,
    /// Номер, который ждём в следующем кадре: 0…7 циклически.
    expected_fn: u8,
    /// Байты вне кадра: о блоке пишется одно `Info`, когда блок заканчивается.
    outside: usize,
    /// О превышении предела E1381 для текущего кадра уже сообщено.
    long_frame: bool,
    nak_on_bad_checksum: bool,
    frame_limit: usize,
    /// Момент `ENQ` или последнего байта кадра: от него считается [`FRAME_TIMEOUT`].
    idle_since: Option<Now>,
    /// После `ENQ` кадр ещё не начался: истечение срока — молчание прибора, а не потеря.
    awaiting_frame: bool,
}

impl Astm {
    /// Автоответчик, который подтверждает кадр с неверной суммой (`ACK`): цель — получить
    /// данные, а ошибка попадает в журнал (FR-17).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            frame: Vec::new(),
            high: 0,
            expected_fn: 0,
            outside: 0,
            long_frame: false,
            nak_on_bad_checksum: false,
            frame_limit: MAX_FRAME_BYTES,
            idle_since: None,
            awaiting_frame: false,
        }
    }

    /// Отвечать [`NAK`] на неверную контрольную сумму вместо [`ACK`].
    #[must_use]
    pub const fn with_nak_on_bad_checksum(mut self, nak_on_bad_checksum: bool) -> Self {
        self.nak_on_bad_checksum = nak_on_bad_checksum;
        self
    }

    /// Другой предел кадра.
    ///
    /// Нужен тестам: проверять переполнение на 64 КиБ значило бы гонять их впустую.
    #[must_use]
    pub const fn with_frame_limit(mut self, frame_limit: usize) -> Self {
        self.frame_limit = frame_limit;
        self
    }

    /// Что автомат отвечает на неверную контрольную сумму (FR-17).
    #[must_use]
    pub const fn nak_on_bad_checksum(&self) -> bool {
        self.nak_on_bad_checksum
    }

    /// Предел кадра в байтах.
    #[must_use]
    pub const fn frame_limit(&self) -> usize {
        self.frame_limit
    }

    /// Номер кадра, который автомат ждёт следующим.
    #[must_use]
    pub const fn expected_frame_number(&self) -> u8 {
        self.expected_fn
    }

    /// Срок ожидания истёк: продолжения кадра нет дольше [`FRAME_TIMEOUT`].
    fn timed_out(&self, now: Now) -> bool {
        self.idle_since.is_some_and(|since| {
            now.timestamp_millis() - since.timestamp_millis()
                >= i64::try_from(FRAME_TIMEOUT.as_millis()).unwrap_or(i64::MAX)
        })
    }

    /// Сообщает о потере по истечении [`FRAME_TIMEOUT`] и возвращает автомат в ожидание.
    fn expire(&mut self, actions: &mut Vec<Action>) {
        let seconds = FRAME_TIMEOUT.as_secs();
        match self.state {
            State::Idle => {
                // Кадра не было вовсе: терять нечего, прибор просто замолчал после `ENQ`.
                actions.push(
                    ResponderEvent::Info {
                        message: format!("no ASTM frame within {seconds} s after ENQ"),
                        code: None,
                    }
                    .into(),
                );
            }
            State::Frame => {
                actions.push(
                    ResponderEvent::Error {
                        message: format!(
                            "incomplete ASTM frame dropped after {seconds} s: {} bytes without ETX or ETB",
                            self.frame.len()
                        ),
                        code: None,
                    }
                    .into(),
                );
                self.frame.clear();
                self.long_frame = false;
            }
            State::ChecksumHigh | State::ChecksumLow => {
                actions.push(
                    ResponderEvent::Error {
                        message: format!(
                            "incomplete ASTM frame dropped after {seconds} s: {} bytes without the checksum",
                            self.frame.len()
                        ),
                        code: None,
                    }
                    .into(),
                );
                self.frame.clear();
                self.long_frame = false;
            }
        }
        self.state = State::Idle;
        self.awaiting_frame = false;
        self.idle_since = None;
    }

    /// Отправляет управляющий байт.
    fn answer(&mut self, byte: u8, actions: &mut Vec<Action>) {
        actions.push(Action::Send(vec![byte]));
    }

    /// Начинает кадр с этого `STX`.
    fn begin_frame(&mut self, now: Now) {
        self.frame.clear();
        self.long_frame = false;
        self.state = State::Frame;
        self.awaiting_frame = false;
        self.idle_since = Some(now);
    }

    /// Запоминает, что кадр обещан `ENQ` и ещё не начался.
    fn await_frame(&mut self, now: Now) {
        self.state = State::Idle;
        self.awaiting_frame = true;
        self.idle_since = Some(now);
    }

    /// `EOT`: передача закончена, следующий кадр снова первый.
    fn end_of_transmission(&mut self) {
        self.state = State::Idle;
        self.frame.clear();
        self.long_frame = false;
        self.expected_fn = 0;
        self.awaiting_frame = false;
        self.idle_since = None;
    }

    /// Сбрасывает недособранный кадр, не теряя его из журнала.
    fn drop_incomplete(&mut self, error: bool, reason: &str, actions: &mut Vec<Action>) {
        let bytes = self.frame.len();
        if bytes > 0 {
            let message = format!(
                "incomplete ASTM frame dropped at {reason}: {bytes} bytes without ETX or ETB"
            );
            actions.push(if error {
                ResponderEvent::Error {
                    message,
                    code: None,
                }
                .into()
            } else {
                ResponderEvent::Info {
                    message,
                    code: None,
                }
                .into()
            });
        }
        self.state = State::Idle;
        self.frame.clear();
        self.long_frame = false;
        self.idle_since = None;
    }

    /// Сообщает о накопленном блоке мусора вне кадра — один раз на блок.
    fn report_outside(&mut self, actions: &mut Vec<Action>) {
        if self.outside > 0 {
            actions.push(
                ResponderEvent::Info {
                    message: format!("{} bytes outside the ASTM frame ignored", self.outside),
                    code: None,
                }
                .into(),
            );
            self.outside = 0;
        }
    }

    /// Разбирает байт внутри кадра.
    fn accept_in_frame(&mut self, byte: u8, now: Now, actions: &mut Vec<Action>) {
        // Байт сразу после `STX` — номер кадра, даже если он совпадает с управляющим:
        // номера 2, 3, 4 и 5 — это те же байты, что `STX`, `ETX`, `EOT` и `ENQ`.
        let number_position = self.frame.is_empty();
        let terminator = !number_position && (byte == ETX || byte == ETB);
        if self.frame.len() >= self.frame_limit {
            actions.push(
                ResponderEvent::Error {
                    message: format!(
                        "ASTM frame exceeded {} bytes and was dropped; its bytes are treated as garbage until the next STX",
                        self.frame_limit
                    ),
                    code: None,
                }
                .into(),
            );
            self.state = State::Idle;
            self.frame.clear();
            self.long_frame = false;
            self.idle_since = None;
            self.outside += 1;
            return;
        }
        self.frame.push(byte);
        self.idle_since = Some(now);
        if terminator {
            self.state = State::ChecksumHigh;
        } else if self.frame.len() > E1381_FRAME_MAX && !self.long_frame {
            // Кадр не по стандарту, но данные нужны: сообщаем и принимаем дальше.
            self.long_frame = true;
            actions.push(
                ResponderEvent::Info {
                    message: format!(
                        "ASTM frame is longer than the E1381 limit of {E1381_FRAME_MAX} bytes: the frame is accepted as is"
                    ),
                    code: None,
                }
                .into(),
            );
        }
    }

    /// Принят второй знак суммы: отвечает на кадр и начинает новый.
    fn complete_frame(&mut self, checksum: u8, actions: &mut Vec<Action>) {
        self.state = State::Idle;
        self.idle_since = None;
        self.awaiting_frame = false;
        // `STX`, сразу за которым `ETX`/`ETB`: номера кадра нет, рамка негодна.
        if self.frame.len() < 2 {
            self.frame.clear();
            actions.push(
                ResponderEvent::Error {
                    message: "ASTM frame has no frame number: STX is followed by ETX or ETB"
                        .to_owned(),
                    code: None,
                }
                .into(),
            );
            self.answer(NAK, actions);
            return;
        }
        let number = self.frame[0];
        let expected: u8 = self
            .frame
            .iter()
            .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
        if number > 7 {
            actions.push(
                ResponderEvent::Info {
                    message: format!(
                        "ASTM frame number is out of the 0-7 range: got {number}; the frame is acknowledged anyway"
                    ),
                    code: None,
                }
                .into(),
            );
        } else if number != self.expected_fn {
            actions.push(
                ResponderEvent::Info {
                    message: format!(
                        "ASTM frame number out of order: got {number}, expected {}; the frame is acknowledged anyway",
                        self.expected_fn
                    ),
                    code: None,
                }
                .into(),
            );
        }
        self.expected_fn = number.wrapping_add(1) & 0x07;
        if checksum == expected {
            self.answer(ACK, actions);
        } else {
            actions.push(
                ResponderEvent::Error {
                    message: format!(
                        "ASTM checksum mismatch: got {checksum:02X}, expected {expected:02X}"
                    ),
                    code: None,
                }
                .into(),
            );
            // По умолчанию всё равно `ACK`: анализ данных важнее целости суммы (FR-17).
            let answer = if self.nak_on_bad_checksum { NAK } else { ACK };
            self.answer(answer, actions);
        }
        self.frame.clear();
        self.long_frame = false;
    }

    /// Знак контрольной суммы не hex: рамка негодна, ответ всегда [`NAK`].
    fn bad_checksum_digit(&mut self, byte: u8, position: &str, actions: &mut Vec<Action>) {
        actions.push(
            ResponderEvent::Error {
                message: format!(
                    "ASTM checksum is not hexadecimal: got 0x{byte:02X} instead of the {position} digit"
                ),
                code: None,
            }
            .into(),
        );
        self.answer(NAK, actions);
        self.state = State::Idle;
        self.frame.clear();
        self.long_frame = false;
        self.idle_since = None;
        self.awaiting_frame = false;
    }

    /// Обрабатывает один байт потока.
    fn accept(&mut self, byte: u8, now: Now, actions: &mut Vec<Action>) {
        // Сразу после `STX` идёт номер кадра: управляющим байтом он не считается, иначе
        // кадры с номерами 2…5 (те же байты, что `STX`, `ETX`, `EOT`, `ENQ`) разваливались бы.
        let number_position = self.state == State::Frame && self.frame.is_empty();
        if self.state != State::Idle && !number_position {
            // Управляющие байты начала и конца передачи обрывают недособранный кадр.
            match byte {
                EOT => {
                    self.drop_incomplete(false, "EOT", actions);
                    self.end_of_transmission();
                    return;
                }
                ENQ => {
                    self.drop_incomplete(false, "ENQ", actions);
                    self.report_outside(actions);
                    self.answer(ACK, actions);
                    self.await_frame(now);
                    return;
                }
                STX => {
                    // Новый кадр поверх недособранного — потеря прежнего.
                    self.drop_incomplete(true, "STX", actions);
                    self.begin_frame(now);
                    return;
                }
                _ => {}
            }
        }
        match self.state {
            State::Idle => match byte {
                ENQ => {
                    self.report_outside(actions);
                    self.answer(ACK, actions);
                    self.await_frame(now);
                }
                STX => {
                    self.report_outside(actions);
                    self.begin_frame(now);
                }
                EOT => {
                    self.report_outside(actions);
                    self.end_of_transmission();
                }
                // `CR LF` закрывают кадр; вне кадра это не мусор, а разделитель.
                CR | LF => {}
                _ => self.outside += 1,
            },
            State::Frame => self.accept_in_frame(byte, now, actions),
            State::ChecksumHigh => match hex_digit(byte) {
                Some(digit) => {
                    self.high = digit;
                    self.state = State::ChecksumLow;
                    self.idle_since = Some(now);
                }
                None => self.bad_checksum_digit(byte, "first", actions),
            },
            State::ChecksumLow => match hex_digit(byte) {
                Some(digit) => self.complete_frame((self.high << 4) | digit, actions),
                None => self.bad_checksum_digit(byte, "second", actions),
            },
        }
    }
}

impl Default for Astm {
    fn default() -> Self {
        Self::new()
    }
}

/// Значение hex-цифры: верхний и нижний регистр равноправны.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl Responder for Astm {
    fn feed(&mut self, data: &[u8], now: Now) -> Vec<Action> {
        let mut actions = Vec::new();
        // Пустой `feed` — это будильник сессии по `poll_timeout`: сначала срок, потом байты.
        if self.timed_out(now) {
            self.expire(&mut actions);
        }
        for &byte in data {
            self.accept(byte, now, &mut actions);
        }
        actions
    }

    fn finish(&mut self, _now: Now) -> Vec<Action> {
        let mut actions = Vec::new();
        match self.state {
            // `STX` без единого байта — пустой кадр: терять нечего, как и пустая рамка MLLP.
            State::Frame if self.frame.is_empty() => {}
            State::Frame => actions.push(
                ResponderEvent::Error {
                    message: format!(
                        "incomplete ASTM frame at the end of the connection: {} bytes without ETX or ETB",
                        self.frame.len()
                    ),
                    code: None,
                }
                .into(),
            ),
            State::ChecksumHigh | State::ChecksumLow => actions.push(
                ResponderEvent::Error {
                    message: format!(
                        "incomplete ASTM frame at the end of the connection: {} bytes without the checksum",
                        self.frame.len()
                    ),
                    code: None,
                }
                .into(),
            ),
            State::Idle => {}
        }
        self.state = State::Idle;
        self.frame.clear();
        self.long_frame = false;
        self.idle_since = None;
        self.awaiting_frame = false;
        // Хвост мусора заканчивается вместе с соединением и тоже должен попасть в журнал.
        self.report_outside(&mut actions);
        actions
    }

    fn reset(&mut self) {
        self.state = State::Idle;
        self.frame.clear();
        self.long_frame = false;
        self.outside = 0;
        self.expected_fn = 0;
        self.idle_since = None;
        self.awaiting_frame = false;
    }

    fn poll_timeout(&mut self, now: Now) -> Option<Duration> {
        let since = self.idle_since?;
        let limit = i64::try_from(FRAME_TIMEOUT.as_millis()).unwrap_or(i64::MAX);
        let elapsed = now.timestamp_millis() - since.timestamp_millis();
        Some(if elapsed >= limit {
            Duration::ZERO
        } else {
            Duration::from_millis(u64::try_from(limit - elapsed).unwrap_or(0))
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta};

    use super::*;

    /// Текст кадра: `H` и сегмент результата.
    const RECORD: &[u8] = b"H|\\^&|||ANALYZER\rP|1|PATIENT^TEST\rL|1|N\r";

    fn now() -> Now {
        DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
    }

    fn later(seconds: i64) -> Now {
        now() + TimeDelta::try_seconds(seconds).unwrap()
    }

    /// Кадр с верной суммой.
    fn framed(number: u8, text: &[u8], terminator: u8) -> Vec<u8> {
        let mut body = vec![number];
        body.extend_from_slice(text);
        body.push(terminator);
        let checksum = body.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
        let mut frame = vec![STX];
        frame.extend_from_slice(&body);
        frame.extend_from_slice(format!("{checksum:02X}\r\n").as_bytes());
        frame
    }

    fn sends(actions: &[Action]) -> Vec<Vec<u8>> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(bytes) => Some(bytes.clone()),
                Action::Event(_) => None,
            })
            .collect()
    }

    fn events(actions: &[Action]) -> Vec<&ResponderEvent> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Event(event) => Some(event),
                Action::Send(_) => None,
            })
            .collect()
    }

    fn feed(responder: &mut Astm, stream: &[u8]) -> Vec<Action> {
        responder.feed(stream, now())
    }

    #[test]
    fn enq_is_answered_with_ack_and_eot_is_silent() {
        let mut responder = Astm::new();
        assert_eq!(sends(&feed(&mut responder, &[ENQ])), vec![vec![ACK]]);
        assert_eq!(
            sends(&feed(&mut responder, &[EOT])),
            Vec::<Vec<u8>>::new(),
            "на EOT ответа нет"
        );
        assert!(events(&feed(&mut responder, &[EOT])).is_empty());
    }

    #[test]
    fn a_frame_is_answered_with_ack_after_the_checksum() {
        let mut responder = Astm::new();
        let stream = framed(0, RECORD, ETX);
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
        assert_eq!(responder.expected_frame_number(), 1);
    }

    #[test]
    fn the_checksum_is_the_sum_modulo_256_with_an_uppercase_hex_pair() {
        // 0x00 + 'H' + '|' + CR + ETX = 0 + 72 + 124 + 13 + 3 = 212 = 0xD4 (mod 256).
        let mut responder = Astm::new();
        let actions = feed(&mut responder, b"\x02\x00H|\r\x03D4\r\n");
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");

        // Сумма с hex-буквой: 0x00 + 'x' + ETX = 0x7B.
        let mut responder = Astm::new();
        assert_eq!(
            sends(&feed(&mut responder, b"\x02\x00x\x037B\r\n")),
            vec![vec![ACK]]
        );
        // Нижний регистр принимается: сумма та же.
        let mut responder = Astm::new();
        assert_eq!(
            sends(&feed(&mut responder, b"\x02\x00x\x037b\r\n")),
            vec![vec![ACK]]
        );
        // Границы: сумма 0x00 (0 + 0xFD + ETX = 0x100) и 0xFF (0 + 0xFC + ETX = 0xFF).
        let mut responder = Astm::new();
        let actions = feed(&mut responder, b"\x02\x00\xFD\x0300\r\n");
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
        let mut responder = Astm::new();
        let actions = feed(&mut responder, b"\x02\x00\xFC\x03FF\r\n");
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_bad_checksum_is_acknowledged_with_an_error_by_default() {
        let mut responder = Astm::new();
        let actions = feed(&mut responder, b"\x02\x00x\x037C\r\n");
        assert_eq!(sends(&actions), vec![vec![ACK]], "по умолчанию ACK (FR-17)");
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Error { message, .. }]
                if message == "ASTM checksum mismatch: got 7C, expected 7B"),
            "{actions:?}"
        );
    }

    #[test]
    fn nak_mode_answers_a_bad_checksum_with_nak() {
        let mut responder = Astm::new().with_nak_on_bad_checksum(true);
        assert!(responder.nak_on_bad_checksum());
        let actions = feed(&mut responder, b"\x02\x00x\x037C\r\n");
        assert_eq!(sends(&actions), vec![vec![NAK]]);
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Error { .. }]),
            "{actions:?}"
        );

        // Верная сумма и в NAK-режиме получает ACK.
        let actions = feed(&mut responder, &framed(1, RECORD, ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_checksum_digit_that_is_not_hex_is_always_nakked() {
        let mut responder = Astm::new();
        let actions = feed(&mut responder, b"\x02\x00x\x03GB\r\n");
        assert_eq!(sends(&actions), vec![vec![NAK]], "рамка негодна");
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Error { message, .. }]
                if message.contains("not hexadecimal")),
            "{actions:?}"
        );
    }

    #[test]
    fn etb_frames_continue_the_record_and_keep_the_frame_numbers() {
        let mut responder = Astm::new();
        let stream = [
            framed(0, b"H|\\^&|||ANALYZER\r", ETB),
            framed(1, b"P|1|PATIENT^TEST\r", ETB),
            framed(2, b"L|1|N\r", ETX),
        ]
        .concat();
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions), vec![vec![ACK]; 3]);
        assert!(
            events(&actions).is_empty(),
            "продолжения — не нарушение: {actions:?}"
        );
    }

    #[test]
    fn frame_numbers_go_from_zero_to_seven_and_wrap_around() {
        let mut responder = Astm::new();
        let mut stream = Vec::new();
        for number in 0..=7u8 {
            stream.extend_from_slice(&framed(number, b"P|1\r", ETX));
        }
        stream.extend_from_slice(&framed(0, b"P|1\r", ETX));
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions).len(), 9);
        assert!(events(&actions).is_empty(), "7→0 не нарушение: {actions:?}");
        assert_eq!(responder.expected_frame_number(), 1);
    }

    #[test]
    fn a_skipped_frame_number_is_reported_but_the_frame_is_acknowledged() {
        let mut responder = Astm::new();
        let actions = feed(&mut responder, &framed(3, b"P|1\r", ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Info { message, .. }]
                if message.contains("out of order: got 3, expected 0")),
            "{actions:?}"
        );
        assert_eq!(responder.expected_frame_number(), 4);

        let actions = feed(&mut responder, &framed(200, b"P|1\r", ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Info { message, .. }]
                if message.contains("0-7 range")),
            "{actions:?}"
        );
    }

    #[test]
    fn eot_resets_the_frame_number() {
        let mut responder = Astm::new();
        feed(&mut responder, &framed(5, b"P|1\r", ETX));
        assert_eq!(responder.expected_frame_number(), 6);
        feed(&mut responder, &[EOT]);
        assert_eq!(responder.expected_frame_number(), 0);
        let actions = feed(&mut responder, &framed(0, b"P|1\r", ETX));
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn several_frames_in_one_fragment_get_one_ack_each() {
        let mut responder = Astm::new();
        let stream = [
            framed(0, b"H|\r", ETB),
            framed(1, b"P|1\r", ETX),
            framed(2, b"Q|1\r", ETX),
        ]
        .concat();
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions).len(), 3);
    }

    #[test]
    fn a_frame_split_byte_by_byte_gives_the_same_answers() {
        let stream = framed(0, RECORD, ETX);
        let mut whole = Astm::new();
        let expected = feed(&mut whole, &stream);

        let mut piecewise = Astm::new();
        let mut actual = Vec::new();
        for byte in &stream {
            actual.extend(piecewise.feed(std::slice::from_ref(byte), now()));
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn garbage_outside_the_frame_is_reported_once_per_block() {
        let mut responder = Astm::new();
        assert!(
            events(&feed(&mut responder, b"junk")).is_empty(),
            "блок не закончен"
        );
        let mut stream = b"more".to_vec();
        stream.push(STX);
        stream.extend_from_slice(&framed(0, b"P|1\r", ETX)[1..]);
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions), vec![vec![ACK]], "мусор не мешает ответу");
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Info { message, .. }]
                if message.contains("8 bytes outside")),
            "{actions:?}"
        );
    }

    #[test]
    fn cr_and_lf_outside_the_frame_are_not_garbage() {
        let mut responder = Astm::new();
        let mut stream = framed(0, b"P|1\r", ETX);
        stream.extend_from_slice(b"\r\n\r\n");
        stream.extend_from_slice(&framed(1, b"P|1\r", ETX));
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions).len(), 2);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn an_unknown_protocol_byte_is_not_answered() {
        // «Авто» решает по первому байту, но сам ASTM на 0x0B не отвечает: это чужой протокол.
        let mut responder = Astm::new();
        let actions = feed(&mut responder, b"\x0bMSH|nothing\r");
        assert!(sends(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_frame_longer_than_the_standard_is_accepted_with_an_info() {
        let mut responder = Astm::new();
        let long = vec![b'P'; E1381_FRAME_MAX + 5];
        let actions = feed(&mut responder, &framed(0, &long, ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Info { message, .. }]
                if message.contains("longer than the E1381 limit")),
            "одно сообщение на кадр, не на байт: {actions:?}"
        );
    }

    #[test]
    fn an_overlong_frame_is_dropped_with_an_error() {
        let mut responder = Astm::new().with_frame_limit(64);
        assert_eq!(responder.frame_limit(), 64);
        let mut stream = vec![STX];
        stream.extend_from_slice(&[b'x'; 100]);
        let actions = feed(&mut responder, &stream);
        assert!(sends(&actions).is_empty());
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Error { message, .. }]
                if message.contains("exceeded 64 bytes")),
            "{actions:?}"
        );

        // После сброса автомат снова готов.
        let actions = feed(&mut responder, &framed(0, b"P|1\r", ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
    }

    #[test]
    fn a_query_record_gets_only_an_ack() {
        // FR-18: запрос подтверждается только на транспортном уровне.
        let mut responder = Astm::new();
        let actions = feed(&mut responder, &framed(0, b"Q|1|^PATIENT^TEST||ALL\r", ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]], "других байтов нет");
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_new_frame_over_an_unfinished_one_reports_the_loss() {
        let mut responder = Astm::new();
        let mut stream = vec![STX];
        stream.extend_from_slice(b"\x00P|1\r");
        stream.extend_from_slice(&framed(0, b"P|2\r", ETX));
        let actions = feed(&mut responder, &stream);
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Error { message, .. }]
                if message.contains("dropped at STX")),
            "{actions:?}"
        );
    }

    #[test]
    fn eot_over_an_unfinished_frame_reports_it_as_info() {
        let mut responder = Astm::new();
        let mut stream = vec![STX];
        stream.extend_from_slice(b"\x00P|1\r");
        stream.push(EOT);
        let actions = feed(&mut responder, &stream);
        assert!(sends(&actions).is_empty(), "на EOT ответа нет");
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Info { message, .. }]
                if message.contains("dropped at EOT")),
            "{actions:?}"
        );
    }

    #[test]
    fn finish_reports_an_unfinished_frame_once() {
        let mut responder = Astm::new();
        let mut stream = vec![STX];
        stream.extend_from_slice(b"\x00H|\\^&");
        let body = 6;
        assert!(feed(&mut responder, &stream).is_empty());
        let actions = responder.finish(now());
        assert!(sends(&actions).is_empty());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Error { message, .. })]
                if message.contains(&format!("{body} bytes"))),
            "{actions:?}"
        );
        assert!(responder.finish(now()).is_empty(), "повторный finish пуст");
    }

    #[test]
    fn finish_reports_a_truncated_checksum_and_a_garbage_tail() {
        let mut responder = Astm::new();
        let mut stream = b"junk".to_vec();
        stream.push(STX);
        stream.extend_from_slice(b"\x00P|1\r\x03A");
        let actions = feed(&mut responder, &stream);
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Info { .. })]),
            "блок мусора закончился на STX: {actions:?}"
        );
        let actions = responder.finish(now());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Error { message, .. })]
                if message.contains("without the checksum")),
            "{actions:?}"
        );

        // Хвост мусора в конце соединения.
        let mut responder = Astm::new();
        assert!(feed(&mut responder, b"tail").is_empty());
        let actions = responder.finish(now());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Info { message, .. })]
                if message.contains("4 bytes")),
            "{actions:?}"
        );
    }

    #[test]
    fn a_lone_frame_start_is_not_an_error_at_finish() {
        let mut responder = Astm::new();
        assert!(feed(&mut responder, &[STX]).is_empty());
        assert!(
            responder.finish(now()).is_empty(),
            "пустой кадр — не потеря"
        );
    }

    #[test]
    fn reset_forgets_the_frame_and_the_number() {
        let mut responder = Astm::new();
        feed(&mut responder, &framed(3, b"P|1\r", ETX));
        let mut stream = vec![STX];
        stream.extend_from_slice(b"\x00H|");
        feed(&mut responder, &stream);
        responder.reset();
        assert_eq!(responder.expected_frame_number(), 0);
        assert!(
            responder.finish(now()).is_empty(),
            "недособранный кадр и мусор сброшены"
        );

        // И номер снова с нуля: кадр 0 не считается нарушением порядка.
        let actions = feed(&mut responder, &framed(0, b"P|1\r", ETX));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn poll_timeout_counts_from_the_last_frame_byte() {
        let mut responder = Astm::new();
        assert_eq!(responder.poll_timeout(now()), None, "в ожидании срока нет");

        let mut stream = vec![STX];
        stream.extend_from_slice(b"\x00P|1\r");
        assert!(responder.feed(&stream, now()).is_empty());
        assert_eq!(responder.poll_timeout(now()), Some(FRAME_TIMEOUT));
        assert_eq!(
            responder.poll_timeout(later(10)),
            Some(Duration::from_secs(5)),
            "срок отсчитывается от последнего байта"
        );
        assert_eq!(responder.poll_timeout(later(20)), Some(Duration::ZERO));
    }

    #[test]
    fn an_unfinished_frame_is_dropped_when_the_timeout_expires() {
        let mut responder = Astm::new();
        let mut stream = vec![STX];
        stream.extend_from_slice(b"\x00P|1\r");
        assert!(responder.feed(&stream, now()).is_empty());

        // Сессия будит автомат пустым `feed` по сроку из `poll_timeout`.
        let actions = responder.feed(&[], later(15));
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Error { message, .. })]
                if message.contains("dropped after 15 s")),
            "{actions:?}"
        );
        assert_eq!(responder.poll_timeout(later(15)), None, "кадр сброшен");

        // Следующий кадр обслуживается как обычно.
        let actions = responder.feed(&framed(0, b"P|1\r", ETX), later(16));
        assert_eq!(sends(&actions), vec![vec![ACK]]);
    }

    #[test]
    fn silence_after_enq_is_reported_as_info() {
        let mut responder = Astm::new();
        assert_eq!(sends(&feed(&mut responder, &[ENQ])), vec![vec![ACK]]);
        assert!(responder.poll_timeout(now()).is_some());
        let actions = responder.feed(&[], later(20));
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Info { message, .. })]
                if message.contains("after ENQ")),
            "{actions:?}"
        );
    }

    #[test]
    fn every_answer_is_a_single_control_byte() {
        let mut responder = Astm::new();
        let mut stream = vec![ENQ];
        stream.extend_from_slice(&framed(0, b"P|1\r", ETX));
        stream.push(EOT);
        let actions = feed(&mut responder, &stream);
        let answers = sends(&actions);
        assert_eq!(answers, vec![vec![ACK], vec![ACK]]);
        assert!(
            answers.iter().all(|answer| answer.len() == 1),
            "ответ — один управляющий байт"
        );
    }
}
