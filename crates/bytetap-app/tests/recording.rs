//! Запись через ядро без моста (U1-AC11, U1-AC13, U1-AC17): сессия на loopback,
//! эмулятор анализатора HL7 из testkit, лента в кольцевом буфере, ошибки старта и
//! закрытие окна во время записи. Данные — только синтетические.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytetap_app::errors::ErrorCode;
use bytetap_app::feed::{FEED_CAPACITY, FeedKind};
use bytetap_app::live::{self, Phase};
use bytetap_app::settings::Settings;
use bytetap_app::{BytetapApp, Screen};
use bytetap_core::journal::{Autoresponder, Dir};
use bytetap_core::session::SessionConfig;
use bytetap_testkit::emulator::{AnalyzerEmulator, frame};
use eframe::egui::{self, ViewportCommand, ViewportEvent, ViewportId};
use egui_kittest::Harness;

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
            "MSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|MSG{index:05}|P|2.3.1\rPID|1||000001||PATIENT^TEST\r"
        )
        .as_bytes(),
    )
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

/// Сумма длин `b64` строк `data` журнала по направлениям: сколько байт ушло в каждую
/// сторону — по самим данным журнала, а не по счётчикам сессии.
fn journal_bytes_by_dir(path: &Path) -> (u64, u64) {
    let mut to_lis = 0u64;
    let mut to_analyzer = 0u64;
    for line in std::fs::read_to_string(path)
        .expect("журнал читается")
        .lines()
    {
        let value: serde_json::Value = serde_json::from_str(line).expect("строка журнала — JSON");
        if value["type"].as_str() != Some("data") {
            continue;
        }
        let b64 = value["b64"].as_str().expect("у data есть b64");
        let len = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("b64 разбирается")
            .len() as u64;
        match value["dir"].as_str() {
            Some("analyzer_to_lis") => to_lis += len,
            Some("lis_to_analyzer") => to_analyzer += len,
            other => panic!("неизвестное направление {other:?}"),
        }
    }
    (to_lis, to_analyzer)
}

fn server(dir: &Path) -> SessionConfig {
    SessionConfig::new("127.0.0.1:0".parse().unwrap())
        .with_responder(Autoresponder::Auto)
        .with_log_dir(dir)
}

/// Старт на 127.0.0.1:0, эмулятор HL7, стоп: в ленте open, data a2l, автоответ l2a,
/// close; журнал заканчивается `session_end`.
#[test]
fn a_session_on_loopback_fills_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    rt.block_on(async {
        let session = live::start(server(dir.path()), egui::Context::default())
            .await
            .expect("старт");
        let addr: SocketAddr = session.local_addr().parse().unwrap();
        assert_ne!(addr.port(), 0, "порт выбран системой");
        let report = AnalyzerEmulator::new(addr)
            .with_messages([hl7(1), hl7(2)])
            .run()
            .await
            .expect("эмулятор");
        assert_eq!(report.acks.len(), 2);
        // Закрытие соединения доходит до ленты раньше остановки.
        let feed = session.feed();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !live::lock(&feed)
            .iter()
            .any(|event| matches!(event.kind, FeedKind::Close { .. }))
        {
            assert!(Instant::now() < deadline, "close не пришёл");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let path = session.journal_path().to_path_buf();
        let outcome = session.stop().await;
        assert!(
            outcome.failure.is_none() && outcome.error.is_none(),
            "{outcome:?}"
        );
        let stats = outcome.stats.expect("итоги");
        assert_eq!(stats.connections, 1);

        let ring = live::lock(&feed);
        let kinds: Vec<&str> = ring
            .iter()
            .map(|event| match &event.kind {
                FeedKind::Open { .. } => "open",
                FeedKind::Close { .. } => "close",
                FeedKind::Data(data) if data.dir == Dir::AnalyzerToLis => "a2l",
                FeedKind::Data(data) if data.synthetic => "l2a synthetic",
                FeedKind::Data(_) => "l2a",
                FeedKind::Error { .. } => "error",
                FeedKind::Info { .. } => "info",
                FeedKind::Note { .. } => "note",
            })
            .collect();
        assert_eq!(kinds.first(), Some(&"open"), "{kinds:?}");
        assert_eq!(kinds.last(), Some(&"close"), "{kinds:?}");
        assert!(kinds.contains(&"a2l"), "{kinds:?}");
        assert!(kinds.contains(&"l2a synthetic"), "{kinds:?}");
        let types = journal_types(&path);
        assert_eq!(types.first().map(String::as_str), Some("session"));
        assert_eq!(types.last().map(String::as_str), Some("session_end"));
    });
}

/// 50 000 фрагментов через сессию на loopback: буфер ленты не растёт сверх предела,
/// всё, что не дошло до ленты, посчитано, а счётчики совпадают с журналом (U1-AC13).
#[test]
fn fifty_thousand_fragments_keep_the_feed_bounded() {
    const MESSAGES: usize = 25_000;
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    rt.block_on(async {
        let session = live::start(server(dir.path()), egui::Context::default())
            .await
            .expect("старт");
        let addr: SocketAddr = session.local_addr().parse().unwrap();
        // Сообщение и ACK — по фрагменту в каждую сторону: 50 000 фрагментов.
        let report = AnalyzerEmulator::new(addr)
            .with_messages((0..MESSAGES).map(hl7))
            .run()
            .await
            .expect("эмулятор");
        assert_eq!(report.acks.len(), MESSAGES);
        let counters = session.counters();
        let feed = session.feed();
        let path = session.journal_path().to_path_buf();
        let outcome = session.stop().await;
        let stats = outcome.stats.expect("итоги");

        let types = journal_types(&path);
        let feed_lines = types
            .iter()
            .filter(|t| !matches!(t.as_str(), "session" | "session_end"))
            .count() as u64;
        let data = types.iter().filter(|t| *t == "data").count();
        assert!(data >= 2 * MESSAGES, "фрагментов {data}");
        let ring = live::lock(&feed);
        assert!(ring.len() <= FEED_CAPACITY, "буфер {}", ring.len());
        assert_eq!(ring.len(), FEED_CAPACITY);
        // `Lagged` считает все события подписки, а лента без заголовка и итогов: число
        // пропущенных может отличаться на эти два события (U1-AC13).
        let skipped = ring.next_seq() + ring.dropped();
        assert!(
            (feed_lines..=feed_lines + 2).contains(&skipped),
            "дошедшее и пропущенное = журнал: {skipped} против {feed_lines}"
        );
        assert_eq!(counters.snapshot(), stats, "счётчики шапки = итоги журнала");
        // Итоги совпадают с самими данными журнала: сумма длин `b64` по направлениям
        // равна счётчикам сессии (U1-AC13).
        let (to_lis, to_analyzer) = journal_bytes_by_dir(&path);
        assert_eq!(to_lis, stats.bytes_analyzer_to_lis, "байты к ЛИС");
        assert_eq!(to_analyzer, stats.bytes_lis_to_analyzer, "байты от ЛИС");
    });
}

/// Порт занят: ошибка `port_in_use` с портом, файл журнала не создан (U1-AC11).
#[test]
fn a_start_error_leaves_no_journal() {
    let dir = tempfile::tempdir().unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let rt = runtime();
    let error = rt
        .block_on(live::start(
            SessionConfig::new(SocketAddr::from(([127, 0, 0, 1], port))).with_log_dir(dir.path()),
            egui::Context::default(),
        ))
        .expect_err("порт занят");
    assert_eq!(error.code, ErrorCode::PortInUse);
    assert_eq!(error.params.get("port"), Some(&port.to_string()));
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "журнала нет"
    );

    // Папку журналов не создать: ошибка `log_dir`, тоже без файла.
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let error = rt
        .block_on(live::start(
            server(&file.join("logs")),
            egui::Context::default(),
        ))
        .expect_err("папка внутри файла");
    assert_eq!(error.code, ErrorCode::LogDir);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// Кадры харнесса до условия, с паузой: сессия живёт в своём runtime.
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

fn commands(harness: &Harness<'static, BytetapApp>) -> Vec<ViewportCommand> {
    harness
        .output()
        .viewport_output
        .values()
        .flat_map(|viewport| viewport.commands.clone())
        .collect()
}

/// Закрытие окна во время записи: окно отменяет закрытие, останавливает сессию
/// штатно и закрывается само; журнал заканчивается `session_end` (U1-AC17).
#[test]
fn closing_the_window_stops_the_recording_first() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let handle = rt.handle().clone();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, Settings::with_log_dir(&log_dir), None, None)
                .with_runtime(handle)
                .with_test_autostart(&cc.egui_ctx, "tcp_server:127.0.0.1:0")
        });
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    assert_eq!(harness.state().screen, Screen::Recording);
    let path = harness.state().recorded_path().cloned().expect("журнал");
    let addr: SocketAddr = harness
        .state()
        .recording()
        .unwrap()
        .local_addr()
        .parse()
        .unwrap();
    let report = rt
        .block_on(AnalyzerEmulator::new(addr).with_messages([hl7(1)]).run())
        .expect("эмулятор");
    assert_eq!(report.acks.len(), 1);

    harness
        .input_mut()
        .viewports
        .get_mut(&ViewportId::ROOT)
        .unwrap()
        .events
        .push(ViewportEvent::Close);
    harness.step();
    assert!(
        commands(&harness).contains(&ViewportCommand::CancelClose),
        "закрытие отменено до остановки"
    );
    step_until(&mut harness, "закрытие окна", |h| {
        commands(h).contains(&ViewportCommand::Close)
    });
    assert!(!harness.state().session_busy());
    let types = journal_types(&path);
    assert_eq!(
        types.last().map(String::as_str),
        Some("session_end"),
        "{types:?}"
    );
    assert!(types.iter().any(|t| t == "data"));
}

/// «Старт» на занятом порту из формы: ошибка у поля порта, экран формы, журнала нет.
#[test]
fn a_busy_port_from_the_form_stays_on_the_form() {
    use bytetap_app::settings::SessionMode;
    use egui_kittest::kittest::Queryable as _;

    let dir = tempfile::tempdir().unwrap();
    let busy = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let rt = runtime();
    let handle = rt.handle().clone();
    let mut settings = Settings::with_log_dir(&dir.path().to_string_lossy());
    settings.config.listen_port = port;
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_runtime(handle)
                .with_screen(Screen::Step(SessionMode::TcpServer))
        });
    harness.run_steps(2);
    harness.get_by_label("Старт").click();
    step_until(&mut harness, "ошибка старта", |h| {
        h.state().start_error().is_some()
    });
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::TcpServer));
    assert_eq!(harness.state().phase(), Phase::Idle);
    harness.get_by_label(&format!(
        "Порт {port} уже занят другой программой (возможно, ЛИС). Выберите другой."
    ));
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "журнала нет"
    );
    drop(busy);
}

/// Комментарий из меню «…» во время записи и «Стоп»: журнал получает `note` и
/// `session_end`, окно возвращается к сценариям (U1-AC17).
#[test]
fn a_comment_and_stop_from_the_recording_header() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;

    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let handle = rt.handle().clone();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, Settings::with_log_dir(&log_dir), None, None)
                .with_runtime(handle)
                .with_test_autostart(&cc.egui_ctx, "tcp_server:127.0.0.1:0")
        });
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    let path = harness.state().recorded_path().cloned().expect("журнал");
    harness
        .get_by_role_and_label(Role::Button, "Меню записи")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Комментарий…").click();
    harness.run_steps(3);
    harness.get_by_label("Комментарий к сессии");
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Комментарий")
        .type_text("проба 3");
    harness.run_steps(2);
    harness.get_by_label("Добавить").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Комментарий к сессии").is_none());
    harness.get_by_label_contains("проба 3");
    harness.get_by_label_contains("Стоп 00:").click();
    step_until(&mut harness, "остановка", |h| {
        h.state().phase() == Phase::Idle
    });
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("\"type\":\"note\"") && text.contains("проба 3"),
        "{text}"
    );
    let types = journal_types(&path);
    assert_eq!(types.last().map(String::as_str), Some("session_end"));
}
