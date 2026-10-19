//! ASTM E1381 и «Авто»: синтетические фикстуры, proptest на разбиение потока и обмен с
//! эмулятором анализатора на loopback (M3-AC7, M3-AC8, M3-AC9, M3-AC10, FR-17, FR-18, T-1).
//!
//! Фикстуры лежат в `tests/fixtures/astm/` и содержат только вымышленные данные
//! (`ANALYZER`, `LIS`, `PATIENT^TEST`).

use bytetap_core::responder::{Action, Astm, Auto, ControlIdSource, Responder, ResponderEvent};
use bytetap_testkit::astm::{
    ACK, AstmEmulator, DEFAULT_CHUNK, ENQ, EOT, ETB, ETX, NAK, STX, next_number, record_frames,
};
use bytetap_testkit::emulator::Fragmentation;
use bytetap_testkit::fixtures_dir;
use chrono::{DateTime, FixedOffset};
use proptest::prelude::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("astm").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Сообщение HL7 для профиля «Авто», выбравшего MLLP.
fn hl7_fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("hl7").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Кадр с верной суммой: то, что должен принять автоответчик.
///
/// `number` — ASCII-цифра номера кадра: `'1'` — первый кадр после `ENQ`.
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

#[test]
fn a_patient_record_is_acknowledged_without_events() {
    let mut responder = Astm::new();
    let actions = responder.feed(
        &framed(b'1', &fixture("analyzer_patient_result.astm"), ETX),
        now(),
    );
    assert_eq!(sends(&actions), vec![vec![ACK]]);
    assert!(events(&actions).is_empty(), "{actions:?}");
}

#[test]
fn a_query_record_gets_a_single_ack_and_nothing_else() {
    // FR-18, M3-AC8: запрос подтверждается только на транспортном уровне.
    let mut responder = Astm::new();
    let actions = responder.feed(&framed(b'1', &fixture("analyzer_query.astm"), ETX), now());
    assert_eq!(sends(&actions), vec![vec![ACK]]);
    assert!(events(&actions).is_empty(), "{actions:?}");
}

#[test]
fn a_long_record_arrives_as_etb_continuations() {
    let record = fixture("analyzer_long_result.astm");
    let frames = record_frames(&record, DEFAULT_CHUNK, b'1', false);
    assert!(frames.len() > 1, "запись не влезает в один кадр");

    let mut responder = Astm::new();
    let mut actions = Vec::new();
    for frame in &frames {
        actions.extend(responder.feed(frame, now()));
    }
    assert_eq!(sends(&actions), vec![vec![ACK]; frames.len()]);
    assert!(
        events(&actions).is_empty(),
        "продолжения не нарушают нумерацию: {actions:?}"
    );
}

/// Поток эмулятора целиком: `ENQ`, кадры записей по кругу, `EOT`.
fn expected_stream(records: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = vec![ENQ];
    // Первый кадр после `ENQ` — `'1'`, дальше номер идёт по кругу ASCII-цифрами.
    let mut number = b'1';
    for record in records {
        let frames = record_frames(record, DEFAULT_CHUNK, number, false);
        for _ in &frames {
            number = next_number(number);
        }
        for frame in frames {
            stream.extend_from_slice(&frame);
        }
    }
    stream.push(EOT);
    stream
}

/// Что автоответчик получил и что вернул за одно соединение.
struct Exchange {
    received: Vec<u8>,
    actions: Vec<Action>,
}

impl Exchange {
    fn answers(&self) -> Vec<Vec<u8>> {
        sends(&self.actions)
    }

    fn events(&self) -> Vec<&ResponderEvent> {
        events(&self.actions)
    }
}

/// Обслуживает одно соединение автоответчиком: читает фрагменты, отдаёт их автомату и
/// отправляет ответы. Так тест повторяет горячий путь сессии без журнала и сети наружу.
async fn serve<R: Responder>(mut responder: R, listener: TcpListener) -> Exchange {
    let (mut stream, _) = listener.accept().await.expect("соединение эмулятора");
    let mut exchange = Exchange {
        received: Vec::new(),
        actions: Vec::new(),
    };
    // Буфер меньше кадра: границы кадров попадают на границы чтения.
    let mut buffer = [0u8; 16];
    loop {
        let read = stream.read(&mut buffer).await.expect("чтение");
        if read == 0 {
            break;
        }
        exchange.received.extend_from_slice(&buffer[..read]);
        let produced = responder.feed(&buffer[..read], now());
        for action in &produced {
            if let Action::Send(bytes) = action {
                stream.write_all(bytes).await.expect("ответ");
            }
        }
        exchange.actions.extend(produced);
    }
    exchange.actions.extend(responder.finish(now()));
    exchange
}

/// Поднимает автоответчик на свободном порту и возвращает адрес и задачу обслуживания.
async fn spawn<R: Responder + Send + 'static>(
    responder: R,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<Exchange>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("порт");
    let addr = listener.local_addr().expect("адрес");
    (addr, tokio::spawn(serve(responder, listener)))
}

#[tokio::test]
async fn the_emulator_and_the_astm_answerer_exchange_the_same_bytes_in_any_fragmentation() {
    let records = vec![
        fixture("analyzer_patient_result.astm"),
        fixture("analyzer_long_result.astm"),
        fixture("analyzer_query.astm"),
    ];
    let modes = [
        Fragmentation::Whole,
        Fragmentation::ByteByByte,
        Fragmentation::Random { seed: 7, max: 5 },
        Fragmentation::Random {
            seed: 2026,
            max: 4096,
        },
    ];
    for mode in modes {
        let (addr, server) = spawn(Astm::new()).await;
        let report = AstmEmulator::new(addr)
            .with_records(records.clone())
            .with_fragmentation(mode)
            .run()
            .await
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        let exchange = server.await.expect("обслуживание");

        assert_eq!(
            exchange.received,
            expected_stream(&records),
            "{mode:?}: нарезка не меняет отправленные байты"
        );
        let frames: usize = records
            .iter()
            .map(|record| record_frames(record, DEFAULT_CHUNK, b'1', false).len())
            .sum();
        assert_eq!(
            report.frames, frames,
            "{mode:?}: кадр на каждую часть записи"
        );
        assert_eq!(
            report.acks(),
            report.frames + 1,
            "{mode:?}: ACK на ENQ и кадр"
        );
        assert_eq!(report.naks(), 0, "{mode:?}");
        // Каждый кадр подтверждён ровно одним управляющим байтом.
        assert_eq!(
            exchange.answers(),
            vec![vec![ACK]; report.frames + 1],
            "{mode:?}"
        );
        assert!(
            exchange.events().is_empty(),
            "{mode:?}: {:?}",
            exchange.events()
        );
    }
}

#[tokio::test]
async fn a_broken_checksum_is_acknowledged_with_an_error_by_default() {
    let (addr, server) = spawn(Astm::new()).await;
    let report = AstmEmulator::new(addr)
        .push_record(fixture("analyzer_patient_result.astm"))
        .with_bad_checksum()
        .run()
        .await
        .expect("ответы приходят");
    let exchange = server.await.expect("обслуживание");

    assert_eq!(report.acks(), report.frames + 1, "по умолчанию ACK (FR-17)");
    let events = exchange.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        matches!(events[0], ResponderEvent::Error { message, .. }
            if message.starts_with("ASTM checksum mismatch: got ")),
        "{events:?}"
    );
}

#[tokio::test]
async fn the_nak_setting_turns_broken_checksums_into_nak() {
    let (addr, server) = spawn(Astm::new().with_nak_on_bad_checksum(true)).await;
    let report = AstmEmulator::new(addr)
        .push_record(fixture("analyzer_patient_result.astm"))
        .with_bad_checksum()
        .expect_nak()
        .run()
        .await
        .expect("NAK приходит");
    let exchange = server.await.expect("обслуживание");

    assert_eq!(report.naks(), report.frames);
    assert!(matches!(
        &events(&exchange.actions)[..],
        [ResponderEvent::Error { .. }]
    ));
}

#[tokio::test]
async fn the_auto_profile_answers_astm_from_the_first_byte() {
    // M3-AC6: первый байт — ENQ, решение ASTM, байт не теряется.
    let (addr, server) = spawn(Auto::new(ControlIdSource::new())).await;
    let report = AstmEmulator::new(addr)
        .push_record(fixture("analyzer_patient_result.astm"))
        .run()
        .await
        .expect("обмен");
    let exchange = server.await.expect("обслуживание");

    assert_eq!(report.acks(), report.frames + 1);
    assert_eq!(
        exchange.received,
        expected_stream(&[fixture("analyzer_patient_result.astm")])
    );
    assert!(exchange.events().is_empty(), "{:?}", exchange.events());
}

#[tokio::test]
async fn the_auto_profile_keeps_the_bytes_of_an_unknown_protocol_without_answers() {
    let (addr, server) = spawn(Auto::new(ControlIdSource::new())).await;
    let mut stream = TcpStream::connect(addr).await.expect("подключение");
    stream
        .write_all(b"MSH|this is not framed")
        .await
        .expect("запись");
    stream.shutdown().await.expect("закрытие");
    let exchange = server.await.expect("обслуживание");

    assert!(exchange.answers().is_empty(), "ответов нет");
    assert_eq!(exchange.received, b"MSH|this is not framed");
    let events = exchange.events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0].code().map(|code| code.as_str()),
        Some("protocol_undetected"),
        "{events:?}"
    );
}

#[tokio::test]
async fn the_auto_profile_skips_the_noise_before_enq() {
    // M3-AC6: `EOT` перед `ENQ` — обычный шум приборов, решение принимает `ENQ`.
    let (addr, server) = spawn(Auto::new(ControlIdSource::new())).await;
    let mut stream = TcpStream::connect(addr).await.expect("подключение");
    let mut request = vec![EOT, ENQ];
    request.extend_from_slice(&framed(b'1', &fixture("analyzer_patient_result.astm"), ETX));
    stream.write_all(&request).await.expect("запись");
    let mut answers = [0u8; 2];
    stream.read_exact(&mut answers).await.expect("ответы");
    assert_eq!(answers, [ACK, ACK], "ACK на ENQ и на кадр");
    stream.shutdown().await.expect("закрытие");

    let exchange = server.await.expect("обслуживание");
    assert_eq!(exchange.received, request, "шум записан как обычные данные");
    assert!(exchange.events().is_empty(), "{:?}", exchange.events());
}

#[tokio::test]
async fn the_auto_profile_skips_the_noise_before_the_mllp_frame() {
    // M3-AC6: `NUL`, `CR`, `LF` и `XON` перед рамкой — шум линии, а не протокол.
    let (addr, server) = spawn(Auto::new(ControlIdSource::new())).await;
    let mut stream = TcpStream::connect(addr).await.expect("подключение");
    let mut request = vec![0x00, b'\r', b'\n', 0x11, 0x0B];
    request.extend_from_slice(&hl7_fixture("analyzer_oru_r01.hl7"));
    request.extend_from_slice(&[0x1C, b'\r']);
    stream.write_all(&request).await.expect("запись");
    stream.shutdown().await.expect("закрытие");

    let exchange = server.await.expect("обслуживание");
    let answers = exchange.answers();
    assert_eq!(answers.len(), 1, "один ACK: {answers:?}");
    assert_eq!(answers[0].first(), Some(&0x0B), "ACK в рамке MLLP");
    assert!(
        answers[0].windows(6).any(|window| window == b"MSA|AA"),
        "{answers:?}"
    );
    assert_eq!(exchange.received, request, "шум записан как обычные данные");
}

/// Байт потока ASTM: управляющие байты попадаются чаще случайных — на них живёт автомат.
fn astm_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        3 => Just(ENQ),
        3 => Just(EOT),
        3 => Just(STX),
        3 => Just(ETX),
        3 => Just(ETB),
        4 => Just(b'\r'),
        2 => Just(b'\n'),
        4 => proptest::sample::select(b"0123456789ABCDEFabcdefHPQRL|^".to_vec()),
        20 => any::<u8>(),
    ]
}

/// Кусок потока: мусор или кадр фикстуры, иногда с испорченной суммой или номером.
fn astm_piece() -> impl Strategy<Value = Vec<u8>> {
    const POOL: [&str; 3] = [
        "analyzer_patient_result.astm",
        "analyzer_query.astm",
        "analyzer_long_result.astm",
    ];
    prop_oneof![
        3 => proptest::collection::vec(astm_byte(), 0..=48),
        5 => (0usize..POOL.len(), any::<u8>(), any::<bool>()).prop_map(|(pick, number, damage)| {
            let record = fixture(POOL[pick]);
            let mut frame = framed(number, &record, if number % 2 == 0 { ETX } else { ETB });
            if damage {
                let at = frame.len() - 3;
                frame[at] = if frame[at] == b'0' { b'1' } else { b'0' };
            }
            frame
        }),
    ]
}

proptest! {
    /// Поток целиком и тот же поток по фрагментам дают одинаковые ответы и события (T-1).
    #[test]
    fn a_stream_is_the_same_whole_and_in_fragments(
        pieces in proptest::collection::vec(astm_piece(), 0..=6),
        cuts in proptest::collection::vec(1usize..=23, 1..=64),
    ) {
        let stream: Vec<u8> = pieces.concat();

        let mut whole = Astm::new();
        let mut expected = whole.feed(&stream, now());
        expected.extend(whole.finish(now()));

        let mut piecewise = Astm::new();
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
                prop_assert_eq!(bytes.len(), 1, "ответ — один управляющий байт");
                prop_assert!(
                    matches!(bytes[0], ACK | NAK),
                    "ответ — ACK или NAK, а не {:?}",
                    bytes
                );
            }
        }
    }

    /// Любые байты и любая нарезка: ASTM не паникует и не отвечает ничем, кроме ACK/NAK.
    #[test]
    fn any_bytes_keep_the_astm_invariants(
        pieces in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..=64), 0..=4),
        cuts in proptest::collection::vec(1usize..=24, 1..=64),
    ) {
        let stream: Vec<u8> = pieces.concat();
        let mut responder = Astm::new().with_nak_on_bad_checksum(true);
        let mut actions = Vec::new();
        let mut offset = 0;
        let mut sizes = cuts.iter().cycle();
        while offset < stream.len() {
            let size = *sizes.next().expect("размеры фрагментов не кончаются");
            let end = (offset + size).min(stream.len());
            actions.extend(responder.feed(&stream[offset..end], now()));
            offset = end;
        }
        actions.extend(responder.finish(now()));

        for action in &actions {
            if let Action::Send(bytes) = action {
                prop_assert_eq!(bytes.len(), 1);
                prop_assert!(matches!(bytes[0], ACK | NAK));
            }
        }
    }

    /// «Авто» не паникует на произвольных байтах, а поток целиком равен потоку по фрагментам.
    #[test]
    fn the_auto_profile_is_the_same_whole_and_in_fragments(
        pieces in proptest::collection::vec(astm_piece(), 0..=4),
        cuts in proptest::collection::vec(1usize..=19, 1..=48),
    ) {
        let stream: Vec<u8> = pieces.concat();

        let mut whole = Auto::new(ControlIdSource::new());
        let mut expected = whole.feed(&stream, now());
        expected.extend(whole.finish(now()));

        let mut piecewise = Auto::new(ControlIdSource::new());
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

        prop_assert_eq!(&actual, &expected, "решение принимается один раз");
    }
}
