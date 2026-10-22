//! Отказ журнала останавливает пересылку прокси, а не только запись (NFR-5, FR-20, M4-AC10).
//!
//! Тестовый хук `TEST_FAIL_AFTER_EVENTS` (feature `test-hooks`) ломает запись посреди
//! обмена. После отказа байты дальше не идут: обе стороны пары закрываются, `session_end`
//! не пишется, а остановка возвращает ошибку с причиной — как в остальных режимах.
//! Набор проходит по обеим топологиям: механика пары от роли не зависит (M4-AC16).
//!
//! Хук глобальный для процесса, поэтому тест живёт в отдельном бинаре и обе топологии
//! проверяет по очереди.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::Event;
use bytetap_core::journal::writer::{TEST_FAIL_AFTER_EVENTS, test_events_in_batches};
use bytetap_core::session::{Session, SessionConfig, journal_failure_reason};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// Сколько тест ждёт обмена и закрытия.
const WAIT: Duration = Duration::from_secs(10);

/// Обе топологии: цель слушает, принятая сторона приходит на порт bytetap.
const ROLES: [ProxyRole; 2] = [ProxyRole::AnalyzerClient, ProxyRole::AnalyzerServer];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journal_failure_stops_the_forwarding() {
    for role in ROLES {
        let dir = tempfile::tempdir().expect("временная папка");
        let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
            .await
            .expect("цель слушает")
            .with_behaviour(LisBehaviour::Echo)
            .with_timeout(WAIT);
        let lis_addr = lis.local_addr().expect("адрес цели");
        let lis_task = tokio::spawn(lis.run());

        let handle = Session::start(
            SessionConfig::proxy(
                "127.0.0.1:0".parse().unwrap(),
                lis_addr.to_string(),
                role,
                Duration::from_millis(100),
            )
            .with_log_dir(dir.path()),
        )
        .await
        .expect("сессия прокси");
        let listen: SocketAddr = handle.local_addr().parse().expect("адрес");
        let path = handle.journal_path().to_path_buf();
        let mut failure = handle.journal_failure();

        let mut stream = TcpStream::connect(listen).await.expect("принятая сторона");
        // Первый обмен должен пройти: журнал ещё пишет.
        stream.write_all(b"ORU0001").await.expect("первый фрагмент");
        let mut echo = [0u8; 7];
        timeout(WAIT, stream.read_exact(&mut echo))
            .await
            .unwrap_or_else(|_| panic!("{role}: эхо первого обмена не пришло"))
            .expect("первый обмен");
        assert_eq!(&echo, b"ORU0001");

        // Журнал асинхронный: `open` и `data` первого обмена могут ещё лежать в очереди
        // писателя, а хук ломает ту пачку, в которую они попадут. Ждём, пока `open`
        // окажется в файле, иначе проверка ниже увидела бы отказ раньше записи.
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
        .unwrap_or_else(|_| panic!("{role}: open первого обмена не дошёл до файла"));

        // Отказ на ближайшей пачке: `open` и оба `data` первого обмена уже записаны.
        TEST_FAIL_AFTER_EVENTS.store(test_events_in_batches() + 1, Ordering::Relaxed);

        // Дальше пересылка обязана остановиться. События должны дойти до писателя, поэтому
        // отправляем несколько фрагментов, а отказ ждём по метке, а не по числу кругов:
        // на загруженной машине писатель успевает за разное время.
        for _ in 0..100 {
            if stream.write_all(b"ORU0002").await.is_err() {
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
        .expect("{role}: сессия сообщила об отказе журнала");

        // Обе стороны пары закрыты: принятая сторона читает до конца, цель видит закрытие.
        // Сброс на закрытии — тоже признак того, что пара закрыта: в буфере могли остаться
        // невычитанные байты эха.
        let mut tail = Vec::new();
        match timeout(WAIT, stream.read_to_end(&mut tail)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == ErrorKind::ConnectionReset => {}
            Ok(Err(error)) => panic!("{role}: чтение остатка: {error}"),
            Err(_) => panic!("{role}: принятая сторона не увидела закрытие пары"),
        }
        timeout(WAIT, lis_task)
            .await
            .expect("{role}: цель увидела закрытие пары")
            .expect("задача цели")
            .expect("обмен цели");

        let err = handle
            .stop()
            .await
            .expect_err("остановка после отказа журнала");
        let reason = journal_failure_reason(&err).expect("причина отказа журнала в ошибке");
        assert!(
            reason.message().contains("test hook"),
            "{role}: {}",
            reason.message()
        );
        TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);

        // Даём писателю закрыть файл и читаем его.
        sleep(Duration::from_millis(50)).await;
        let outcome = bytetap_core::journal::read_all(&path).expect("журнал читается");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let last = outcome.lines.last().expect("журнал не пуст");
        assert_ne!(
            last.event.kind(),
            "session_end",
            "{role}: без журнала итогов сессии нет"
        );
        // `close` после отказа в файл уже не попадает: писатель его не принимает. Важно,
        // что пара открывалась и что итогов сессии нет.
        assert!(
            outcome
                .lines
                .iter()
                .any(|line| matches!(line.event, Event::Open(_))),
            "{role}: open пары записан до отказа"
        );
    }
}
