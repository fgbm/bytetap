//! Интеграционные тесты сессии на loopback: эмулятор анализатора ↔ TCP-сервер (T-2,
//! M1-AC11…M1-AC14).
//!
//! Эмулятор из `bytetap-testkit` подключается к сессии как анализатор, шлёт синтетические
//! сообщения MLLP из `tests/fixtures/hl7` (целиком, побайтно, случайными кусками, с
//! переподключением) и ждёт ACK. Журнал проверяется по JSON Schema и по правилам
//! `bytetap-cli verify`: `len` и `text` выводятся из `b64`, заголовок один и первый,
//! последняя строка — `session_end` со статистикой, совпадающей с суммами строк.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use bytetap_core::journal::{Autoresponder, Dir, Event, Mode, Stats, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_testkit::emulator::{AnalyzerEmulator, EmulatorReport, Fragmentation, ack_msa};
use bytetap_testkit::fixtures_dir;
use serde_json::Value;
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Сообщения без испорченных: каждое подтверждается `AA`, `MSA-2` известен.
const SCENARIO: [(&str, &str); 3] = [
    ("analyzer_oru_r01.hl7", "MSG0001"),
    ("analyzer_qry_q02.hl7", "MSG0002"),
    ("analyzer_custom_separators.hl7", "MSG0004"),
];

fn fixtures(names: &[&str]) -> Vec<Vec<u8>> {
    names
        .iter()
        .map(|name| {
            let path = fixtures_dir().join("hl7").join(name);
            std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect()
}

async fn start(dir: &Path) -> bytetap_core::session::SessionHandle {
    Session::start(SessionConfig::new("127.0.0.1:0".parse().unwrap()).with_log_dir(dir))
        .await
        .expect("сессия стартует")
}

/// Полный обмен одного соединения: сессия, эмулятор, штатная остановка.
async fn capture(
    dir: &Path,
    messages: Vec<Vec<u8>>,
    fragmentation: Fragmentation,
) -> (PathBuf, Stats, EmulatorReport, std::net::SocketAddr) {
    let handle = start(dir).await;
    let addr = handle.local_addr();
    let report = AnalyzerEmulator::new(addr)
        .with_messages(messages)
        .with_fragmentation(fragmentation)
        .with_timeout(Duration::from_secs(10))
        .run()
        .await
        .expect("обмен с сессией");
    let path = handle.journal_path().to_path_buf();
    let stats = handle.stop().await.expect("остановка");
    (path, stats, report, addr)
}

/// Строки журнала: исходный JSON и событие.
fn load(path: &Path) -> Vec<(Value, Event)> {
    let outcome = read_all(path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    outcome
        .lines
        .into_iter()
        .map(|line| (line.value, line.event))
        .collect()
}

/// Байты одного направления одного соединения, склеенные из `b64` в порядке журнала.
fn recorded_bytes(
    lines: &[(Value, Event)],
    conn: u32,
    dir: Dir,
    synthetic: Option<bool>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (_, event) in lines {
        if let Event::Data(data) = event
            && data.conn == conn
            && data.dir == dir
            && synthetic.is_none_or(|flag| flag == data.synthetic)
        {
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 из журнала декодируется"),
            );
        }
    }
    bytes
}

/// Проверяет журнал так же, как `bytetap-cli verify`, и возвращает статистику `session_end`.
fn check_journal(lines: &[(Value, Event)], connections: u32) -> Stats {
    let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/public/schema/bytetap-session-1.schema.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&schema_path).expect("схема формата рядом с документацией"),
    )
    .expect("схема — корректный JSON");
    let validator = jsonschema::validator_for(&schema).expect("схема принимается движком");
    for (value, event) in lines {
        if let Err(error) = validator.validate(value) {
            panic!(
                "строка `{}` не прошла схему: {error}\n{value}",
                event.kind()
            );
        }
    }

    assert_eq!(
        lines.first().map(|(_, event)| event.kind()),
        Some("session"),
        "первая строка — заголовок сессии"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|(_, event)| matches!(event, Event::Session(_)))
            .count(),
        1,
        "заголовок ровно один"
    );
    assert_eq!(
        lines.last().map(|(_, event)| event.kind()),
        Some("session_end"),
        "при штатной остановке последняя строка — итоги"
    );

    let mut opened = Vec::new();
    let mut closed = Vec::new();
    let mut incoming = 0usize;
    let mut sum_in = 0u64;
    let mut sum_out = 0u64;
    for (_, event) in lines {
        match event {
            Event::Open(open) => opened.push(open.conn),
            Event::Close(close) => closed.push(close.conn),
            Event::Data(data) => {
                let payload = base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 декодируется");
                assert_eq!(
                    payload.len() as u64,
                    data.len,
                    "len равен длине декодированного b64"
                );
                assert_eq!(
                    bytetap_core::text::escape(&payload),
                    data.text,
                    "text выводится из b64"
                );
                match data.dir {
                    Dir::AnalyzerToLis => {
                        assert!(!data.synthetic, "байты анализатора не synthetic");
                        incoming += 1;
                        sum_in = sum_in.saturating_add(data.len);
                    }
                    Dir::LisToAnalyzer => {
                        assert!(
                            data.synthetic,
                            "ответы автоответчика помечены synthetic (FR-4)"
                        );
                        assert!(incoming > 0, "ответ не может идти раньше запроса");
                        sum_out = sum_out.saturating_add(data.len);
                    }
                }
            }
            Event::Session(_)
            | Event::Error(_)
            | Event::Info(_)
            | Event::Note(_)
            | Event::SessionEnd(_) => {}
        }
    }

    assert_eq!(
        opened,
        (1..=connections).collect::<Vec<_>>(),
        "conn нумеруется с 1 сквозь всю сессию"
    );
    let mut sorted = closed.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, opened, "каждому open соответствует close");

    let Event::SessionEnd(end) = &lines.last().expect("журнал не пуст").1 else {
        unreachable!("последняя строка проверена выше");
    };
    assert_eq!(end.stats.connections, connections);
    assert_eq!(
        end.stats.bytes_analyzer_to_lis, sum_in,
        "сумма по направлению"
    );
    assert_eq!(
        end.stats.bytes_lis_to_analyzer, sum_out,
        "сумма по направлению"
    );
    end.stats.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_fragmentation_mode_is_recorded_byte_for_byte() {
    let modes = [
        Fragmentation::Whole,
        Fragmentation::ByteByByte,
        Fragmentation::Random { seed: 7, max: 5 },
        Fragmentation::Random {
            seed: 2026,
            max: 64,
        },
    ];
    for fragmentation in modes {
        let dir = tempfile::tempdir().expect("временная папка");
        let messages = fixtures(&SCENARIO.map(|(name, _)| name));
        let sent: Vec<u8> = messages.concat();
        let (path, stats, report, addr) =
            capture(dir.path(), messages.clone(), fragmentation).await;

        let lines = load(&path);
        let journal_stats = check_journal(&lines, 1);
        assert_eq!(stats, journal_stats, "итоги сессии и журнала совпадают");

        assert_eq!(
            recorded_bytes(&lines, 1, Dir::AnalyzerToLis, Some(false)),
            sent,
            "склейка analyzer_to_lis по conn равна отправленному байт в байт: {fragmentation:?}"
        );
        assert_eq!(report.connections, 1);
        assert_eq!(report.acks.len(), SCENARIO.len(), "ACK на каждое сообщение");
        for (ack, (_, id)) in report.acks.iter().zip(SCENARIO) {
            assert_eq!(
                ack_msa(ack),
                Some(("AA".to_owned(), id.to_owned())),
                "{fragmentation:?}"
            );
        }
        assert_eq!(
            recorded_bytes(&lines, 1, Dir::LisToAnalyzer, Some(true)),
            report.acks.concat(),
            "в журнале те же ответы, что получил анализатор: {fragmentation:?}"
        );

        let Event::Session(header) = &lines[0].1 else {
            panic!("первая строка — заголовок");
        };
        assert_eq!(header.mode, Mode::TcpServer);
        assert_eq!(header.autoresponder, Autoresponder::Hl7Mllp);
        assert_eq!(header.settings["listen"], addr.to_string());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnecting_analyzer_gets_a_new_connection_number() {
    let dir = tempfile::tempdir().expect("временная папка");
    let handle = start(dir.path()).await;
    let mut events = handle.subscribe();
    let messages = fixtures(&[
        "analyzer_oru_r01.hl7",
        "analyzer_qry_q02.hl7",
        "analyzer_oru_r01.hl7",
    ]);
    let report = AnalyzerEmulator::new(handle.local_addr())
        .with_messages(messages.clone())
        .reconnect_between_messages()
        .run()
        .await
        .expect("обмен");

    assert_eq!(report.connections, 3, "эмулятор открыл три соединения");
    assert_eq!(report.acks.len(), 3, "ACK пришёл в каждое соединение");

    // Ждём закрытия всех трёх соединений до остановки: иначе последнее закроет сама
    // остановка, и `by` будет `bytetap`, а не `analyzer`.
    let mut closed = 0;
    while closed < 3 {
        let event = timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("close в пределах таймаута")
            .expect("канал подписчика жив");
        if let Event::Close(close) = &*event {
            closed += 1;
            assert_eq!(close.by, bytetap_core::journal::ClosedBy::Analyzer);
            assert_eq!(close.reason, "eof");
        }
    }

    let path = handle.journal_path().to_path_buf();
    let stats = handle.stop().await.expect("остановка");
    let lines = load(&path);
    let journal_stats = check_journal(&lines, 3);
    assert_eq!(stats, journal_stats);

    for (index, message) in messages.iter().enumerate() {
        let conn = u32::try_from(index + 1).unwrap();
        assert_eq!(
            recorded_bytes(&lines, conn, Dir::AnalyzerToLis, Some(false)),
            *message,
            "соединение {conn} записало только своё сообщение"
        );
    }
    for (_, event) in &lines {
        if let Event::Close(close) = event {
            assert_eq!(close.by, bytetap_core::journal::ClosedBy::Analyzer);
            assert_eq!(close.reason, "eof");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_parallel_connections_are_numbered_apart() {
    let dir = tempfile::tempdir().expect("временная папка");
    let handle = start(dir.path()).await;
    let addr = handle.local_addr();

    let first = fixtures(&["analyzer_oru_r01.hl7", "analyzer_qry_q02.hl7"]);
    let second = fixtures(&["analyzer_custom_separators.hl7"]);
    let (first_report, second_report) = tokio::join!(
        AnalyzerEmulator::new(addr)
            .with_messages(first.clone())
            .with_fragmentation(Fragmentation::Random { seed: 3, max: 7 })
            .run(),
        AnalyzerEmulator::new(addr)
            .with_messages(second.clone())
            .with_fragmentation(Fragmentation::ByteByByte)
            .run(),
    );
    let first_report = first_report.expect("первое соединение");
    let second_report = second_report.expect("второе соединение");
    assert_eq!(first_report.connections, 1);
    assert_eq!(second_report.connections, 1);

    let path = handle.journal_path().to_path_buf();
    let stats = handle.stop().await.expect("остановка");
    let lines = load(&path);
    let journal_stats = check_journal(&lines, 2);
    assert_eq!(stats, journal_stats);

    let mut recorded = vec![
        recorded_bytes(&lines, 1, Dir::AnalyzerToLis, Some(false)),
        recorded_bytes(&lines, 2, Dir::AnalyzerToLis, Some(false)),
    ];
    let mut sent = vec![first.concat(), second.concat()];
    recorded.sort();
    sent.sort();
    assert_eq!(recorded, sent, "у каждого соединения свои байты");

    // Все ответы на месте: два ACK на запросы первого соединения и один на второе.
    let mut answers = vec![
        recorded_bytes(&lines, 1, Dir::LisToAnalyzer, Some(true)),
        recorded_bytes(&lines, 2, Dir::LisToAnalyzer, Some(true)),
    ];
    answers.sort();
    let mut expected = vec![first_report.acks.concat(), second_report.acks.concat()];
    expected.sort();
    assert_eq!(answers, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_responder_records_the_exchange_and_notifies_subscribers() {
    let dir = tempfile::tempdir().expect("временная папка");
    let handle = Session::start(
        SessionConfig::new("127.0.0.1:0".parse().unwrap())
            .with_responder(Autoresponder::None)
            .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия");
    let mut events = handle.subscribe();
    let message = fixtures(&["analyzer_oru_r01.hl7"]).pop().expect("фикстура");

    let mut stream = TcpStream::connect(handle.local_addr())
        .await
        .expect("подключение анализатора");
    stream.write_all(&message).await.expect("отправка");
    stream.shutdown().await.expect("закрытие");
    drop(stream);

    let mut kinds = Vec::new();
    let mut fragments = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("событие в пределах таймаута")
            .expect("канал подписчика жив");
        kinds.push(event.kind());
        if let Event::Data(data) = &*event {
            fragments.push(data.clone());
        }
        if event.kind() == "close" {
            break;
        }
    }
    assert_eq!(kinds, ["open", "data", "close"], "лента в порядке обмена");
    assert_eq!(fragments.len(), 1);
    assert_eq!(fragments[0].dir, Dir::AnalyzerToLis);
    assert!(!fragments[0].synthetic);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&fragments[0].b64)
            .expect("b64"),
        message
    );

    let path = handle.journal_path().to_path_buf();
    let stats = handle.stop().await.expect("остановка");
    assert_eq!(
        stats.bytes_lis_to_analyzer, 0,
        "в режиме `none` ответов нет"
    );
    let lines = load(&path);
    check_journal(&lines, 1);
    assert_eq!(
        recorded_bytes(&lines, 1, Dir::LisToAnalyzer, None),
        Vec::<u8>::new(),
        "synthetic-фрагментов нет"
    );
}
