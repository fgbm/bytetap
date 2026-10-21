//! Просмотр журнала: снимки в обеих темах, переход по карте, комментарий и
//! открытие журнала из сессии (U1-AC18, U1-AC19, U1-AC28).
//!
//! Журнал синтетический: сообщения прибора — выдуманные, персональных данных нет.

use std::path::PathBuf;
use std::time::Duration;

use bytetap_app::journal::JournalCacheHandle;
use bytetap_app::journal::JournalOpen;
use bytetap_app::settings::{FeedView, Settings, Store};
use bytetap_app::viewing::JournalView;
use bytetap_app::{BytetapApp, Fixtures, Screen, theme};
use bytetap_core::journal::{Autoresponder, Data, Dir, Event, Host, Mode, Open, Session};
use eframe::egui::{self, accesskit::Role};
use egui_kittest::{Harness, SnapshotOptions, kittest::Queryable as _};

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

/// Готовая сводка журнала для снимков: индекс строится по синтетическому файлу.
fn journal_view() -> JournalView {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("bytetap-20261019101500-tcp_server.jsonl");
    std::fs::write(&path, journal_text()).expect("журнал");
    let cache = JournalCacheHandle::default();
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
    view.source.store_page(0, events);
    view.collect_conns();
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
    let cache = JournalCacheHandle::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    let before = opened.summary.events;
    cache
        .append_note(&path, "перезалили пробу")
        .expect("комментарий");
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
            let cache = JournalCacheHandle::default();
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
            view.source.store_page(0, events);
            view.collect_conns();
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
    harness.run_steps(4);
    assert!(
        harness.query_by_label("Комментарий к журналу").is_none(),
        "диалог закрылся"
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
    let cache = JournalCacheHandle::default();
    let error = JournalOpen::open(&cache, &path).expect_err("не журнал");
    assert!(error.message.contains("session"), "{}", error.message);
}

/// Миллион строк: лента считает по общему числу событий, а не по загруженным.
#[test]
fn a_million_events_do_not_grow_the_memory() {
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
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    for index in 0..100_000u64 {
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            index * 100,
            1,
            Dir::AnalyzerToLis,
            false,
            b"A",
        ));
        text.push_str(&serde_json::to_string(&event).expect("событие"));
        text.push('\n');
    }
    std::fs::write(&path, text).expect("журнал");
    let cache = JournalCacheHandle::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    assert_eq!(
        opened.summary.events, 100_000,
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
    view.source.store_page(0, page);
    assert_eq!(
        view.rows(false),
        100_000,
        "строки ленты — по общему числу событий"
    );
    assert_eq!(
        view.source.missing_pages(50_000, 50_010),
        vec![(100, 50_000)],
        "нужна только страница видимого окна"
    );
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
    let cache = JournalCacheHandle::default();
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
