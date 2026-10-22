//! Прокси TCP на loopback: эмулятор анализатора ↔ прокси ↔ эмулятор ЛИС (T-2, M4-AC1…AC6).
//!
//! Обе топологии: «анализатор — клиент» (анализатор приходит на порт bytetap) и
//! «анализатор — сервер» (приходит ЛИС). Проверяется, что байты доходят без изменений
//! в обе стороны, что направление `data` считается по роли стороны, что `open` несёт обе
//! стороны пары, что закрытие и недоступная цель не ломают сессию.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use bytetap_core::journal::{Autoresponder, ClosedBy, Dir, Event, LinkRole, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig, SessionHandle};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::astm::{AstmEmulator, record_frames};
use bytetap_testkit::emulator::{AnalyzerEmulator, Fragmentation, ack_msa, fragment_bounds};
use bytetap_testkit::fixtures_dir;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator, hl7_ack};
use bytetap_testkit::server::AnalyzerServer;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// Пауза прокси после неудачного подключения к цели.
const RETRY: Duration = Duration::from_millis(100);

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(10);

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("hl7").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn astm_fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("astm").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Сессия прокси с журналом во временной папке.
async fn start(dir: &Path, target: SocketAddr, role: ProxyRole) -> SessionHandle {
    Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target.to_string(),
            role,
            RETRY,
        )
        .with_log_dir(dir),
    )
    .await
    .expect("сессия прокси")
}

fn listen_addr(handle: &SessionHandle) -> SocketAddr {
    handle.local_addr().parse().expect("адрес прослушивания")
}

/// Отправляет `payload` и читает ответ до закрытия соединения.
///
/// Полузакрытие после отправки — это `FIN` стороны: обратное направление должно дожить
/// до своего `FIN` (FR-7).
async fn exchange(addr: SocketAddr, payload: &[u8], fragmentation: Fragmentation) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("сторона подключается");
    for (start, end) in fragment_bounds(payload.len(), fragmentation) {
        stream
            .write_all(&payload[start..end])
            .await
            .expect("фрагмент");
    }
    stream.shutdown().await.expect("FIN стороны");
    let mut answer = Vec::new();
    timeout(WAIT, stream.read_to_end(&mut answer))
        .await
        .expect("ответ пришёл")
        .expect("чтение ответа");
    answer
}

/// Байты одного направления одного соединения, склеенные из `b64`.
fn recorded_bytes(path: &Path, conn: u32, dir: Dir) -> Vec<u8> {
    let outcome = read_all(path).expect("журнал читается");
    let mut bytes = Vec::new();
    for line in outcome.lines {
        if let Event::Data(data) = line.event
            && data.conn == conn
            && data.dir == dir
        {
            assert!(!data.synthetic, "в прокси нет синтетических байтов (FR-6)");
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 декодируется"),
            );
        }
    }
    bytes
}

/// Ждёт, пока в журнале появится столько событий `close`.
async fn wait_for_closes(path: &Path, expected: usize) {
    for _ in 0..500 {
        let outcome = read_all(path).expect("журнал читается");
        let closes = outcome
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Close(_)))
            .count();
        if closes >= expected {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("{expected} событий close не появились в журнале");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_analyzer_client_pair_is_recorded_byte_for_byte() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Hl7Ack);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();
    let message = fixture("analyzer_oru_r01.hl7");

    let report = AnalyzerEmulator::new(listen)
        .with_messages([message.clone()])
        .run()
        .await
        .expect("обмен через прокси");
    assert_eq!(report.connections, 1);
    let ack = report.acks.first().expect("ACK от ЛИС");
    assert_eq!(
        ack_msa(ack).map(|(code, _)| code),
        Some("AA".to_owned()),
        "анализатор получил ACK ЛИС"
    );

    let lis_report = timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    assert_eq!(
        lis_report.all_received(),
        message,
        "ЛИС получила сообщение целиком"
    );

    wait_for_closes(&path, 1).await;
    let stats = handle.stop().await.expect("остановка");

    let outcome = read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(header.format, bytetap_core::journal::FORMAT);
    assert_eq!(header.mode, Mode::Proxy);
    assert_eq!(
        header.autoresponder,
        Autoresponder::None,
        "автоответчика нет (FR-6)"
    );
    assert_eq!(header.settings["listen"], listen.to_string());
    assert_eq!(header.settings["target"], lis_addr.to_string());
    assert_eq!(header.settings["role"], "analyzer_client");
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("-proxy.jsonl")),
        "имя файла режима: {}",
        path.display()
    );

    let mut opens = 0;
    for line in &outcome.lines {
        match &line.event {
            Event::Open(open) => {
                opens += 1;
                assert_eq!(open.conn, 1);
                assert_eq!(
                    open.local,
                    listen.to_string(),
                    "принята сторона на порту bytetap"
                );
                assert_ne!(open.peer, "", "peer — адрес анализатора");
                let link = open.link.as_ref().expect("у прокси open несёт обе стороны");
                assert_eq!(link.role, LinkRole::Lis, "дальний конец — ЛИС");
                assert_eq!(link.peer, lis_addr.to_string());
                assert!(!link.local.is_empty(), "локальный адрес исходящего сокета");
            }
            Event::Close(close) => {
                assert_eq!(close.conn, 1);
                assert_eq!(close.by, ClosedBy::Analyzer);
                assert_eq!(close.reason, "eof");
            }
            _ => {}
        }
    }
    assert_eq!(opens, 1, "одна пара — одно open");

    assert_eq!(
        recorded_bytes(&path, 1, Dir::AnalyzerToLis),
        message,
        "журнал записал отправленное анализатором байт в байт"
    );
    let answers = recorded_bytes(&path, 1, Dir::LisToAnalyzer);
    assert_eq!(answers, hl7_ack(), "журнал записал ответ ЛИС байт в байт");
    assert_eq!(stats.connections, 1);
    assert_eq!(stats.bytes_analyzer_to_lis, message.len() as u64);
    assert_eq!(stats.bytes_lis_to_analyzer, answers.len() as u64);
    let Event::SessionEnd(end) = &outcome.lines.last().expect("итоги").event else {
        panic!("последняя строка — итоги");
    };
    assert_eq!(end.stats, stats);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_analyzer_server_pair_keeps_directions_by_side_role() {
    let dir = tempfile::tempdir().expect("временная папка");
    let message = fixture("analyzer_oru_r01.hl7");
    // Анализатор — сервер: bytetap подключается к нему, а на порт bytetap приходит ЛИС.
    let analyzer = AnalyzerServer::bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("сервер анализатора");
    let analyzer_addr = analyzer.local_addr().expect("адрес анализатора");
    let analyzer_task = tokio::spawn(
        analyzer
            .with_connection([message.clone()])
            .with_timeout(WAIT)
            .run(),
    );

    let handle = start(dir.path(), analyzer_addr, ProxyRole::AnalyzerServer).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let lis = LisEmulator::connect(listen).with_behaviour(LisBehaviour::Hl7Ack);
    let lis_task = tokio::spawn(lis.run());

    let report = timeout(WAIT, analyzer_task)
        .await
        .expect("анализатор закончил")
        .expect("задача анализатора")
        .expect("обмен анализатора");
    let lis_report = timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    assert_eq!(
        lis_report.all_received(),
        message,
        "ЛИС получила сообщение целиком"
    );
    assert_eq!(
        ack_msa(&report.acks[0][0]).map(|(code, _)| code),
        Some("AA".to_owned())
    );

    wait_for_closes(&path, 1).await;
    let stats = handle.stop().await.expect("остановка");

    let outcome = read_all(&path).expect("журнал читается");
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(header.settings["role"], "analyzer_server");
    assert_eq!(header.settings["target"], analyzer_addr.to_string());
    let open = outcome
        .lines
        .iter()
        .find_map(|line| match &line.event {
            Event::Open(open) => Some(open),
            _ => None,
        })
        .expect("open пары");
    assert_eq!(
        open.local,
        listen.to_string(),
        "принята ЛИС на порту bytetap"
    );
    let link = open.link.as_ref().expect("ссылка на дальний конец");
    assert_eq!(link.role, LinkRole::Analyzer, "дальний конец — анализатор");
    assert_eq!(link.peer, analyzer_addr.to_string());

    // Направление считается по роли стороны, а не по тому, кто начал соединение:
    // байты из сокета ЛИС (здесь это ACK) — `lis_to_analyzer`, байты из сокета
    // анализатора (сообщение) — `analyzer_to_lis`.
    assert_eq!(
        recorded_bytes(&path, 1, Dir::AnalyzerToLis),
        message,
        "журнал записал сообщение анализатора байт в байт"
    );
    assert_eq!(
        recorded_bytes(&path, 1, Dir::LisToAnalyzer),
        hl7_ack(),
        "журнал записал ACK ЛИС из сокета ЛИС байт в байт"
    );
    assert_eq!(stats.bytes_analyzer_to_lis, message.len() as u64);
    assert_eq!(stats.bytes_lis_to_analyzer, hl7_ack().len() as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_astm_exchange_is_forwarded_and_acknowledged() {
    let dir = tempfile::tempdir().expect("временная папка");
    let record = astm_fixture("analyzer_patient_result.astm");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::AstmAck);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let report = AstmEmulator::new(listen)
        .push_record(record.clone())
        .with_timeout(WAIT)
        .run()
        .await
        .expect("обмен ASTM через прокси");
    assert_eq!(report.connections, 1);
    assert_eq!(report.naks(), 0, "NAK в прокси не применяется (FR-6)");
    assert_eq!(
        report.acks(),
        report.frames + 1,
        "ACK на ENQ и на каждый кадр"
    );

    let lis_report = timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    // Отправленное анализатором: `ENQ`, кадры записи и `EOT` — ровно то, что получила ЛИС.
    let mut sent = vec![bytetap_testkit::astm::ENQ];
    for frame in record_frames(&record, bytetap_testkit::astm::DEFAULT_CHUNK, b'1', false) {
        sent.extend_from_slice(&frame);
    }
    sent.push(bytetap_testkit::astm::EOT);
    assert_eq!(
        lis_report.all_received(),
        sent,
        "ЛИС получила обмен целиком"
    );

    wait_for_closes(&path, 1).await;
    handle.stop().await.expect("остановка");

    assert_eq!(
        recorded_bytes(&path, 1, Dir::AnalyzerToLis),
        sent,
        "журнал записал обмен ASTM байт в байт"
    );
    assert_eq!(
        recorded_bytes(&path, 1, Dir::LisToAnalyzer),
        report.responses,
        "журнал записал ответы ЛИС байт в байт"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn astm_bytes_cross_the_proxy_in_an_analyzer_server_topology() {
    let dir = tempfile::tempdir().expect("временная папка");
    // Анализатор — сервер: возвращает полученное, поэтому обмен виден побайтно.
    let analyzer = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("анализатор слушает");
    let analyzer_addr = analyzer.local_addr().expect("адрес анализатора");
    let analyzer_task = tokio::spawn(analyzer.run());

    let handle = start(dir.path(), analyzer_addr, ProxyRole::AnalyzerServer).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let record = astm_fixture("analyzer_query.astm");
    let mut frame = vec![bytetap_testkit::astm::ENQ];
    for piece in record_frames(&record, bytetap_testkit::astm::DEFAULT_CHUNK, b'1', false) {
        frame.extend_from_slice(&piece);
    }
    frame.push(bytetap_testkit::astm::EOT);

    let answer = exchange(listen, &frame, Fragmentation::Whole).await;
    assert_eq!(answer, frame, "анализатор вернул обмен ЛИС без изменений");

    timeout(WAIT, analyzer_task)
        .await
        .expect("анализатор закончил")
        .expect("задача анализатора")
        .expect("обмен анализатора");

    wait_for_closes(&path, 1).await;
    handle.stop().await.expect("остановка");
    assert_eq!(
        recorded_bytes(&path, 1, Dir::LisToAnalyzer),
        frame,
        "байты ЛИС записаны в направлении lis_to_analyzer"
    );
    assert_eq!(
        recorded_bytes(&path, 1, Dir::AnalyzerToLis),
        frame,
        "ответ анализатора записан в направлении analyzer_to_lis"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn any_fragmentation_reaches_the_other_side_without_changes() {
    let payload = fixture("analyzer_oru_r01.hl7");
    for fragmentation in [
        Fragmentation::Whole,
        Fragmentation::ByteByByte,
        Fragmentation::Random { seed: 7, max: 5 },
        Fragmentation::Random {
            seed: 42,
            max: 4096,
        },
    ] {
        let dir = tempfile::tempdir().expect("временная папка");
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .expect("ЛИС слушает")
            .with_behaviour(LisBehaviour::Echo);
        let lis_addr = lis.local_addr().expect("адрес ЛИС");
        let lis_task = tokio::spawn(lis.run());
        let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
        let listen = listen_addr(&handle);
        let path = handle.journal_path().to_path_buf();

        let answer = exchange(listen, &payload, fragmentation).await;
        let lis_report = timeout(WAIT, lis_task)
            .await
            .expect("ЛИС закончила")
            .expect("задача ЛИС")
            .expect("обмен ЛИС");
        assert_eq!(lis_report.all_received(), payload, "{fragmentation:?}");

        wait_for_closes(&path, 1).await;
        handle.stop().await.expect("остановка");
        assert_eq!(
            recorded_bytes(&path, 1, Dir::AnalyzerToLis),
            payload,
            "{fragmentation:?}"
        );
        assert_eq!(
            recorded_bytes(&path, 1, Dir::LisToAnalyzer),
            payload,
            "{fragmentation:?}"
        );
        assert_eq!(answer, payload, "{fragmentation:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_half_close_lets_the_other_direction_finish() {
    let dir = tempfile::tempdir().expect("временная папка");
    let request = fixture("analyzer_qry_q02.hl7");
    let answer = fixture("lis_ack_aa.hl7");
    // ЛИС отвечает не сразу: хвост приходит уже после `FIN` анализатора (M4-В7).
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Hl7Ack);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let mut stream = TcpStream::connect(listen).await.expect("анализатор");
    stream.write_all(&request).await.expect("запрос");
    stream.shutdown().await.expect("FIN анализатора");
    // ACK уходит после полузакрытия: прокси обязан донести его до анализатора.
    let mut tail = Vec::new();
    timeout(WAIT, stream.read_to_end(&mut tail))
        .await
        .expect("хвост дошёл")
        .expect("чтение хвоста");
    assert_eq!(tail, hl7_ack(), "обратное направление дожило до своего FIN");
    assert_ne!(tail, answer, "ЛИС отвечает своим ACK, а не фикстурой");

    let lis_report = timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    assert_eq!(lis_report.all_received(), request);

    wait_for_closes(&path, 1).await;
    handle.stop().await.expect("остановка");
    let outcome = read_all(&path).expect("журнал читается");
    let close = outcome
        .lines
        .iter()
        .find_map(|line| match &line.event {
            Event::Close(close) => Some(close),
            _ => None,
        })
        .expect("close пары");
    assert_eq!(close.by, ClosedBy::Analyzer);
    assert_eq!(close.reason, "eof");
    assert_eq!(recorded_bytes(&path, 1, Dir::LisToAnalyzer), hl7_ack());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_closes_both_sides_at_once() {
    let dir = tempfile::tempdir().expect("временная папка");
    // ЛИС ждёт и запоминает, что соединение закрылось.
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_timeout(WAIT);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let mut stream = TcpStream::connect(listen).await.expect("анализатор");
    // Нулевой `SO_LINGER`: закрытие сокета отправляет RST, а не FIN. Предупреждение
    // о блокировке в `Drop` относится к ненулевому сроку, здесь он нулевой.
    #[allow(deprecated)]
    stream
        .set_linger(Some(Duration::ZERO))
        .expect("нулевой linger");
    stream.write_all(b"ORU").await.expect("фрагмент");
    drop(stream); // RST: сброс, а не FIN

    timeout(WAIT, lis_task)
        .await
        .expect("ЛИС увидела закрытие")
        .expect("задача ЛИС")
        .expect("сброс не ошибка ЛИС");
    wait_for_closes(&path, 1).await;
    handle.stop().await.expect("остановка");

    let outcome = read_all(&path).expect("журнал читается");
    let close = outcome
        .lines
        .iter()
        .find_map(|line| match &line.event {
            Event::Close(close) => Some(close),
            _ => None,
        })
        .expect("close пары");
    assert_eq!(close.by, ClosedBy::Analyzer, "сбросил анализатор");
    assert_eq!(close.reason, "reset");
    // Пара закрыта: сессия продолжает слушать.
    assert!(matches!(
        outcome.lines.last().expect("последняя строка").event,
        Event::SessionEnd(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_target_is_an_error_and_the_session_keeps_listening() {
    let dir = tempfile::tempdir().expect("временная папка");
    // Порт цели занят сокетом без `listen`: подключение откажет. Сокет держится, пока
    // проверяется серия ошибок, и освобождается перед удачным соединением.
    let reserved = tokio::net::TcpSocket::new_v4().expect("сокет");
    reserved
        .bind("127.0.0.1:0".parse().unwrap())
        .expect("свободный порт");
    let target = reserved.local_addr().expect("адрес цели");

    let (handle, mut feed) = Session::start_with_feed(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target.to_string(),
            ProxyRole::AnalyzerClient,
            RETRY,
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("старт без доступной цели");
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    // Три неудачных соединения подряд: ошибка схлопывается, `open` не пишется.
    for _ in 0..3 {
        let mut stream = TcpStream::connect(listen).await.expect("анализатор");
        let mut buffer = [0u8; 8];
        let read = timeout(WAIT, stream.read(&mut buffer))
            .await
            .expect("входящее закрылось")
            .expect("чтение");
        assert_eq!(read, 0, "без цели входящее соединение закрывается без open");
    }
    let error = timeout(WAIT, async {
        loop {
            let event = feed.recv().await.expect("лента жива");
            if let Event::Error(error) = event.as_ref() {
                break error.clone();
            }
        }
    })
    .await
    .expect("ошибка подключения пришла");
    assert_eq!(
        error.code.as_deref(),
        Some("proxy_connect_failed"),
        "{error:?}"
    );

    // Цель появляется: следующее соединение проходит, повторы схлопнуты.
    drop(reserved);
    let lis = LisEmulator::listen(target)
        .await
        .expect("ЛИС заняла порт цели");
    let lis_task = tokio::spawn(lis.run());
    let message = fixture("analyzer_oru_r01.hl7");
    let answer = exchange(listen, &message, Fragmentation::Whole).await;
    assert_eq!(answer, message, "обмен через прокси прошёл");
    timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");

    wait_for_closes(&path, 1).await;
    handle.stop().await.expect("остановка");
    let outcome = read_all(&path).expect("журнал читается");
    let errors: Vec<_> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Error(error) => Some(error),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1, "серия ошибок схлопнута: {errors:?}");
    assert_eq!(errors[0].code.as_deref(), Some("proxy_connect_failed"));
    assert!(errors[0].conn.is_none(), "ошибка уровня сессии");
    let infos: Vec<_> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Info(info) => Some(info.message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        infos
            .iter()
            .any(|info| info.contains("recovered after 2 repeated failures")),
        "восстановление сообщено: {infos:?}"
    );
    let opens = outcome
        .lines
        .iter()
        .filter(|line| matches!(line.event, Event::Open(_)))
        .count();
    assert_eq!(opens, 1, "open только у удачной пары");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_pairs_run_in_parallel_with_their_own_numbers() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_connections(2)
        .with_behaviour(LisBehaviour::Echo)
        .with_timeout(WAIT);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let first = fixture("analyzer_oru_r01.hl7");
    let second = fixture("analyzer_qry_q02.hl7");
    let (one, two) = tokio::join!(
        exchange(listen, &first, Fragmentation::Whole),
        exchange(listen, &second, Fragmentation::Whole)
    );
    assert_eq!(one, first);
    assert_eq!(two, second);

    timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    wait_for_closes(&path, 2).await;
    let stats = handle.stop().await.expect("остановка");

    assert_eq!(stats.connections, 2, "две пары — два соединения журнала");
    assert_eq!(
        stats.bytes_analyzer_to_lis,
        (first.len() + second.len()) as u64
    );
    let outcome = read_all(&path).expect("журнал читается");
    let mut conns: Vec<u32> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Open(open) => Some(open.conn),
            _ => None,
        })
        .collect();
    conns.sort_unstable();
    assert_eq!(conns, [1, 2], "нумерация сквозная");
    let closes = outcome
        .lines
        .iter()
        .filter(|line| matches!(line.event, Event::Close(_)))
        .count();
    assert_eq!(closes, 2, "close один раз на пару");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_closes_the_pair_as_bytetap() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_timeout(WAIT);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());
    let handle = start(dir.path(), lis_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    let mut stream = TcpStream::connect(listen).await.expect("анализатор");
    stream.write_all(b"ORU").await.expect("фрагмент");
    // Даём прокси открыть пару и переслать фрагмент, затем останавливаем сессию:
    // «Стоп» закрывает пару, не дожидаясь второй стороны (FR-7).
    let mut forwarded = false;
    for _ in 0..200 {
        let outcome = read_all(&path).expect("журнал читается");
        let opened = outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Open(_)));
        forwarded = outcome
            .lines
            .iter()
            .any(|line| matches!(&line.event, Event::Data(data) if data.dir == Dir::AnalyzerToLis));
        if opened && forwarded {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(forwarded, "фрагмент дошёл до журнала до остановки");

    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, 1);
    timeout(WAIT, lis_task)
        .await
        .expect("ЛИС увидела закрытие")
        .expect("задача ЛИС")
        .expect("сессия закрыла пару");

    let outcome = read_all(&path).expect("журнал читается");
    let close = outcome
        .lines
        .iter()
        .find_map(|line| match &line.event {
            Event::Close(close) => Some(close),
            _ => None,
        })
        .expect("close пары");
    assert_eq!(close.by, ClosedBy::Bytetap);
    assert_eq!(close.reason, "stop");
    assert!(matches!(
        outcome.lines.last().expect("последняя строка").event,
        Event::SessionEnd(_)
    ));
    assert_eq!(recorded_bytes(&path, 1, Dir::AnalyzerToLis), b"ORU");
    drop(stream);
}
