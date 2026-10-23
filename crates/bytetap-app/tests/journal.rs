//! Просмотр журнала: снимки в обеих темах, переход по карте, комментарий и
//! открытие журнала из сессии (U1-AC18, U1-AC19, U1-AC28).
//!
//! Журнал синтетический: сообщения прибора — выдуманные, персональных данных нет.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bytetap_app::feedrows::TEXT_ROW_HEIGHT;
use bytetap_app::journal::{JournalCache, JournalOpen, PAGE_SIZE};
use bytetap_app::live::Phase;
use bytetap_app::settings::{FeedView, Settings, Store};
use bytetap_app::viewing::JournalView;
use bytetap_app::{BytetapApp, Fixtures, Screen, theme};
use bytetap_core::journal::{Autoresponder, Data, Dir, Event, Host, Mode, Open, Session};
use eframe::egui::{self, Key, accesskit::Role};
use egui_kittest::{Harness, SnapshotOptions, kittest::NodeT as _, kittest::Queryable as _};

/// Рендер-тесты по одному: загрузчик Vulkan (lavapipe) изредка падает с SIGSEGV.
fn gpu() -> std::sync::MutexGuard<'static, ()> {
    static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GPU.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Допуск сравнения: версии lavapipe на машине разработчика и в CI сглаживают немного
/// по-разному, но не больше чем в 20 пикселях и на 0.6 по цветовой разнице.
fn options() -> SnapshotOptions {
    SnapshotOptions::new().threshold(0.6).max_failed_pixels(20)
}

const LOG_DIR: &str = r"C:\Users\operator\Documents\bytetap";
const WINDOW: (f32, f32) = (1200.0, 800.0);

fn settings(dark: bool) -> Settings {
    let mut settings = Settings::with_log_dir(LOG_DIR);
    settings.theme = if dark {
        bytetap_app::settings::ThemeMode::Dark
    } else {
        bytetap_app::settings::ThemeMode::Light
    };
    settings
}

fn fixtures() -> Fixtures {
    Fixtures {
        interfaces: Vec::new(),
        ports: Vec::new(),
        recents: Vec::new(),
        now: chrono::DateTime::parse_from_rfc3339("2026-10-19T12:00:00+03:00").unwrap(),
    }
}

/// Синтетический журнал: заголовок, два соединения, обмен HL7 и комментарий.
fn journal_text() -> String {
    use base64::Engine as _;

    let cp1251 = |text: &str| -> Vec<u8> {
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(text);
        bytes.into_owned()
    };
    let at = |ms: u64| {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-19T10:15:00.000+03:00").unwrap();
        (base + chrono::Duration::milliseconds(ms as i64))
            .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
            .to_string()
    };
    let mut header = Session::new(
        Mode::TcpServer,
        Autoresponder::Hl7Mllp,
        serde_json::json!({"listen": "127.0.0.1:5000"}),
        Host {
            os: "Windows 11 23H2".to_owned(),
        },
    );
    header.analyzer = Some("ANALYZER-2".to_owned());
    header.note = Some("проба 1".to_owned());
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    let mut push = |event: Event| {
        text.push_str(&serde_json::to_string(&event).expect("событие"));
        text.push('\n');
    };
    push(Event::Open(Open {
        t: at(0),
        mono_us: 0,
        conn: 1,
        peer: "192.168.1.51:49152".to_owned(),
        local: "127.0.0.1:5000".to_owned(),
        link: None,
    }));
    let msg = |id: &str, body: &str| {
        cp1251(&format!(
            "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|{id}|P|2.3.1\r{body}\x1c\r"
        ))
    };
    let data = |ms: u64, conn: u32, dir: Dir, synthetic: bool, bytes: &[u8]| {
        Event::Data(Data::new(at(ms), ms * 1000, conn, dir, synthetic, bytes))
    };
    push(data(
        500,
        1,
        Dir::AnalyzerToLis,
        false,
        &msg(
            "MSG0001",
            "PID|1||000001||PATIENT^TEST\rOBX|1|NM|GLU^Glucose||5.4|mmol/L\r",
        ),
    ));
    push(data(
        1000,
        1,
        Dir::AnalyzerToLis,
        false,
        &msg(
            "MSG0002",
            "PID|1||000001||ПАЦИЕНТ^ТЕСТ\rOBX|1|NM|GLU^Глюкоза||5.1|ммоль/л\r",
        ),
    ));
    push(data(1500, 1, Dir::LisToAnalyzer, true, b"\x06"));
    push(data(
        2000,
        1,
        Dir::AnalyzerToLis,
        false,
        &msg("MSG0003", "PID|1||000002||PATIENT^TEST\r"),
    ));
    push(data(2500, 1, Dir::LisToAnalyzer, true, b"\x06"));
    push(data(3000, 1, Dir::AnalyzerToLis, false, b"\x05\x01\x02"));
    let _ = base64::engine::general_purpose::STANDARD.encode(b"");
    text
}

/// Кадры харнесса до условия, с паузой: страницы журнала читает фоновый пул.
fn step_until(
    harness: &mut Harness<'static, BytetapApp>,
    what: &str,
    mut done: impl FnMut(&mut Harness<'static, BytetapApp>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        harness.step();
        if done(harness) {
            return;
        }
        assert!(Instant::now() < deadline, "не дождались: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Видна ли в дереве AccessKit строка с такой подписью.
fn shown(harness: &Harness<'static, BytetapApp>, label: &str) -> bool {
    harness.query_by_label_contains(label).is_some()
}

/// Длинный синтетический журнал: у каждого события своё время, 50 байт — 4 строки HEX.
fn long_journal(events: usize) -> (String, Vec<String>) {
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-10-19T10:15:00.000+03:00").expect("время");
    let mut header = Session::new(
        Mode::TcpServer,
        Autoresponder::None,
        serde_json::json!({"listen": "127.0.0.1:5000"}),
        Host {
            os: "Linux".to_owned(),
        },
    );
    header.analyzer = Some("ANALYZER".to_owned());
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    let mut times = Vec::with_capacity(events);
    for index in 0..events {
        let at = base + chrono::Duration::seconds(index as i64);
        let event = Event::Data(Data::new(
            at.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string(),
            index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            &[b'A'; 50],
        ));
        text.push_str(&serde_json::to_string(&event).expect("событие"));
        text.push('\n');
        times.push(at.format("%H:%M:%S.%3f").to_string());
    }
    (text, times)
}

/// Готовая сводка журнала для снимков: индекс строится по синтетическому файлу.
fn journal_view() -> JournalView {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("bytetap-20261019101500-tcp_server.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    let mut view = JournalView::new(
        opened.path.clone(),
        opened.summary.clone(),
        opened.lengths.clone(),
        opened.first_event.clone(),
    );
    // Страница ленты: события грузятся по требованию, в снимке — первая.
    let events = cache
        .read(&opened.path, 0, bytetap_app::journal::PAGE_SIZE)
        .expect("страница");
    let generation = view.source.request_page(0);
    view.source.store_page(0, generation, events);
    // Временная папка удалится — путь в снимке показываем как настоящий.
    view.path = PathBuf::from(format!(
        r"{LOG_DIR}\bytetap-20261019101500-tcp_server.jsonl"
    ));
    view
}

/// Приложение с открытым журналом: снимки просмотра.
fn journal_harness(dark: bool, view: FeedView) -> Harness<'static, BytetapApp> {
    let mut settings = settings(dark);
    settings.feed.view = view;
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .with_theme(if dark {
            eframe::egui::Theme::Dark
        } else {
            eframe::egui::Theme::Light
        })
        .wgpu()
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(journal_view())
        });
    harness.run_steps(8);
    harness
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journal_light() {
    let _gpu = gpu();
    journal_harness(false, FeedView::Text).snapshot_options("journal-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journal_dark() {
    let _gpu = gpu();
    journal_harness(true, FeedView::Text).snapshot_options("journal-dark", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journal_hex_light() {
    let _gpu = gpu();
    journal_harness(false, FeedView::Hex).snapshot_options("journal-hex-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journal_hex_dark() {
    let _gpu = gpu();
    journal_harness(true, FeedView::Hex).snapshot_options("journal-hex-dark", &options());
}

/// Шапка просмотра: имя файла, анализатор, длительность, строка о персональных данных.
#[test]
fn the_journal_header_and_notice_have_names() {
    let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
        BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
            .with_fixtures(fixtures())
            .with_journal_fixture(journal_view())
    });
    harness.run_steps(3);
    harness.get_by_label("bytetap-20261019101500-tcp_server.jsonl");
    harness.get_by_label_contains("ANALYZER-2 · 00:00:03");
    harness.get_by_label_contains("Может содержать персональные данные");
    harness.get_by_label_contains("«проба 1»");
    harness.get_by_role_and_label(Role::Button, "Закрыть журнал");
    harness.get_by_role_and_label(Role::Button, "Комментарий");
    // Строки ленты — с временем и глифом направления.
    harness.get_by_label_contains("10:15:00.500 →");
    harness.get_by_label_contains("10:15:01.000 →");
    harness.get_by_label_contains("#1 подключился");
}

/// Переход по карте сессии: щелчок по трассе переводит ленту к событию.
#[test]
fn a_click_on_the_map_moves_the_feed() {
    let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
        BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
            .with_fixtures(fixtures())
            .with_journal_fixture(journal_view())
    });
    harness.run_steps(3);
    let before = harness.state().viewing().unwrap().events();
    assert!(before > 0, "в журнале есть события");
    // Карта сессии: клик по правому краю — к последнему событию журнала.
    let map = harness.get_by_label("Перейти к месту ленты").rect();
    let target = egui::pos2(map.right() - 2.0, map.center().y);
    harness.event(egui::Event::PointerMoved(target));
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: target,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(2);
    harness.event(egui::Event::PointerButton {
        pos: target,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(3);
    // Лента переехала к последнему событию: видна строка конца журнала.
    harness.get_by_label_contains("10:15:03.000");
}

/// «×» в шапке возвращает стартовый экран.
#[test]
fn the_close_button_returns_to_the_scenarios() {
    let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
        BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
            .with_fixtures(fixtures())
            .with_journal_fixture(journal_view())
    });
    harness.run_steps(3);
    assert_eq!(harness.state().screen, Screen::Viewing);
    harness
        .get_by_role_and_label(Role::Button, "Закрыть журнал")
        .click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
    assert!(harness.state().viewing().is_none());
}

/// Комментарий к журналу: дописывается в конец файла и виден в сводке.
#[test]
fn a_comment_is_appended_to_the_journal() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    let before = opened.summary.events;
    bytetap_app::journal::JournalOpen::append_note(&path, "перезалили пробу").expect("комментарий");
    let after = JournalOpen::open(&cache, &path).expect("журнал после комментария");
    assert_eq!(after.summary.events, before + 1, "событие note добавлено");
    let events = cache
        .read(&after.path, before, 1)
        .expect("последнее событие");
    assert!(matches!(
        &events[0],
        bytetap_core::journal::Event::Note(note) if note.text == "перезалили пробу"
    ));
}

/// Комментарий из шапки просмотра: дописывается в конец файла, сводка и лента
/// видят новое событие (U1-AC19).
#[test]
fn the_comment_from_the_header_reaches_the_journal() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    let journal = path.clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            let mut view = JournalView::new(
                opened.path.clone(),
                opened.summary.clone(),
                opened.lengths.clone(),
                opened.first_event.clone(),
            );
            let events = cache
                .read(&opened.path, 0, bytetap_app::journal::PAGE_SIZE)
                .expect("страница");
            let generation = view.source.request_page(0);
            view.source.store_page(0, generation, events);
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    harness.run_steps(3);
    harness
        .get_by_role_and_label(Role::Button, "Комментарий")
        .click();
    harness.run_steps(3);
    harness.get_by_label("Комментарий к журналу");
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Что дописать в конец журнала")
        .type_text("перезалили пробу");
    harness.run_steps(2);
    harness.get_by_label("Дописать").click();
    // Диалог закрывается, когда комментарий лёг в файл: запись идёт в фоне.
    step_until(
        &mut harness,
        "комментарий в журнале",
        |h| h.query_by_label("Комментарий к журналу").is_none(),
    );
    // Строка под шапкой видит дописанный комментарий: последнюю страницу лента
    // перечитывает сама (U1-AC19).
    step_until(
        &mut harness,
        "строка комментариев под шапкой",
        |h| {
            h.query_by_label_contains("проба 1 · перезалили пробу")
                .is_some()
        },
    );
    // Файл получил комментарий, а сводка — новое событие.
    let text = std::fs::read_to_string(&path).expect("журнал");
    assert!(text.contains("\"type\":\"note\""), "{text}");
    assert!(text.contains("перезалили пробу"), "{text}");
}

/// Ошибка открытия журнала: строка `danger` под шапкой.
#[test]
fn a_file_that_is_not_a_journal_gives_an_error() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "просто текст\n").expect("файл");
    let cache = JournalCache::default();
    let error = JournalOpen::open(&cache, &path).expect_err("не журнал");
    assert!(error.message.contains("session"), "{}", error.message);
}

/// Миллион строк: лента считает по общему числу событий, а не по загруженным.
///
/// Файл получается большой (около 100 МБ), поэтому строки пишутся потоком, а не
/// собираются в память: иначе проверка сама съела бы память.
#[test]
fn a_million_events_do_not_grow_the_memory() {
    const EVENTS: u64 = 1_000_000;
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("big.jsonl");
    let mut header = Session::new(
        Mode::TcpServer,
        Autoresponder::None,
        serde_json::json!({}),
        Host {
            os: "Linux".to_owned(),
        },
    );
    header.analyzer = Some("ANALYZER".to_owned());
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).expect("файл"));
    use std::io::Write as _;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&Event::Session(header)).expect("заголовок")
    )
    .expect("заголовок");
    for index in 0..EVENTS {
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            index * 100,
            1,
            Dir::AnalyzerToLis,
            false,
            b"A",
        ));
        writeln!(file, "{}", serde_json::to_string(&event).expect("событие")).expect("событие");
    }
    file.flush().expect("сброс");

    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    assert_eq!(
        opened.summary.events, EVENTS as usize,
        "события посчитаны по индексу"
    );
    // Страница читается целиком, а не весь файл.
    let page = cache
        .read(&opened.path, 0, bytetap_app::journal::PAGE_SIZE)
        .expect("страница");
    assert_eq!(page.len(), bytetap_app::journal::PAGE_SIZE);
    let mut view = JournalView::new(
        opened.path.clone(),
        opened.summary.clone(),
        opened.lengths.clone(),
        opened.first_event.clone(),
    );
    let generation = view.source.request_page(0);
    view.source.store_page(0, generation, page);
    assert_eq!(
        view.rows(false),
        EVENTS as usize,
        "строки ленты — по общему числу событий"
    );
    assert_eq!(
        view.source.missing_pages(500_000, 500_010),
        vec![(1000, 500_000)],
        "нужна только страница видимого окна"
    );
    // В памяти журнала — страницы LRU, а не миллион событий.
    assert!(
        view.source.pages_in_memory() <= bytetap_app::journal::PAGES_KEPT,
        "в памяти {} страниц",
        view.source.pages_in_memory()
    );
}

/// После «Стоп» записанный журнал открывается со стартового экрана.
#[test]
fn the_recorded_journal_opens_from_the_start_screen() {
    use bytetap_app::live;
    use bytetap_core::session::SessionConfig;

    let dir = tempfile::tempdir().expect("временная папка");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let path = rt.block_on(async {
        let session = live::start(
            SessionConfig::new("127.0.0.1:0".parse().unwrap()).with_log_dir(dir.path()),
            eframe::egui::Context::default(),
        )
        .await
        .expect("старт");
        let path = session.journal_path().to_path_buf();
        session.stop().await;
        path
    });
    let journal = path.clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_recorded_path(journal.clone())
        });
    harness.run_steps(3);
    harness.get_by_label_contains("Открыть записанное").click();
    harness.run_steps(4);
    assert_eq!(harness.state().screen, Screen::Viewing);
}

/// Журнал сессии на loopback открывается на просмотре и показывает те же события.
#[test]
fn a_journal_of_a_session_opens_and_shows_the_same_events() {
    use bytetap_app::live;
    use bytetap_core::session::SessionConfig;
    use bytetap_testkit::emulator::{AnalyzerEmulator, frame};

    let dir = tempfile::tempdir().expect("временная папка");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let path = rt.block_on(async {
        let session = live::start(
            SessionConfig::new("127.0.0.1:0".parse().unwrap())
                .with_responder(Autoresponder::Auto)
                .with_log_dir(dir.path()),
            eframe::egui::Context::default(),
        )
        .await
        .expect("старт");
        let addr: std::net::SocketAddr = session.local_addr().parse().unwrap();
        AnalyzerEmulator::new(addr)
            .with_messages([frame(
                b"MSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|MSG1|P|2.3.1\rPID|1||1||PATIENT^TEST\r"
                    .as_slice(),
            )])
            .run()
            .await
            .expect("эмулятор");
        let path = session.journal_path().to_path_buf();
        session.stop().await;
        path
    });
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    assert!(opened.summary.events > 0, "в журнале есть события");
    // События журнала и ленты совпадают: заголовок и итоги в ленту не входят.
    let page = cache
        .read(&opened.path, 0, bytetap_app::journal::PAGE_SIZE)
        .expect("страница");
    assert_eq!(page.len(), opened.summary.events);
    assert!(
        page.iter()
            .any(|event| matches!(event, bytetap_core::journal::Event::Data(_))),
        "в ленте есть фрагменты обмена"
    );
    assert!(
        page.iter()
            .any(|event| matches!(event, bytetap_core::journal::Event::Open(_))),
        "в ленте есть открытие соединения"
    );
    let _ = Duration::from_secs(1);
    let _ = Store::new(std::path::PathBuf::from("settings.json"));
    let _ = theme::palette(false);
}

/// Лента сама просит страницы: без заранее положенной страницы 0 строки появляются,
/// когда фоновый пул прочитает файл (U1-AC18, U1-AC19).
#[test]
fn the_feed_loads_its_first_page_itself() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    let (text, times) = long_journal(2000);
    std::fs::write(&path, text).expect("журнал");
    let journal = path.clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            // Страница 0 не положена: её должен прочитать пул страниц ленты.
            let view = JournalView::new(
                opened.path,
                opened.summary,
                opened.lengths,
                opened.first_event,
            );
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    harness.run_steps(2);
    harness.get_by_label_contains(&times[0]);
}

/// HEX через карту сессии: переход просит страницы по номерам событий, а не строк HEX,
/// и лента показывает событие из середины журнала (U1-AC19).
#[test]
fn the_hex_seek_reaches_events_past_the_first_page() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    let (text, times) = long_journal(2000);
    std::fs::write(&path, text).expect("журнал");
    let journal = path.clone();
    let mut setup = settings(false);
    setup.feed.view = FeedView::Hex;
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            let view = JournalView::new(
                opened.path,
                opened.summary,
                opened.lengths,
                opened.first_event,
            );
            BytetapApp::new(&cc.egui_ctx, setup, None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    // Середина карты: переход к событию примерно из середины журнала.
    let map = harness.get_by_label("Перейти к месту ленты").rect();
    click(&mut harness, map.center());
    harness.run_steps(2);
    let (event, time) = {
        let view = harness.state_mut().viewing_mut().expect("журнал открыт");
        let rows = view.rows(true);
        let row = view.scroll.first_row(rows);
        let (event, _) = view.event_of_row(row, true).expect("строка в журнале");
        (event, times[event].clone())
    };
    assert!(
        (700..=1300).contains(&event),
        "переход в середину журнала: событие {event}"
    );
    // Строка события из середины появляется, когда пул прочитает её страницу.
    step_until(
        &mut harness,
        "событие середины в ленте",
        |h| shown(h, &time),
    );
}

/// Строка, чья страница ещё едет, — заполнитель той же высоты: строки не сдвигаются,
/// событие 500 остаётся на 500·h от первой видимой строки (U1-AC19).
#[test]
fn a_pending_row_keeps_its_height_until_the_page_arrives() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    let (text, times) = long_journal(1000);
    std::fs::write(&path, text).expect("журнал");
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    // Источник — файла нет: страница 0 не подгрузится сама и останется заполнителем.
    let mut view = JournalView::new(
        PathBuf::from("/нет/такого/журнала.jsonl"),
        opened.summary,
        opened.lengths,
        opened.first_event,
    );
    // Загружена только вторая страница: события 500…999.
    let events = cache
        .read(&opened.path, PAGE_SIZE, PAGE_SIZE)
        .expect("страница");
    let generation = view.source.request_page(1);
    assert_eq!(
        view.source.store_page(1, generation, events),
        bytetap_app::journal::PageLoad::Stored
    );
    view.scroll.set_top(480);
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    harness.run_steps(3);
    let real = harness
        .get_by_label_contains(&times[PAGE_SIZE])
        .rect()
        .top();
    let placeholder = harness
        .query_all_by_label("…")
        .filter_map(|node| {
            let rect = node.rect();
            rect.top().is_finite().then_some(rect.top())
        })
        .fold(f32::INFINITY, f32::min);
    let shift = real - placeholder;
    assert!(
        (shift - 20.0 * TEXT_ROW_HEIGHT).abs() < 0.5,
        "событие 500 через 20 строк заполнителей: {shift} px"
    );
}

/// «Отмена» в комментарии к журналу не оставляет цель: следующий комментарий во время
/// записи идёт в журнал сессии (U1-AC17, U1-AC19).
#[test]
fn a_cancelled_journal_comment_does_not_steal_the_recording_comment() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("свободный порт");
        listener.local_addr().expect("адрес").port()
    };
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let handle = runtime.handle().clone();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let journal = path.clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            let mut view = JournalView::new(
                opened.path.clone(),
                opened.summary,
                opened.lengths,
                opened.first_event,
            );
            let events = cache
                .read(&opened.path, 0, PAGE_SIZE)
                .expect("первая страница");
            let generation = view.source.request_page(0);
            view.source.store_page(0, generation, events);
            let mut settings = Settings::with_log_dir(&log_dir);
            settings.config.listen_host = "127.0.0.1".into();
            settings.config.listen_port = port;
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_runtime(handle)
                .with_journal_fixture(view)
        });
    harness.run_steps(3);
    // Комментарий в просмотре: открыли и отменили.
    harness
        .get_by_role_and_label(Role::Button, "Комментарий")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Комментарий к журналу");
    harness.get_by_label("Отмена").click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Комментарий к журналу").is_none(),
        "диалог закрылся по «Отмене»"
    );
    // Escape закрывает диалог так же, как «Отмена» (U1-AC25).
    harness
        .get_by_role_and_label(Role::Button, "Комментарий")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Комментарий к журналу");
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Комментарий к журналу").is_none(),
        "Escape не закрыл диалог комментария"
    );
    // «×» — назад к сценариям и запись TCP-сервера на свободном порту.
    harness
        .get_by_role_and_label(Role::Button, "Закрыть журнал")
        .click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    harness.get_by_label("Старт").click();
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    let recorded = harness
        .state()
        .recorded_path()
        .cloned()
        .expect("журнал записи");
    harness
        .get_by_role_and_label(Role::Button, "Меню записи")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Комментарий…").click();
    harness.run_steps(2);
    harness.get_by_label("Комментарий к сессии");
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Комментарий")
        .type_text("проба 9");
    harness.run_steps(2);
    harness.get_by_label("Добавить").click();
    step_until(
        &mut harness,
        "диалог комментария закрылся",
        |h| h.query_by_label("Комментарий к сессии").is_none(),
    );
    // Остановка сессии: журнал заканчивается штатно, и комментарий уже в файле.
    harness.get_by_label_contains("Стоп 00:").click();
    step_until(&mut harness, "остановка", |h| {
        h.state().phase() == Phase::Idle
    });
    let text = std::fs::read_to_string(&recorded).expect("журнал записи");
    assert!(
        text.contains("\"type\":\"note\"") && text.contains("проба 9"),
        "комментарий не попал в журнал сессии: {text}"
    );
    runtime.shutdown_background();
}

/// Ошибка дописывания: диалог не закрывается молча, причина — строкой в диалоге.
#[cfg(unix)]
#[test]
fn a_comment_error_keeps_the_dialog_open() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    // Только чтение: дописать комментарий нельзя.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("права");
    let journal = path.clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            let mut view = JournalView::new(
                opened.path.clone(),
                opened.summary,
                opened.lengths,
                opened.first_event,
            );
            let events = cache
                .read(&opened.path, 0, PAGE_SIZE)
                .expect("первая страница");
            let generation = view.source.request_page(0);
            view.source.store_page(0, generation, events);
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    harness.run_steps(3);
    harness
        .get_by_role_and_label(Role::Button, "Комментарий")
        .click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Что дописать в конец журнала")
        .type_text("не ляжет");
    harness.run_steps(2);
    harness.get_by_label("Дописать").click();
    step_until(&mut harness, "ошибка комментария", |h| {
        shown(h, "Комментарий не записан")
    });
    // Диалог на месте: строку ошибки видно в нём, а не только в журнале запуска.
    harness.get_by_label("Комментарий к журналу");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("права");
    let text = std::fs::read_to_string(&path).expect("журнал");
    assert!(!text.contains("не ляжет"), "{text}");
}

/// Приложение с журналом, открытым из файла: страницу ленты просит пул чтения.
fn harness_with_journal(path: &std::path::Path) -> Harness<'static, BytetapApp> {
    let journal = path.to_path_buf();
    Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            let view = JournalView::new(
                opened.path,
                opened.summary,
                opened.lengths,
                opened.first_event,
            );
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        })
}

/// Видимая область ленты и сколько строк в неё помещается — число дробное, если
/// высота ленты не кратна строке.
fn feed_fit(harness: &Harness<'static, BytetapApp>, row_height: f32) -> (egui::Rect, f64) {
    let view = harness.state().viewing().expect("журнал открыт");
    let rect = view.feed_rect.expect("лента нарисована");
    (rect, f64::from(rect.height()) / f64::from(row_height))
}

/// `End` в ленте высотой не кратной строке: низ последней строки — по низу ленты, и
/// она видна целиком. Смещение в целых строках резало последнюю строку на долю.
#[test]
fn the_end_key_shows_the_last_row_whole() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    let (text, times) = long_journal(2000);
    std::fs::write(&path, text).expect("журнал");
    let mut harness = harness_with_journal(&path);
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    let (feed, fit) = feed_fit(&harness, TEXT_ROW_HEIGHT);
    assert!(
        fit.fract() > 0.01,
        "высота ленты не кратна строке: {fit} строк"
    );
    harness.key_press(Key::End);
    harness.run_steps(2);
    // Последняя строка — из последней страницы: ждём, пока пул её прочитает.
    step_until(&mut harness, "последняя строка", |h| {
        shown(h, &times[1999])
    });
    let view = harness.state().viewing().expect("журнал открыт");
    let offset = view.scroll.offset();
    assert!(
        (offset + fit - 2000.0).abs() < 1e-6,
        "низ последней строки — по низу ленты: смещение {offset}, помещается {fit}"
    );
    let row = harness.get_by_label_contains(&times[1999]).rect();
    assert!(
        row.bottom() <= feed.bottom() + 0.5,
        "низ строки {} ниже ленты {}",
        row.bottom(),
        feed.bottom()
    );
    assert!(
        row.top() >= feed.top() - 0.5,
        "верх строки {} выше ленты {}",
        row.top(),
        feed.top()
    );
    assert!(
        (row.height() - TEXT_ROW_HEIGHT).abs() < 0.5,
        "строка целиком: высота {}",
        row.height()
    );
}

/// Журнал, который выше ленты на доли строки, всё равно прокручивается до конца:
/// последняя строка иначе оставалась срезанной снизу.
#[test]
fn a_journal_a_hair_taller_than_the_feed_scrolls_to_its_end() {
    let dir = tempfile::tempdir().expect("временная папка");
    // Сколько строк помещается в ленте — узнаём по настоящей отрисовке.
    let path = dir.path().join("measure.jsonl");
    let (text, _times) = long_journal(2000);
    std::fs::write(&path, text).expect("журнал");
    let mut harness = harness_with_journal(&path);
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    let (_feed, fit) = feed_fit(&harness, TEXT_ROW_HEIGHT);
    let rows = fit.ceil() as usize;
    drop(harness);

    // Столько строк лишь чуть выше ленты: rows·h > высоты ленты, но целых строк
    // помещается ровно столько, сколько их есть.
    let path = dir.path().join("short.jsonl");
    let (text, times) = long_journal(rows);
    std::fs::write(&path, text).expect("журнал");
    let mut harness = harness_with_journal(&path);
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    harness.key_press(Key::End);
    harness.run_steps(2);
    let (feed, fit) = feed_fit(&harness, TEXT_ROW_HEIGHT);
    let view = harness.state().viewing().expect("журнал открыт");
    let offset = view.scroll.offset();
    assert!(offset > 0.0, "лента не прокрутилась: смещение {offset}");
    assert!(
        (offset + fit - rows as f64).abs() < 1e-6,
        "смещение {offset}, помещается {fit}, строк {rows}"
    );
    let row = harness.get_by_label_contains(&times[rows - 1]).rect();
    assert!(
        row.bottom() <= feed.bottom() + 0.5,
        "низ последней строки {} ниже ленты {}",
        row.bottom(),
        feed.bottom()
    );
}

/// После дописывания лента просит последнюю страницу: комментарий виден и в ленте,
/// когда её прокрутить до конца, а не только строкой под шапкой (U1-AC19).
#[test]
fn a_long_journal_shows_the_appended_comment_at_its_end() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    let (text, _times) = long_journal(2000);
    std::fs::write(&path, text).expect("журнал");
    let mut harness = harness_with_journal(&path);
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    harness
        .get_by_role_and_label(Role::Button, "Комментарий")
        .click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Что дописать в конец журнала")
        .type_text("перезалили пробу");
    harness.run_steps(2);
    harness.get_by_label("Дописать").click();
    step_until(&mut harness, "комментарий в файле", |h| {
        h.query_by_label("Комментарий к журналу").is_none()
    });
    // Последняя страница в памяти: комментарий — последнее событие журнала.
    step_until(&mut harness, "последняя страница", |h| {
        h.state_mut().viewing_mut().is_some_and(|view| {
            let last = view.source.total().saturating_sub(1);
            view.source.event(last).is_some()
        })
    });
    harness.get_by_label_contains("«перезалили пробу»");
    // И в ленте: прокрутка до конца показывает строку комментария. Подпись не одна —
    // та же строка есть под шапкой, поэтому считаем совпадения: подпись ленты
    // начинается со времени события.
    harness.key_press(Key::End);
    step_until(&mut harness, "комментарий в ленте", |h| {
        // Та же строка есть и под шапкой, в «…»: строка ленты начинается со времени.
        h.query_all_by_label_contains("перезалили пробу")
            .filter_map(|node| node.accesskit_node().label())
            .any(|label| !label.starts_with('«'))
    });
}

/// Панель деталей забрала низ ленты: строка, по которой кликнули у самого низа,
/// подтягивается над панелью, а не остаётся под ней (находка контрольного ревью шага 1,
/// L1-AC4).
#[test]
fn the_details_panel_pulls_the_clicked_row_into_view() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    // 600 событий с разными идентификаторами: последнюю строку видно по подписи.
    let mut header = Session::new(
        Mode::TcpServer,
        Autoresponder::None,
        serde_json::json!({"listen": "127.0.0.1:5000"}),
        Host {
            os: "Linux".to_owned(),
        },
    );
    header.analyzer = Some("ANALYZER".to_owned());
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    for index in 0..600 {
        let event = Event::Data(Data::new(
            format!(
                "2026-10-19T10:15:{:02}.{:03}+03:00",
                index / 1000,
                index % 1000
            ),
            index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            format!("\x0bMSH|^~\\&|A|B|C|D|E||ORU^R01|JEV{index:05}|P|2.3.1\r\x1c\r").as_bytes(),
        ));
        text.push_str(&serde_json::to_string(&event).expect("событие"));
        text.push('\n');
    }
    std::fs::write(&path, text).expect("журнал");
    let mut harness = harness_with_journal(&path);
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    // Конец журнала: последняя строка у нижнего края ленты, панели ещё нет.
    harness.key_press(Key::End);
    step_until(&mut harness, "последняя строка", |h| {
        h.query_by_label_contains("JEV00599").is_some()
    });
    let row = harness.get_by_label_contains("JEV00599").rect().center();
    harness.event(egui::Event::PointerMoved(row));
    harness.run_steps(3);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: row,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    harness.run_steps(3);
    let after = harness.get_by_label_contains("JEV00599").rect();
    let panel_top = harness.get_by_label_contains("Детали события").rect().top();
    assert!(
        after.bottom() <= panel_top + 0.5,
        "строка подтянута над панелью: {after:?}, панель с {panel_top}"
    );
    let view = harness.state().viewing().expect("журнал открыт");
    let total = view.summary.events;
    assert!(
        view.selection.contains((total - 1) as u64),
        "строка выделена и открыта в панели"
    );
}

/// Комментарий лёг в файл, а перечитать сводку не удалось: диалог закрыт, причина —
/// строкой под шапкой. Это не «комментарий не записан» (U1-AC19).
#[test]
fn a_written_comment_is_reported_even_when_the_reread_fails() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    let mut harness = harness_with_journal(&path);
    step_until(&mut harness, "первая страница", |h| {
        h.state()
            .viewing()
            .is_some_and(|view| view.source.pages_in_memory() > 0)
    });
    // Файл испортили после открытия: заголовок стал неполным. Дописать комментарий
    // ещё можно — `append_note` смотрит только на `type` первой строки и на хвост
    // файла, — а индекс журнала такой файл уже не собирает.
    let text = std::fs::read_to_string(&path).expect("журнал");
    let mut lines: Vec<&str> = text.lines().collect();
    lines[0] = r#"{"type":"session"}"#;
    std::fs::write(&path, lines.join("\n") + "\n").expect("журнал");
    harness
        .get_by_role_and_label(Role::Button, "Комментарий")
        .click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::MultilineTextInput, "Что дописать в конец журнала")
        .type_text("не ляжет в сводку");
    harness.run_steps(2);
    harness.get_by_label("Дописать").click();
    // Диалог закрывается: текст в файле. Причина сбоя перечитывания — под шапкой.
    step_until(&mut harness, "диалог закрылся", |h| {
        h.query_by_label("Комментарий к журналу").is_none()
    });
    step_until(&mut harness, "строка под шапкой", |h| {
        shown(h, "Комментарий записан, но журнал не перечитан")
    });
    let text = std::fs::read_to_string(&path).expect("журнал");
    assert!(text.contains("не ляжет в сводку"), "{text}");
}

/// Клик мышью по точке экрана: наведение, нажатие и отпускание.
fn click(harness: &mut Harness<'static, BytetapApp>, at: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(at));
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(1);
}

/// Просмотр журнала: у каждой кнопки и у полосы прокрутки есть роль и подпись, а Tab
/// идёт по шапке слева направо (U1-AC25).
#[test]
fn the_journal_screen_names_its_widgets_and_tabs_in_order() {
    use egui_kittest::kittest::NodeT as _;

    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("journal.jsonl");
    // Журнал длиннее ленты: у полосы прокрутки появляется ползунок.
    let (text, _) = long_journal(2000);
    std::fs::write(&path, text).expect("журнал");
    let mut harness = harness_with_journal(&path);
    harness.run_steps(3);
    harness.check_accessibility();

    let mut names = Vec::new();
    for node in harness.root().children_recursive() {
        let node = node.accesskit_node();
        if node.is_hidden() {
            continue;
        }
        if matches!(node.role(), Role::Button | Role::ScrollBar | Role::ComboBox) {
            let name = node.label().or_else(|| node.value()).unwrap_or_default();
            assert!(!name.trim().is_empty(), "у {:?} нет подписи", node.role());
            names.push((node.role(), name));
        }
    }
    for want in [Role::Button, Role::ScrollBar] {
        assert!(
            names.iter().any(|(role, _)| *role == want),
            "нет {want:?}: {names:?}"
        );
    }
    // «Комментарий» — в шапке, «Закрыть журнал» — левее её, обе до кнопок окна.
    let labels: Vec<String> = names.into_iter().map(|(_, name)| name).collect();
    let close = labels
        .iter()
        .position(|name| name == "Закрыть журнал")
        .expect("«×» просмотра");
    let comment = labels
        .iter()
        .position(|name| name == "Комментарий")
        .expect("комментарий");
    assert!(close < comment, "{labels:?}");

    // Порядок сверяется по подписям, а не по координатам: строки ленты занимают на разных
    // платформах разное число шагов (на windows-latest они в обход Tab не попадают), а
    // ползунок полосы прокрутки — внутренний прямоугольник её дорожки: «следующая цель
    // правее» для него неверно, и на коротком экране он стоит у верхнего края (U1-AC25).
    let mut order: Vec<String> = Vec::new();
    for _ in 0..200 {
        harness.key_press(Key::Tab);
        harness.run_steps(2);
        let Some(node) = harness
            .root()
            .children_recursive()
            .find(egui_kittest::Node::is_focused)
        else {
            continue;
        };
        let node = node.accesskit_node();
        // Строки ленты в списке не идут: видно их столько, сколько помещается на экране.
        if node.role() == Role::ListItem {
            continue;
        }
        let label = node.label().or_else(|| node.value()).unwrap_or_default();
        assert!(!label.is_empty(), "Tab попал на виджет без подписи");
        // Кнопка темы называет состояние интерфейса, а не место: обе подписи — одна цель.
        let label = match label.as_str() {
            "Светлая тема" | "Тёмная тема" => "тема".to_owned(),
            _ => label,
        };
        // Круг замкнулся: обход дошёл до первой цели снова.
        if order.contains(&label) {
            break;
        }
        order.push(label);
    }
    // Шапка слева направо и до кнопок окна, затем карта и фильтры, лента и полоса прокрутки.
    assert_eq!(
        order.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "Закрыть журнал",
            "тема",
            "Комментарий",
            "Свернуть",
            "Развернуть",
            "Закрыть",
            "Перейти к месту ленты",
            "Вид ленты",
            "Кодировка",
            "Прокрутка ленты",
            "Полоса прокрутки ленты",
        ],
        "порядок Tab: шапка, карта, фильтры, лента, полоса прокрутки"
    );
}
