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
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytetap_app::background::Interface;
use bytetap_app::live::{FRAME, FRAME_SLOWEST, Phase};
use bytetap_app::settings::{SessionMode, Settings};
use bytetap_app::{BytetapApp, Fixtures, Screen};
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

const LOG_DIR: &str = r"C:\Users\operator\Documents\bytetap";
const WINDOW: (f32, f32) = (1200.0, 800.0);

/// Сколько ждать старта сессии и адреса её слушателя.
///
/// Старт — привязка порта, файл журнала и задача runtime: на загруженном раннере это
/// не первый кадр, а иногда и не четвёртый, поэтому ждём условие, а не число кадров.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Сколько ждать кадра, который окно просит таймером, а не событием.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Предел окна замера при потоке фрагментов: клиент шлёт 1.5 с, остальное — запас
/// загруженного раннера.
const STREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// Сколько последних задержек задают установившийся период кадра: по нему видно,
/// залипла ли оценка цены, а минимум по всему окну этого не показывает.
const SETTLED_FRAMES: usize = 5;

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

/// Кадр окна и короткая пауза: задачи runtime и потоки фоновой работы идут не в нём.
///
/// Часы egui в [`Harness`] управляемые (`predicted_dt`), а пауза нужна внешним задачам:
/// без неё поток окна не отдаёт им процессор.
fn tick(harness: &mut Harness<'static, BytetapApp>) {
    harness.step();
    std::thread::sleep(Duration::from_millis(1));
}

/// Ждёт, пока запись начнётся и сессия ядра назовёт адрес слушателя.
///
/// Экран записи появляется уже в кадре «Старт», а сессия ядра — только когда задача
/// старта отдаст результат: сколько кадров это займёт, зависит от машины, поэтому
/// готовность — условие с таймаутом, а не число кадров. Порт `0` выбирает система
/// (M3-AC16): адрес из настроек совпал бы с настоящим только случайно, и подключаться
/// к записи можно лишь по адресу сессии.
fn wait_for_listener(harness: &mut Harness<'static, BytetapApp>) -> SocketAddr {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut local = String::new();
    while Instant::now() <= deadline {
        if harness.state().phase() == Phase::Recording
            && let Some(rec) = harness.state().recording()
        {
            local = rec.local_addr().to_owned();
            if let Ok(addr) = local.parse() {
                return addr;
            }
        }
        tick(harness);
    }
    panic!(
        "запись не началась за {READY_TIMEOUT:?}: фаза {:?}, адрес слушателя {local:?}",
        harness.state().phase()
    );
}

/// Ждёт интервал, который окно просит таймером, и возвращает его.
///
/// Готовность сессии будит окно немедленно (`request_repaint` задачи старта), и такой
/// запрос — событие, а не интервал кадра: он виден нулевой задержкой и первым в окне
/// замера не считается. Дальше идут только запросы по таймеру.
fn wait_for_timer_frame(harness: &mut Harness<'static, BytetapApp>) -> Duration {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    loop {
        tick(harness);
        let delay = repaint_delay(harness);
        if delay != Duration::ZERO && delay != Duration::MAX {
            return delay;
        }
        assert!(
            Instant::now() <= deadline,
            "окно не просит кадр по таймеру за {FRAME_TIMEOUT:?}: {delay:?}"
        );
    }
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
    // Кто именно просил кадр, видно по причинам egui: файл, строка и текст запроса.
    assert!(
        asked.is_empty(),
        "в простое запрошено {} кадров: {asked:?}; причины: {:?}",
        asked.len(),
        harness.ctx.repaint_causes()
    );
}

/// Запись идёт, новых событий нет: кадр всё равно просится по таймеру — на аппаратном
/// растре раз в [`FRAME`] ради пульса «Стоп», на WARP к смене секунды таймера (NFR-5,
/// U1-AC24).
#[test]
fn a_quiet_recording_keeps_the_pulse_frame() {
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
            // Порт выбирает система: занимать его тесту незачем, и чужой процесс его
            // не перехватит между проверкой и старт-сессией.
            settings.config.listen_port = 0;
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_runtime(handle)
                .with_test_autostart(&cc.egui_ctx, "tcp_server:127.0.0.1:0")
        });
    // Никто не подключается: событий в ленте нет, запись ждёт.
    wait_for_listener(&mut harness);
    // Первый интервал — кадр по таймеру: мгновенный запрос от старта уже позади.
    let mut asked = vec![wait_for_timer_frame(&mut harness)];
    for _ in 1..8 {
        harness.step();
        asked.push(repaint_delay(&harness));
    }
    assert!(
        asked.iter().all(|delay| *delay != Duration::MAX),
        "запись идёт: кадр просится по таймеру, а не по событию: {asked:?}"
    );
    // Кадр без событий идёт по таймеру: на аппаратном растре — пульсом каждые
    // [`FRAME`], на WARP — к границе секунды `elapsed`, но не чаще раза в секунду.
    assert!(
        asked
            .iter()
            .all(|delay| (FRAME..=FRAME_SLOWEST).contains(delay)),
        "без новых событий кадр не чаще раза в секунду: {asked:?}"
    );
    // Харнесс едет на аппаратном адаптере: пульс там непрерывный, кадр раз в 33 мс —
    // так же, как в UI v2 с его CSS-анимацией. На WARP кадры без событий идут к смене
    // секунды, и точка стоит статично (`recording::stop_pill`).
    assert!(
        asked.iter().all(|delay| *delay == FRAME),
        "на аппаратном растре кадр без событий — раз в {FRAME:?}: {asked:?}"
    );
}

/// Профиль кадра записи без GPU: сколько стоит раскладка и тесселяция egui.
///
/// Запуск руками: `cargo test -p bytetap-app --test performance -- --ignored --nocapture`.
/// В CI не идёт: время кадра зависит от машины. На 2026-10-22 (1200×800, лента «Текст»)
/// кадр целиком — 0.13 мс на 8 событиях, 0.22 на 400 и 0.30 на 2000 в release-сборке
/// (в debug — 0.8…2.0 мс). Отсюда решение не кешировать галлеи строк: на фоне 160 мс
/// кадра WARP экономить нечего, а цена кадра — растр программного адаптера, а не
/// раскладка текста.
#[test]
#[ignore = "профиль машины, не проверка"]
fn frame_cost_probe() {
    use std::time::Instant;

    for rows in [8usize, 400, 2000] {
        let mut harness = Harness::builder()
            .with_size(WINDOW)
            .build_eframe(move |cc| {
                BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                    .with_fixtures(fixtures())
                    .with_recording_fixture(feed_fixture(rows))
            });
        harness.run_steps(6);
        let mut times = Vec::new();
        for _ in 0..30 {
            let started = Instant::now();
            harness.step();
            times.push(started.elapsed());
        }
        times.sort_unstable();
        let drawn = harness.state().recording().map_or(0, |rec| rec.rows());
        eprintln!(
            "{rows} событий, строк {drawn}: медиана кадра {:?}",
            times[times.len() / 2]
        );
    }
}

/// Цена кадра с открытой панелью деталей (L1-AC10): 10 000 строк в ленте и фрагмент
/// 16 КБ из управляющих байтов — худший случай раскладки панели.
///
/// Запуск руками: `cargo test -p bytetap-app --test performance -- --ignored --nocapture`.
/// В CI не идёт: время кадра зависит от машины. Панель показывает полный фрагмент, а не
/// первые 2048 байт, поэтому кадр меряется на вкладке «Текст» и на вкладке «HEX» (1024
/// строки дампа). Худшее состояние — фокус в поле и Ctrl+A: выделен весь фрагмент; оно
/// меряется и с чтением дерева AccessKit каждый кадр, как это делает диктор.
#[test]
#[ignore = "профиль машины, не проверка"]
fn details_panel_frame_cost() {
    use std::time::Instant;

    use egui_kittest::kittest::Queryable as _;

    let rows = 10_000usize;
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(control_fixture(rows))
        });
    harness.run_steps(6);
    // Чтение дерева AccessKit в каждом кадре: диктор обходит узлы, и это тоже цена кадра.
    let measure = |harness: &mut Harness<'static, BytetapApp>, label: &str, reader: bool| {
        let mut times = Vec::new();
        for _ in 0..30 {
            let started = Instant::now();
            harness.step();
            if reader {
                let nodes: Vec<_> = harness
                    .root()
                    .children_recursive()
                    .map(|node| node.accesskit_node().role())
                    .collect();
                std::hint::black_box(nodes);
            }
            times.push(started.elapsed());
        }
        times.sort_unstable();
        eprintln!(
            "{label}: медиана кадра {:?}, худший {:?}",
            times[times.len() / 2],
            times[times.len() - 1]
        );
    };
    measure(&mut harness, "панель закрыта, 10 000 строк", false);
    let row = harness
        .get_all_by_label_contains("CTRL16K")
        .next()
        .expect("строка длинного фрагмента")
        .rect();
    click_at(&mut harness, row.center());
    harness.run_steps(4);
    let text = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .expect("поле панели")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    eprintln!("фрагмент в поле: {} символов", text.chars().count());
    measure(
        &mut harness,
        "панель открыта, «Текст», 16 КБ управляющих байт",
        false,
    );
    // Худшее состояние: фокус в поле и выделен весь фрагмент (Ctrl+A).
    click_at(&mut harness, row.center());
    harness.run_steps(2);
    let field = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .expect("поле панели")
        .rect();
    click_at(&mut harness, field.center());
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    measure(&mut harness, "«Текст», Ctrl+A на всём фрагменте", false);
    measure(
        &mut harness,
        "«Текст», Ctrl+A и чтение дерева AccessKit",
        true,
    );
    harness.get_by_label("HEX").click();
    harness.run_steps(4);
    measure(&mut harness, "панель открыта, «HEX», 16 КБ", false);
}

/// Клик мышью: указатель сперва встаёт на место, и только следующий кадр нажимает —
/// иначе прокрутка ленты видит рывок указателя и уезжает по инерции.
fn click_at(harness: &mut Harness<'static, BytetapApp>, at: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(at));
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(1);
}

/// Запись из `rows` событий; последнее — 16 КБ управляющих байтов (худший случай).
fn control_fixture(rows: usize) -> bytetap_app::recording::RecordingFixture {
    use bytetap_core::journal::{Data, Dir, Event, Stats};

    let mut events: Vec<Event> = (0..rows.saturating_sub(1))
        .map(|index| {
            let body = format!(
                "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019232710||ORU^R01|MSG{index:06}|P|2.3.1\rOBX|1|NM|GLU^Glucose||5.4|mmol/L\r\x1c\r"
            );
            Event::Data(Data::new(
                "2026-10-19T23:27:10.046000+03:00",
                index as u64 * 100_000 + 1,
                1,
                Dir::AnalyzerToLis,
                false,
                body.as_bytes(),
            ))
        })
        .collect();
    let mut long =
        b"\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019232710||ORU^R01|CTRL16K|P|2.3.1\r".to_vec();
    long.resize(16 * 1024 - 3, 0x01);
    long.extend_from_slice(b"\x1c\r");
    events.push(Event::Data(Data::new(
        "2026-10-19T23:27:10.046000+03:00",
        rows as u64 * 100_000,
        1,
        Dir::AnalyzerToLis,
        false,
        &long,
    )));
    bytetap_app::recording::RecordingFixture {
        events,
        stats: Stats {
            connections: 1,
            bytes_analyzer_to_lis: 1_000_000,
            bytes_lis_to_analyzer: 0,
        },
        elapsed: Duration::from_secs(10),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: None,
        dropped: 0,
    }
}

/// Запись с `events` фрагментами HL7 для профиля: столько же строк ленты.
fn feed_fixture(events: usize) -> bytetap_app::recording::RecordingFixture {
    use bytetap_core::journal::{Data, Dir, Event, Stats};

    let list = (0..events)
        .map(|index| {
            let body = format!(
                "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019232710||ORU^R01|MSG{index:06}|P|2.3.1\rOBX|1|NM|GLU^Glucose||5.4|mmol/L\r\x1c\r"
            );
            Event::Data(Data::new(
                "2026-10-19T23:27:10.046000+03:00",
                index as u64 * 100_000 + 1,
                1,
                Dir::AnalyzerToLis,
                false,
                body.as_bytes(),
            ))
        })
        .collect();
    bytetap_app::recording::RecordingFixture {
        events: list,
        stats: Stats {
            connections: 1,
            bytes_analyzer_to_lis: 1000,
            bytes_lis_to_analyzer: 0,
        },
        elapsed: Duration::from_secs(10),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: None,
        dropped: 0,
    }
}

/// Запись с потоком фрагментов: приложение просит кадр не чаще раза в [`FRAME`],
/// то есть не больше 30 кадров в секунду (NFR-5).
#[test]
fn a_recording_session_asks_for_at_most_thirty_frames_per_second() {
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
            // Порт выбирает система: адрес слушателя берём у сессии.
            settings.config.listen_port = 0;
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_runtime(handle)
                .with_test_autostart(&cc.egui_ctx, "tcp_server:127.0.0.1:0")
        });
    // Запись начинается с кадра «Старт», а порт слушает сессия ядра: до её готовности
    // подключаться некуда, и адреса в состоянии окна ещё нет.
    let addr = wait_for_listener(&mut harness);
    // Первый интервал после старта — событие сессии, а не кадр ленты.
    wait_for_timer_frame(&mut harness);

    // Клиент шлёт по фрагменту каждые 100 мс — 10 фрагментов в секунду.
    let done = Arc::new(AtomicBool::new(false));
    let client_done = Arc::clone(&done);
    let client = std::thread::spawn(move || {
        let Ok(mut stream) = TcpStream::connect(addr) else {
            client_done.store(true, Ordering::Relaxed);
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
        client_done.store(true, Ordering::Relaxed);
        sent
    });

    // Кадры идут, пока идёт поток; каждый кадр смотрит, что просит приложение. Окно
    // замера — сам поток, а не секунды по часам: на загруженном раннере кадров за те же
    // секунды меньше, и число замеров не должно от этого зависеть от машины.
    let started = Instant::now();
    let mut requested = Vec::new();
    while !done.load(Ordering::Relaxed) {
        assert!(
            started.elapsed() < STREAM_TIMEOUT,
            "клиент не закончил поток за {STREAM_TIMEOUT:?}"
        );
        harness.step();
        requested.push(repaint_delay(&harness));
        std::thread::sleep(Duration::from_millis(10));
    }
    let sent = client.join().expect("клиент HL7");
    assert!(sent > 0, "клиент не отправил ни одного сообщения");
    // Последний фрагмент мог прийти к самому концу потока: его видит следующий кадр.
    harness.step();

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
    // Мгновенная перерисовка — событие сессии (старт, остановка) или перестройка
    // самого окна (полоса прокрутки ленты появилась), а не фрагмент ленты: 10
    // фрагментов в секунду не просят по кадру на каждый. Запросы старта сняты до
    // замера (`wait_for_timer_frame`), поэтому здесь их не больше горстки.
    let immediate = asked
        .iter()
        .filter(|delay| **delay == Duration::ZERO)
        .count();
    assert!(
        immediate <= 4,
        "{immediate} мгновенных перерисовок за поток: событие окна просит кадр, фрагмент — нет"
    );
    let periodic: Vec<Duration> = asked
        .iter()
        .copied()
        .filter(|delay| *delay != Duration::ZERO)
        .collect();
    assert!(!periodic.is_empty(), "кадры ленты идут по таймеру");
    let fastest = periodic.iter().min().copied().expect("есть запросы");
    // Период считается по цене кадра: на медленной машине он больше [`FRAME`], но
    // чаще, чем раз в [`FRAME`], окно кадр не просит.
    assert!(
        (FRAME..=FRAME_SLOWEST).contains(&fastest),
        "кадры ленты — по таймеру от {FRAME:?} до {FRAME_SLOWEST:?}: {fastest:?}"
    );
    // Установившийся период — последние запросы потока, а не минимум по всему окну:
    // минимум прошёл бы и на залипшем периоде, если в начале замера случился один
    // частый кадр. Поток идёт, а период — [`FRAME`], а не секунда (находка ревью шага 5).
    let settled = periodic
        .iter()
        .rev()
        .take(SETTLED_FRAMES)
        .copied()
        .max()
        .expect("есть запросы");
    assert!(
        (FRAME..=FRAME_SLOWEST).contains(&settled),
        "установившийся период кадра ленты — от {FRAME:?} до {FRAME_SLOWEST:?}: {settled:?}"
    );
    assert!(
        settled <= 2 * FRAME,
        "поток идёт, а кадр просится раз в {settled:?}: период залип"
    );
    let per_second = 1.0 / settled.as_secs_f64();
    // Сколько кадров нарисует цикл событий за окно замера: мгновенные плюс таймер.
    let window = started.elapsed().as_secs_f64().max(0.001);
    let drawn = immediate as f64 + window / fastest.as_secs_f64();
    eprintln!(
        "кадров в секунду при записи: {per_second:.1}; за {window:.1} с поток нарисует {drawn:.0} кадров"
    );
    assert!(
        per_second <= 30.4,
        "{per_second:.1} кадров в секунду при записи"
    );
    assert!(
        drawn <= 30.0 * window + 4.0,
        "{drawn:.0} кадров за {window:.1} с"
    );

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
