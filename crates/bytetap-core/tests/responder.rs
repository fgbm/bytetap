//! Автоответчик HL7/MLLP: синтетические фикстуры и proptest на разбиение потока
//! (M1-AC8, M1-AC9, M1-AC10, T-1).
//!
//! Фикстуры лежат в `tests/fixtures/hl7/` и содержат только вымышленные данные
//! (`ANALYZER`, `LIS`, `PATIENT^TEST`).

use bytetap_core::responder::{Action, ControlIdSource, Hl7Mllp, Responder, ResponderEvent};
use bytetap_testkit::fixtures_dir;
use chrono::{DateTime, FixedOffset};
use proptest::prelude::*;

const SB: u8 = 0x0B;
const EB: u8 = 0x1C;
const CR: u8 = 0x0D;

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("hl7").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn framed(message: &[u8]) -> Vec<u8> {
    let mut stream = Vec::with_capacity(message.len() + 3);
    stream.push(SB);
    stream.extend_from_slice(message);
    stream.push(EB);
    stream.push(CR);
    stream
}

fn feed(stream: &[u8]) -> Vec<Action> {
    Hl7Mllp::new(ControlIdSource::new()).feed(stream, now())
}

fn acks(actions: &[Action]) -> Vec<Vec<u8>> {
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

/// Тело ACK из рамки MLLP.
fn ack_body(ack: &[u8]) -> &[u8] {
    assert_eq!(ack.first(), Some(&SB), "ответ в рамке MLLP");
    assert_eq!(
        ack.get(ack.len() - 2..),
        Some([EB, CR].as_slice()),
        "рамка закрыта 0x1C 0x0D"
    );
    &ack[1..ack.len() - 2]
}

/// Код `MSA-1` и идентификатор `MSA-2` из тела ACK.
fn msa(ack: &[u8]) -> (String, String) {
    let body = ack_body(ack);
    // Разделитель полей ACK — `MSH-1` самого ответа: он скопирован из входящего сообщения.
    let field_sep = *body.get(3).expect("MSH-1 в ACK");
    let segment = body
        .split(|&byte| byte == b'\r')
        .find(|segment| segment.starts_with(b"MSA"))
        .expect("сегмент MSA");
    let fields: Vec<&[u8]> = segment.split(|&byte| byte == field_sep).collect();
    assert_eq!(fields.len(), 3, "MSA-1 и MSA-2 на месте");
    (
        String::from_utf8(fields[1].to_vec()).expect("MSA-1 — ASCII"),
        String::from_utf8(fields[2].to_vec()).expect("MSA-2 — ASCII"),
    )
}

fn ack_text(ack: &[u8]) -> String {
    String::from_utf8(ack_body(ack).to_vec()).expect("ACK — ASCII")
}

#[test]
fn analyzer_message_is_acknowledged_field_by_field() {
    let actions = feed(&fixture("analyzer_oru_r01.hl7"));
    let acks = acks(&actions);
    assert_eq!(acks.len(), 1);
    assert!(events(&actions).is_empty(), "{actions:?}");
    assert_eq!(
        ack_text(&acks[0]),
        "MSH|^~\\&|LIS|LAB|ANALYZER|LAB|20261019101530+0300||ACK^R01|BT261019101530000001|P|2.3.1\r\
         MSA|AA|MSG0001\r"
    );
}

#[test]
fn an_incoming_ack_is_not_acknowledged() {
    // FR-16: ответ на подтверждение завёл бы петлю ACK↔ACK.
    let actions = feed(&fixture("lis_ack_aa.hl7"));
    assert!(acks(&actions).is_empty(), "ответа нет: {actions:?}");
    let events = events(&actions);
    assert_eq!(events.len(), 1, "{actions:?}");
    assert!(
        matches!(events[0], ResponderEvent::Info { message, .. } if message.contains("incoming ACK")),
        "{events:?}"
    );
}

#[test]
fn custom_separators_are_used_in_the_ack() {
    let actions = feed(&fixture("analyzer_custom_separators.hl7"));
    let acks = acks(&actions);
    assert_eq!(acks.len(), 1);
    let text = ack_text(&acks[0]);
    assert!(
        text.starts_with("MSH#$%\\*#LIS#LAB#ANALYZER#LAB#"),
        "{text}"
    );
    assert!(text.contains("#ACK$R01#BT"), "{text}");
    assert!(text.ends_with("\rMSA#AA#MSG0004\r"), "{text}");
    assert!(text.is_ascii());
}

#[test]
fn queries_are_acknowledged_without_a_dsr() {
    // FR-18: QRY^Q02 подтверждается только на транспортном уровне.
    let actions = feed(&fixture("analyzer_qry_q02.hl7"));
    let acks = acks(&actions);
    assert_eq!(acks.len(), 1, "один ACK на запрос");
    let text = ack_text(&acks[0]);
    assert!(text.contains("||ACK^Q02|"), "{text}");
    assert_eq!(msa(&acks[0]), ("AA".to_owned(), "MSG0002".to_owned()));
    assert!(!text.contains("DSR"), "DSR не формируется: {text}");
    assert!(
        !text.contains("QRD"),
        "на запрос отвечает только ACK: {text}"
    );
}

#[test]
fn a_message_without_msh_is_an_error_without_a_response() {
    let actions = feed(&fixture("analyzer_broken.hl7"));
    assert!(acks(&actions).is_empty(), "подтверждать нечего");
    let events = events(&actions);
    assert_eq!(events.len(), 1, "{actions:?}");
    assert!(matches!(events[0], ResponderEvent::Error { .. }));
}

#[test]
fn a_message_with_msh_not_first_is_nacked_when_the_id_is_extractable() {
    let actions = feed(&fixture("analyzer_msh_not_first.hl7"));
    let acks = acks(&actions);
    assert_eq!(acks.len(), 1);
    assert_eq!(msa(&acks[0]), ("AE".to_owned(), "MSG0006".to_owned()));
    assert!(matches!(
        &events(&actions)[..],
        [ResponderEvent::Error { .. }]
    ));
}

#[test]
fn garbage_between_frames_is_reported_but_does_not_change_the_answers() {
    let first = fixture("analyzer_oru_r01.hl7");
    let second = fixture("analyzer_qry_q02.hl7");
    let mut stream = b"noise before".to_vec();
    stream.extend_from_slice(&first);
    stream.extend_from_slice(b"\x00junk between");
    stream.extend_from_slice(&second);

    let actions = feed(&stream);
    let acks = acks(&actions);
    assert_eq!(acks.len(), 2);
    assert_eq!(msa(&acks[0]).1, "MSG0001");
    assert_eq!(msa(&acks[1]).1, "MSG0002");
    // Два блока мусора — два Info; хвост после последней рамки блока не заканчивает.
    assert_eq!(events(&actions).len(), 2, "{actions:?}");
    for event in events(&actions) {
        assert!(matches!(event, ResponderEvent::Info { .. }), "{event:?}");
    }
}

/// Пул сообщений для proptest: стандартные и нестандартные разделители, запрос.
const POOL: &[(&[u8], &str)] = &[
    (
        b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0001|P|2.3.1\r\
          PID|1||PATIENT^TEST\r",
        "MSG0001",
    ),
    (
        b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101600||QRY^Q02|MSG0002|P|2.3.1\r\
          QRD|20261019101600|R|I|Q1|||1^RD|PATIENT^TEST|RES\r",
        "MSG0002",
    ),
    (
        b"MSH#$%\\*#ANALYZER#LAB#LIS#LAB#20261019101700##ORU$R01#MSG0004#P#2.4\r\
          PID#1##PATIENT$TEST\r",
        "MSG0004",
    ),
];

/// Байт потока: байты рамки и разделители попадаются чаще случайных — на них живёт автомат.
fn stream_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        3 => Just(SB),
        3 => Just(EB),
        4 => Just(CR),
        2 => Just(b'\n'),
        4 => proptest::sample::select(b"MSH|^~\\&$%*".to_vec()),
        20 => any::<u8>(),
    ]
}

/// Кусок потока: мусор из случайных байтов или рамка с сообщением пула, иногда испорченным.
fn piece() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => proptest::collection::vec(stream_byte(), 0..=48),
        5 => (0usize..POOL.len(), proptest::option::of((0usize..32, any::<u8>()))).prop_map(
            |(pick, damage)| {
                let mut message = POOL[pick].0.to_vec();
                if let Some((at, byte)) = damage {
                    if at < message.len() {
                        message[at] = byte;
                    } else {
                        message.push(byte);
                    }
                }
                framed(&message)
            },
        ),
    ]
}

proptest! {
    /// Поток из нескольких сообщений при любом разбиении на фрагменты даёт те же действия,
    /// что поданный целиком: столько же ACK, те же MSA-2 (M1-AC8, T-1).
    #[test]
    fn any_fragmentation_gives_the_same_acks(
        picks in proptest::collection::vec(0usize..POOL.len(), 1..=4),
        cuts in proptest::collection::vec(1usize..=17, 1..=40),
    ) {
        let mut stream = Vec::new();
        for (index, pick) in picks.iter().enumerate() {
            if index > 0 {
                stream.extend_from_slice(b"junk between frames");
            }
            stream.extend_from_slice(&framed(POOL[*pick].0));
        }

        let mut whole = Hl7Mllp::new(ControlIdSource::new());
        let expected = whole.feed(&stream, now());

        let mut piecewise = Hl7Mllp::new(ControlIdSource::new());
        let mut actual = Vec::new();
        let mut offset = 0;
        let mut sizes = cuts.iter().cycle();
        while offset < stream.len() {
            let size = *sizes.next().expect("размеры фрагментов не кончаются");
            let end = (offset + size).min(stream.len());
            actual.extend(piecewise.feed(&stream[offset..end], now()));
            offset = end;
        }

        prop_assert_eq!(&actual, &expected, "разбиение не меняет результат");

        let acks = acks(&actual);
        prop_assert_eq!(acks.len(), picks.len(), "один ACK на сообщение");
        let ids: Vec<String> = acks.iter().map(|ack| msa(ack).1).collect();
        let wanted: Vec<String> = picks.iter().map(|pick| POOL[*pick].1.to_owned()).collect();
        prop_assert_eq!(ids, wanted);
        prop_assert!(acks.iter().all(|ack| msa(ack).0 == "AA"));
    }

    /// Любые байты, испорченные сообщения и любая нарезка: автомат не паникует, разбиение
    /// не меняет действий, а каждый ответ — ASCII `MSH` в рамке MLLP (M1-AC8, T-1).
    #[test]
    fn any_bytes_and_fragmentation_keep_the_invariants(
        pieces in proptest::collection::vec(piece(), 0..=6),
        cuts in proptest::collection::vec(1usize..=24, 1..=64),
        limit in 8usize..=1024,
    ) {
        let stream: Vec<u8> = pieces.concat();

        let mut whole = Hl7Mllp::with_frame_limit(limit, ControlIdSource::new());
        let mut expected = whole.feed(&stream, now());
        expected.extend(whole.finish(now()));

        let mut piecewise = Hl7Mllp::with_frame_limit(limit, ControlIdSource::new());
        let mut actual = Vec::new();
        let mut offset = 0;
        let mut sizes = cuts.iter().cycle();
        while offset < stream.len() {
            let size = *sizes.next().expect("размеры фрагментов не кончаются");
            let end = (offset + size).min(stream.len());
            actual.extend(piecewise.feed(&stream[offset..end], now()));
            offset = end;
        }
        actual.extend(piecewise.finish(now()));

        prop_assert_eq!(&actual, &expected, "разбиение не меняет результат");
        for action in &actual {
            if let Action::Send(bytes) = action {
                prop_assert_eq!(bytes.first(), Some(&SB), "рамка MLLP начинается с 0x0B");
                prop_assert!(bytes.len() > 3, "в рамке есть тело и закрывающая пара");
                prop_assert_eq!(
                    bytes.get(bytes.len() - 2..),
                    Some([EB, CR].as_slice()),
                    "рамка закрыта 0x1C 0x0D"
                );
                let body = &bytes[1..bytes.len() - 2];
                prop_assert!(body.starts_with(b"MSH"), "ACK начинается с MSH");
                prop_assert!(
                    body.iter()
                        .all(|&byte| (0x20..=0x7E).contains(&byte) || byte == CR),
                    "тело ACK — печатный ASCII и CR"
                );
            }
        }
    }

    /// Мусор вне рамки: любой байт, кроме `0x0B`, не меняет ответ, а блок даёт ровно одно `Info`.
    #[test]
    fn garbage_never_changes_the_ack(
        garbage in proptest::collection::vec(
            any::<u8>().prop_filter("0x0B начинает рамку", |byte| *byte != SB),
            1..=64,
        ),
    ) {
        let clean = feed(&framed(POOL[0].0));

        let mut stream = garbage.clone();
        stream.extend_from_slice(&framed(POOL[0].0));
        let actions = feed(&stream);

        prop_assert_eq!(acks(&actions), acks(&clean), "мусор не меняет ACK");
        let events = events(&actions);
        prop_assert_eq!(events.len(), 1, "одно Info на блок мусора: {:?}", events);
        prop_assert!(matches!(events[0], ResponderEvent::Info { .. }), "{events:?}");
    }
}
