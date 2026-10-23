//! Порядок закрытия пары прокси: `close` называет сторону, закончившуюся первой (M4-AC5, FR-7).
//!
//! Сторону в `close` выбирает не тот насос, чья задача вернулась первой, а тот, кто увидел
//! конец своей стороны раньше: выход насоса ждёт `shutdown` обратной стороны, поэтому на
//! загруженном раннере первым возвращается та задача, которую отпустил планировщик, и в
//! журнал попадала не та сторона. Тест разводит два порядка: конец стороны анализатора
//! задержан хуком `TEST_PUMP_EOF_DELAY_SIDE` (feature `test-hooks`), поэтому её задача
//! возвращается последней, а `close` обязан назвать всё равно её.
//!
//! Хуки глобальны для процесса, поэтому проверка живёт в своём бинаре.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytetap_core::journal::{Close, ClosedBy, Event, read_all};
use bytetap_core::session::{
    Session, SessionConfig, SessionHandle, TEST_PUMP_EOF_DELAY_MS, TEST_PUMP_EOF_DELAY_SIDE,
};
use bytetap_core::transport::ProxyRole;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(10);

/// Задержка выхода насоса анализатора: задача стороны ЛИС успевает вернуться первой.
const DELAY_MS: u64 = 200;

/// Пауза прокси после неудачного подключения к цели.
const RETRY: Duration = Duration::from_millis(100);

/// Снимает хуки при выходе из теста, даже если проверка упала.
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        TEST_PUMP_EOF_DELAY_MS.store(0, Ordering::Relaxed);
        TEST_PUMP_EOF_DELAY_SIDE.store(0, Ordering::Relaxed);
    }
}

/// Сессия прокси с журналом во временной папке.
async fn start(dir: &Path, target: SocketAddr, role: ProxyRole) -> SessionHandle {
    Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target.to_string(),
            role,
            RETRY,
        )
        .with_log_dir(dir),
    )
    .await
    .expect("сессия прокси")
}

fn listen_addr(handle: &SessionHandle) -> SocketAddr {
    handle.local_addr().parse().expect("адрес прослушивания")
}

/// Цель пары: слушает адрес, к которому подключается прокси.
async fn target_listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("цель слушает");
    let addr = listener.local_addr().expect("адрес цели");
    (listener, addr)
}

/// Пара целиком: принятая сторона пришла, прокси подключился к цели.
async fn pair(listener: &TcpListener, listen: SocketAddr) -> (TcpStream, TcpStream) {
    let accepted = TcpStream::connect(listen).await.expect("принятая сторона");
    let (target, _) = timeout(WAIT, listener.accept())
        .await
        .expect("прокси подключился к цели")
        .expect("приём цели");
    (accepted, target)
}

/// Ждёт событие `close` пары и возвращает его.
async fn wait_for_the_close(path: &Path) -> Close {
    for _ in 0..500 {
        let outcome = read_all(path).expect("журнал читается");
        let closes: Vec<Close> = outcome
            .lines
            .iter()
            .filter_map(|line| match &line.event {
                Event::Close(close) => Some(close.clone()),
                _ => None,
            })
            .collect();
        if let Some(close) = closes.first() {
            assert_eq!(closes.len(), 1, "close один раз на пару");
            return close.clone();
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("close пары не появился в журнале");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_names_the_side_that_ended_first_not_the_task_that_returned_first() {
    let _hooks = HookGuard;
    // Задержан выход насоса анализатора: сторона ЛИС закроется второй, но её задача
    // вернётся первой — так делает планировщик на загруженном раннере.
    TEST_PUMP_EOF_DELAY_MS.store(DELAY_MS, Ordering::Relaxed);
    TEST_PUMP_EOF_DELAY_SIDE.store(1, Ordering::Relaxed);

    let dir = tempfile::tempdir().expect("временная папка");
    let (listener, target_addr) = target_listener().await;
    let handle = start(dir.path(), target_addr, ProxyRole::AnalyzerClient).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();
    let (mut accepted, mut target) = pair(&listener, listen).await;

    // Анализатор закрывает первым: `FIN` уходит цели полузакрытием, а задача его насоса
    // возвращается только через [`DELAY_MS`].
    accepted.write_all(b"ORU").await.expect("фрагмент");
    accepted.shutdown().await.expect("FIN анализатора");
    let mut forwarded = Vec::new();
    timeout(WAIT, target.read_to_end(&mut forwarded))
        .await
        .expect("FIN дошёл до цели")
        .expect("чтение цели");
    assert_eq!(forwarded, b"ORU", "фрагмент переслан до закрытия пары");

    // Цель закрывает второй, и её насос возвращается первым.
    target.shutdown().await.expect("FIN цели");

    let close = wait_for_the_close(&path).await;
    assert_eq!(
        close.by,
        ClosedBy::Analyzer,
        "пару закрыл анализатор: его конец увидели первым"
    );
    assert_eq!(close.reason, "eof");
    handle.stop().await.expect("остановка");
}
