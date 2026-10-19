//! Отказ журнала останавливает сессию (NFR-5, FR-20, ADR 0001, «Ошибка записи»).
//!
//! Тестовый хук `TEST_FAIL_AFTER_EVENTS` (feature `test-hooks`) ломает запись после
//! нескольких событий, как сломанный диск. После отказа сессия не отвечает анализатору,
//! подписчики получают `error` с причиной, а остановка возвращает ошибку с той же причиной.
//!
//! Хук глобальный для процесса, поэтому тест живёт в отдельном бинаре: другие
//! интеграционные тесты сессии он не задевает.

use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::Event;
use bytetap_core::journal::writer::TEST_FAIL_AFTER_EVENTS;
use bytetap_core::session::{Session, SessionConfig, journal_failure_reason};
use bytetap_testkit::fixtures_dir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Сколько тест ждёт один ACK.
const ACK_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journal_failure_stops_the_session_without_acks() {
    let dir = tempfile::tempdir().expect("временная папка");
    // `open` и обмен первого сообщения (входящий `data` и ответ) — три события;
    // отказ приходится на первое событие второго сообщения.
    TEST_FAIL_AFTER_EVENTS.store(3, Ordering::Relaxed);
    let handle =
        Session::start(SessionConfig::new("127.0.0.1:0".parse().unwrap()).with_log_dir(dir.path()))
            .await
            .expect("сессия");
    let mut events = handle.subscribe();
    let message = std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7"))
        .expect("фикстура analyzer_oru_r01.hl7");

    let mut stream = TcpStream::connect(handle.local_addr())
        .await
        .expect("подключение анализатора");

    // Первое сообщение: журнал ещё пишет, ACK приходит.
    stream.write_all(&message).await.expect("отправка");
    let ack = timeout(ACK_TIMEOUT, read_frame(&mut stream))
        .await
        .expect("ACK на первое сообщение")
        .expect("чтение ACK");
    assert!(!ack.is_empty(), "первое сообщение подтверждено");

    // Дальше шлём, пока сессия не перестанет отвечать: отказ журнала наступает
    // на втором сообщении, а до него ACK ещё возможен.
    for _ in 0..10 {
        if stream.write_all(&message).await.is_err() {
            break;
        }
        match timeout(ACK_TIMEOUT, read_frame(&mut stream)).await {
            Ok(Ok(frame)) if !frame.is_empty() => {}
            _ => break,
        }
    }

    // Подписчик получает `error` уровня сессии с причиной отказа.
    let mut failure = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Ok(event)) => {
                if let Event::Error(error) = &*event
                    && error.conn.is_none()
                    && error.message.contains("journal write failed")
                {
                    failure = Some(error.message.clone());
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {}
        }
    }
    let failure = failure.expect("подписчик узнал об отказе журнала");
    assert!(
        failure.contains("no ACKs are sent"),
        "причина описывает прекращение ответов: {failure}"
    );

    // После отказа подтверждений нет: соединение закрыто или молчит.
    if stream.write_all(&message).await.is_ok()
        && let Ok(Ok(frame)) = timeout(Duration::from_secs(1), read_frame(&mut stream)).await
    {
        assert!(
            frame.is_empty(),
            "после отказа журнала пришёл ACK: {frame:?}"
        );
    }

    let err = handle
        .stop()
        .await
        .expect_err("остановка после отказа журнала");
    let reason = journal_failure_reason(&err).expect("причина отказа журнала в ошибке");
    assert!(reason.contains("test hook"), "{reason}");
    TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);

    // `session_end` не пишется: журнал уже не принимает строк.
    let outcome = bytetap_core::journal::read_all(handle_path(&dir)).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let last = outcome.lines.last().expect("журнал не пуст");
    assert_ne!(
        last.event.kind(),
        "session_end",
        "без журнала итогов сессии нет"
    );
}

/// Путь к единственному журналу во временной папке.
fn handle_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let entry = std::fs::read_dir(dir.path())
        .expect("временная папка")
        .next()
        .expect("журнал создан")
        .expect("запись папки");
    entry.path()
}

/// Читает один ответ в рамке MLLP; пустой `Vec` — собеседник закрыл соединение.
async fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut frame = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte).await? == 0 {
            return Ok(frame);
        }
        frame.push(byte[0]);
        if frame.ends_with(&[0x1c, 0x0d]) {
            return Ok(frame);
        }
    }
}
