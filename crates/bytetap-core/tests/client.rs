//! TCP-клиент «к анализатору-серверу» на loopback (T-2, M3-AC1).
//!
//! Эмулятор-сервер из `bytetap-testkit` принимает соединение, шлёт синтетическое
//! сообщение MLLP из `tests/fixtures/hl7`, читает ACK и закрывает соединение; второе
//! подключение получает новый сценарий. Так проверяются переподключение, сквозная
//! нумерация `conn`, пауза между попытками и то, что «Стоп» её не ждёт.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use bytetap_core::journal::{Autoresponder, ClosedBy, Dir, Event, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_testkit::emulator::ack_msa;
use bytetap_testkit::fixtures_dir;
use bytetap_testkit::server::AnalyzerServer;
use tokio::time::{sleep, timeout};

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("hl7").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Байты одного направления одного соединения, склеенные из `b64`.
fn recorded_bytes(path: &Path, conn: u32, dir: Dir, synthetic: Option<bool>) -> Vec<u8> {
    let outcome = read_all(path).expect("журнал читается");
    let mut bytes = Vec::new();
    for line in outcome.lines {
        if let Event::Data(data) = line.event
            && data.conn == conn
            && data.dir == dir
            && synthetic.is_none_or(|flag| flag == data.synthetic)
        {
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 декодируется"),
            );
        }
    }
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_reconnects_after_the_analyzer_hangs_up() {
    let dir = tempfile::tempdir().expect("временная папка");
    let message = fixture("analyzer_oru_r01.hl7");

    let server = AnalyzerServer::bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("сервер анализатора");
    let addr = server.local_addr().expect("адрес сервера");
    let server = tokio::spawn(
        server
            .with_connection([message.clone()])
            .with_connection([message.clone()])
            .with_timeout(Duration::from_secs(10))
            .run(),
    );

    let handle = Session::start(
        SessionConfig::tcp_client(addr.to_string(), Duration::from_secs(1))
            .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия стартует без анализатора");
    assert_eq!(handle.local_addr(), addr.to_string());
    let path = handle.journal_path().to_path_buf();

    let report = timeout(Duration::from_secs(20), server)
        .await
        .expect("сервер успел принять оба соединения")
        .expect("задача сервера")
        .expect("обмен");
    assert_eq!(report.connections, 2, "анализатор принял два соединения");
    assert_eq!(report.acks.len(), 2);
    for acks in &report.acks {
        assert_eq!(acks.len(), 1);
        assert_eq!(
            ack_msa(&acks[0]),
            Some(("AA".to_owned(), "MSG0001".to_owned()))
        );
    }
    let pause = report.accepted_at[1] - report.accepted_at[0];
    assert!(
        pause >= Duration::from_millis(900),
        "пауза переподключения не меньше заданной: {pause:?}"
    );

    // Второе `close` должно попасть в журнал до остановки: иначе его напишет «Стоп».
    for _ in 0..200 {
        let outcome = read_all(&path).expect("журнал читается");
        if outcome
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Close(_)))
            .count()
            >= 2
        {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, 2, "номера соединений сквозные");

    let outcome = read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(header.mode, Mode::TcpClient);
    assert_eq!(header.autoresponder, Autoresponder::Hl7Mllp);
    assert_eq!(header.settings["connect"], addr.to_string());
    assert_eq!(header.settings["reconnect_sec"], 1);
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("-tcp_client.jsonl")),
        "имя файла режима: {}",
        path.display()
    );

    let mut opens = Vec::new();
    for line in &outcome.lines {
        match &line.event {
            Event::Open(open) => {
                opens.push(open.conn);
                assert_eq!(open.peer, addr.to_string(), "peer — адрес анализатора");
                assert!(!open.local.is_empty(), "local — локальный адрес сокета");
            }
            Event::Close(close) => {
                assert_eq!(close.by, ClosedBy::Analyzer);
                assert_eq!(close.reason, "eof");
            }
            _ => {}
        }
    }
    assert_eq!(opens, [1, 2], "два open с разными conn");

    for conn in [1, 2] {
        assert_eq!(
            recorded_bytes(&path, conn, Dir::AnalyzerToLis, Some(false)),
            message,
            "соединение {conn} записало отправленное байт в байт"
        );
        let answers = recorded_bytes(&path, conn, Dir::LisToAnalyzer, Some(true));
        assert!(!answers.is_empty(), "ответы помечены synthetic");
        assert_eq!(
            ack_msa(&answers).map(|(code, _)| code),
            Some("AA".to_owned()),
            "в журнале тот же ACK, что получил анализатор"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_analyzer_is_an_error_and_does_not_hold_the_stop() {
    // Порт занят сокетом без `listen`: подключаться некуда, но сессия обязана стартовать.
    // Сокет держится до конца проверки: иначе порт достался бы другому тесту (флак M4).
    let socket = tokio::net::TcpSocket::new_v4().expect("сокет");
    socket
        .bind("127.0.0.1:0".parse().unwrap())
        .expect("свободный порт");
    let addr = socket.local_addr().expect("адрес");

    let dir = tempfile::tempdir().expect("временная папка");
    let (handle, mut feed) = Session::start_with_feed(
        SessionConfig::tcp_client(addr.to_string(), Duration::from_secs(60))
            .with_log_dir(dir.path()),
    )
    .await
    .expect("старт не требует доступного анализатора");

    let error = timeout(Duration::from_secs(10), async {
        loop {
            let event = feed.recv().await.expect("лента жива");
            if let Event::Error(error) = event.as_ref() {
                break error.clone();
            }
        }
    })
    .await
    .expect("ошибка подключения пришла");
    assert_eq!(error.code.as_deref(), Some("connect_failed"), "{error:?}");

    let stats = timeout(Duration::from_secs(2), handle.stop())
        .await
        .expect("остановка не ждёт интервал переподключения")
        .expect("остановка штатная");
    assert_eq!(stats.connections, 0);
    drop(socket);
}
