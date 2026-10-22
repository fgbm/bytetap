//! Прокси из интерфейса без моста (M4-AC1, M4-AC7, M4-AC13): шаг сценария запускает
//! сессию на loopback, эмулятор анализатора HL7 из testkit проходит через прокси к
//! эмулятору ЛИС, лента показывает пару соединений, журнал несёт `link` и `session_end`.
//! Данные — только синтетические (`ANALYZER`, `LIS`, `PATIENT^TEST`).

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use bytetap_app::feed::FeedKind;
use bytetap_app::live::Phase;
use bytetap_app::settings::{ProxyMode, SessionMode, Settings};
use bytetap_app::{BytetapApp, Screen};
use bytetap_core::journal::Dir;
use bytetap_testkit::emulator::{AnalyzerEmulator, frame};
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

fn hl7(index: usize) -> Vec<u8> {
    frame(
        format!(
            "MSH|^~\\&|ANALYZER|LAB|LIS||20261022101500||ORU^R01|MSG{index:05}|P|2.3.1\rPID|1||000123||PATIENT^TEST\r"
        )
        .as_bytes(),
    )
}

/// Свободный порт loopback: слушатель занимает его и сразу отпускает.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("свободный порт");
    listener.local_addr().expect("адрес").port()
}

/// Кадры столько, сколько нужно условию: сессия живёт в своём runtime.
fn step_until(
    harness: &mut Harness<'static, BytetapApp>,
    what: &str,
    mut done: impl FnMut(&mut Harness<'static, BytetapApp>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        harness.step();
        if done(harness) {
            return;
        }
        assert!(Instant::now() < deadline, "не дождались: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Строки журнала: тип каждой строки по порядку.
fn journal_types(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("журнал читается")
        .lines()
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("строка журнала — JSON");
            value["type"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

/// Шаг прокси в харнессе: поля из настроек, «Старт» нажимается тестом.
fn proxy_harness(
    log_dir: &str,
    listen_port: u16,
    lis: SocketAddr,
    handle: tokio::runtime::Handle,
) -> Harness<'static, BytetapApp> {
    let mut settings = Settings::with_log_dir(log_dir);
    settings.config.mode = SessionMode::Proxy;
    settings.config.listen_host = "127.0.0.1".into();
    settings.config.listen_port = listen_port;
    settings.config.proxy_role = ProxyMode::AnalyzerClient;
    settings.config.proxy_host = lis.ip().to_string();
    settings.config.proxy_port = lis.port();
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_runtime(handle)
                .with_screen(Screen::Step(SessionMode::Proxy))
        });
    harness.run_steps(2);
    harness
}

/// Прокси из формы на loopback: анализатор идёт через прокси к ЛИС, лента получает
/// `open` пары с `link`, `data` обеих сторон и `close`, журнал — `session_end`.
#[test]
fn a_proxy_from_the_form_records_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let handle = rt.handle().clone();
    let lis_port = free_port();
    let listen_port = free_port();
    let lis_addr: SocketAddr = format!("127.0.0.1:{lis_port}").parse().unwrap();
    let log_dir = dir.path().to_string_lossy().into_owned();

    // ЛИС ждёт прокси и подтверждает каждое сообщение MLLP (M4-AC1).
    let lis = rt.block_on(async {
        LisEmulator::listen(lis_addr)
            .await
            .expect("ЛИС слушает")
            .with_behaviour(LisBehaviour::Hl7Ack)
    });
    let lis_task = rt.spawn(async move { lis.run().await });

    let mut harness = proxy_harness(&log_dir, listen_port, lis_addr, handle);
    harness.get_by_label("Старт").click();
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    assert_eq!(harness.state().screen, Screen::Recording);
    let path = harness.state().recorded_path().cloned().expect("журнал");
    let recorded: SocketAddr = harness
        .state()
        .recording()
        .unwrap()
        .local_addr()
        .parse()
        .unwrap();
    assert_eq!(recorded.port(), listen_port, "прокси слушает порт из формы");

    let report = rt
        .block_on(
            AnalyzerEmulator::new(recorded)
                .with_messages([hl7(1), hl7(2)])
                .run(),
        )
        .expect("анализатор прошёл через прокси");
    // ACK пришли от ЛИС через прокси: байты не менялись (FR-6).
    assert_eq!(report.acks.len(), 2);
    assert!(
        report.acks[0].windows(6).any(|w| w == b"MSA|AA"),
        "ACK ЛИС: {:?}",
        String::from_utf8_lossy(&report.acks[0])
    );
    let lis_report = rt.block_on(lis_task).expect("ЛИС").expect("сценарий ЛИС");
    assert_eq!(lis_report.connections, 1);
    assert!(
        lis_report.all_received().windows(5).any(|w| w == b"MSG00"),
        "ЛИС получила сообщения анализатора"
    );

    // Лента: `open` пары несёт исходящую сторону, `data` — обе стороны, `close` — кто
    // закрыл (FR-5, FR-7).
    let feed = harness.state().recording().unwrap().shared();
    step_until(&mut harness, "закрытие пары", |_| {
        bytetap_app::live::lock(&feed)
            .iter()
            .any(|event| matches!(event.kind, FeedKind::Close { .. }))
    });
    let ring = bytetap_app::live::lock(&feed);
    let open = ring.iter().find_map(|event| match &event.kind {
        FeedKind::Open { peer, link } => Some((peer.clone(), link.clone())),
        _ => None,
    });
    let (peer, link) = open.expect("open пары в ленте");
    assert!(
        peer.starts_with("127.0.0.1:"),
        "принятая сторона — анализатор: {peer}"
    );
    let link = link.expect("у пары есть исходящая сторона");
    assert_eq!(link.role, bytetap_core::journal::LinkRole::Lis);
    assert_eq!(link.peer, lis_addr.to_string(), "цель — ЛИС");
    let dirs: Vec<Dir> = ring
        .iter()
        .filter_map(|event| event.data().map(|data| data.dir))
        .collect();
    assert!(dirs.contains(&Dir::AnalyzerToLis), "{dirs:?}");
    assert!(dirs.contains(&Dir::LisToAnalyzer), "{dirs:?}");
    drop(ring);

    harness.get_by_label_contains("Стоп 00:").click();
    step_until(&mut harness, "остановка", |h| {
        h.state().phase() == Phase::Idle
    });
    // Итог записи: соединение прокси — пара, счётчики обеих сторон (FR-25, M4-AC13).
    let stats = harness.state().stop_stats().expect("итоги записи");
    assert_eq!(stats.connections, 1);
    assert!(stats.bytes_analyzer_to_lis > 0 && stats.bytes_lis_to_analyzer > 0);
    let types = journal_types(&path);
    assert_eq!(types.first().map(String::as_str), Some("session"));
    assert_eq!(types.last().map(String::as_str), Some("session_end"));
    let text = std::fs::read_to_string(&path).unwrap();
    let open_line = text
        .lines()
        .find(|line| line.contains("\"type\":\"open\""))
        .expect("open в журнале");
    let value: serde_json::Value = serde_json::from_str(open_line).unwrap();
    assert_eq!(value["link"]["role"], "lis");
    assert_eq!(value["link"]["peer"], lis_addr.to_string());
    let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(header["mode"], "proxy");
    assert_eq!(header["autoresponder"], "none");
    assert_eq!(header["settings"]["role"], "analyzer_client");
    assert_eq!(
        header["settings"]["listen"],
        format!("127.0.0.1:{listen_port}")
    );
}

/// Порт прослушивания занят: ошибка `port_in_use` под полем порта, экран шага остаётся,
/// журнала нет (U1-AC11, M4-AC13).
#[test]
fn a_busy_proxy_port_is_reported_under_the_port_field() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let handle = rt.handle().clone();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let lis_port = free_port();
    let lis_addr: SocketAddr = format!("127.0.0.1:{lis_port}").parse().unwrap();
    let mut harness = proxy_harness(&dir.path().to_string_lossy(), busy_port, lis_addr, handle);
    harness.get_by_label("Старт").click();
    step_until(&mut harness, "ошибка старта", |h| {
        h.state().start_error().is_some()
    });
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Proxy));
    assert_eq!(harness.state().phase(), Phase::Idle);
    harness.get_by_label(&format!(
        "Порт {busy_port} уже занят другой программой (возможно, ЛИС). Выберите другой."
    ));
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "журнала нет"
    );
    drop(busy);
}
