//! Отказ журнала виден сессии без следующего события (NFR-5, FR-20, ADR 0001).
//!
//! Писатель сообщает об отказе отдельным сигналом ([`Journal::failed`]), а не только
//! `false` из `Journal::record`: если анализатор замолчал ровно в момент отказа,
//! следующего события не будет, и по одному `record` сессия ждала бы его бесконечно.
//! Тест проверяет именно этот случай: после отказа по соединению не идёт ни одного байта,
//! а лента уже сообщает об ошибке — до того, как кто-либо вызвал `stop`.
//!
//! Хук `TEST_FAIL_AFTER_EVENTS` глобальный для процесса, поэтому тест живёт отдельным
//! бинаром: соседние интеграционные тесты он не задевает.

use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::Event;
use bytetap_core::journal::writer::TEST_FAIL_AFTER_EVENTS;
use bytetap_core::session::{Session, SessionConfig, journal_failure_reason};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Сколько тест ждёт сигнал отказа, не отправляя ничего анализатором.
const FAILURE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journal_failure_is_reported_without_further_traffic() {
    let dir = tempfile::tempdir().expect("временная папка");
    // Первая же пачка (`open` подключившегося анализатора) отказывает: предел 0 событий.
    TEST_FAIL_AFTER_EVENTS.store(0, Ordering::Relaxed);
    let handle =
        Session::start(SessionConfig::new("127.0.0.1:0".parse().unwrap()).with_log_dir(dir.path()))
            .await
            .expect("сессия");
    let mut events = handle.subscribe();
    let failure_watch = handle.journal_failure();

    // Анализатор подключается и молчит: после `open` в сессии не будет ни одного события,
    // которое могло бы сообщить об отказе через `record`.
    let _stream = TcpStream::connect(handle.local_addr())
        .await
        .expect("подключение анализатора");

    let message = timeout(FAILURE_TIMEOUT, async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Event::Error(error) = &*event
                        && error.conn.is_none()
                        && error.message.contains("journal write failed")
                    {
                        return error.message.clone();
                    }
                }
                Err(err) => panic!("лента закрылась до сообщения об отказе: {err}"),
            }
        }
    })
    .await
    .expect("подписчик узнал об отказе журнала без следующего события");

    assert!(
        message.contains("no ACKs are sent"),
        "причина описывает прекращение ответов: {message}"
    );
    // Тот же отказ виден `capture`: по этой метке CLI заканчивает ленту до Ctrl+C.
    assert!(
        failure_watch.borrow().is_some(),
        "метка отказа журнала выставлена до `stop`"
    );

    // Никто не отправлял анализатору ничего: подтверждений после отказа нет.
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
    TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);
}
