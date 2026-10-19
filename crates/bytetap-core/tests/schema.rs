//! События, которые пишет ядро, проходят валидацию по JSON Schema формата (M1-AC1).

use std::path::PathBuf;

use bytetap_core::journal::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, Host, Mode, Note, Session,
    SessionEnd, Stats, read_all,
};
use serde_json::{Value, json};

fn schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/public/schema/bytetap-session-1.schema.json")
}

fn schema() -> Value {
    let path = schema_path();
    let text = std::fs::read_to_string(&path).expect("схема формата рядом с документацией");
    serde_json::from_str(&text).expect("схема — корректный JSON")
}

fn events() -> Vec<Event> {
    let mut session = Session::new(
        Mode::TcpServer,
        Autoresponder::Hl7Mllp,
        json!({"listen": "0.0.0.0:5000"}),
        Host::current(),
    );
    session.analyzer = Some("ANALYZER".to_owned());
    session.note = Some("синтетический журнал".to_owned());
    let payload = b"\x0bMSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|1|P|2.3.1\r\x1c\r";
    vec![
        Event::Session(session),
        Event::Open(bytetap_core::journal::Open {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 1000,
            conn: 1,
            peer: "192.168.1.50:49152".to_owned(),
            local: "192.168.1.10:5000".to_owned(),
        }),
        Event::Data(Data::new(
            bytetap_core::journal::now_rfc3339(),
            3000,
            1,
            Dir::AnalyzerToLis,
            false,
            payload,
        )),
        Event::Data(Data::new(
            bytetap_core::journal::now_rfc3339(),
            4000,
            1,
            Dir::LisToAnalyzer,
            true,
            b"\x06",
        )),
        Event::Close(Close {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 9_000_000,
            conn: 1,
            by: ClosedBy::Analyzer,
            reason: "eof".to_owned(),
        }),
        Event::Error(ErrorEvent {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 9_100_000,
            conn: Some(1),
            message: "неразбираемое сообщение".to_owned(),
        }),
        Event::Note(Note {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: None,
            text: "прогнали три пробы".to_owned(),
        }),
        Event::SessionEnd(SessionEnd {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 12_000_000,
            stats: Stats {
                connections: 1,
                bytes_analyzer_to_lis: 10240,
                bytes_lis_to_analyzer: 512,
            },
        }),
    ]
}

#[test]
fn every_event_type_validates_against_the_schema() {
    let schema = schema();
    let validator = jsonschema::validator_for(&schema).expect("схема проверяется движком");
    for event in events() {
        let value = serde_json::to_value(&event).expect("сериализация");
        if let Err(error) = validator.validate(&value) {
            panic!("событие {} не прошло схему: {error}\n{value}", event.kind());
        }
    }
}

#[test]
fn journal_written_by_the_core_validates_line_by_line() {
    let schema = schema();
    let validator = jsonschema::validator_for(&schema).expect("схема проверяется движком");
    let text: String = events()
        .iter()
        .map(|event| serde_json::to_string(event).expect("сериализация"))
        .collect::<Vec<_>>()
        .join("\n");
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, format!("{text}\n")).expect("запись журнала");

    let outcome = read_all(&path).expect("чтение журнала");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    assert_eq!(outcome.lines.len(), events().len());
    for line in outcome.lines {
        if let Err(error) = validator.validate(&line.value) {
            panic!("строка {} не прошла схему: {error}", line.number);
        }
    }
}

#[test]
fn schema_rejects_broken_events() {
    let schema = schema();
    let validator = jsonschema::validator_for(&schema).expect("схема проверяется движком");
    let broken = [
        // нет обязательного `b64`
        json!({"type":"data","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1,"conn":1,"dir":"analyzer_to_lis","synthetic":false,"len":1,"text":"A"}),
        // направление не из перечня
        json!({"type":"data","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1,"conn":1,"dir":"to_lis","synthetic":false,"len":1,"b64":"QQ==","text":"A"}),
        // время без микросекунд
        json!({"type":"note","t":"2026-10-19T10:15:03+03:00","text":"x"}),
        // лишнее поле
        json!({"type":"close","t":"2026-10-19T10:15:09.000000+03:00","mono_us":1,"conn":1,"by":"analyzer","reason":"eof","extra":1}),
        // неизвестный тип события
        json!({"type":"telemetry","t":"2026-10-19T10:15:03.000512+03:00"}),
    ];
    for value in broken {
        assert!(
            !validator.is_valid(&value),
            "схема не должна принимать {value}"
        );
    }
}
