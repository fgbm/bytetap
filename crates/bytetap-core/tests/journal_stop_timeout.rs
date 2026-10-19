//! Остановка не ждёт зависшую фиксацию журнала вечно (NFR-5, FR-20).
//!
//! Писатель журнала замедлен хуком `TEST_SYNC_DELAY_MS` сильнее, чем предел ожидания
//! `TEST_JOURNAL_SHUTDOWN_MS` (feature `test-hooks`: в обычной сборке предел — десять
//! секунд). `stop` обязан вернуть ошибку «journal not flushed», а не висеть, пока
//! медленный диск соберётся ответить.
//!
//! Хуки глобальные для процесса, поэтому тест живёт отдельным бинаром.

use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::writer::{TEST_SYNC_DELAY_MS, TEST_SYNC_DELAYS};
use bytetap_core::session::{Session, SessionConfig, TEST_JOURNAL_SHUTDOWN_MS};
use bytetap_testkit::emulator::AnalyzerEmulator;
use bytetap_testkit::fixtures_dir;

/// Предел ожидания записи журнала при остановке.
const STOP_TIMEOUT: Duration = Duration::from_millis(50);

/// Задержка одной фиксации: заметно больше предела ожидания.
const JOURNAL_DELAY_MS: u64 = 1000;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_journal_fails_the_stop_instead_of_holding_it() {
    let dir = tempfile::tempdir().expect("временная папка");
    let handle =
        Session::start(SessionConfig::new("127.0.0.1:0".parse().unwrap()).with_log_dir(dir.path()))
            .await
            .expect("сессия");
    let message = std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7"))
        .expect("фикстура analyzer_oru_r01.hl7");

    TEST_SYNC_DELAYS.store(0, Ordering::Relaxed);
    TEST_SYNC_DELAY_MS.store(JOURNAL_DELAY_MS, Ordering::Relaxed);
    TEST_JOURNAL_SHUTDOWN_MS.store(
        u64::try_from(STOP_TIMEOUT.as_millis()).unwrap(),
        Ordering::Relaxed,
    );

    let report = AnalyzerEmulator::new(handle.local_addr().parse().expect("адрес сессии"))
        .with_messages([message])
        .run()
        .await
        .expect("обмен");
    assert_eq!(report.acks.len(), 1, "ACK на сообщение");

    // Ждём, пока писатель действительно уснёт в фиксации: только тогда `stop` не сможет
    // уложиться в предел ожидания. Иначе проверка ничего не значит.
    let waiting = std::time::Instant::now();
    while TEST_SYNC_DELAYS.load(Ordering::Relaxed) == 0 {
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "писатель журнала так и не был замедлен"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let stopping = std::time::Instant::now();
    let err = handle
        .stop()
        .await
        .expect_err("остановка не дождалась журнала");
    let stop = stopping.elapsed();
    TEST_SYNC_DELAY_MS.store(0, Ordering::Relaxed);
    TEST_JOURNAL_SHUTDOWN_MS.store(0, Ordering::Relaxed);

    assert!(
        err.to_string().contains("journal not flushed"),
        "ошибка называет причину: {err}"
    );
    assert!(
        stop < Duration::from_millis(JOURNAL_DELAY_MS),
        "остановка ждала {stop:?}, хотя предел — {STOP_TIMEOUT:?}"
    );
}
