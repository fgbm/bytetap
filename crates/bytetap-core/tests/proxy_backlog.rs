//! Очередь писателя журнала ограничена по байтам (NFR-5, M4-AC12).
//!
//! Канал писателя неограниченный (ADR 0001), поэтому предел считается по объёму: пока
//! журнал отстаёт, сессия копит не больше [`MAX_PENDING_BYTES`] и на половине предела
//! один раз предупреждает. Перейдя предел, `record` отказывает — это отказ журнала
//! (`journal_backlog`), а не рост памяти: пересылка останавливается, `session_end` нет,
//! остановка возвращает ошибку.
//!
//! Предел в 64 МиБ потоком за разумное время не пройти, поэтому тест опускает его хуком
//! `TEST_MAX_PENDING_BYTES`, а писатель замедляет `TEST_SYNC_DELAY_MS`. Оба хука глобальны
//! для процесса — тест в своём бинаре.

use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::Event;
use bytetap_core::journal::writer::{TEST_MAX_PENDING_BYTES, TEST_SYNC_DELAY_MS};
use bytetap_core::session::{Session, SessionConfig, journal_failure_reason};
use bytetap_core::transport::ProxyRole;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

/// Сколько тест ждёт обмена и отказа.
const WAIT: Duration = Duration::from_secs(20);

/// Предел очереди в тесте: поток переходит его за доли секунды.
const LIMIT: u64 = 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backlog_beyond_the_limit_is_a_journal_failure() {
    let dir = tempfile::tempdir().expect("временная папка");
    // Цель читает и выбрасывает: обратного потока нет, и журнал отстаёт сам по себе.
    let target = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("цель слушает");
    let target_addr = target.local_addr().expect("адрес цели");
    let sink = tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.expect("прокси подключился");
        let mut buffer = [0u8; 64 * 1024];
        while let Ok(read) = stream.read(&mut buffer).await {
            if read == 0 {
                break;
            }
        }
    });

    let (handle, mut feed) = Session::start_with_feed(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target_addr.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: std::net::SocketAddr = handle.local_addr().parse().expect("адрес");
    let path = handle.journal_path().to_path_buf();
    let mut failure = handle.journal_failure();

    // Писатель замедлен, а предел очереди мал: поток в 4 МиБ не успевает записаться.
    TEST_SYNC_DELAY_MS.store(200, Ordering::Relaxed);
    TEST_MAX_PENDING_BYTES.store(LIMIT, Ordering::Relaxed);

    let mut stream = TcpStream::connect(listen).await.expect("анализатор");
    let writer = tokio::spawn(async move {
        let chunk = vec![b'A'; 16 * 1024];
        for _ in 0..256 {
            if stream.write_all(&chunk).await.is_err() {
                break;
            }
        }
    });

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
    // Предупреждение о половине предела приходит раньше отказа, один раз.
    let mut warnings = 0;
    while let Ok(event) = feed.try_recv() {
        if let Event::Info(info) = event.as_ref()
            && info.code.as_deref() == Some("journal_backlog_warning")
        {
            warnings += 1;
        }
    }
    assert_eq!(warnings, 1, "предупреждение о растущей очереди одно");

    let err = handle
        .stop()
        .await
        .expect_err("остановка после отказа журнала");
    let reason = journal_failure_reason(&err).expect("причина отказа журнала в ошибке");
    assert_eq!(
        reason.code(),
        Some("journal_backlog"),
        "код отказа: {}",
        reason.message()
    );
    assert!(
        reason.message().contains("journal backlog"),
        "{}",
        reason.message()
    );
    let _ = writer.await;
    let _ = sink.await;
    TEST_SYNC_DELAY_MS.store(0, Ordering::Relaxed);
    TEST_MAX_PENDING_BYTES.store(0, Ordering::Relaxed);

    // Даём писателю закрыть файл и читаем его.
    sleep(Duration::from_millis(100)).await;
    let outcome = bytetap_core::journal::read_all(&path).expect("журнал читается");
    let last = outcome.lines.last().expect("журнал не пуст");
    assert_ne!(
        last.event.kind(),
        "session_end",
        "без журнала итогов сессии нет"
    );
    // После отказа писатель не принимает ни одного события: файл — чистый префикс записи,
    // и `close`, который сессия пишет при остановке, в него уже не попадает.
    assert!(
        !outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Close(_))),
        "после journal_backlog журнал не дописывает close"
    );
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Open(_))),
        "open пары записан до отказа"
    );
    let warnings = outcome
        .lines
        .iter()
        .filter(|line| {
            matches!(&line.event, Event::Info(info) if info.code.as_deref() == Some("journal_backlog_warning"))
        })
        .count();
    assert_eq!(warnings, 1, "предупреждение попало в журнал один раз");
}
