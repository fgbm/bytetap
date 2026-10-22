//! Подключение к цели прокси: отмена `next`, срок и параллельность (M4-AC6, M4-AC16).
//!
//! Сессия вызывает `source.next()` в `select!`, поэтому отмена приходит в произвольный
//! момент: комментарий, «Стоп», отказ журнала, закрытие соседней пары. Отмена не должна
//! терять ни принятое соединение, ни начатое подключение — иначе пара осталась бы без
//! `open` и без `error`.
//!
//! Окно подключения удлиняет тестовый хук `TEST_PROXY_CONNECT_DELAY_MS`, а срок сокращает
//! `TEST_PROXY_CONNECT_TIMEOUT_MS` (feature `test-hooks`). Оба хука глобальны для процесса,
//! поэтому тесты этого файла идут по очереди под одной меткой.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytetap_core::journal::{Dir, Event, read_all};
use bytetap_core::session::{Session, SessionConfig, SessionHandle};
use bytetap_core::transport::{
    ProxyRole, TEST_PROXY_CONNECT_DELAY_MS, TEST_PROXY_CONNECT_TIMEOUT_MS,
};
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(10);

/// Метка хуков: они глобальны для процесса, а тесты идут параллельно.
static HOOKS: Mutex<()> = Mutex::const_new(());

/// Снимает хуки при выходе из теста, даже если проверка упала.
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        TEST_PROXY_CONNECT_DELAY_MS.store(0, Ordering::Relaxed);
        TEST_PROXY_CONNECT_TIMEOUT_MS.store(0, Ordering::Relaxed);
    }
}

/// Сессия прокси с журналом во временной папке.
async fn start(dir: &std::path::Path, target: std::net::SocketAddr) -> SessionHandle {
    Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir),
    )
    .await
    .expect("сессия прокси")
}

fn listen_addr(handle: &SessionHandle) -> std::net::SocketAddr {
    handle.local_addr().parse().expect("адрес прослушивания")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_note_and_a_neighbour_close_do_not_lose_the_started_connect() {
    let _hooks = HOOKS.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_connections(2)
        .with_behaviour(LisBehaviour::Echo)
        .with_timeout(WAIT);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = start(dir.path(), lis_addr).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    // Первая пара: подключение проходит сразу.
    let mut first = TcpStream::connect(listen).await.expect("первый анализатор");
    first.write_all(b"one").await.expect("фрагмент");
    let mut echo = [0u8; 3];
    timeout(WAIT, first.read_exact(&mut echo))
        .await
        .expect("первая пара работает")
        .expect("эхо");

    // Вторая пара: подключение к цели задерживается, окно отмены открыто.
    TEST_PROXY_CONNECT_DELAY_MS.store(500, Ordering::Relaxed);
    let mut second = TcpStream::connect(listen).await.expect("второй анализатор");
    // Пока подключение идёт, приходит комментарий (отмена `next`)…
    sleep(Duration::from_millis(100)).await;
    handle.note("пока идёт подключение").expect("комментарий");
    // …и закрывается соседняя пара (ещё одна отмена `next`).
    first.shutdown().await.expect("FIN соседней пары");
    drop(first);
    TEST_PROXY_CONNECT_DELAY_MS.store(0, Ordering::Relaxed);

    // Задержанная пара не потеряна: её `open` есть, обмен проходит.
    second.write_all(b"two").await.expect("фрагмент");
    let mut echo = [0u8; 3];
    timeout(WAIT, second.read_exact(&mut echo))
        .await
        .expect("подключение не потеряно отменой `next`")
        .expect("эхо второй пары");
    assert_eq!(&echo, b"two");
    second.shutdown().await.expect("FIN второй пары");

    timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    for _ in 0..500 {
        let closes = read_all(&path)
            .expect("журнал читается")
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Close(_)))
            .count();
        if closes == 2 {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    handle.stop().await.expect("остановка");

    let outcome = read_all(&path).expect("журнал читается");
    let mut conns: Vec<u32> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Open(open) => Some(open.conn),
            _ => None,
        })
        .collect();
    conns.sort_unstable();
    assert_eq!(conns, [1, 2], "обе пары получили open");
    let mut forwarded = Vec::new();
    for line in &outcome.lines {
        if let Event::Data(data) = &line.event
            && data.conn == 2
            && data.dir == Dir::AnalyzerToLis
        {
            forwarded.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(data.b64.as_bytes())
                    .expect("b64"),
            );
        }
    }
    assert_eq!(forwarded, b"two", "у второй пары записан обмен");
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Note(_))),
        "комментарий записан"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_target_still_reports_the_connect_timeout() {
    let _hooks = HOOKS.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    // Цель принимает `SYN` и молчит: подключение не завершается, пока идёт срок.
    let silent = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("молчащая цель");
    let target = silent.local_addr().expect("адрес цели");
    let (handle, mut feed) = Session::start_with_feed(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            target.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    TEST_PROXY_CONNECT_TIMEOUT_MS.store(200, Ordering::Relaxed);
    TEST_PROXY_CONNECT_DELAY_MS.store(2000, Ordering::Relaxed);
    let mut first = TcpStream::connect(listen).await.expect("первый анализатор");
    // Вторая пара закрывается, пока подключение первой ещё идёт: отмена `next` не должна
    // съесть ни ошибку первой пары, ни её саму.
    let mut second = TcpStream::connect(listen).await.expect("второй анализатор");

    let error = timeout(WAIT, async {
        loop {
            let event = feed.recv().await.expect("лента жива");
            if let Event::Error(error) = event.as_ref() {
                break error.clone();
            }
        }
    })
    .await
    .expect("таймаут подключения записан");
    TEST_PROXY_CONNECT_DELAY_MS.store(0, Ordering::Relaxed);
    assert_eq!(
        error.code.as_deref(),
        Some("proxy_connect_timeout"),
        "{error:?}"
    );
    // Входящие соединения закрыты без `open`: пары не было.
    for stream in [&mut first, &mut second] {
        let mut tail = Vec::new();
        let _ = timeout(WAIT, stream.read_to_end(&mut tail)).await;
    }
    handle.stop().await.expect("остановка");

    let outcome = read_all(&path).expect("журнал читается");
    assert!(
        !outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::Open(_))),
        "без подключения `open` не пишется"
    );
    drop(silent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connects_to_a_silent_target_do_not_wait_for_each_other() {
    let _hooks = HOOKS.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let target = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("цель слушает");
    let target_addr = target.local_addr().expect("адрес цели");
    let handle = start(dir.path(), target_addr).await;
    let listen = listen_addr(&handle);
    let path = handle.journal_path().to_path_buf();

    // Каждое подключение длится своё окно, но окна идут параллельно.
    const DELAY: Duration = Duration::from_millis(400);
    const ATTEMPTS: usize = 4;
    TEST_PROXY_CONNECT_DELAY_MS.store(DELAY.as_millis() as u64, Ordering::Relaxed);
    let started = Instant::now();
    let mut streams = Vec::new();
    for _ in 0..ATTEMPTS {
        streams.push(TcpStream::connect(listen).await.expect("сторона пришла"));
    }
    // Цель принимает все подключения: если бы они шли по очереди, окна сложились бы.
    for _ in 0..ATTEMPTS {
        timeout(WAIT, target.accept())
            .await
            .expect("прокси подключился")
            .expect("приём цели");
    }
    let opens = loop {
        let opens = read_all(&path)
            .expect("журнал читается")
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Open(_)))
            .count();
        if opens == ATTEMPTS {
            break opens;
        }
        assert!(
            started.elapsed() < WAIT,
            "подключения не дошли до цели: открыто {opens} из {ATTEMPTS}"
        );
        sleep(Duration::from_millis(10)).await;
    };
    let elapsed = started.elapsed();
    TEST_PROXY_CONNECT_DELAY_MS.store(0, Ordering::Relaxed);
    assert_eq!(opens, ATTEMPTS);
    assert!(
        elapsed < DELAY * 3,
        "{ATTEMPTS} подключений заняли {elapsed:?}: они ждали друг друга"
    );

    for mut stream in streams {
        let _ = stream.shutdown().await;
    }
    for _ in 0..500 {
        let closes = read_all(&path)
            .expect("журнал читается")
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Close(_)))
            .count();
        if closes == ATTEMPTS {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, ATTEMPTS as u32);
}
