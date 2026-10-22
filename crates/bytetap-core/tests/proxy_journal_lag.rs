//! Пересылка прокси не ждёт журнал (NFR-6, M4-AC4).
//!
//! Писатель журнала искусственно замедлен тестовым хуком ([`TEST_SYNC_DELAY_MS`],
//! feature `test-hooks`), а обмен через прокси всё равно проходит сразу. Затем остановка
//! дожидается замедленной фиксации: без этого тест не отличил бы «журнал быстрый» от
//! «пересылка не ждёт журнал». Хук глобальный для процесса, поэтому тест в своём бинаре.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytetap_core::journal::writer::{TEST_SYNC_DELAY_MS, TEST_SYNC_DELAYS};
use bytetap_core::journal::{Dir, Event, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::fixtures_dir;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Задержка одной фиксации журнала: на два порядка больше обмена по loopback.
const JOURNAL_DELAY: Duration = Duration::from_millis(1000);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwarding_does_not_wait_for_the_journal() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Echo);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            lis_addr.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: std::net::SocketAddr = handle.local_addr().parse().expect("адрес");
    let path = handle.journal_path().to_path_buf();

    let message =
        std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7")).expect("фикстура");
    TEST_SYNC_DELAYS.store(0, Ordering::Relaxed);
    TEST_SYNC_DELAY_MS.store(JOURNAL_DELAY.as_millis() as u64, Ordering::Relaxed);

    let mut stream = TcpStream::connect(listen).await.expect("анализатор");
    let started = Instant::now();
    stream.write_all(&message).await.expect("сообщение");
    let mut echo = vec![0u8; message.len()];
    timeout(Duration::from_secs(5), stream.read_exact(&mut echo))
        .await
        .expect("эхо дошло")
        .expect("чтение эха");
    let exchange = started.elapsed();
    assert_eq!(echo, message, "пересылка прошла без изменений");

    // Хук действительно сработал: иначе проверка ничего не значит.
    let waiting = Instant::now();
    while TEST_SYNC_DELAYS.load(Ordering::Relaxed) == 0 {
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "писатель журнала так и не был замедлен"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Дальше задерживать нечего: проверяем, что остановка ждёт уже начатую фиксацию.
    TEST_SYNC_DELAY_MS.store(0, Ordering::Relaxed);
    let _ = stream.shutdown().await;

    let stopping = Instant::now();
    let stats = handle.stop().await.expect("остановка");
    let stop = stopping.elapsed();
    timeout(Duration::from_secs(5), lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");

    assert!(
        exchange < Duration::from_millis(700),
        "обмен через прокси занял {exchange:?}, хотя писатель журнала замедлен на {JOURNAL_DELAY:?}"
    );
    assert!(
        stop > Duration::from_millis(400),
        "остановка должна была ждать замедленной фиксации журнала, а заняла {stop:?}"
    );
    assert_eq!(stats.connections, 1);
    assert_eq!(stats.bytes_analyzer_to_lis, message.len() as u64);
    assert_eq!(
        stats.bytes_lis_to_analyzer,
        message.len() as u64,
        "эхо тоже записано"
    );

    let outcome = read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    assert!(matches!(
        outcome.lines.last().expect("последняя строка").event,
        Event::SessionEnd(_)
    ));
    let dirs: Vec<Dir> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Data(data) => Some(data.dir),
            _ => None,
        })
        .collect();
    assert!(dirs.contains(&Dir::AnalyzerToLis) && dirs.contains(&Dir::LisToAnalyzer));
}
