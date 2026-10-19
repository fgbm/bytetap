//! Автоответчик: sans-IO автомат, который получает прочитанные байты и возвращает ответы (FR-3, FR-14…FR-18).
//!
//! Модуль не знает ни про сокеты, ни про журнал: [`Responder::feed`] получает очередной
//! фрагмент потока и возвращает [`Action`] — что отправить обратно анализатору и какие события
//! положить в журнал. Сессия (шаг 3) отдаёт сюда каждый фрагмент и сама отправляет и пишет то,
//! что вернулось.
//!
//! Время передаётся снаружи (`now`): из него строится `MSH-7` ACK. Автомат не смотрит на часы,
//! поэтому тесты детерминированы, а сессия берёт метку сама.
//!
//! Профили FR-14: [`NoResponder`] — «Без ответов», [`Hl7Mllp`] — «HL7/MLLP». ASTM появится
//! в M3, «Авто» — тоже. Поведение HL7/MLLP описано в `docs/reference/autoresponder.md`.

mod hl7;
mod mllp;

use chrono::{DateTime, FixedOffset};

pub use mllp::Hl7Mllp;

/// Момент обработки фрагмента: из него строится время отправки ACK (`MSH-7`).
pub type Now = DateTime<FixedOffset>;

/// Действие автоответчика на прочитанные байты.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Отправить байты в сторону анализатора — целиком, как есть.
    Send(Vec<u8>),
    /// Событие для журнала сессии.
    Event(ResponderEvent),
}

impl From<ResponderEvent> for Action {
    fn from(event: ResponderEvent) -> Self {
        Self::Event(event)
    }
}

/// Событие автоответчика, нейтральное к формату журнала.
///
/// Шаг 3 превращает его в строку журнала: тип и поля события выбирает сессия, а не автомат,
/// чтобы поведение протокола не зависело от формата файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponderEvent {
    /// Ошибка обработки: сообщение потеряно или подтверждено `AE` (FR-16).
    Error {
        /// Текст для журнала и ленты: что именно пошло не так.
        message: String,
    },
    /// Замечание, которое не мешает обмену: мусор вне рамки, поле, выброшенное из ACK.
    Info {
        /// Текст для журнала и ленты.
        message: String,
    },
}

/// Автоответчик одного соединения.
///
/// Состояние живёт столько же, сколько соединение; [`Responder::reset`] вызывается на новое
/// соединение, чтобы недособранное сообщение прошлого не попало в это.
pub trait Responder {
    /// Обрабатывает очередной фрагмент потока и возвращает действия по порядку.
    ///
    /// `now` — момент обработки, из него строится `MSH-7` ответа.
    fn feed(&mut self, data: &[u8], now: Now) -> Vec<Action>;

    /// Забывает незавершённое состояние: вызывается на новое соединение.
    fn reset(&mut self);
}

/// Автоответчик, который ничего не отвечает: профиль «Без ответов» (FR-14).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoResponder;

impl Responder for NoResponder {
    fn feed(&mut self, _data: &[u8], _now: Now) -> Vec<Action> {
        Vec::new()
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn now() -> Now {
        DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
    }

    #[test]
    fn no_responder_never_answers() {
        let mut responder = NoResponder;
        let mut actions = responder.feed(b"\x0bMSH|^~\\&|ANALYZER\r\x1c\r", now());
        assert!(actions.is_empty());
        actions = responder.feed(b"garbage", now());
        assert!(actions.is_empty());
        responder.reset();
    }

    #[test]
    fn responder_event_converts_to_action() {
        let event = ResponderEvent::Info {
            message: "x".to_owned(),
        };
        assert_eq!(Action::from(event.clone()), Action::Event(event));
    }
}
