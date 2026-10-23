//! Отказ журнала останавливает пересылку прослушки (NFR-5, FR-20, M5-AC7).
//!
//! Тестовый хук `TEST_FAIL_AFTER_EVENTS` (feature `test-hooks`) ломает запись посреди
//! обмена. После отказа байты дальше не идут: оба порта закрываются, `session_end`
//! не пишется, а остановка возвращает ошибку с причиной — как в остальных режимах.
//!
//! Хук глобальный для процесса, поэтому тест живёт в отдельном бинаре: другие
//! интеграционные тесты он не задевает. Пара «портов» — виртуальная (M5-AC8): настоящих
//! COM-портов в CI нет.

use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::Event;
use bytetap_core::journal::writer::{TEST_FAIL_AFTER_EVENTS, test_events_in_batches};
use bytetap_core::session::{Session, SessionConfig, journal_failure_reason};
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};
use bytetap_core::transport::{reset_virtual_monitor, virtual_monitor};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::time::{sleep, timeout};

/// Сколько тест ждёт обмена и отказа.
const WAIT: Duration = Duration::from_secs(10);

/// Снимает хуки при выходе из теста, даже если проверка упала.
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);
        reset_virtual_monitor();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journal_failure_stops_the_monitor_forwarding() {
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
    let mut failure = handle.journal_failure();
    let mut pair = ends.pop().expect("пара");

    // Первый обмен проходит: журнал ещё пишет.
    pair.analyzer
        .write_all(b"ENQ")
        .await
        .expect("первый фрагмент");
    pair.analyzer.flush().await.expect("сброс");
    let mut echo = [0u8; 3];
    timeout(WAIT, pair.lis.read_exact(&mut echo))
        .await
        .expect("пересылка первого обмена")
        .expect("чтение");
    assert_eq!(&echo, b"ENQ");

    // Журнал асинхронный: `open` и `data` первого обмена могут ещё лежать в очереди
    // писателя, а хук ломает ту пачку, в которую они попадут.
    timeout(WAIT, async {
        loop {
            let outcome = bytetap_core::journal::read_all(&path).expect("журнал читается");
            if outcome
                .lines
                .iter()
                .any(|line| matches!(line.event, Event::Open(_)))
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("open первого обмена дошёл до файла");

    // Отказ на ближайшей пачке: `open` и оба `data` первого обмена уже записаны.
    TEST_FAIL_AFTER_EVENTS.store(test_events_in_batches() + 1, Ordering::Relaxed);

    // Дальше пересылка обязана остановиться; отказ ждём по метке, а не по числу кругов.
    for _ in 0..100 {
        if pair.analyzer.write_all(b"ACK").await.is_err() {
            break;
        }
    }
    timeout(WAIT, async {
        loop {
            if failure.borrow_and_update().is_some() {
                break;
            }
            if failure.changed().await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("сессия сообщила об отказе журнала");

    // Оба порта закрыты: тестовые концы читают до конца.
    let mut tail = Vec::new();
    timeout(WAIT, pair.lis.read_to_end(&mut tail))
        .await
        .expect("ЛИС увидела закрытие обоих портов")
        .expect("чтение остатка");
    let mut tail = Vec::new();
    timeout(WAIT, pair.analyzer.read_to_end(&mut tail))
        .await
        .expect("анализатор увидел закрытие обоих портов")
        .expect("чтение остатка");

    let err = handle
        .stop()
        .await
        .expect_err("остановка после отказа журнала");
    let reason = journal_failure_reason(&err).expect("причина отказа журнала в ошибке");
    assert!(
        reason.message().contains("test hook"),
        "{}",
        reason.message()
    );

    // Писатель закрывает файл: итогов сессии в нём нет, а пара открывалась.
    sleep(Duration::from_millis(50)).await;
    let outcome = bytetap_core::journal::read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let last = outcome.lines.last().expect("журнал не пуст");
    assert_ne!(
        last.event.kind(),
        "session_end",
        "без журнала итогов сессии нет"
    );
    let outcome = bytetap_core::journal::read_all(&path).expect("журнал читается");
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Open(_))),
        "open пары записан до отказа"
    );
}
