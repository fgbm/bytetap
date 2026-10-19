//! HL7 v2: разбор `MSH` входящего сообщения и построение ACK на него (FR-16, FR-18).
//!
//! Правила и примеры — `docs/reference/autoresponder.md`. Коротко:
//!
//! - разделители (`MSH-1`, `MSH-2`) берутся из входящего и копируются в ACK байтово;
//! - `MSH-3/4` и `MSH-5/6` меняются местами, `MSH-7` — время отправки, `MSH-10` — новый
//!   идентификатор, `MSH-11` и `MSH-12` копируются из входящего;
//! - `MSH-9` ACK — `ACK` или `ACK<компонент><событие из MSH-9.2>`;
//! - `MSA|AA|<MSH-10 входящего>` на разобранное сообщение, `MSA|AE|<MSH-10>` на неразбираемое;
//! - копируются только ASCII-поля: не-ASCII поле остаётся пустым и даёт [`ResponderEvent::Info`].

use chrono::{DateTime, FixedOffset};

use crate::responder::ResponderEvent;

/// Разделители ACK на неразбираемое сообщение: у такого сообщения брать нечего.
const DEFAULT_SEPARATOR: u8 = b'|';
const DEFAULT_ENCODING: &[u8] = b"^~\\&";

/// `MSH-11` ACK, если входящее сообщение его не задаёт.
///
/// Копировать нечего, а пустая процедура обработки выглядит как ошибка формата, поэтому
/// подставляется `P` (production).
const DEFAULT_PROCESSING_ID: &[u8] = b"P";

/// Результат обработки одного сообщения, уже без рамки MLLP.
pub(crate) struct Answer {
    /// Текст ACK; `None` — отвечать нечем.
    pub ack: Option<Vec<u8>>,
    /// События для журнала в порядке возникновения.
    pub events: Vec<ResponderEvent>,
}

/// Разбирает сообщение и строит ACK на него.
///
/// `next_id` — сквозной счётчик нового `MSH-10`: он не сбрасывается на новое соединение,
/// чтобы идентификаторы не повторялись в пределах сессии.
pub(crate) fn answer(message: &[u8], now: DateTime<FixedOffset>, next_id: &mut u64) -> Answer {
    let mut events = Vec::new();
    match parse(message) {
        Ok(msh) => Answer {
            ack: Some(build_ack(&msh, now, next_id, &mut events)),
            events,
        },
        Err(problem) => {
            events.push(ResponderEvent::Error {
                message: problem.message,
            });
            // Без MSH-10 подтверждать нечего: анализатор не сопоставит AE со своим сообщением.
            Answer {
                ack: problem.control_id.map(|id| build_ae(&id, now, next_id)),
                events,
            }
        }
    }
}

/// Поля `MSH` входящего, которые попадают в ACK.
struct Msh<'a> {
    field_sep: u8,
    encoding: &'a [u8],
    sending_app: &'a [u8],
    sending_facility: &'a [u8],
    receiving_app: &'a [u8],
    receiving_facility: &'a [u8],
    event: Option<&'a [u8]>,
    control_id: &'a [u8],
    processing_id: &'a [u8],
    version: &'a [u8],
}

/// Почему сообщение не разобрано и что из него всё-таки удалось взять.
struct Problem {
    message: String,
    control_id: Option<Vec<u8>>,
}

fn parse(message: &[u8]) -> Result<Msh<'_>, Problem> {
    let segments = split_segments(message);
    let Some(first) = segments.first().copied() else {
        return Err(Problem {
            message: "empty message between the MLLP frame delimiters".to_owned(),
            control_id: None,
        });
    };
    if !first.starts_with(b"MSH") {
        return Err(Problem {
            message: "message does not start with an MSH segment".to_owned(),
            control_id: control_id_of(&segments),
        });
    }
    if first.len() < 4 {
        return Err(Problem {
            message: "MSH segment is too short: MSH-1 is missing".to_owned(),
            control_id: None,
        });
    }
    let field_sep = first[3];
    if !is_separator(field_sep) {
        return Err(Problem {
            message: format!("MSH-1 is not a printable ASCII separator (0x{field_sep:02X})"),
            control_id: control_id_of(&segments),
        });
    }
    let fields: Vec<&[u8]> = first.split(|&byte| byte == field_sep).collect();
    let encoding = field(&fields, 1);
    if let Some(bad) = encoding.iter().find(|byte| !is_ascii_printable(**byte)) {
        return Err(Problem {
            message: format!("MSH-2 contains a non-ASCII byte (0x{bad:02X})"),
            control_id: control_id_of(&segments),
        });
    }
    Ok(Msh {
        field_sep,
        encoding,
        sending_app: field(&fields, 2),
        sending_facility: field(&fields, 3),
        receiving_app: field(&fields, 4),
        receiving_facility: field(&fields, 5),
        event: trigger_event(field(&fields, 8), encoding),
        control_id: field(&fields, 9),
        processing_id: field(&fields, 10),
        version: field(&fields, 11),
    })
}

/// Делит сообщение на сегменты по `CR`; `LF` и `CRLF` допускаются как разделитель (FR-16).
fn split_segments(message: &[u8]) -> Vec<&[u8]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (index, &byte) in message.iter().enumerate() {
        if byte == b'\r' || byte == b'\n' {
            if index > start {
                segments.push(&message[start..index]);
            }
            start = index + 1;
        }
    }
    if start < message.len() {
        segments.push(&message[start..]);
    }
    segments
}

/// Поле сегмента `MSH` по номеру; `MSH-1` — сам разделитель, поэтому первое поле после `MSH` — MSH-2.
fn field<'a>(fields: &[&'a [u8]], number: usize) -> &'a [u8] {
    fields.get(number).copied().unwrap_or_default()
}

/// Второй компонент `MSH-9` — событие-триггер; без компонентного разделителя его нет.
fn trigger_event<'a>(message_type: &'a [u8], encoding: &[u8]) -> Option<&'a [u8]> {
    let component = *encoding.first()?;
    message_type
        .split(|&byte| byte == component)
        .nth(1)
        .filter(|event| !event.is_empty())
}

/// Printable ASCII без пробела: разделителем может быть только такой байт.
fn is_separator(byte: u8) -> bool {
    (0x21..=0x7E).contains(&byte)
}

/// Печатный ASCII: только такие байты переносятся в ACK (FR-16, кодировка ACK — ASCII).
fn is_ascii_printable(byte: u8) -> bool {
    (0x20..=0x7E).contains(&byte)
}

/// `MSH-10`, извлекаемый из любого `MSH`-сегмента сообщения.
///
/// Нужен для `MSA|AE`: подтверждение без идентификатора анализатор не сопоставит со своим
/// сообщением, поэтому неразбираемое сообщение подтверждается только при извлекаемом `MSH-10`.
fn control_id_of(segments: &[&[u8]]) -> Option<Vec<u8>> {
    segments
        .iter()
        .filter(|segment| segment.starts_with(b"MSH") && segment.len() > 3)
        .find_map(|segment| {
            let field_sep = segment[3];
            if !is_separator(field_sep) {
                return None;
            }
            let fields: Vec<&[u8]> = segment.split(|&byte| byte == field_sep).collect();
            let id = field(&fields, 9);
            let ascii = !id.is_empty() && id.iter().all(|byte| is_ascii_printable(*byte));
            ascii.then(|| id.to_vec())
        })
}

/// Строит ACK на разобранное сообщение (FR-16).
fn build_ack(
    msh: &Msh<'_>,
    now: DateTime<FixedOffset>,
    next_id: &mut u64,
    events: &mut Vec<ResponderEvent>,
) -> Vec<u8> {
    let sep = msh.field_sep;
    let sending_app = copyable(msh.sending_app, "MSH-3", events);
    let sending_facility = copyable(msh.sending_facility, "MSH-4", events);
    let receiving_app = copyable(msh.receiving_app, "MSH-5", events);
    let receiving_facility = copyable(msh.receiving_facility, "MSH-6", events);
    let control_id = copyable(msh.control_id, "MSH-10", events);
    let version = copyable(msh.version, "MSH-12", events);
    let processing_id = if msh.processing_id.is_empty() {
        DEFAULT_PROCESSING_ID
    } else {
        copyable(msh.processing_id, "MSH-11", events)
    };

    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(b"MSH");
    out.push(sep);
    out.extend_from_slice(msh.encoding);
    // MSH-3/4 и MSH-5/6 меняются местами.
    push_field(&mut out, sep, receiving_app);
    push_field(&mut out, sep, receiving_facility);
    push_field(&mut out, sep, sending_app);
    push_field(&mut out, sep, sending_facility);
    push_field(&mut out, sep, timestamp(now).as_bytes());
    push_field(&mut out, sep, b"");
    push_field(
        &mut out,
        sep,
        &message_type(msh.event, msh.encoding, events),
    );
    push_field(&mut out, sep, next_control_id(now, next_id).as_bytes());
    push_field(&mut out, sep, processing_id);
    push_field(&mut out, sep, version);
    out.extend_from_slice(b"\rMSA");
    push_field(&mut out, sep, b"AA");
    push_field(&mut out, sep, control_id);
    out.push(b'\r');
    out
}

/// Строит ACK на неразбираемое сообщение: `MSA|AE` и разделители по умолчанию.
fn build_ae(control_id: &[u8], now: DateTime<FixedOffset>, next_id: &mut u64) -> Vec<u8> {
    let sep = DEFAULT_SEPARATOR;
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(b"MSH");
    out.push(sep);
    out.extend_from_slice(DEFAULT_ENCODING);
    // MSH-3…MSH-6 взять неоткуда.
    for _ in 0..4 {
        push_field(&mut out, sep, b"");
    }
    push_field(&mut out, sep, timestamp(now).as_bytes());
    push_field(&mut out, sep, b"");
    push_field(&mut out, sep, b"ACK");
    push_field(&mut out, sep, next_control_id(now, next_id).as_bytes());
    push_field(&mut out, sep, DEFAULT_PROCESSING_ID);
    push_field(&mut out, sep, b"");
    out.extend_from_slice(b"\rMSA");
    push_field(&mut out, sep, b"AE");
    push_field(&mut out, sep, control_id);
    out.push(b'\r');
    out
}

/// Поле ACK: разделитель и значение.
fn push_field(out: &mut Vec<u8>, sep: u8, value: &[u8]) {
    out.push(sep);
    out.extend_from_slice(value);
}

/// Значение поля, которое можно перенести байтово; иначе пустое поле и [`ResponderEvent::Info`].
fn copyable<'a>(value: &'a [u8], name: &str, events: &mut Vec<ResponderEvent>) -> &'a [u8] {
    if value.iter().all(|byte| is_ascii_printable(*byte)) {
        value
    } else {
        events.push(ResponderEvent::Info {
            message: format!("{name} is not ASCII: the field is left empty in the ACK"),
        });
        b""
    }
}

/// Поле `MSH-9` ACK: `ACK` или `ACK<компонент><событие>`.
fn message_type(
    event: Option<&[u8]>,
    encoding: &[u8],
    events: &mut Vec<ResponderEvent>,
) -> Vec<u8> {
    let mut message_type = Vec::from(*b"ACK");
    let Some(event) = event else {
        return message_type;
    };
    if !event.iter().all(|byte| is_ascii_printable(*byte)) {
        events.push(ResponderEvent::Info {
            message: "MSH-9.2 is not ASCII: the ACK message type has no event".to_owned(),
        });
        return message_type;
    }
    // Компонентного разделителя нет — событие некуда поставить: остаётся `ACK`.
    if let Some(&component) = encoding.first() {
        message_type.push(component);
        message_type.extend_from_slice(event);
    }
    message_type
}

/// Время отправки в `MSH-7`: `YYYYMMDDHHMMSS±ZZZZ`.
fn timestamp(now: DateTime<FixedOffset>) -> String {
    now.format("%Y%m%d%H%M%S%z").to_string()
}

/// Новый `MSH-10` ACK: `BT`, время отправки и сквозной счётчик — только печатный ASCII.
///
/// Счётчик не сбрасывается на новое соединение, а время отсекает совпадения между соединениями.
fn next_control_id(now: DateTime<FixedOffset>, next_id: &mut u64) -> String {
    *next_id += 1;
    format!("BT{}{:04}", now.format("%Y%m%d%H%M%S"), *next_id)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    const MSG: &[u8] = b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0001|P|2.3.1\r\
          PID|1||PATIENT^TEST\r";

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
    }

    fn answer_of(message: &[u8]) -> Answer {
        let mut next_id = 0;
        answer(message, now(), &mut next_id)
    }

    fn ack_of(message: &[u8]) -> (String, Vec<ResponderEvent>) {
        let answer = answer_of(message);
        let ack = String::from_utf8(answer.ack.expect("ответ есть")).expect("ACK — ASCII");
        (ack, answer.events)
    }

    #[test]
    fn standard_separators_are_copied_and_apps_are_swapped() {
        let (ack, events) = ack_of(MSG);
        assert_eq!(
            ack,
            "MSH|^~\\&|LIS|LAB|ANALYZER|LAB|20261019101530+0300||ACK^R01|BT202610191015300001|P|2.3.1\r\
             MSA|AA|MSG0001\r"
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn non_standard_separators_come_from_the_incoming_message() {
        let (ack, events) = ack_of(
            b"MSH#$%\\*#ANALYZER#LAB#LIS#LAB#20261019101500##ORU$R01#MSG0002#P#2.4\r\
              PID#1##PATIENT$TEST\r",
        );
        assert_eq!(
            ack,
            "MSH#$%\\*#LIS#LAB#ANALYZER#LAB#20261019101530+0300##ACK$R01#BT202610191015300001#P#2.4\r\
             MSA#AA#MSG0002\r"
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn subcomponent_separator_is_copied_as_is() {
        let (ack, events) =
            ack_of(b"MSH|^~\\#|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0003|P|2.3.1\r");
        assert_eq!(
            ack,
            "MSH|^~\\#|LIS|LAB|ANALYZER|LAB|20261019101530+0300||ACK^R01|BT202610191015300001|P|2.3.1\r\
             MSA|AA|MSG0003\r"
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn missing_fields_are_left_empty_and_processing_id_defaults_to_p() {
        let (ack, events) = ack_of(b"MSH|^~\\&|||||20261019101500||ORU^R01|MSG0004\r");
        assert_eq!(
            ack,
            "MSH|^~\\&|||||20261019101530+0300||ACK^R01|BT202610191015300001|P|\rMSA|AA|MSG0004\r"
        );
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn message_type_without_event_has_no_component_separator() {
        let (ack, _) =
            ack_of(b"MSH|^~\\&|LIS|LAB|ANALYZER|LAB|20261019101500||ACK|MSG0005|P|2.3.1\r");
        assert_eq!(
            ack,
            "MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101530+0300||ACK|BT202610191015300001|P|2.3.1\r\
             MSA|AA|MSG0005\r"
        );
    }

    #[test]
    fn non_ascii_copied_field_is_left_empty_and_reported() {
        let (ack, events) =
            ack_of(b"MSH|^~\\&|ANALYZ\xe9R|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0006|P|2.3.1\r");
        assert_eq!(
            ack,
            "MSH|^~\\&|LIS|LAB||LAB|20261019101530+0300||ACK^R01|BT202610191015300001|P|2.3.1\r\
             MSA|AA|MSG0006\r"
        );
        assert!(ack.is_ascii(), "кодировка ACK — ASCII");
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            matches!(&events[0], ResponderEvent::Info { message } if message.contains("MSH-3"))
        );
    }

    #[test]
    fn non_ascii_control_id_gives_an_empty_msa2() {
        let (ack, events) =
            ack_of(b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG\xe90007|P|2.3.1\r");
        assert!(ack.ends_with("MSA|AA|\r"), "{ack:?}");
        assert!(ack.is_ascii());
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            matches!(&events[0], ResponderEvent::Info { message } if message.contains("MSH-10"))
        );
    }

    #[test]
    fn segment_separators_may_be_lf_or_crlf() {
        let lf = b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0007|P|2.3.1\nPID|1||PATIENT^TEST\n";
        let crlf = b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0007|P|2.3.1\r\nPID|1||PATIENT^TEST\r\n";
        let (lf_ack, _) = ack_of(lf);
        let (crlf_ack, _) = ack_of(crlf);
        assert_eq!(lf_ack, crlf_ack);
        assert!(lf_ack.starts_with("MSH|^~\\&|LIS|LAB|ANALYZER|LAB|"));
        assert!(lf_ack.ends_with("MSA|AA|MSG0007\r"));
    }

    #[test]
    fn unparsed_message_with_a_control_id_is_nacked_with_ae() {
        let (ack, events) = ack_of(
            b"OBX|1|NM|GLU||5.4\r\
              MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0008|P|2.3.1\r",
        );
        assert_eq!(
            ack,
            "MSH|^~\\&|||||20261019101530+0300||ACK|BT202610191015300001|P|\rMSA|AE|MSG0008\r"
        );
        assert!(matches!(&events[0], ResponderEvent::Error { message } if message.contains("MSH")));
    }

    #[test]
    fn unparsed_message_without_a_control_id_is_not_answered() {
        let answer = answer_of(b"PID|1||PATIENT^TEST\r");
        assert!(answer.ack.is_none());
        assert!(matches!(&answer.events[..], [ResponderEvent::Error { .. }]));
    }

    #[test]
    fn broken_separators_are_an_error_with_a_nack_when_the_id_survives() {
        // MSH-2 с управляющим байтом: ACK строится по разделителям по умолчанию.
        let (ack, events) =
            ack_of(b"MSH|^~\\\x01&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0009|P|2.3.1\r");
        assert_eq!(
            ack,
            "MSH|^~\\&|||||20261019101530+0300||ACK|BT202610191015300001|P|\rMSA|AE|MSG0009\r"
        );
        assert!(
            matches!(&events[0], ResponderEvent::Error { message } if message.contains("MSH-2"))
        );
    }

    #[test]
    fn unusable_field_separator_leaves_nothing_to_answer() {
        let answer =
            answer_of(b"MSH\x01^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|X1|P|2.3.1\r");
        assert!(answer.ack.is_none());
        assert!(
            matches!(&answer.events[0], ResponderEvent::Error { message } if message.contains("MSH-1"))
        );
    }

    #[test]
    fn empty_message_is_an_error() {
        let answer = answer_of(b"");
        assert!(answer.ack.is_none());
        assert!(matches!(&answer.events[0], ResponderEvent::Error { .. }));
    }

    #[test]
    fn control_ids_are_unique_within_the_counter() {
        let mut next_id = 0;
        let now = now();
        let first = answer(MSG, now, &mut next_id).ack.expect("ACK");
        let second = answer(MSG, now, &mut next_id).ack.expect("ACK");
        let first = String::from_utf8(first).expect("ASCII");
        let second = String::from_utf8(second).expect("ASCII");
        assert!(first.contains("|BT202610191015300001|"), "{first}");
        assert!(second.contains("|BT202610191015300002|"), "{second}");
    }
}
