//! Кадры в простое и во время записи (NFR-5, U1-AC24).
//!
//! Окно рисует кадр только по событию: в простое приложение не просит перерисовку
//! вовсе (`repaint_delay` — `Duration::MAX`), а во время записи просит не чаще, чем
//! раз в [`bytetap_app::live::FRAME`], сколько бы фрагментов ни пришло. Отсюда
//! частота кадров: не больше 30 в секунду, и её видно по задержке, которую приложение
//! отдаёт в `ViewportOutput::repaint_delay`.
//!
//! Живой процесс на Windows меряет CI-смоук `native-windows`: CPU в простое за 10 с.

use std::io::Write as _;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use bytetap_app::background::Interface;
use bytetap_app::live::FRAME;
use bytetap_app::settings::{SessionMode, Settings};
use bytetap_app::{BytetapApp, Fixtures, Screen};
use egui_kittest::{Harness, kittest::Queryable as _};

const LOG_DIR: &str = r"C:\Users\operator\Documents\bytetap";
const WINDOW: (f32, f32) = (1200.0, 800.0);

fn settings() -> Settings {
    let mut settings = Settings::with_log_dir(LOG_DIR);
    settings.config.mode = SessionMode::TcpServer;
    settings
}

fn fixtures() -> Fixtures {
    Fixtures {
        interfaces: vec![Interface {
            name: "Ethernet".into(),
            addr: "192.168.1.10".into(),
        }],
        ports: Vec::new(),
        recents: Vec::new(),
        now: chrono::DateTime::parse_from_rfc3339("2026-10-19T12:00:00+03:00").unwrap(),
    }
}

/// Ближайшая перерисовка, которую просит приложение после кадра.
fn repaint_delay(harness: &Harness<'static, BytetapApp>) -> Duration {
    harness
        .output()
        .viewport_output
        .values()
        .map(|output| output.repaint_delay)
        .min()
        .unwrap_or(Duration::MAX)
}

/// В простое кадров нет: окно открыто, запись не идёт, событий нет (NFR-5).
#[test]
fn an_idle_window_asks_for_no_frames() {
    let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
        BytetapApp::new(&cc.egui_ctx, settings(), None, None)
            .with_fixtures(fixtures())
            .with_screen(Screen::Scenarios)
    });
    // Загрузка шрифтов и иконок догоняет себя сама; дальше — простой.
    harness.run_steps(8);
    let mut requested = Vec::new();
    for _ in 0..40 {
        harness.step();
        requested.push(repaint_delay(&harness));
    }
    let asked: Vec<_> = requested
        .iter()
        .filter(|delay| **delay != Duration::MAX)
        .collect();
    assert!(
        asked.is_empty(),
        "в простое запрошено {} кадров: {asked:?}",
        asked.len()
    );
}

/// Запись с потоком фрагментов: приложение просит кадр не чаще раза в [`FRAME`],
/// то есть не больше 30 кадров в секунду (NFR-5).
#[test]
fn a_recording_session_asks_for_at_most_thirty_frames_per_second() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("свободный порт");
        listener.local_addr().expect("адрес").port()
    };
    let dir = tempfile::tempdir().expect("временная папка");
    let log_dir = dir.path().to_string_lossy().into_owned();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let handle = runtime.handle().clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let mut settings = Settings::with_log_dir(&log_dir);
            settings.config.mode = SessionMode::TcpServer;
            settings.config.listen_host = "127.0.0.1".into();
            settings.config.listen_port = port;
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_runtime(handle)
                .with_test_autostart(&cc.egui_ctx, &format!("tcp_server:127.0.0.1:{port}"))
        });
    harness.run_steps(4);
    let local = harness
        .state()
        .recording()
        .expect("запись началась")
        .local_addr()
        .to_owned();
    let addr: std::net::SocketAddr = local.parse().expect("адрес слушателя");

    // Клиент шлёт 10 фрагментов каждые 100 мс — 100 фрагментов в секунду.
    let client = std::thread::spawn(move || {
        let Ok(mut stream) = TcpStream::connect(addr) else {
            return 0;
        };
        let mut sent = 0;
        for _ in 0..15 {
            let text = format!(
                "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|MSG{sent:06}|P|2.3.1\rPID|1||000001||PATIENT^TEST\r\x1c\r"
            );
            if stream.write_all(text.as_bytes()).is_err() {
                break;
            }
            sent += 1;
            std::thread::sleep(Duration::from_millis(100));
        }
        // Автоответ приходит в ленту: читаем, чтобы буфер сокета не переполнился.
        let _ = stream.set_nonblocking(true);
        std::thread::sleep(Duration::from_millis(200));
        sent
    });

    // Кадры идут, пока идёт поток; каждый кадр смотрит, что просит приложение.
    let started = Instant::now();
    let mut requested = Vec::new();
    while started.elapsed() < Duration::from_secs(2) {
        harness.step();
        requested.push(repaint_delay(&harness));
        std::thread::sleep(Duration::from_millis(10));
    }
    let sent = client.join().expect("клиент HL7");
    assert!(sent > 0, "клиент не отправил ни одного сообщения");

    // Пока идёт запись, кадр запрашивается — но не чаще раза в 33 мс.
    let asked: Vec<Duration> = requested
        .iter()
        .copied()
        .filter(|delay| *delay != Duration::MAX)
        .collect();
    assert!(
        !asked.is_empty(),
        "во время записи перерисовка не запрошена"
    );
    let mut histogram: Vec<(Duration, usize)> = Vec::new();
    for delay in &asked {
        match histogram.iter_mut().find(|(seen, _)| seen == delay) {
            Some((_, count)) => *count += 1,
            None => histogram.push((*delay, 1)),
        }
    }
    eprintln!("задержки перерисовки: {histogram:?}");
    // Мгновенная перерисовка — событие сессии (старт, остановка), а не фрагмент
    // ленты: 100 фрагментов в секунду не просят по кадру на каждый.
    let immediate = asked
        .iter()
        .filter(|delay| **delay == Duration::ZERO)
        .count();
    assert!(
        immediate <= 4,
        "{immediate} мгновенных перерисовок за 2 с: событие сессии просит кадр, фрагмент — нет"
    );
    let periodic: Vec<Duration> = asked
        .iter()
        .copied()
        .filter(|delay| *delay != Duration::ZERO)
        .collect();
    assert!(!periodic.is_empty(), "кадры ленты идут по таймеру");
    let fastest = periodic.iter().min().copied().expect("есть запросы");
    assert_eq!(fastest, FRAME, "кадры ленты — по таймеру {FRAME:?}");
    let per_second = 1.0 / fastest.as_secs_f64();
    // Сколько кадров нарисует цикл событий за окно замера: мгновенные плюс таймер.
    let drawn = immediate as f64 + 2.0 / fastest.as_secs_f64();
    eprintln!(
        "кадров в секунду при записи: {per_second:.1}; за 2 с цикл нарисует {drawn:.0} кадров"
    );
    assert!(
        per_second <= 30.4,
        "{per_second:.1} кадров в секунду при записи"
    );
    assert!(drawn <= 30.0 * 2.0 + 4.0, "{drawn:.0} кадров за 2 с");

    // Лента видит поток: фрагменты и автоответы дошли до окна.
    let rows = harness.state().recording().map_or(0, |rec| rec.rows());
    assert!(rows > 0, "в ленте нет строк после {sent} сообщений");

    // Остановка: журнал дописан, окно возвращается к сценариям.
    harness.get_by_label_contains("Стоп").click();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        harness.step();
        let screen = harness.state().screen;
        if screen == Screen::Scenarios || Instant::now() > deadline {
            assert_eq!(screen, Screen::Scenarios, "запись не остановилась");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
