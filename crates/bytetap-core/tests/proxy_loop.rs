//! Петля прокси на себя: цель — адрес прослушивания bytetap (M4-AC6).
//!
//! Порт `0` выбирает система, а цель приходит строкой (`localhost`, `127.0.0.1` при
//! слушателе на всех адресах, `::`), поэтому проверка старта такую петлю не видит:
//! она узнаётся после подключения, по адресам концов соединения. Пара не создаётся,
//! `open` не пишется, а серия схлопывается в одно `error` с кодом `proxy_target_is_self`.
//!
//! Тест в своём бинаре: он занимает и отпускает порт, чтобы у цели и прослушивания был один
//! номер, а соседние тесты в том же процессе получили бы этот порт себе.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use bytetap_core::journal::{Event, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::ProxyRole;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(10);

/// Пауза прокси после неудачного подключения к цели.
const RETRY: Duration = Duration::from_millis(100);

/// Свободный порт на `host`: занимается и отпускается. `None` — адрес недоступен.
fn free_port(host: IpAddr) -> Option<u16> {
    let listener = std::net::TcpListener::bind(SocketAddr::new(host, 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Петля на себя: одно `error` с кодом петли, ни одного `open`, сессия жива.
///
/// `listen_host` — адрес прослушивания, `target_host` — как цель называет тот же порт.
/// `false` — окружение такую петлю не даёт (нет IPv6 или двойного стека).
async fn assert_a_loop_is_one_error(listen_host: IpAddr, target_host: &str) -> bool {
    let Some(port) = free_port(listen_host) else {
        return false;
    };
    let dir = tempfile::tempdir().expect("временная папка");
    let Ok((handle, mut feed)) = Session::start_with_feed(
        SessionConfig::proxy(
            SocketAddr::new(listen_host, port),
            format!("{target_host}:{port}"),
            ProxyRole::AnalyzerClient,
            RETRY,
        )
        .with_log_dir(dir.path()),
    )
    .await
    else {
        return false;
    };
    let path = handle.journal_path().to_path_buf();
    let come_to = SocketAddr::from(([127, 0, 0, 1], port));

    // Входящее приходит на порт прослушивания, а прокси подключается к цели — к себе же.
    let Ok(_first) = TcpStream::connect(come_to).await else {
        return false;
    };
    let mut events = Vec::new();
    let error = loop {
        let Ok(event) = timeout(WAIT, feed.recv()).await else {
            panic!("{listen_host} → {target_host}: петля не дала ошибки");
        };
        let event = event.expect("лента жива");
        events.push(event.clone());
        if let Event::Error(error) = event.as_ref() {
            break error.clone();
        }
    };
    assert_eq!(
        error.code.as_deref(),
        Some("proxy_target_is_self"),
        "{listen_host} → {target_host}: {error:?}"
    );
    assert!(error.conn.is_none(), "ошибка уровня сессии");

    // Второе входящее: петля уже найдена, пара не создаётся, а серия схлопывается.
    let Ok(_second) = TcpStream::connect(come_to).await else {
        return false;
    };
    while let Ok(Ok(event)) = timeout(Duration::from_millis(500), feed.recv()).await {
        events.push(event);
    }
    let errors = events
        .iter()
        .filter(|event| matches!(***event, Event::Error(_)))
        .count();
    assert_eq!(
        errors, 1,
        "{listen_host} → {target_host}: серия петли схлопнута в одно error"
    );
    assert!(
        !events.iter().any(|event| matches!(**event, Event::Open(_))),
        "петля не пишет open"
    );

    // Сессия жива: остановка штатная, соединений не было.
    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, 0, "пар не было");
    let outcome = read_all(&path).expect("журнал читается");
    let opens = outcome
        .lines
        .iter()
        .filter(|line| matches!(line.event, Event::Open(_)))
        .count();
    assert_eq!(opens, 0, "{listen_host} → {target_host}: open не пишется");
    let errors = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Error(error) => Some(error),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "в журнале одно error: {errors:?}");
    assert_eq!(errors[0].code.as_deref(), Some("proxy_target_is_self"));
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loop_to_the_listening_address_is_one_error_and_no_pair() {
    // Порт занимается и отпускается: цель и прослушивание — один номер. Цель приходит
    // строкой, поэтому проверка старта петлю не видит, её ловит подключение (M4-AC6).
    assert!(
        assert_a_loop_is_one_error(IpAddr::V4(Ipv4Addr::LOCALHOST), "localhost").await,
        "петля через имя узла на петлевом адресе"
    );
    // Слушатель на всех адресах: петля та же, а адрес цели — петлевой. Цель-адрес с тем
    // же портом на старте отвергается как ошибка настройки, строкой — ловится здесь.
    assert!(
        assert_a_loop_is_one_error(IpAddr::V4(Ipv4Addr::UNSPECIFIED), "localhost").await,
        "петля через имя узла при слушателе на всех адресах"
    );
    // `::` на двойном стеке принимает IPv4: семейства адресов разные, слушатель один.
    // Окружение без IPv6 или с выключенным двойным стеком случай пропускает.
    assert_a_loop_is_one_error(IpAddr::V6(Ipv6Addr::UNSPECIFIED), "127.0.0.1").await;
}
