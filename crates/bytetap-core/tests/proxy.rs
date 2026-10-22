//! Прокси TCP на loopback: эмулятор анализатора ↔ прокси ↔ эмулятор ЛИС (T-2, M4-AC1…AC6).
//!
//! Обе топологии: «анализатор — клиент» (анализатор приходит на порт bytetap) и
//! «анализатор — сервер» (приходит ЛИС). Проверяется, что байты доходят без изменений
//! в обе стороны, что направление `data` считается по роли стороны, что `open` несёт обе
//! стороны пары, что закрытие и недоступная цель не ломают сессию.
//!
//! Набор закрытия — общий для обеих топологий: тест перебирает [`ROLES`] и закрывает
//! пару то принятой стороной, то целью. Для теста важна роль, а не то, кто из двух
//! слушает: механика у сторон одна, различаются только `by`, `dir` и адреса (M4-AC5).

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytetap_core::journal::{Autoresponder, Close, ClosedBy, Dir, Event, LinkRole, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig, SessionHandle};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::astm::{AstmEmulator, record_frames};
use bytetap_testkit::emulator::{AnalyzerEmulator, Fragmentation, ack_msa, fragment_bounds};
use bytetap_testkit::fixtures_dir;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator, hl7_ack};
use bytetap_testkit::server::AnalyzerServer;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

/// Пауза прокси после неудачного подключения к цели.
const RETRY: Duration = Duration::from_millis(100);

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(10);

/// Обе топологии: набор закрытия проходит по обеим (M4-AC5, M4-AC16).
const ROLES: [ProxyRole; 2] = [ProxyRole::AnalyzerClient, ProxyRole::AnalyzerServer];

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

/// Цель пары: слушает адрес, к которому подключается прокси.
async fn target_listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("цель слушает");
    let addr = listener.local_addr().expect("адрес цели");
    (listener, addr)
}

/// Роль принятой стороны: она приходит на порт bytetap, то есть противоположна цели.
fn accepted_role(role: ProxyRole) -> LinkRole {
    role.target_role().opposite()
}

/// Пара целиком: принятая сторона пришла, прокси подключился к цели.
///
/// Возвращает оба конца пары. Тесту всё равно, анализатор это или ЛИС: он закрывает
/// сторону по её роли.
async fn pair(listener: &TcpListener, listen: SocketAddr) -> (TcpStream, TcpStream) {
    let accepted = TcpStream::connect(listen).await.expect("принятая сторона");
    let (target, _) = timeout(WAIT, listener.accept())
        .await
        .expect("прокси подключился к цели")
        .expect("приём цели");
    (accepted, target)
}

/// Ждёт событие `close` пары и возвращает его.
async fn wait_for_the_close(path: &Path) -> Close {
    for _ in 0..500 {
        let outcome = read_all(path).expect("журнал читается");
        let closes: Vec<Close> = outcome
            .lines
            .iter()
            .filter_map(|line| match &line.event {
                Event::Close(close) => Some(close.clone()),
                _ => None,
            })
            .collect();
        if let Some(close) = closes.first() {
            assert_eq!(closes.len(), 1, "close один раз на пару");
            return close.clone();
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("close пары не появился в журнале");
}

/// Дочитывает сокет до конца: после закрытия пары в буфере ещё есть переданное.
///
/// Сброс на закрытии — тоже конец: сторона уже увидела всё, что успело дойти.
async fn drain(stream: &mut TcpStream) -> Vec<u8> {
    let (rest, _) = drain_to_close(stream).await;
    rest
}

/// То же, но говорит, пришёл ли сброс (`RST`) вместо `FIN`.
async fn drain_to_close(stream: &mut TcpStream) -> (Vec<u8>, bool) {
    let mut rest = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match timeout(WAIT, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => return (rest, false),
            Ok(Ok(read)) => rest.extend_from_slice(&buffer[..read]),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                return (rest, true);
            }
            Ok(Err(error)) => panic!("чтение остатка: {error}"),
            Err(_) => panic!("сторона не увидела закрытие пары"),
        }
    }
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
async fn the_other_side_answers_after_the_half_close() {
    // Протокол «запрос — ответ»: сторона полузакрывается после запроса, а вторая отвечает
    // только после `FIN`. Пара обязана донести ответ до закрытия (FR-7, M4-В7).
    let request = fixture("analyzer_qry_q02.hl7");
    let answer = fixture("lis_ack_aa.hl7");
    for role in ROLES {
        let dir = tempfile::tempdir().expect("временная папка");
        let (listener, target_addr) = target_listener().await;
        let handle = start(dir.path(), target_addr, role).await;
        let listen = listen_addr(&handle);
        let path = handle.journal_path().to_path_buf();

        let (mut accepted, mut target) = pair(&listener, listen).await;
        accepted.write_all(&request).await.expect("запрос");
        accepted.shutdown().await.expect("FIN принятой стороны");

        let mut received = Vec::new();
        timeout(WAIT, target.read_to_end(&mut received))
            .await
            .expect("{role}: FIN дошёл до цели")
            .expect("чтение цели");
        assert_eq!(received, request, "{role}: запрос дошёл целиком");
        target.write_all(&answer).await.expect("ответ после EOF");
        target.shutdown().await.expect("FIN цели");

        let tail = drain(&mut accepted).await;
        assert_eq!(tail, answer, "{role}: ответ дошёл после полузакрытия");

        wait_for_closes(&path, 1).await;
        handle.stop().await.expect("остановка");
        assert_eq!(
            recorded_bytes(&path, 1, accepted_role(role).dir()),
            request,
            "{role}: запрос записан по роли принятой стороны"
        );
        assert_eq!(
            recorded_bytes(&path, 1, role.target_role().dir()),
            answer,
            "{role}: ответ записан по роли цели"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_side_closing_is_recorded_as_that_side() {
    // `close` один раз на пару, `by` — сторона, закрывшая первой (M4-AC5).
    for role in ROLES {
        for by_target in [false, true] {
            let dir = tempfile::tempdir().expect("временная папка");
            let (listener, target_addr) = target_listener().await;
            let handle = start(dir.path(), target_addr, role).await;
            let listen = listen_addr(&handle);
            let path = handle.journal_path().to_path_buf();
            let (mut accepted, mut target) = pair(&listener, listen).await;

            let closer_role = if by_target {
                role.target_role()
            } else {
                accepted_role(role)
            };
            if by_target {
                target.write_all(b"bye").await.expect("фрагмент");
                target.shutdown().await.expect("FIN цели");
                assert_eq!(
                    drain(&mut accepted).await,
                    b"bye",
                    "{role}: фрагмент переслан до закрытия пары"
                );
                accepted.shutdown().await.expect("FIN второй стороны");
            } else {
                accepted.write_all(b"bye").await.expect("фрагмент");
                accepted.shutdown().await.expect("FIN принятой стороны");
                assert_eq!(
                    drain(&mut target).await,
                    b"bye",
                    "{role}: фрагмент переслан до закрытия пары"
                );
                target.shutdown().await.expect("FIN второй стороны");
            }
            let close = wait_for_the_close(&path).await;
            assert_eq!(close.by, closer_role.closed_by(), "{role}");
            assert_eq!(close.reason, "eof", "{role}");
            handle.stop().await.expect("остановка");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_closes_the_pair_as_the_resetting_side() {
    // `RST` закрывает обе стороны сразу, а `close` называет сторону, сбросившую соединение.
    for role in ROLES {
        for by_target in [false, true] {
            let dir = tempfile::tempdir().expect("временная папка");
            let (listener, target_addr) = target_listener().await;
            let handle = start(dir.path(), target_addr, role).await;
            let listen = listen_addr(&handle);
            let path = handle.journal_path().to_path_buf();
            let (mut accepted, mut target) = pair(&listener, listen).await;

            let closer_role = if by_target {
                role.target_role()
            } else {
                accepted_role(role)
            };
            // Нулевой `SO_LINGER`: закрытие уходит сбросом, а не `FIN`.
            let reset_second = if by_target {
                target.write_all(b"ORU").await.expect("фрагмент");
                target.set_zero_linger().expect("нулевой linger");
                drop(target);
                drain_to_close(&mut accepted).await.1
            } else {
                accepted.write_all(b"ORU").await.expect("фрагмент");
                accepted.set_zero_linger().expect("нулевой linger");
                drop(accepted);
                drain_to_close(&mut target).await.1
            };
            // Вторая сторона закрыта сбросом, а не `FIN`: сброс одной стороны пары
            // пересылается второй (M4-AC5).
            assert!(reset_second, "{role}: вторая сторона закрыта сбросом");

            let close = wait_for_the_close(&path).await;
            assert_eq!(close.by, closer_role.closed_by(), "{role}");
            assert_eq!(close.reason, "reset", "{role}");
            handle.stop().await.expect("остановка");
            let outcome = read_all(&path).expect("журнал читается");
            assert!(
                matches!(
                    outcome.lines.last().expect("последняя строка").event,
                    Event::SessionEnd(_)
                ),
                "{role}: сессия закрылась штатно"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_target_is_an_error_and_the_session_keeps_listening() {
    for role in ROLES {
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
                role,
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
            let mut stream = TcpStream::connect(listen).await.expect("сторона пришла");
            let mut buffer = [0u8; 8];
            let read = timeout(WAIT, stream.read(&mut buffer))
                .await
                .expect("входящее закрылось")
                .expect("чтение");
            assert_eq!(
                read, 0,
                "{role}: без цели входящее соединение закрывается без open"
            );
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
        .expect("{role}: ошибка подключения пришла");
        assert_eq!(
            error.code.as_deref(),
            Some("proxy_connect_failed"),
            "{role}: {error:?}"
        );

        // Цель появляется: следующее соединение проходит, повторы схлопнуты.
        drop(reserved);
        let lis = LisEmulator::listen(target).await.expect("цель заняла порт");
        let lis_task = tokio::spawn(lis.run());
        let message = fixture("analyzer_oru_r01.hl7");
        let answer = exchange(listen, &message, Fragmentation::Whole).await;
        assert_eq!(answer, message, "{role}: обмен через прокси прошёл");
        timeout(WAIT, lis_task)
            .await
            .expect("цель закончила")
            .expect("задача цели")
            .expect("обмен цели");

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
        assert_eq!(
            errors.len(),
            1,
            "{role}: серия ошибок схлопнута: {errors:?}"
        );
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
            "{role}: восстановление сообщено: {infos:?}"
        );
        let opens = outcome
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Open(_)))
            .count();
        assert_eq!(opens, 1, "{role}: open только у удачной пары");
    }
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
    for role in ROLES {
        let dir = tempfile::tempdir().expect("временная папка");
        let (listener, target_addr) = target_listener().await;
        let handle = start(dir.path(), target_addr, role).await;
        let listen = listen_addr(&handle);
        let path = handle.journal_path().to_path_buf();
        let (mut accepted, mut target) = pair(&listener, listen).await;

        accepted.write_all(b"ORU").await.expect("фрагмент");
        // Даём прокси переслать фрагмент, затем останавливаем сессию: «Стоп» закрывает
        // пару, не дожидаясь второй стороны (FR-7).
        let mut buffer = [0u8; 8];
        timeout(WAIT, target.read(&mut buffer))
            .await
            .expect("{role}: фрагмент дошёл до цели")
            .expect("чтение цели");

        let stats = handle.stop().await.expect("остановка");
        assert_eq!(stats.connections, 1, "{role}");
        let _ = drain(&mut target).await;

        let close = wait_for_the_close(&path).await;
        assert_eq!(close.by, ClosedBy::Bytetap, "{role}");
        assert_eq!(close.reason, "stop", "{role}");
        let outcome = read_all(&path).expect("журнал читается");
        assert!(
            matches!(
                outcome.lines.last().expect("последняя строка").event,
                Event::SessionEnd(_)
            ),
            "{role}: итоги сессии записаны"
        );
        assert_eq!(
            recorded_bytes(&path, 1, accepted_role(role).dir()),
            b"ORU",
            "{role}: фрагмент записан"
        );
        drop(accepted);
    }
}

/// Сторона, которая не читает, не отнимает у журнала отправленное: на «Стоп» в журнале
/// ровно те байты, которые получил адресат (M4-AC10).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_pair_records_exactly_the_bytes_the_target_received() {
    // Поток заведомо больше буферов сокетов (на loopback это единицы мегабайт):
    // запись в цель, которая не читает, обязательно оборвётся на середине.
    let payload = vec![b'A'; 16 * 1024 * 1024];
    for role in ROLES {
        let dir = tempfile::tempdir().expect("временная папка");
        let (listener, target_addr) = target_listener().await;
        let handle = start(dir.path(), target_addr, role).await;
        let listen = listen_addr(&handle);
        let path = handle.journal_path().to_path_buf();
        let (mut accepted, mut target) = pair(&listener, listen).await;

        // Цель не читает: насос упирается в её буфер, и запись обрывается на середине.
        let sent = payload.clone();
        let writer = tokio::spawn(async move {
            let _ = accepted.write_all(&sent).await;
            let _ = accepted.shutdown().await;
        });
        sleep(Duration::from_millis(300)).await;
        handle.stop().await.expect("остановка");
        let _ = writer.await;

        let received = drain(&mut target).await;
        let recorded = recorded_bytes(&path, 1, accepted_role(role).dir());
        assert_eq!(
            received, recorded,
            "{role}: журнал записал ровно то, что получила цель"
        );
        assert!(
            received.len() < payload.len(),
            "{role}: запись оборвана на середине ({} из {})",
            received.len(),
            payload.len()
        );
    }
}

/// `FIN` одной стороны, а вторая молчит: по истечении `PROXY_LINGER` пару закрывает
/// bytetap, иначе зависшая сторона держала бы её вечно (M4-AC5, M4-В7).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_linger_expiry_closes_the_pair_as_bytetap_timeout() {
    for role in ROLES {
        let dir = tempfile::tempdir().expect("временная папка");
        let (listener, target_addr) = target_listener().await;
        let handle = start(dir.path(), target_addr, role).await;
        let listen = listen_addr(&handle);
        let path = handle.journal_path().to_path_buf();
        let (mut accepted, mut target) = pair(&listener, listen).await;

        accepted.write_all(b"ORU").await.expect("фрагмент");
        accepted.shutdown().await.expect("FIN принятой стороны");
        // Цель читает до конца и молчит: соединение не закрывает.
        let mut buffer = [0u8; 64];
        let mut received = 0;
        while let Ok(read) = target.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            received += read;
        }
        assert_eq!(received, 3, "{role}: фрагмент дошёл до цели");

        let started = Instant::now();
        let close = wait_for_the_close(&path).await;
        let waited = started.elapsed();
        assert_eq!(close.by, ClosedBy::Bytetap, "{role}");
        assert_eq!(close.reason, "timeout", "{role}");
        assert!(
            waited >= Duration::from_secs(4),
            "{role}: пара доживала обратное направление {waited:?}"
        );
        handle.stop().await.expect("остановка");
    }
}

/// Сброс второго направления во время `PROXY_LINGER` — ошибка пары, но `close` остаётся
/// за стороной, закрывшей первой (M4-AC5, M4-AC16).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_during_the_linger_is_an_error_and_keeps_the_first_close() {
    for role in ROLES {
        let dir = tempfile::tempdir().expect("временная папка");
        let (listener, target_addr) = target_listener().await;
        let handle = start(dir.path(), target_addr, role).await;
        let listen = listen_addr(&handle);
        let path = handle.journal_path().to_path_buf();
        let (mut accepted, mut target) = pair(&listener, listen).await;

        accepted.write_all(b"ORU").await.expect("фрагмент");
        accepted.shutdown().await.expect("FIN принятой стороны");
        let mut buffer = [0u8; 8];
        timeout(WAIT, target.read(&mut buffer))
            .await
            .expect("{role}: фрагмент дошёл до цели")
            .expect("чтение цели");
        target.write_all(b"ACK").await.expect("ответ");
        // Ответ дочитан прокси, и цель сбрасывает соединение, пока пара доживает
        // обратное направление.
        sleep(Duration::from_millis(50)).await;
        target.set_zero_linger().expect("нулевой linger");
        drop(target);

        let close = wait_for_the_close(&path).await;
        assert_eq!(close.by, accepted_role(role).closed_by(), "{role}");
        assert_eq!(close.reason, "eof", "{role}");
        let outcome = read_all(&path).expect("журнал читается");
        let error = outcome
            .lines
            .iter()
            .find_map(|line| match &line.event {
                Event::Error(error) => Some(error),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{role}: ошибка второго направления не записана"));
        assert_eq!(error.conn, Some(1), "{role}: ошибка относится к паре");
        assert_eq!(
            error.code.as_deref(),
            Some("connection_read_failed"),
            "{role}: {error:?}"
        );
        handle.stop().await.expect("остановка");
    }
}
