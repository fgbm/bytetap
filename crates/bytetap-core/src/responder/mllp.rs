//! MLLP: выделение HL7-сообщений по рамке `0x0B … 0x1C 0x0D` и ACK на каждое (FR-16).
//!
//! Автомат собирает сообщение из любого числа фрагментов: границы рамки могут попадать
//! на границу фрагмента, а сама рамка — приходить побайтно.
//!
//! - Байты вне рамки ответа не вызывают. Они не теряются: их пишет журнал как `data`,
//!   а автомат один раз сообщает о блоке мусора [`ResponderEvent::Info`], когда блок
//!   заканчивается — то есть на открывающем `0x0B` следующей рамки.
//! - `0x0B` внутри незакрытой рамки — потеря предыдущего сообщения: [`ResponderEvent::Error`]
//!   и новая рамка с этого байта.
//! - `0x1C`, за которым не идёт `0x0D`, — обычный байт сообщения, рамка не закрыта.
//! - Незакрытая рамка дольше [`Hl7Mllp::frame_limit`] байт сбрасывается с
//!   [`ResponderEvent::Error`]: бесконечно растущий буфер недопустим (NFR-5).
//! - Подряд идущие `0x0B` с пустой рамкой — не ошибка: прибор мог начать сообщение заново.
//! - При закрытии соединения [`Responder::finish`] сообщает о недособранной рамке и о хвосте
//!   мусора вне неё: иначе они пропали бы из журнала молча. Пустая рамка и здесь не ошибка.

use crate::responder::hl7;
use crate::responder::{Action, ControlIdSource, Now, Responder, ResponderEvent};

/// Начало рамки MLLP — `VT`.
pub const SB: u8 = 0x0B;

/// Конец рамки MLLP — `FS`; за ним идёт `CR`.
pub const EB: u8 = 0x1C;

/// Завершение рамки MLLP — `CR`.
pub const CR: u8 = 0x0D;

/// Предельный размер незакрытой рамки: больше сообщения приборы не шлют.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Автоответчик HL7/MLLP (FR-16): профиль `hl7_mllp`.
#[derive(Debug, Clone)]
pub struct Hl7Mllp {
    frame: Vec<u8>,
    in_frame: bool,
    pending_eb: bool,
    outside: usize,
    frame_limit: usize,
    ids: ControlIdSource,
}

impl Hl7Mllp {
    /// Автоответчик с пределом рамки [`MAX_FRAME_BYTES`].
    ///
    /// `ids` — общий на сессию источник `MSH-10`: соединения сессии передают один и тот же
    /// счётчик, поэтому их ответы не совпадают идентификаторами.
    #[must_use]
    pub fn new(ids: ControlIdSource) -> Self {
        Self::with_frame_limit(MAX_FRAME_BYTES, ids)
    }

    /// Автоответчик с другим пределом рамки.
    ///
    /// Нужен тестам: проверять переполнение на 16 МиБ значило бы гонять их впустую.
    #[must_use]
    pub fn with_frame_limit(frame_limit: usize, ids: ControlIdSource) -> Self {
        Self {
            frame: Vec::new(),
            in_frame: false,
            pending_eb: false,
            outside: 0,
            frame_limit,
            ids,
        }
    }

    /// Предел незакрытой рамки в байтах.
    #[must_use]
    pub const fn frame_limit(&self) -> usize {
        self.frame_limit
    }

    /// Открывает новую рамку: закрывает блок мусора и сообщает о потерянном сообщении.
    ///
    /// Пустая незакрытая рамка потерей не считается: прибор мог начать сообщение заново.
    fn start_frame(&mut self, actions: &mut Vec<Action>) {
        self.report_outside(actions);
        if self.in_frame && !self.frame.is_empty() {
            actions.push(
                ResponderEvent::Error {
                    message: format!(
                        "incomplete MLLP frame dropped: {} bytes without 0x1C 0x0D",
                        self.frame.len()
                    ),
                }
                .into(),
            );
        }
        self.frame.clear();
        self.in_frame = true;
    }

    /// Принимает байт внутри рамки или считает его мусором вне рамки.
    fn accept(&mut self, byte: u8, actions: &mut Vec<Action>) {
        if !self.in_frame {
            self.outside += 1;
            return;
        }
        if self.frame.len() >= self.frame_limit {
            actions.push(
                ResponderEvent::Error {
                    message: format!(
                        "MLLP frame exceeded {} bytes and was dropped; its bytes are treated as garbage until the next 0x0B",
                        self.frame_limit
                    ),
                }
                .into(),
            );
            self.frame.clear();
            self.in_frame = false;
            self.outside += 1;
            return;
        }
        self.frame.push(byte);
    }

    /// Закрывает рамку и отвечает на собранное сообщение.
    fn finish_frame(&mut self, now: Now, actions: &mut Vec<Action>) {
        if !self.in_frame {
            // Одиночные `0x1C 0x0D` вне рамки — такой же мусор, как остальные байты.
            self.outside += 2;
            return;
        }
        self.in_frame = false;
        let answer = hl7::answer(&self.frame, now, &self.ids);
        self.frame.clear();
        actions.extend(answer.events.into_iter().map(Action::from));
        if let Some(ack) = answer.ack {
            actions.push(Action::Send(framed(&ack)));
        }
    }

    /// Сообщает о накопленном блоке мусора вне рамки — один раз на блок.
    fn report_outside(&mut self, actions: &mut Vec<Action>) {
        if self.outside > 0 {
            actions.push(
                ResponderEvent::Info {
                    message: format!("{} bytes outside the MLLP frame ignored", self.outside),
                }
                .into(),
            );
            self.outside = 0;
        }
    }
}

impl Default for Hl7Mllp {
    fn default() -> Self {
        Self::new(ControlIdSource::new())
    }
}

impl Responder for Hl7Mllp {
    fn feed(&mut self, data: &[u8], now: Now) -> Vec<Action> {
        let mut actions = Vec::new();
        for &byte in data {
            if self.pending_eb {
                self.pending_eb = false;
                if byte == CR {
                    self.finish_frame(now, &mut actions);
                    continue;
                }
                // `0x1C` не перед `CR` — это байт сообщения, а не конец рамки.
                self.accept(EB, &mut actions);
            }
            match byte {
                SB => self.start_frame(&mut actions),
                EB => self.pending_eb = true,
                other => self.accept(other, &mut actions),
            }
        }
        actions
    }

    fn finish(&mut self, _now: Now) -> Vec<Action> {
        let mut actions = Vec::new();
        // `0x1C` в самом конце: либо байт незакрытой рамки, либо мусор вне неё.
        if self.pending_eb {
            self.pending_eb = false;
            if self.in_frame {
                self.frame.push(EB);
            } else {
                self.outside += 1;
            }
        }
        if self.in_frame {
            // Пустая рамка (`0x0B` без единого байта данных) потерей не считается: как и
            // подряд идущие `0x0B`, это прибор мог начать сообщение и передумать.
            if !self.frame.is_empty() {
                actions.push(
                    ResponderEvent::Error {
                        message: format!(
                            "incomplete MLLP frame at the end of the connection: {} bytes without 0x1C 0x0D",
                            self.frame.len()
                        ),
                    }
                    .into(),
                );
            }
            self.frame.clear();
            self.in_frame = false;
        }
        self.report_outside(&mut actions);
        actions
    }

    fn reset(&mut self) {
        self.frame.clear();
        self.in_frame = false;
        self.pending_eb = false;
        self.outside = 0;
        // Источник `MSH-10` общий на сессию и не сбрасывается: номера не должны повторяться.
    }
}

/// Оборачивает текст ACK в рамку MLLP.
fn framed(ack: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ack.len() + 3);
    out.push(SB);
    out.extend_from_slice(ack);
    out.push(EB);
    out.push(CR);
    out
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    const MESSAGE: &[u8] =
        b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0001|P|2.3.1\r\
                              PID|1||PATIENT^TEST\r";

    fn now() -> Now {
        DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
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

    #[test]
    fn a_framed_message_is_answered_with_a_framed_ack() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let actions = responder.feed(&framed(MESSAGE), now());
        let sends = sends(&actions);
        assert_eq!(sends.len(), 1);
        let ack = &sends[0];
        assert_eq!(ack.first(), Some(&SB));
        assert_eq!(ack.get(ack.len() - 2..), Some(&[EB, CR][..]));
        let text = String::from_utf8(ack.clone()).expect("ACK — ASCII");
        assert!(text.contains("MSH|^~\\&|LIS|LAB|ANALYZER|LAB|"));
        assert!(text.contains("MSA|AA|MSG0001\r"));
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn byte_by_byte_feed_matches_whole_feed() {
        let stream = framed(MESSAGE);
        let mut whole = Hl7Mllp::new(ControlIdSource::new());
        let expected = whole.feed(&stream, now());

        let mut piecewise = Hl7Mllp::new(ControlIdSource::new());
        let mut actual = Vec::new();
        for byte in &stream {
            actual.extend(piecewise.feed(std::slice::from_ref(byte), now()));
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn terminator_split_between_fragments_still_closes_the_frame() {
        let stream = framed(MESSAGE);
        let cut = stream.len() - 1;
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let mut actions = responder.feed(&stream[..cut], now());
        assert!(sends(&actions).is_empty(), "рамка ещё не закрыта");
        actions.extend(responder.feed(&stream[cut..], now()));
        assert_eq!(sends(&actions).len(), 1);
    }

    #[test]
    fn garbage_outside_the_frame_is_reported_once_per_block() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let mut actions = responder.feed(b"junk", now());
        assert!(actions.is_empty(), "блок ещё не закончился");
        actions = responder.feed(b"more", now());
        assert!(actions.is_empty());

        let mut stream = Vec::new();
        stream.extend_from_slice(b"tail");
        stream.extend_from_slice(&framed(MESSAGE));
        let actions = responder.feed(&stream, now());
        let events = events(&actions);
        assert_eq!(events.len(), 1, "{actions:?}");
        assert!(
            matches!(events[0], ResponderEvent::Info { message } if message.contains("outside")),
            "{events:?}"
        );
        assert_eq!(sends(&actions).len(), 1, "мусор не мешает ответу");
    }

    #[test]
    fn a_new_frame_inside_an_open_frame_drops_the_previous_one() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let mut stream = Vec::new();
        stream.push(SB);
        stream.extend_from_slice(b"MSH|truncated");
        stream.extend_from_slice(&framed(MESSAGE));
        let actions = responder.feed(&stream, now());
        let events = events(&actions);
        assert!(
            matches!(events[0], ResponderEvent::Error { message } if message.contains("incomplete")),
            "{events:?}"
        );
        assert_eq!(sends(&actions).len(), 1);
    }

    #[test]
    fn an_end_of_block_byte_not_followed_by_cr_is_message_data() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let mut message = MESSAGE.to_vec();
        // 0x1C внутри поля PID-3: за ним идёт не CR, поэтому рамка не закрывается.
        message.insert(message.len() - 8, EB);
        let actions = responder.feed(&framed(&message), now());
        assert_eq!(sends(&actions).len(), 1, "{actions:?}");
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_stray_terminator_outside_a_frame_is_garbage() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let actions = responder.feed(b"\x1c\r", now());
        assert!(sends(&actions).is_empty());
        assert!(actions.is_empty());
        let mut stream = b"junk".to_vec();
        stream.extend_from_slice(&framed(MESSAGE));
        let actions = responder.feed(&stream, now());
        assert_eq!(sends(&actions).len(), 1);
        assert!(
            matches!(events(&actions)[0], ResponderEvent::Info { message } if message.contains("6 bytes")),
            "0x1C 0x0D и junk вместе дают один блок мусора: {actions:?}"
        );
    }

    #[test]
    fn an_empty_frame_is_an_error_without_a_response() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let actions = responder.feed(b"\x0b\x1c\r", now());
        assert!(sends(&actions).is_empty());
        assert!(
            matches!(events(&actions)[..], [ResponderEvent::Error { .. }]),
            "{actions:?}"
        );
    }

    #[test]
    fn an_overlong_frame_is_dropped_with_an_error() {
        let mut responder = Hl7Mllp::with_frame_limit(256, ControlIdSource::new());
        assert_eq!(responder.frame_limit(), 256);
        let mut stream = Vec::new();
        stream.push(SB);
        stream.extend_from_slice(&[b'x'; 300]);
        let actions = responder.feed(&stream, now());
        assert!(sends(&actions).is_empty());
        assert!(
            matches!(events(&actions)[..], [ResponderEvent::Error { .. }]),
            "{actions:?}"
        );

        // После сброса автомат снова готов: следующая рамка обслуживается.
        let actions = responder.feed(&framed(MESSAGE), now());
        assert_eq!(sends(&actions).len(), 1);
    }

    #[test]
    fn reset_forgets_the_open_frame_but_keeps_the_id_counter() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let first = sender_id(&responder.feed(&framed(MESSAGE), now()));
        assert_eq!(first, "BT261019101530000001");

        let mut stream = Vec::new();
        stream.push(SB);
        stream.extend_from_slice(b"MSH|unfinished");
        assert!(responder.feed(&stream, now()).is_empty());
        responder.reset();

        let second = sender_id(&responder.feed(&framed(MESSAGE), now()));
        assert_eq!(second, "BT261019101530000002", "счётчик не сбрасывается");
    }

    /// Поле MSH-10 из отправленного ACK.
    fn sender_id(actions: &[Action]) -> String {
        let ack = sends(actions).pop().expect("ACK");
        let text = String::from_utf8(ack).expect("ASCII");
        let msh = text
            .trim_start_matches('\u{b}')
            .split('\r')
            .next()
            .expect("MSH");
        msh.split('|').nth(9).expect("MSH-10").to_owned()
    }

    #[test]
    fn consecutive_frame_starts_over_an_empty_frame_are_not_an_error() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let actions = responder.feed(b"\x0b\x0b\x0b", now());
        assert!(actions.is_empty(), "пустая рамка потерей не считается");
        let actions = responder.feed(&framed(MESSAGE), now());
        assert_eq!(sends(&actions).len(), 1);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn finish_after_a_lone_frame_start_is_not_an_error() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        assert!(responder.feed(b"\x0b", now()).is_empty());
        assert!(
            responder.finish(now()).is_empty(),
            "пустая рамка — не потеря сообщения"
        );
    }

    #[test]
    fn finish_reports_an_unfinished_frame_and_a_garbage_tail() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let mut stream = b"junk".to_vec();
        stream.push(SB);
        stream.extend_from_slice(b"MSH|^~\\&|ANALYZER");
        let body_len = b"MSH|^~\\&|ANALYZER".len();
        let actions = responder.feed(&stream, now());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Info { .. })]),
            "блок мусора закончился на 0x0B: {actions:?}"
        );

        let actions = responder.finish(now());
        assert!(sends(&actions).is_empty());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Error { message })]
                if message.contains(&format!("{body_len} bytes"))),
            "{actions:?}"
        );
        assert!(responder.finish(now()).is_empty(), "повторный finish пуст");

        // Мусор в конце соединения: рамки не было вовсе.
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        assert!(responder.feed(b"tail\x1c", now()).is_empty());
        let actions = responder.finish(now());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Info { message })]
                if message.contains("5 bytes")),
            "{actions:?}"
        );
    }

    #[test]
    fn two_responders_of_one_session_do_not_repeat_control_ids() {
        let ids = ControlIdSource::new();
        let mut first = Hl7Mllp::new(ids.clone());
        let mut second = Hl7Mllp::new(ids);
        let first = sender_id(&first.feed(&framed(MESSAGE), now()));
        let second = sender_id(&second.feed(&framed(MESSAGE), now()));
        assert_ne!(first, second, "MSH-10 общий на сессию");
        assert_eq!(first, "BT261019101530000001");
        assert_eq!(second, "BT261019101530000002");
    }

    #[test]
    fn several_messages_in_one_fragment_get_one_ack_each() {
        let mut responder = Hl7Mllp::new(ControlIdSource::new());
        let mut stream = Vec::new();
        for _ in 0..3 {
            stream.extend_from_slice(&framed(MESSAGE));
        }
        let actions = responder.feed(&stream, now());
        assert_eq!(sends(&actions).len(), 3);
    }
}
