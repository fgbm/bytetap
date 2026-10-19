//! Автоответчик: sans-IO автомат, который получает прочитанные байты и возвращает ответы (FR-3, FR-14…FR-18).
//!
//! Модуль не знает ни про сокеты, ни про журнал: [`Responder::feed`] получает очередной
//! фрагмент потока и возвращает [`Action`] — что отправить обратно анализатору и какие события
//! положить в журнал. Сессия отдаёт сюда каждый фрагмент и сама отправляет и пишет то,
//! что вернулось.
//!
//! Время передаётся снаружи (`now`): из него строится `MSH-7` ACK и считается срок ожидания
//! кадра ASTM. Автомат не смотрит на часы, поэтому тесты детерминированы, а сессия берёт
//! метку сама.
//!
//! Номера для `MSH-10` берутся из общего на сессию [`ControlIdSource`]: счётчик передаётся
//! в конструктор, поэтому два соединения сессии не выдают одинаковых идентификаторов.
//!
//! Профили FR-14 собираются в [`ResponderKind`]: [`NoResponder`] — «Без ответов»,
//! [`Hl7Mllp`] — «HL7/MLLP», [`Astm`] — «ASTM E1381», [`Auto`] — «Авто» по первому байту
//! соединения. Поведение профилей описано в `docs/reference/autoresponder.md`.

mod astm;
mod auto;
mod hl7;
mod mllp;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, FixedOffset};

pub use astm::Astm;
pub use auto::Auto;
pub use mllp::Hl7Mllp;

/// Источник `MSH-10` ответов: один счётчик на сессию.
///
/// Автоответчик одного соединения не может гарантировать уникальность сам: соединений за
/// сессию много, а `MSH-10` должен не повторяться в её пределах (FR-16). Поэтому счётчик
/// живёт снаружи и передаётся в конструктор [`Hl7Mllp`]; клонирование дешёвое.
#[derive(Debug, Clone, Default)]
pub struct ControlIdSource(Arc<AtomicU64>);

impl ControlIdSource {
    /// Новый счётчик сессии.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Следующий номер, начиная с 1.
    #[must_use]
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Момент обработки фрагмента: из него строится время отправки ACK (`MSH-7`) и считается
/// срок ожидания кадра ASTM.
pub type Now = DateTime<FixedOffset>;

/// Машинно-различимый код события журнала.
///
/// `message` события — английский текст для журнала и поиска, а код интерфейс заменяет
/// русской строкой из i18n (FR-15). Значение кода — строка в поле `code` журнала
/// (`docs/reference/session-format.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCode {
    /// «Авто» не распознал протокол по первому байту: ответов для соединения нет (FR-15).
    ProtocolUndetected,
}

impl EventCode {
    /// Значение в поле `code` журнала.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolUndetected => "protocol_undetected",
        }
    }
}

impl fmt::Display for EventCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
/// Сессия превращает его в строку журнала: тип и поля события выбирает она, а не автомат,
/// чтобы поведение протокола не зависело от формата файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponderEvent {
    /// Ошибка обработки: сообщение потеряно или подтверждено `AE` (FR-16).
    Error {
        /// Текст для журнала и ленты: что именно пошло не так.
        message: String,
        /// Код для интерфейса, если у события есть русская строка (FR-15).
        code: Option<EventCode>,
    },
    /// Замечание, которое не мешает обмену: мусор вне рамки, поле, выброшенное из ACK.
    Info {
        /// Текст для журнала и ленты.
        message: String,
        /// Код для интерфейса, если у события есть русская строка (FR-15).
        code: Option<EventCode>,
    },
}

impl ResponderEvent {
    /// Текст события для журнала.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Error { message, .. } | Self::Info { message, .. } => message,
        }
    }

    /// Код события для интерфейса, если он есть.
    #[must_use]
    pub const fn code(&self) -> Option<EventCode> {
        match self {
            Self::Error { code, .. } | Self::Info { code, .. } => *code,
        }
    }
}

/// Автоответчик одного соединения.
///
/// Состояние живёт столько же, сколько соединение. Когда соединение закрывается, сессия
/// вызывает [`Responder::finish`]: он не выбрасывает хвост молча, а сообщает о нём в журнал.
pub trait Responder {
    /// Обрабатывает очередной фрагмент потока и возвращает действия по порядку.
    ///
    /// `now` — момент обработки, из него строится `MSH-7` ответа.
    fn feed(&mut self, data: &[u8], now: Now) -> Vec<Action>;

    /// Завершает соединение и возвращает действия по оставшемуся состоянию.
    ///
    /// В отличие от [`Responder::reset`] ничего не теряет молча: незакрытая рамка и мусор,
    /// накопившиеся к концу соединения, попадают в журнал.
    fn finish(&mut self, now: Now) -> Vec<Action>;

    /// Забывает незавершённое состояние: новое соединение не должно получить чужое сообщение.
    ///
    /// Нужен там, где «соединение» не заканчивается само, — на COM-порту.
    fn reset(&mut self);

    /// Сколько ждать до следующего события автомата, если байтов больше не будет.
    ///
    /// `None` — автомат ждёт только данных, и сессии нечего держать в `select!`. Иначе
    /// сессия заводит срок и по его истечении будит автомат пустым `feed(&[], now)`: тот
    /// сообщает о недособранном состоянии в журнал и возвращается в ожидание. Нужен ASTM,
    /// чтобы недособранный кадр не ждал следующего соединения (FR-17); HL7/MLLP, «Авто» до
    /// решения и «Без ответов» срока не задают.
    fn poll_timeout(&mut self, _now: Now) -> Option<Duration> {
        None
    }
}

/// Автоответчик, который ничего не отвечает: профиль «Без ответов» (FR-14).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoResponder;

impl Responder for NoResponder {
    fn feed(&mut self, _data: &[u8], _now: Now) -> Vec<Action> {
        Vec::new()
    }

    fn finish(&mut self, _now: Now) -> Vec<Action> {
        Vec::new()
    }

    fn reset(&mut self) {}
}

/// Профиль автоответчика в виде, из которого строится автомат соединения (FR-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponderKind {
    /// «Авто»: протокол по первому байту первого фрагмента соединения (FR-15).
    Auto,
    /// HL7/MLLP (FR-16).
    Hl7,
    /// ASTM E1381 (FR-17).
    Astm {
        /// Отвечать `NAK` на неверную контрольную сумму вместо `ACK`.
        nak_on_bad_checksum: bool,
    },
    /// «Без ответов» (FR-18).
    None,
}

impl ResponderKind {
    /// Автоответчик одного соединения.
    ///
    /// `ids` — общий на сессию источник `MSH-10`: его получают HL7/MLLP и «Авто», чтобы
    /// номера ACK не повторялись между соединениями.
    #[must_use]
    pub fn build(self, ids: ControlIdSource) -> Box<dyn Responder + Send> {
        match self {
            Self::Auto => Box::new(Auto::new(ids)),
            Self::Hl7 => Box::new(Hl7Mllp::new(ids)),
            Self::Astm {
                nak_on_bad_checksum,
            } => Box::new(Astm::new().with_nak_on_bad_checksum(nak_on_bad_checksum)),
            Self::None => Box::new(NoResponder),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn now() -> Now {
        DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
    }

    const HL7: &[u8] = b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0001|P|2.3.1\r\
          PID|1||PATIENT^TEST\r";

    #[test]
    fn no_responder_never_answers() {
        let mut responder = NoResponder;
        let mut actions = responder.feed(b"\x0bMSH|^~\\&|ANALYZER\r\x1c\r", now());
        assert!(actions.is_empty());
        actions = responder.feed(b"garbage", now());
        assert!(actions.is_empty());
        assert!(responder.finish(now()).is_empty());
        assert_eq!(responder.poll_timeout(now()), None);
        responder.reset();
    }

    #[test]
    fn control_ids_come_from_a_shared_counter() {
        let source = ControlIdSource::new();
        assert_eq!(source.next(), 1);
        assert_eq!(source.next(), 2);
        assert_eq!(source.clone().next(), 3, "клон смотрит в тот же счётчик");
    }

    #[test]
    fn responder_event_converts_to_action() {
        let event = ResponderEvent::Info {
            message: "x".to_owned(),
            code: None,
        };
        assert_eq!(Action::from(event.clone()), Action::Event(event));
    }

    #[test]
    fn every_profile_builds_a_responder_with_its_own_behaviour() {
        let ids = ControlIdSource::new();

        let mut hl7 = ResponderKind::Hl7.build(ids.clone());
        let mut stream = vec![0x0B];
        stream.extend_from_slice(HL7);
        stream.extend_from_slice(&[0x1C, 0x0D]);
        let answers = sends(&hl7.feed(&stream, now()));
        assert_eq!(answers.len(), 1, "один ACK на сообщение");
        assert_eq!(answers[0].first(), Some(&0x0B), "ACK в рамке MLLP");

        let mut astm = ResponderKind::Astm {
            nak_on_bad_checksum: true,
        }
        .build(ids.clone());
        assert_eq!(
            sends(&astm.feed(&[astm::ENQ], now())),
            vec![vec![astm::ACK]]
        );
        let bad = b"\x020x\x03AC\r\n";
        assert_eq!(
            sends(&astm.feed(bad, now())),
            vec![vec![astm::NAK]],
            "настройка NAK доходит до автомата"
        );

        let mut astm = ResponderKind::Astm {
            nak_on_bad_checksum: false,
        }
        .build(ids.clone());
        assert_eq!(sends(&astm.feed(bad, now())), vec![vec![astm::ACK]]);

        let mut auto = ResponderKind::Auto.build(ids.clone());
        assert_eq!(
            sends(&auto.feed(&[astm::ENQ], now())),
            vec![vec![astm::ACK]]
        );

        let mut none = ResponderKind::None.build(ids);
        assert!(none.feed(&[astm::ENQ], now()).is_empty());
    }

    #[test]
    fn an_undetected_protocol_carries_a_machine_readable_code() {
        let mut auto = ResponderKind::Auto.build(ControlIdSource::new());
        let actions = auto.feed(b"?", now());
        let [Action::Event(event)] = actions.as_slice() else {
            panic!("одно событие: {actions:?}");
        };
        assert_eq!(event.code(), Some(EventCode::ProtocolUndetected));
        assert_eq!(
            event.code().map(EventCode::as_str),
            Some("protocol_undetected")
        );
        assert!(event.message().contains("protocol is not detected"));
        assert_eq!(
            EventCode::ProtocolUndetected.to_string(),
            "protocol_undetected"
        );
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
}
