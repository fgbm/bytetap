//! M1-AC14 (NFR-6): пересылка ответа не ждёт журнал.
//!
//! Писатель журнала искусственно замедлен тестовым хуком ([`TEST_SYNC_DELAY_MS`],
//! feature `test-hooks`), а эмулятор всё равно получает ACK сразу. Затем остановка
//! дожидается замедленной фиксации: без этого тест не отличил бы «журнал быстрый» от
//! «ответ не ждёт журнал».

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytetap_core::journal::writer::{TEST_SYNC_DELAY_MS, TEST_SYNC_DELAYS};
use bytetap_core::journal::{Dir, Event, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_testkit::emulator::AnalyzerEmulator;
use bytetap_testkit::fixtures_dir;

/// Задержка одной фиксации журнала: на два порядка больше обмена по loopback.
const JOURNAL_DELAY: Duration = Duration::from_millis(1000);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ack_does_not_wait_for_the_journal() {
    let dir = tempfile::tempdir().expect("временная папка");
    let handle =
        Session::start(SessionConfig::new("127.0.0.1:0".parse().unwrap()).with_log_dir(dir.path()))
            .await
            .expect("сессия");

    let path = fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7");
    let message = std::fs::read(&path).expect("фикстура");
    TEST_SYNC_DELAYS.store(0, Ordering::Relaxed);
    TEST_SYNC_DELAY_MS.store(JOURNAL_DELAY.as_millis() as u64, Ordering::Relaxed);

    let started = Instant::now();
    let report = AnalyzerEmulator::new(handle.local_addr())
        .with_messages([message.clone()])
        .run()
        .await
        .expect("обмен");
    let exchange = started.elapsed();
    assert_eq!(report.acks.len(), 1, "ACK на сообщение");

    // Убеждаемся, что хук действительно сработал: иначе проверка ничего не значит.
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

    let journal_path = handle.journal_path().to_path_buf();
    let stopping = Instant::now();
    let stats = handle.stop().await.expect("остановка");
    let stop = stopping.elapsed();

    assert!(
        exchange < Duration::from_millis(700),
        "ACK пришёл за {exchange:?}, хотя писатель журнала замедлен на {JOURNAL_DELAY:?}"
    );
    assert!(
        stop > Duration::from_millis(400),
        "остановка должна была ждать замедленной фиксации журнала, а заняла {stop:?}"
    );
    assert_eq!(stats.bytes_analyzer_to_lis, message.len() as u64);
    assert!(stats.bytes_lis_to_analyzer > 0, "ACK тоже записан");

    let outcome = read_all(&journal_path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let last = outcome.lines.last().expect("журнал не пуст");
    assert!(matches!(last.event, Event::SessionEnd(_)));
    let data: Vec<&bytetap_core::journal::Data> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Data(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(data.len(), 2, "входящее сообщение и ответ");
    assert_eq!(data[0].dir, Dir::AnalyzerToLis);
    assert!(data[1].synthetic, "ответ автоответчика");
}
