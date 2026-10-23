//! Пересылка прослушки не ждёт журнал (NFR-6, M5-AC4, M5-AC5).
//!
//! Писатель журнала искусственно замедлен тестовым хуком ([`TEST_SYNC_DELAY_MS`], feature
//! `test-hooks`), а обмен через пару портов всё равно проходит сразу. Затем остановка
//! дожидается замедленной фиксации: без этого тест не отличил бы «журнал быстрый» от
//! «пересылка не ждёт журнал». Хук глобальный для процесса, поэтому тест в своём бинаре.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytetap_core::journal::writer::{TEST_SYNC_DELAY_MS, TEST_SYNC_DELAYS};
use bytetap_core::journal::{Dir, Event, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};
use bytetap_core::transport::{reset_virtual_monitor, virtual_monitor};
use bytetap_testkit::fixtures_dir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::time::timeout;

/// Задержка одной фиксации журнала: на порядки больше обмена через память.
const JOURNAL_DELAY: Duration = Duration::from_millis(1000);

/// Снимает хуки при выходе из теста, даже если проверка упала.
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        TEST_SYNC_DELAY_MS.store(0, Ordering::Relaxed);
        reset_virtual_monitor();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwarding_does_not_wait_for_the_journal() {
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let (source, mut ends) = virtual_monitor(
        SerialSettings::new("COM3", 9600, SerialFormat::EIGHT_N_ONE),
        SerialSettings::new("COM4", 9600, SerialFormat::EIGHT_N_ONE),
        Duration::from_millis(100),
        1,
    )
    .expect("хук свободен");
    let handle = Session::start(
        SessionConfig::com_monitor(
            SerialSettings::new("COM3", 9600, SerialFormat::EIGHT_N_ONE),
            SerialSettings::new("COM4", 9600, SerialFormat::EIGHT_N_ONE),
            Duration::from_millis(100),
        )
        .with_source(source)
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прослушки");
    let path = handle.journal_path().to_path_buf();
    let mut pair = ends.pop().expect("пара");

    let message =
        std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7")).expect("фикстура");
    TEST_SYNC_DELAYS.store(0, Ordering::Relaxed);
    TEST_SYNC_DELAY_MS.store(JOURNAL_DELAY.as_millis() as u64, Ordering::Relaxed);

    // Анализатор отправляет сообщение, ЛИС возвращает его же: журнал замедлен, обмен — нет.
    let started = Instant::now();
    pair.analyzer.write_all(&message).await.expect("сообщение");
    pair.analyzer.flush().await.expect("сброс");
    let mut relayed = vec![0u8; message.len()];
    timeout(Duration::from_secs(5), pair.lis.read_exact(&mut relayed))
        .await
        .expect("сообщение дошло до ЛИС")
        .expect("чтение у ЛИС");
    pair.lis.write_all(&relayed).await.expect("ответ ЛИС");
    pair.lis.flush().await.expect("сброс ответа");
    let mut echo = vec![0u8; message.len()];
    timeout(Duration::from_secs(5), pair.analyzer.read_exact(&mut echo))
        .await
        .expect("ответ дошёл до анализатора")
        .expect("чтение ответа");
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

    let stopping = Instant::now();
    let stats = handle.stop().await.expect("остановка");
    let stop = stopping.elapsed();

    assert!(
        exchange < Duration::from_millis(700),
        "обмен через прослушку занял {exchange:?}, хотя писатель журнала замедлен на {JOURNAL_DELAY:?}"
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
        "ответ тоже записан"
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
