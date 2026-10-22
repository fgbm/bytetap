//! Фильтр по адресу входящего в прокси (FR-8): принимается только настроенный адрес.
//!
//! Без фильтра любой узел сети, достучавшийся до порта, говорит с ЛИС от имени
//! анализатора. Поэтому входящее с чужого адреса закрывается до подключения к цели:
//! пары нет, `open` не пишется, а отказ схлопывается как ошибка источника.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytetap_core::journal::{Event, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// Пауза прокси после отказа.
const RETRY: Duration = Duration::from_millis(100);

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(10);

/// Адрес, которого нет на этой машине: `192.0.2.0/24` — TEST-NET-1 из RFC 5737.
const FOREIGN: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 7));

/// Входящее с чужого адреса закрывается сразу, а сессия остаётся жива.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_address_is_rejected_with_its_own_code() {
    let dir = tempfile::tempdir().expect("временная папка");
    // Цель — слушатель без обработчика: важно, что к нему никто не постучался.
    let target = tokio::net::TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .await
        .expect("цель слушает");
    let target_addr = target.local_addr().expect("адрес цели");

    let handle = Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target_addr.to_string(),
            ProxyRole::AnalyzerClient,
            RETRY,
        )
        .with_allow_from(Some(FOREIGN))
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: SocketAddr = handle.local_addr().parse().expect("адрес прослушивания");
    let path = handle.journal_path().to_path_buf();

    // Два входящих подряд: оба отвергнуты, серия схлопывается в одно `error`.
    for _ in 0..2 {
        let mut stream = TcpStream::connect(listen).await.expect("сторона пришла");
        let mut buffer = [0u8; 8];
        let read = timeout(WAIT, stream.read(&mut buffer))
            .await
            .expect("входящее закрылось сразу")
            .expect("чтение");
        assert_eq!(read, 0, "отвергнутое соединение закрывается без байтов");
    }
    assert!(
        timeout(Duration::from_millis(300), target.accept())
            .await
            .is_err(),
        "к цели bytetap не подключается"
    );

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
    assert_eq!(errors.len(), 1, "серия отвергнутых схлопнута: {errors:?}");
    assert_eq!(errors[0].code.as_deref(), Some("proxy_peer_rejected"));
    assert!(
        errors[0].message.contains("192.0.2.7"),
        "адрес отвергнутого — в событии: {errors:?}"
    );
    let opens = outcome
        .lines
        .iter()
        .filter(|line| matches!(line.event, Event::Open(_)))
        .count();
    assert_eq!(opens, 0, "пары нет — нет open");
    // Сессия жива до остановки и знает, что серия не кончилась.
    assert!(
        outcome.lines.iter().any(|line| matches!(
            &line.event,
            Event::Info(info) if info.code.as_deref() == Some("source_failing_at_stop")
                && info.repeats == Some(1)
        )),
        "серия повторов записана перед session_end"
    );
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::SessionEnd(_))),
        "остановка штатная"
    );
    let header = outcome
        .lines
        .iter()
        .find_map(|line| match &line.event {
            Event::Session(session) => Some(session),
            _ => None,
        })
        .expect("заголовок сессии");
    assert_eq!(header.mode, Mode::Proxy);
    assert_eq!(
        header.settings["allow_from"],
        serde_json::json!("192.0.2.7"),
        "фильтр записан в настройках заголовка"
    );
}

/// Разрешённый адрес проходит: фильтр не мешает обычной паре.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_allowed_address_reaches_the_target() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_connections(1)
        .with_behaviour(LisBehaviour::Echo);
    let target = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target.to_string(),
            ProxyRole::AnalyzerClient,
            RETRY,
        )
        .with_allow_from(Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)))
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: SocketAddr = handle.local_addr().parse().expect("адрес прослушивания");
    let path = handle.journal_path().to_path_buf();

    let mut stream = TcpStream::connect(listen).await.expect("анализатор пришёл");
    stream
        .write_all(b"MSH|^~\\&|A|L|LIS|LAB|20261022101500||ORU^R01|1|P|2.3.1\r")
        .await
        .expect("запрос");
    let mut buffer = [0u8; 256];
    let read = timeout(WAIT, stream.read(&mut buffer))
        .await
        .expect("ответ пришёл")
        .expect("чтение ответа");
    assert!(read > 0, "эхо цели вернулось через прокси");
    stream.shutdown().await.expect("FIN стороны");

    // `open` пары в журнале: пара создана, а не отвергнута.
    for _ in 0..200 {
        if read_all(&path).is_ok_and(|outcome| {
            outcome
                .lines
                .iter()
                .any(|line| matches!(line.event, Event::Open(_)))
        }) {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    handle.stop().await.expect("остановка");
    timeout(WAIT, lis_task)
        .await
        .expect("цель закончила")
        .expect("задача цели")
        .expect("обмен цели");
    let outcome = read_all(&path).expect("журнал читается");
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Open(_))),
        "разрешённая сторона даёт пару"
    );
    assert!(
        !outcome
            .lines
            .iter()
            .any(|line| matches!(&line.event, Event::Error(error)
                if error.code.as_deref() == Some("proxy_peer_rejected"))),
        "отказов по адресу нет"
    );
}

/// Чужой адрес на этой машине: так проверяется фильтр. `127.0.0.0/8` — петлевые адреса,
/// и второй из них есть только в Linux; другие среды случай пропускают.
async fn foreign_peer(to: SocketAddr) -> Option<TcpStream> {
    let socket = tokio::net::TcpSocket::new_v4().ok()?;
    socket.bind(SocketAddr::from(([127, 0, 0, 2], 0))).ok()?;
    socket.connect(to).await.ok()
}

/// Отвергнутая сторона, затем разрешённая в той же сессии: отказ схлопывается, серия
/// восстановлена (`source_recovered`), а разрешённая сторона даёт пару (FR-8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_peer_is_recovered_by_the_allowed_one() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_connections(1)
        .with_behaviour(LisBehaviour::Echo);
    let target = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = Session::start(
        SessionConfig::proxy(
            "0.0.0.0:0".parse().unwrap(),
            target.to_string(),
            ProxyRole::AnalyzerClient,
            RETRY,
        )
        .with_allow_from(Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)))
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: SocketAddr = handle.local_addr().parse().expect("адрес прослушивания");
    let path = handle.journal_path().to_path_buf();

    // Два чужих входа подряд: серия повторов, а потом восстановление.
    for _ in 0..2 {
        let Some(mut stream) =
            foreign_peer(SocketAddr::from(([127, 0, 0, 2], listen.port()))).await
        else {
            handle.stop().await.expect("остановка");
            return;
        };
        let mut buffer = [0u8; 8];
        let read = timeout(WAIT, stream.read(&mut buffer))
            .await
            .expect("чужое входящее закрылось")
            .expect("чтение");
        assert_eq!(read, 0, "чужое соединение закрывается без байтов");
    }

    // Разрешённый адрес приходит следом и сразу получает пару. Порт слушателя занят на
    // всех адресах, а подключаться надо к конкретному: `0.0.0.0` как адрес цели годится
    // не везде.
    let allowed = SocketAddr::from(([127, 0, 0, 1], listen.port()));
    let mut stream = TcpStream::connect(allowed)
        .await
        .expect("анализатор пришёл");
    stream
        .write_all(b"MSH|^~\\&|A|L|LIS|LAB|20261022101500||ORU^R01|1|P|2.3.1\r")
        .await
        .expect("запрос");
    let mut buffer = [0u8; 256];
    let read = timeout(WAIT, stream.read(&mut buffer))
        .await
        .expect("ответ пришёл")
        .expect("чтение ответа");
    assert!(read > 0, "эхо цели вернулось через прокси");
    stream.shutdown().await.expect("FIN стороны");

    for _ in 0..200 {
        if read_all(&path).is_ok_and(|outcome| {
            outcome
                .lines
                .iter()
                .any(|line| matches!(line.event, Event::Open(_)))
        }) {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    handle.stop().await.expect("остановка");
    timeout(WAIT, lis_task)
        .await
        .expect("цель закончила")
        .expect("задача цели")
        .expect("обмен цели");

    let outcome = read_all(&path).expect("журнал читается");
    let errors: Vec<_> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Error(error) => Some(error),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1, "серия отвергнутых схлопнута: {errors:?}");
    assert_eq!(errors[0].code.as_deref(), Some("proxy_peer_rejected"));
    assert_eq!(
        errors[0].peer.as_deref(),
        Some("127.0.0.2"),
        "адрес отвергнутого — отдельным полем события, а не в английском тексте"
    );
    assert!(
        outcome.lines.iter().any(|line| matches!(
            &line.event,
            Event::Info(info) if info.code.as_deref() == Some("source_recovered")
                && info.repeats == Some(1)
        )),
        "серия восстановлена разрешённой стороной: {:?}",
        outcome.lines
    );
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Open(_))),
        "разрешённая сторона даёт пару"
    );
}
