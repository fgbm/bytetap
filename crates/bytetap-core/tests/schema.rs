//! События, которые пишет ядро, проходят валидацию по JSON Schema формата (M1-AC1, M4-AC9).
//!
//! Новые журналы пишутся в формате `/2` и проверяются схемой `/2`; журналы `/1` читаются
//! и проверяются своей схемой, которая не менялась.

use std::path::PathBuf;

use bytetap_core::journal::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, Host, InfoEvent, Link, LinkRole,
    Mode, Note, Open, Session, SessionEnd, Stats, read_all,
};
use serde_json::{Value, json};

fn schema_path(version: u32) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../docs/public/schema/bytetap-session-{version}.schema.json"
    ))
}

fn schema(version: u32) -> Value {
    let path = schema_path(version);
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
        Event::Open(Open {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 1000,
            conn: 1,
            peer: "192.168.1.50:49152".to_owned(),
            local: "192.168.1.10:5000".to_owned(),
            link: None,
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
            code: Some("connect_failed".to_owned()),
            message: "connect failed: connection refused".to_owned(),
        }),
        Event::Info(InfoEvent {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 9_200_000,
            conn: Some(1),
            code: None,
            message: "4 bytes outside the MLLP frame ignored".to_owned(),
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
    let schema = schema(2);
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
    let schema = schema(2);
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
fn tcp_client_and_serial_headers_validate_against_the_schema() {
    let schema = schema(2);
    let validator = jsonschema::validator_for(&schema).expect("схема проверяется движком");
    for (mode, settings) in [
        (
            Mode::TcpClient,
            json!({"connect": "192.168.1.50:5000", "reconnect_sec": 5}),
        ),
        (
            Mode::Serial,
            json!({
                "port": "COM5",
                "baud": 9600,
                "format": "8N1",
                "flow": "none",
                "dtr": true,
                "rts": true
            }),
        ),
    ] {
        let header = Session::new(mode, Autoresponder::Auto, settings, Host::current());
        let value = serde_json::to_value(Event::Session(header)).expect("сериализация");
        if let Err(error) = validator.validate(&value) {
            panic!("заголовок {mode} не прошёл схему: {error}\n{value}");
        }
    }
}

#[test]
fn a_proxy_pair_validates_against_the_schema() {
    let schema = schema(2);
    let validator = jsonschema::validator_for(&schema).expect("схема проверяется движком");
    let header = Session::new(
        Mode::Proxy,
        Autoresponder::None,
        json!({"listen": "127.0.0.1:50025", "target": "127.0.0.1:5000", "role": "analyzer_server"}),
        Host::current(),
    );
    let events = [
        Event::Session(header),
        Event::Open(Open {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 1000,
            conn: 1,
            peer: "192.168.1.60:49152".to_owned(),
            local: "127.0.0.1:50025".to_owned(),
            link: Some(Link {
                role: LinkRole::Analyzer,
                peer: "127.0.0.1:5000".to_owned(),
                local: "127.0.0.1:50026".to_owned(),
            }),
        }),
        Event::Data(Data::new(
            bytetap_core::journal::now_rfc3339(),
            2000,
            1,
            Dir::LisToAnalyzer,
            false,
            b"\x05",
        )),
        Event::Close(Close {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 3000,
            conn: 1,
            by: ClosedBy::Lis,
            reason: "eof".to_owned(),
        }),
        Event::Error(ErrorEvent {
            t: bytetap_core::journal::now_rfc3339(),
            mono_us: 4000,
            conn: None,
            code: Some("proxy_connect_timeout".to_owned()),
            message: "connect failed: connect to 10.0.0.5:5000 timed out after 10 s".to_owned(),
        }),
    ];
    for event in events {
        let value = serde_json::to_value(&event).expect("сериализация");
        if let Err(error) = validator.validate(&value) {
            panic!("событие {} не прошло схему: {error}\n{value}", event.kind());
        }
    }
}

#[test]
fn the_v1_fixture_reads_and_validates_against_the_v1_schema() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/journal/minimal.jsonl");
    let outcome = read_all(&path).expect("фикстура /1 читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(
        header.format, "bytetap-session/1",
        "фикстура старого формата"
    );

    let schema = schema(1);
    let validator = jsonschema::validator_for(&schema).expect("схема /1 проверяется движком");
    for line in &outcome.lines {
        if let Err(error) = validator.validate(&line.value) {
            panic!("строка {} не прошла схему /1: {error}", line.number);
        }
    }
}

#[test]
fn schema_rejects_broken_events() {
    let schema = schema(2);
    let validator = jsonschema::validator_for(&schema).expect("схема проверяется движком");
    let broken = [
        // нет обязательного `b64`
        json!({"type":"data","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1,"conn":1,"dir":"analyzer_to_lis","synthetic":false,"len":1,"text":"A"}),
        // направление не из перечня
        json!({"type":"data","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1,"conn":1,"dir":"to_lis","synthetic":false,"len":1,"b64":"QQ==","text":"A"}),
        // время без микросекунд
        json!({"type":"note","t":"2026-10-19T10:15:03+03:00","text":"x"}),
        // `text` не по каноническим правилам: `<xHH>` в нижнем регистре
        json!({"type":"data","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1,"conn":1,"dir":"analyzer_to_lis","synthetic":false,"len":1,"b64":"QQ==","text":"<x6a>"}),
        // `text` с сырым `<`, который обязан быть `<x3C>`
        json!({"type":"data","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1,"conn":1,"dir":"analyzer_to_lis","synthetic":false,"len":1,"b64":"QQ==","text":"a<b"}),
        // лишнее поле
        json!({"type":"close","t":"2026-10-19T10:15:09.000000+03:00","mono_us":1,"conn":1,"by":"analyzer","reason":"eof","extra":1}),
        // у `info` нет обязательного `message`
        json!({"type":"info","t":"2026-10-19T10:15:03.000512+03:00","mono_us":1}),
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
