//! Снимки экранов в обеих темах и поведение стартового экрана и шагов сценария
//! (U1-AC9…AC12, U1-AC28) через `egui_kittest`.
//!
//! Рендер — wgpu, на Linux программный Vulkan (lavapipe, `mesa-vulkan-drivers`):
//! снимки сравниваются только на Linux, где эталоны и сняты. Обновить эталоны:
//! `UPDATE_SNAPSHOTS=1 cargo test -p bytetap-app --test snapshots`. Глифы проверяются
//! на всех ОС без рендера: отсутствующий глиф роняет харнесс. Окружение (сеть, порты,
//! последние записи, «сейчас») — синтетическое, через `Fixtures`.

use bytetap_app::background::Interface;
use bytetap_app::errors::{ErrorCode, ErrorField, StartError};
use bytetap_app::live::{JournalFailure, JournalFailureCode};
use bytetap_app::recording::RecordingFixture;
use bytetap_app::settings::{
    FeedView, LastRun, LoadWarning, ProxyMode, SessionMode, Settings, Store, ThemeMode,
};
use bytetap_app::{BytetapApp, Fixtures, Screen, theme};
use bytetap_core::journal::RecentJournal;
use bytetap_core::transport::Source;
use bytetap_core::transport::serial::PortInfo;
use eframe::egui::{self, Key, Theme, accesskit::Role};
use egui_kittest::{Harness, SnapshotOptions, kittest::Queryable as _};

/// Рендер-тесты по одному: загрузчик Vulkan (lavapipe) изредка падает с SIGSEGV, когда
/// несколько харнессов wgpu создают и закрывают устройства одновременно.
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

fn fixtures() -> Fixtures {
    let journal =
        |name: &str, started_at: &str, analyzer: &str, minutes: u64, mode: &str| RecentJournal {
            path: format!(r"{LOG_DIR}\{name}").into(),
            started_at: started_at.to_owned(),
            analyzer: analyzer.to_owned(),
            duration_us: minutes * 60 * 1_000_000,
            mode: mode.to_owned(),
        };
    Fixtures {
        interfaces: vec![
            Interface {
                name: "Ethernet".into(),
                addr: "192.168.1.10".into(),
            },
            Interface {
                name: "Wi-Fi".into(),
                addr: "172.16.4.21".into(),
            },
        ],
        ports: vec![PortInfo {
            name: "COM3".into(),
            description: "USB-SERIAL CH340".into(),
        }],
        recents: vec![
            journal(
                "bytetap-20261019101500-tcp_server.jsonl",
                "2026-10-19T10:15:00+03:00",
                "ANALYZER",
                42,
                "tcp_server",
            ),
            journal(
                "bytetap-20261018180200-tcp_client.jsonl",
                "2026-10-18T18:02:00+03:00",
                "ANALYZER-2",
                21,
                "tcp_client",
            ),
            journal(
                "bytetap-20261017094000-serial.jsonl",
                "2026-10-17T09:40:00+03:00",
                "",
                8,
                "serial",
            ),
        ],
        now: chrono::DateTime::parse_from_rfc3339("2026-10-19T12:00:00+03:00").unwrap(),
    }
}

fn settings(dark: bool) -> Settings {
    let mut settings = Settings::with_log_dir(LOG_DIR);
    settings.theme = if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    settings.last_run = Some(LastRun {
        mode: SessionMode::TcpServer,
        port: "5000".into(),
        host: String::new(),
        baud: 0,
    });
    settings
}

/// Приложение на синтетическом окружении; `prepare` донастраивает его до первого кадра.
fn harness_with(
    settings: Settings,
    store: Option<Store>,
    screen: Screen,
    size: (f32, f32),
    prepare: impl FnOnce(BytetapApp) -> BytetapApp + 'static,
) -> Harness<'static, BytetapApp> {
    let dark = settings.theme == ThemeMode::Dark;
    let mut harness = Harness::builder()
        .with_size(size)
        .with_theme(if dark { Theme::Dark } else { Theme::Light })
        .wgpu()
        .build_eframe(move |cc| {
            prepare(
                BytetapApp::new(&cc.egui_ctx, settings, None, store)
                    .with_fixtures(fixtures())
                    .with_screen(screen),
            )
        });
    // Иконки-SVG растеризуются за несколько кадров.
    harness.run_steps(8);
    harness
}

fn harness(screen: Screen, dark: bool, size: (f32, f32)) -> Harness<'static, BytetapApp> {
    harness_with(settings(dark), None, screen, size, |app| app)
}

/// Один снимок на тест: результаты нескольких харнессов в одном тесте kittest не сводит.
fn snapshot(name: &str, screen: Screen, dark: bool, size: (f32, f32)) {
    harness(screen, dark, size).snapshot_options(name, &options());
}

const TITLE_BAR: (f32, f32) = (1200.0, 44.0);
const WINDOW: (f32, f32) = (1200.0, 800.0);

/// Наименьшее окно приложения (U1-AC3) и типичный ноутбук: шапка не должна выталкивать
/// кнопки окна за край.
const NARROW: (f32, f32) = (720.0, 560.0);
const LAPTOP: (f32, f32) = (1366.0, 768.0);

/// Анализатор в 200 символов: длинные метаданные просмотра журнала.
fn long_analyzer() -> String {
    let mut name = "ANALYZER-IMPORT-".to_owned();
    while name.len() < 200 {
        name.push_str("LONG-NAME-0123456789");
    }
    name.truncate(200);
    assert_eq!(name.chars().count(), 200, "ровно 200 символов");
    name
}

/// Журнал с анализатором `analyzer`: шапка просмотра в узком окне.
///
/// Файл лежит во временной папке, а страница 0 прочитана заранее — снимок и проверка
/// раскладки видят шапку и первую страницу, как настоящее открытие журнала.
fn journal_view(analyzer: Option<String>) -> bytetap_app::viewing::JournalView {
    use bytetap_app::journal::{JournalCache, JournalOpen};

    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("bytetap-20261019101500-tcp_server.jsonl");
    let mut header = bytetap_core::journal::Session::new(
        bytetap_core::journal::Mode::TcpServer,
        bytetap_core::journal::Autoresponder::Hl7Mllp,
        serde_json::json!({ "listen": "127.0.0.1:5000" }),
        bytetap_core::journal::Host {
            os: "Windows 11 23H2".to_owned(),
        },
    );
    header.analyzer = analyzer;
    let mut text =
        serde_json::to_string(&bytetap_core::journal::Event::Session(header)).expect("заголовок");
    text.push('\n');
    let data = bytetap_core::journal::Event::Data(bytetap_core::journal::Data::new(
        "2026-10-19T10:15:01.000+03:00",
        1_000_000,
        1,
        bytetap_core::journal::Dir::AnalyzerToLis,
        false,
        b"\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|MSG1|P|2.3.1\r\x1c\r",
    ));
    text.push_str(&serde_json::to_string(&data).expect("событие"));
    text.push('\n');
    std::fs::write(&path, text).expect("журнал");
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    let mut view = bytetap_app::viewing::JournalView::new(
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
    // Временная папка удалится: в шапке — настоящее имя файла.
    view.path = format!(r"{LOG_DIR}\bytetap-20261019101500-tcp_server.jsonl").into();
    view
}

/// Просмотр журнала с длинным анализатором в окне `size`.
fn long_analyzer_viewing(size: (f32, f32)) -> Harness<'static, BytetapApp> {
    analyzer_viewing(size, Some(long_analyzer()))
}

/// Просмотр журнала с анализатором `analyzer` в окне `size`.
fn analyzer_viewing(size: (f32, f32), analyzer: Option<String>) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(size)
        .with_theme(Theme::Light)
        .wgpu()
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(journal_view(analyzer))
        });
    harness.run_steps(8);
    harness
}

/// Запись TCP-клиентом: длинная сводка режима и большие счётчики — шапка `size`.
fn long_summary_recording(size: (f32, f32)) -> Harness<'static, BytetapApp> {
    use bytetap_app::settings::Responder;
    use bytetap_core::journal::Stats;

    let mut settings = settings(false);
    settings.config.mode = SessionMode::TcpClient;
    settings.config.connect_host = "analizator-import.lab.example.internal".into();
    settings.config.connect_port = 5000;
    settings.config.responder = Responder::Hl7;
    let fixture = RecordingFixture {
        events: recording_fixture(0).events,
        // Гигабайты и сотни соединений: счётчики шапки шире обычных.
        stats: Stats {
            connections: 128,
            bytes_analyzer_to_lis: 999_999_999,
            bytes_lis_to_analyzer: 888_888_888,
        },
        elapsed: std::time::Duration::from_secs(3_599),
        local_addr: String::new(),
        journal_path: Some(format!(r"{LOG_DIR}\bytetap-20261019232710-tcp_client.jsonl").into()),
        dropped: 0,
    };
    let mut harness = Harness::builder()
        .with_size(size)
        .with_theme(Theme::Light)
        .wgpu()
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture)
        });
    harness.run_steps(8);
    harness
}

/// Журнал прокси: заголовок с двумя сторонами, пара соединений и `session_end`.
///
/// Файл лежит во временной папке, страница 0 прочитана заранее: снимок просмотра видит
/// шапку со сводкой пары и строки ленты «#1 открыто: анализатор → ЛИС» (M4-AC13).
fn proxy_journal_view() -> bytetap_app::viewing::JournalView {
    use bytetap_app::journal::{JournalCache, JournalOpen};
    use bytetap_core::journal::{Close, ClosedBy, Data, Dir, Event, Link, LinkRole, Open};

    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("bytetap-20261022101503-proxy.jsonl");
    let mut header = bytetap_core::journal::Session::new(
        bytetap_core::journal::Mode::Proxy,
        bytetap_core::journal::Autoresponder::None,
        serde_json::json!({
            "listen": "192.168.1.10:50025",
            "target": "10.0.0.5:5000",
            "role": "analyzer_client",
        }),
        bytetap_core::journal::Host {
            os: "Windows 11 23H2".to_owned(),
        },
    );
    header.analyzer = Some("ANALYZER".to_owned());
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    let open = Open {
        t: "2026-10-22T10:15:03.000000+03:00".into(),
        mono_us: 0,
        conn: 1,
        peer: "192.168.1.51:49152".into(),
        local: "192.168.1.10:50025".into(),
        link: Some(Link {
            role: LinkRole::Lis,
            peer: "10.0.0.5:5000".into(),
            local: "192.168.1.10:50231".into(),
        }),
    };
    text.push_str(&serde_json::to_string(&Event::Open(open)).expect("open"));
    text.push('\n');
    let data = Event::Data(Data::new(
        "2026-10-22T10:15:03.004000+03:00",
        4_000,
        1,
        Dir::AnalyzerToLis,
        false,
        b"\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261022101503||ORU^R01|MSG0001|P|2.3.1\r\x1c\r",
    ));
    text.push_str(&serde_json::to_string(&data).expect("событие"));
    text.push('\n');
    let close = Close {
        t: "2026-10-22T10:15:09.120000+03:00".into(),
        mono_us: 6_120_000,
        conn: 1,
        by: ClosedBy::Lis,
        reason: "eof".into(),
    };
    text.push_str(&serde_json::to_string(&Event::Close(close)).expect("close"));
    text.push('\n');
    std::fs::write(&path, text).expect("журнал");
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    let mut view = bytetap_app::viewing::JournalView::new(
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
    view.path = format!("{LOG_DIR}\\bytetap-20261022101503-proxy.jsonl").into();
    view
}

fn proxy_viewing(dark: bool, size: (f32, f32)) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(size)
        .with_theme(if dark { Theme::Dark } else { Theme::Light })
        .wgpu()
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(dark), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(proxy_journal_view())
        });
    harness.run_steps(8);
    harness
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn viewing_proxy_light() {
    let _gpu = gpu();
    proxy_viewing(false, WINDOW).snapshot_options("viewing-proxy-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn viewing_proxy_dark() {
    let _gpu = gpu();
    proxy_viewing(true, WINDOW).snapshot_options("viewing-proxy-dark", &options());
}

/// Узкое окно: кнопки окна внутри экрана, шапка не налезает сама на себя (находка
/// ревью шага 5).
///
/// Слева — длинные подписи (анализатор в 200 символов, сводка TCP-клиента со
/// счётчиками в гигабайтах, сводка двух сторон прокси): раньше правая группа вставала
/// за концом левой части и «Закрыть» уезжало за край окна.
#[test]
fn the_header_fits_a_narrow_window() {
    for (size, label) in [(NARROW, "720×560"), (LAPTOP, "1366×768")] {
        for (name, harness) in [
            ("viewing-text", long_analyzer_viewing(size)),
            ("recording-text", long_summary_recording(size)),
            ("viewing-proxy", proxy_viewing(false, size)),
            ("recording-proxy", proxy_recording(false, false, size)),
        ] {
            assert_header_fits(&harness, size, &format!("{name} на {label}"));
        }
    }
}

/// Шапка экрана в окне `size`: «Закрыть» внутри окна, свои виджеты шапки не налезают
/// друг на друга, длинные подписи обрезаны, а не вытолкнуты за край.
fn assert_header_fits(harness: &Harness<'static, BytetapApp>, size: (f32, f32), screen: &str) {
    use egui_kittest::kittest::NodeT as _;

    harness.check_accessibility();
    let header = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(size.0, 44.0));
    let mut found = Vec::new();
    for node in harness.root().children_recursive() {
        let accesskit = node.accesskit_node();
        if accesskit.is_hidden() || !INTERACTIVE.contains(&accesskit.role()) {
            continue;
        }
        let rect = node.rect();
        if !rect.intersects(header) {
            continue;
        }
        let label = accesskit
            .label()
            .or_else(|| accesskit.value())
            .unwrap_or_default();
        assert!(
            rect.left() >= -1.0 && rect.right() <= size.0 + 1.0 && rect.bottom() <= 44.0 + 1.0,
            "{screen}: {label:?} за краем шапки: {rect:?}"
        );
        found.push((label, rect));
    }
    let close = found
        .iter()
        .find(|(label, _)| label == "Закрыть")
        .unwrap_or_else(|| panic!("{screen}: в шапке нет «Закрыть»: {found:?}"));
    assert!(
        (close.1.right() - size.0).abs() <= 1.0,
        "{screen}: «Закрыть» не у правого края: {:?}",
        close.1
    );
    // Наложений нет: каждый виджет шапки стоит на своём месте.
    for (index, (label, rect)) in found.iter().enumerate() {
        for (other, other_rect) in &found[index + 1..] {
            let overlap = rect.intersect(*other_rect);
            assert!(
                overlap.width() <= 0.5 || overlap.height() <= 0.5,
                "{screen}: «{label}» {:?} налезает на «{other}» {other_rect:?}",
                rect
            );
        }
    }
    // Обрезанная подпись не теряется целиком: её полный текст остаётся в AccessKit, и
    // его находит поиск по точной подписи — так его читает экранный диктор (находка
    // повторного ревью шага 5).
    let full = header_full_texts(harness);
    assert!(
        !full.is_empty(),
        "{screen}: у шапки нет подписей с полным текстом"
    );
    for text in full {
        assert!(
            harness.query_by_label(&text).is_some(),
            "{screen}: в AccessKit нет полного текста {text:?}"
        );
    }
}

/// Полные тексты подписей шапки: сводка записи, имя файла и метаданные просмотра.
///
/// В узком окне они обрезаны «…», и полный текст виден только в подсказке и в AccessKit.
fn header_full_texts(harness: &Harness<'static, BytetapApp>) -> Vec<String> {
    if let Some(rec) = harness.state().recording() {
        let stats = rec.stats();
        return vec![rec.summary(&stats)];
    }
    if let Some(view) = harness.state().viewing() {
        // Подпись — та же, что рисует шапка: у журнала прокси в ней сводка двух сторон.
        return vec![
            bytetap_app::viewing::file_name(&view.path),
            bytetap_app::viewing::journal_meta(&view.summary.header, view.summary.duration_us),
        ];
    }
    Vec::new()
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn title_bar_light() {
    let _gpu = gpu();
    snapshot("titlebar-light", Screen::Scenarios, false, TITLE_BAR);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn title_bar_dark() {
    let _gpu = gpu();
    snapshot("titlebar-dark", Screen::Scenarios, true, TITLE_BAR);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn close_button_turns_red_on_hover() {
    let _gpu = gpu();
    let mut harness = harness(Screen::Scenarios, false, TITLE_BAR);
    harness.get_by_label("Закрыть").hover();
    harness.run_steps(2);
    harness.snapshot_options("titlebar-close-hover", &options());
}

/// Рамка окна 1 px по периметру, обе темы (жалоба владельца: окно без рамки сливается
/// с фоном). Снимок — окно целиком: рамка идёт по периметру.
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn the_window_has_a_one_pixel_frame() {
    let _gpu = gpu();
    let mut harness = harness(Screen::Scenarios, false, (400.0, 300.0));
    harness.run_steps(2);
    harness.snapshot_options("frame-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn the_window_has_a_one_pixel_frame_in_the_dark_theme() {
    let _gpu = gpu();
    let mut harness = harness(Screen::Scenarios, true, (400.0, 300.0));
    harness.run_steps(2);
    harness.snapshot_options("frame-dark", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn scenarios_light() {
    let _gpu = gpu();
    snapshot("scenarios-light", Screen::Scenarios, false, WINDOW);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn scenarios_dark() {
    let _gpu = gpu();
    snapshot("scenarios-dark", Screen::Scenarios, true, WINDOW);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journals_menu() {
    let _gpu = gpu();
    let mut harness = harness(Screen::Scenarios, false, WINDOW);
    harness.get_by_label("Журналы").click();
    harness.run_steps(4);
    harness.get_by_label("Открыть журнал…");
    harness.get_by_label("Открыть папку журналов");
    harness.snapshot_options("journals-menu-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_server_light() {
    let _gpu = gpu();
    snapshot(
        "step-server-light",
        Screen::Step(SessionMode::TcpServer),
        false,
        WINDOW,
    );
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_server_dark() {
    let _gpu = gpu();
    snapshot(
        "step-server-dark",
        Screen::Step(SessionMode::TcpServer),
        true,
        WINDOW,
    );
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_client_light() {
    let _gpu = gpu();
    snapshot(
        "step-client-light",
        Screen::Step(SessionMode::TcpClient),
        false,
        WINDOW,
    );
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_client_dark() {
    let _gpu = gpu();
    snapshot(
        "step-client-dark",
        Screen::Step(SessionMode::TcpClient),
        true,
        WINDOW,
    );
}

fn serial_more(dark: bool) -> Harness<'static, BytetapApp> {
    harness_with(
        settings(dark),
        None,
        Screen::Step(SessionMode::Serial),
        WINDOW,
        BytetapApp::with_more,
    )
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_serial_more_light() {
    let _gpu = gpu();
    serial_more(false).snapshot_options("step-serial-more-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_serial_more_dark() {
    let _gpu = gpu();
    serial_more(true).snapshot_options("step-serial-more-dark", &options());
}

/// Прокси: радио-пара топологии — группа `RadioGroup` с подписью «Топология»; щелчок,
/// стрелки и Space выбирают сторону, `toggled` говорит состояние, Tab обходит обе кнопки
/// (M4-В1, M4-AC13).
#[test]
fn the_proxy_topology_is_a_radio_pair() {
    use egui_kittest::kittest::NodeT as _;
    let mut harness = plain(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
    );
    // Пара — один узел дерева AccessKit: экранный диктор читает её как группу выбора.
    harness.get_by_role_and_label(Role::RadioGroup, "Топология");
    let client = harness.get_by_role_and_label(Role::RadioButton, "Анализатор подключается к ЛИС");
    assert_eq!(
        client.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True),
        "выбранная кнопка пары"
    );
    let server = harness.get_by_role_and_label(Role::RadioButton, "ЛИС подключается к анализатору");
    assert_eq!(
        server.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    assert_eq!(
        harness.state().settings().config.proxy_role,
        ProxyMode::AnalyzerClient
    );

    // Щелчок выбирает вторую топологию: смысл полей ниже меняется вместе с ним.
    harness
        .get_by_role_and_label(Role::RadioButton, "ЛИС подключается к анализатору")
        .click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().settings().config.proxy_role,
        ProxyMode::AnalyzerServer
    );
    harness.get_by_label("Адрес анализатора");

    // Стрелка с фокуса на кнопке переключает пару и уводит фокус выбранной кнопке:
    // из группы стрелки не выходят.
    let mut harness = plain(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
    );
    harness
        .get_by_role_and_label(Role::RadioButton, "Анализатор подключается к ЛИС")
        .focus();
    harness.run_steps(1);
    // Space нажимает кнопку пары, как обычную кнопку: Tab увёл фокус на вторую.
    harness.key_press(Key::Tab);
    harness.run_steps(1);
    harness.key_press(Key::Space);
    harness.run_steps(2);
    assert_eq!(
        harness.state().settings().config.proxy_role,
        ProxyMode::AnalyzerServer,
        "Space нажимает кнопку пары"
    );
    harness.key_press(Key::ArrowUp);
    harness.run_steps(2);
    assert_eq!(
        harness.state().settings().config.proxy_role,
        ProxyMode::AnalyzerClient
    );
    assert_eq!(
        harness.state().screen,
        Screen::Step(SessionMode::Proxy),
        "стрелки не уходят с шага"
    );
}

/// Замена текста поля: фокус, выделение всего (Ctrl+A), ввод — текст не приклеивается
/// к тому, что было в поле.
fn replace_in(harness: &mut Harness<'static, BytetapApp>, label: &str, text: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .focus();
    harness.run_steps(1);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    harness.run_steps(1);
    // Выделение снимается Backspace: так поле пустеет и когда новый текст пустой.
    harness.key_press(Key::Backspace);
    harness.run_steps(1);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(text);
    harness.run_steps(1);
}

/// Свежие настройки: цель прокси пуста — ни адреса, ни порта, только подсказки, — и
/// «Старт» просит адрес второй стороны, а сессии не собирает (M4-AC13). Подставленный
/// адрес чужого узла отправил бы к нему живой обмен.
#[test]
fn a_fresh_proxy_form_asks_for_the_target() {
    let settings = settings(false);
    assert_eq!(settings.config.proxy_host, "");
    assert_eq!(settings.config.proxy_port, 0);
    let mut harness = plain(settings, None, Screen::Step(SessionMode::Proxy));
    harness.get_by_role_and_label(Role::TextInput, "Адрес ЛИС");
    harness.get_by_role_and_label(Role::TextInput, "Порт цели");
    harness.get_by_label("Старт").click();
    harness.run_steps(2);
    harness.get_by_label("Укажите адрес ЛИС");
    assert!(
        harness.state().pending_session().is_none(),
        "без цели сессия не собирается"
    );
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Proxy));

    // Порт цели тоже пуст: подсказка «5000» — пример, а не значение.
    assert_eq!(
        harness.state().settings().config.proxy_port,
        0,
        "порт цели не подставлен"
    );
}

/// Ошибка адреса в прокси встаёт под своим полем: недоступный интерфейс — под
/// «Интерфейс», неразборчивая цель — под адресом цели, и «Дополнительно» не
/// раскрывается — оба поля на виду (M4-AC13).
#[test]
fn the_proxy_address_errors_keep_their_own_fields() {
    let harness = harness_with(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
        WINDOW,
        |app| {
            app.with_start_error(
                StartError::new(
                    ErrorCode::AddressUnavailable,
                    "cannot assign requested address",
                )
                .with_param("addr", "192.168.1.99"),
            )
        },
    );
    assert_eq!(
        harness
            .state()
            .start_error()
            .map(|error| error.field(SessionMode::Proxy)),
        Some(ErrorField::Interface),
        "недоступный адрес прослушивания — поле «Интерфейс»"
    );
    harness.get_by_label(
        "Адрес 192.168.1.99 недоступен на этом компьютере. Выберите другой интерфейс.",
    );
    {
        use egui_kittest::kittest::NodeT as _;
        let more = harness.get_by_role_and_label(Role::Button, "Дополнительно");
        assert_eq!(
            more.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::False),
            "«Дополнительно» не раскрыто: поле «Интерфейс» на виду"
        );
    }

    let harness = harness_with(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
        WINDOW,
        |app| {
            app.with_start_error(StartError::new(
                ErrorCode::AddressInvalid,
                "target is not an address",
            ))
        },
    );
    assert_eq!(
        harness
            .state()
            .start_error()
            .map(|error| error.field(SessionMode::Proxy)),
        Some(ErrorField::Address),
        "неразборчивая цель — поле адреса цели"
    );
    harness.get_by_label("Адрес цели указан неверно: нужен IPv4-адрес или имя.");
}

/// Прокси: поля проверяются до старта — порт прослушивания, адрес и порт цели; ошибка
/// встаёт под своим полем, а сборка конфигурации несёт топологию и цель (M4-AC13).
#[test]
fn the_proxy_form_checks_its_fields_and_builds_the_config() {
    let mut harness = plain(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
    );
    replace_in(&mut harness, "Порт, который слушает bytetap", "70000");
    replace_in(&mut harness, "Порт цели", "0");
    harness.run_steps(2);
    assert_eq!(
        harness
            .query_all_by_label("Порт — целое число от 1 до 65535.")
            .count(),
        2,
        "своя ошибка у каждого порта"
    );
    assert!(harness.state().pending_session().is_none());

    // Пустой адрес цели краснеет только после «Старт».
    replace_in(&mut harness, "Порт, который слушает bytetap", "50025");
    replace_in(&mut harness, "Порт цели", "5000");
    replace_in(&mut harness, "Адрес ЛИС", "");
    harness.get_by_label("Старт").click();
    harness.run_steps(2);
    harness.get_by_label("Укажите адрес ЛИС");
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Proxy));

    replace_in(&mut harness, "Адрес ЛИС", "10.0.0.5");
    harness.get_by_label("Старт").click();
    harness.run_steps(2);
    let config = harness
        .state()
        .pending_session()
        .expect("конфигурация прокси собрана");
    assert_eq!(
        config.source,
        Source::Proxy {
            listen: "0.0.0.0:50025".parse().unwrap(),
            target: "10.0.0.5:5000".into(),
            role: bytetap_core::transport::ProxyRole::AnalyzerClient,
            retry: bytetap_app::form::PROXY_RETRY,
        }
    );
    assert_eq!(
        harness
            .state()
            .settings()
            .last_run
            .as_ref()
            .map(|run| format!("{} {}", run.port, run.host)),
        Some("50025 10.0.0.5:5000".to_owned())
    );
}

/// Прокси: крупный адрес, предупреждение FR-8 и строка брандмауэра — на шаге, в
/// спокойных muted-строках, без баннера и модального окна (M4-AC7, M4-AC8).
#[test]
fn the_proxy_step_shows_the_address_and_the_warnings() {
    let harness = plain(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
    );
    harness.get_by_label("Впишите в анализатор");
    harness.get_by_label("192.168.1.10:50025");
    harness.get_by_label("Автоответ выключен: байты идут без изменений.");
    harness.get_by_label(
        "Многие ЛИС определяют анализатор по IP-адресу: пока идёт запись, в ЛИС укажите IP ноутбука — 192.168.1.10.",
    );
    harness.get_by_label(
        "При первом старте Windows спросит разрешение брандмауэра — разрешите для частных сетей.",
    );
    harness.get_by_role_and_label(Role::Button, "Скопировать");
    // Поля автоответчика в режиме нет вовсе.
    assert!(harness.query_by_label("Автоответ").is_none());

    // Топология «анализатор — сервер»: адрес вписывается в ЛИС, предупреждение о нём.
    let mut settings = proxy_settings(false, ProxyMode::AnalyzerServer);
    settings.config.listen_port = 5000;
    let harness = plain(settings, None, Screen::Step(SessionMode::Proxy));
    harness.get_by_label("Впишите в ЛИС");
    harness.get_by_label("192.168.1.10:5000");
    harness.get_by_label(
        "В ЛИС анализатор теперь виден с адреса ноутбука: если ЛИС определяет его по IP, укажите там адрес — 192.168.1.10.",
    );
}

/// Набор текста в поле: фокус и ввод в разных кадрах, курсор — в конце текста.
fn type_into(harness: &mut Harness<'static, BytetapApp>, label: &str, text: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .focus();
    harness.run_steps(1);
    harness.key_press(Key::End);
    harness.run_steps(1);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(text);
    harness.run_steps(2);
}

/// Ошибки у полей: неверный порт после ввода и «выберите порт» после «Старт».
fn field_errors(dark: bool) -> Harness<'static, BytetapApp> {
    let mut settings = settings(dark);
    settings.config.connect_host = String::new();
    let mut harness = harness_with(
        settings,
        None,
        Screen::Step(SessionMode::TcpClient),
        WINDOW,
        |app| app,
    );
    type_into(&mut harness, "Порт", "x");
    harness.get_by_label("Порт — целое число от 1 до 65535.");
    assert!(
        harness
            .query_by_label("Укажите адрес анализатора")
            .is_none(),
        "пустое поле не краснеет до «Старт»"
    );
    harness.get_by_label("Старт").click();
    harness.run_steps(4);
    harness.get_by_label("Укажите адрес анализатора");
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::TcpClient));
    assert!(harness.state().pending_session().is_none());
    harness
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_errors_light() {
    let _gpu = gpu();
    field_errors(false).snapshot_options("step-errors-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_errors_dark() {
    let _gpu = gpu();
    field_errors(true).snapshot_options("step-errors-dark", &options());
}

/// Без рендера: харнесс без wgpu, чтобы поведение проверялось на всех ОС.
fn plain(settings: Settings, store: Option<Store>, screen: Screen) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, store)
                .with_fixtures(fixtures())
                .with_screen(screen)
        });
    harness.run_steps(2);
    harness
}

#[test]
fn arrows_choose_a_card_and_enter_opens_it() {
    let mut harness = plain(settings(false), None, Screen::Scenarios);
    assert_eq!(
        harness.state().selected_card(),
        0,
        "последний сценарий выбран"
    );
    harness.key_press(Key::ArrowDown);
    harness.run_steps(2);
    assert_eq!(harness.state().selected_card(), 1);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::TcpClient));

    let mut harness = plain(settings(false), None, Screen::Scenarios);
    harness.key_press(Key::ArrowUp);
    harness.run_steps(2);
    assert_eq!(
        harness.state().selected_card(),
        3,
        "вверх с первой — на последнюю, прокси"
    );
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Proxy));
}

#[test]
fn the_last_card_shows_when_it_ran_and_a_click_opens_a_card() {
    let mut harness = plain(settings(false), None, Screen::Scenarios);
    harness.get_by_label_contains("последний раз: порт 5000");
    harness
        .get_by_role_and_label(
            Role::Button,
            "Кабель COM-порта. RS232 через USB-адаптер или нуль-модем",
        )
        .click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Serial));
    harness.get_by_label("Сценарии").click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
}

/// Четвёртая карточка стартового экрана — прокси: описание из макета M4-1, последний
/// запуск обеими сторонами, щелчок открывает шаг прокси (M4-В2, M4-AC13).
#[test]
fn the_proxy_card_opens_the_proxy_step() {
    let mut settings = settings(false);
    settings.last_run = Some(LastRun {
        mode: SessionMode::Proxy,
        port: "50025".into(),
        host: "10.0.0.5:5000".into(),
        baud: 0,
    });
    let mut harness = plain(settings, None, Screen::Scenarios);
    harness.get_by_label_contains("последний раз: :50025 → 10.0.0.5:5000");
    harness
        .get_by_role_and_label(
            Role::Button,
            "Встать между анализатором и ЛИС. bytetap слушает порт и пересылает дальше — запись при работающей ЛИС · последний раз: :50025 → 10.0.0.5:5000",
        )
        .click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Proxy));
    harness.get_by_role_and_label(Role::RadioButton, "Анализатор подключается к ЛИС");
}

#[test]
fn a_recent_journal_opens_the_viewer() {
    let mut harness = plain(settings(false), None, Screen::Scenarios);
    harness.get_by_label("Последние записи");
    harness.get_by_label_contains("ANALYZER · сегодня 10:15 · 42");
    harness
        .get_by_label_contains("bytetap-20261018180200-tcp_client.jsonl")
        .click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Viewing);
    // Файла записи нет — журнал не открылся, и под шапкой строка с причиной. Открытие
    // журнала идёт в фоне, поэтому ждём результата.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        harness.run_steps(2);
        if harness
            .query_by_label_contains("Не удалось открыть журнал")
            .is_some()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "журнал не открылся и не сообщил о причине"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(harness.state().viewing().is_none(), "журнал не открылся");
}

#[test]
fn widgets_have_roles_and_names() {
    let harness = plain(settings(false), None, Screen::Step(SessionMode::Serial));
    harness.get_by_role_and_label(Role::ComboBox, "Порт");
    harness.get_by_role_and_label(Role::ComboBox, "Скорость");
    harness.get_by_role_and_label(Role::ComboBox, "Формат");
    harness.get_by_role_and_label(Role::ComboBox, "Автоответ");
    harness.get_by_role_and_label(Role::Button, "Обновить список портов");
    harness.get_by_role_and_label(Role::Button, "Старт");
    harness.get_by_role_and_label(Role::Button, "Дополнительно");
    let harness = plain(settings(false), None, Screen::Step(SessionMode::TcpServer));
    harness.get_by_role_and_label(Role::TextInput, "Порт");
    harness.get_by_role_and_label(Role::Button, "Скопировать");
    harness.get_by_role_and_label(
        Role::Image,
        "Разрешение нужно только для частных сетей; само приложение в сеть не ходит.",
    );
    harness.get_by_label("192.168.1.10:5000");
    harness.get_by_label("другая сеть: 172.16.4.21 (Wi-Fi)");
}

#[test]
fn a_combo_box_changes_the_field() {
    let mut harness = plain(settings(false), None, Screen::Step(SessionMode::Serial));
    harness
        .get_by_role_and_label(Role::ComboBox, "Порт")
        .click();
    harness.run_steps(2);
    harness.get_by_label("USB-SERIAL CH340 (COM3)").click();
    harness.run_steps(2);
    assert_eq!(harness.state().settings().config.serial.port, "COM3");
    harness
        .get_by_role_and_label(Role::ComboBox, "Формат")
        .click();
    harness.run_steps(2);
    harness.get_by_label("7E1").click();
    harness.run_steps(2);
    let serial = &harness.state().settings().config.serial;
    assert_eq!((serial.data_bits, serial.stop_bits), (7, 1));
}

#[test]
fn start_builds_the_session_config() {
    let mut harness = plain(settings(false), None, Screen::Step(SessionMode::TcpServer));
    type_into(&mut harness, "Порт", "1");
    harness.get_by_label("Старт").click();
    harness.run_steps(2);
    let app = harness.state();
    assert_eq!(app.screen, Screen::Recording);
    let config = app.pending_session().expect("конфигурация собрана");
    assert_eq!(
        config.source,
        Source::TcpServer {
            listen: "0.0.0.0:50001".parse().unwrap()
        }
    );
    assert_eq!(
        app.settings()
            .last_run
            .as_ref()
            .map(|run| run.port.as_str()),
        Some("50001")
    );
}

#[test]
fn a_start_error_is_shown_under_its_field() {
    let mut settings = settings(false);
    settings.config.listen_host = "eth0".into();
    let mut harness = plain(settings, None, Screen::Step(SessionMode::TcpServer));
    harness.get_by_label("Старт").click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::TcpServer));
    harness.get_by_label("Адрес eth0 недоступен на этом компьютере. Выберите другой интерфейс.");
    // Поле интерфейса — в «Дополнительно»: оно раскрыто, чтобы ошибку было видно.
    harness.get_by_role_and_label(Role::ComboBox, "Интерфейс");
}

#[test]
fn a_profile_is_saved_applied_and_kept_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(dir.path().join("settings.json"));
    let mut settings = settings(false);
    settings.config.analyzer = "Прибор 1".into();
    let mut harness = plain(
        settings,
        Some(store.clone()),
        Screen::Step(SessionMode::Serial),
    );
    harness.get_by_label("Дополнительно").click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::ComboBox, "Порт")
        .click();
    harness.run_steps(2);
    harness.get_by_label("USB-SERIAL CH340 (COM3)").click();
    harness.run_steps(2);
    harness
        .get_all_by_label("Сохранить")
        .next()
        .unwrap()
        .click();
    harness.run_steps(3);
    assert_eq!(
        harness.get_all_by_label("Имя профиля").count(),
        2,
        "заголовок и подпись поля"
    );
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    assert_eq!(harness.state().settings().profiles.len(), 1);
    assert_eq!(harness.state().settings().profiles[0].name, "Прибор 1");

    // Последние настройки меняются, профиль — нет; выбор профиля возвращает его поля.
    harness
        .get_by_role_and_label(Role::ComboBox, "Профиль")
        .click();
    harness.run_steps(2);
    harness.get_by_label("последние настройки").click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::ComboBox, "Порт")
        .click();
    harness.run_steps(2);
    harness.get_by_label("— выберите порт —").click();
    harness.run_steps(2);
    assert_eq!(harness.state().settings().config.serial.port, "");
    harness
        .get_by_role_and_label(Role::ComboBox, "Профиль")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Прибор 1").click();
    harness.run_steps(2);
    assert_eq!(harness.state().settings().config.serial.port, "COM3");
    harness.get_by_label("Сценарии").click();
    harness.run_steps(2);

    let (loaded, warning) = store.load("");
    assert_eq!(warning, None);
    assert_eq!(loaded.profiles.len(), 1);
    assert_eq!(loaded.profiles[0].config.serial.port, "COM3");
    assert_eq!(loaded.config.mode, SessionMode::Serial);
    assert_eq!(loaded.config.serial.port, "COM3");

    // Удаление выбранного профиля.
    let mut harness = plain(
        loaded,
        Some(store.clone()),
        Screen::Step(SessionMode::Serial),
    );
    harness.get_by_label("Дополнительно").click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::ComboBox, "Профиль")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Прибор 1").click();
    harness.run_steps(2);
    harness.get_by_label("Удалить").click();
    harness.run_steps(2);
    assert!(store.load("").0.profiles.is_empty());
}

#[test]
fn typed_fields_are_kept_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(dir.path().join("settings.json"));
    let mut harness = plain(
        settings(false),
        Some(store.clone()),
        Screen::Step(SessionMode::TcpClient),
    );
    type_into(&mut harness, "Адрес анализатора", "7");
    harness.get_by_label("Сценарии").click();
    harness.run_steps(2);
    let (loaded, _) = store.load("");
    assert_eq!(loaded.config.connect_host, "192.168.1.507");
    assert_eq!(loaded.config.mode, SessionMode::TcpClient);
    let harness = plain(loaded, None, Screen::Step(SessionMode::TcpClient));
    let value = harness
        .get_by_role_and_label(Role::TextInput, "Адрес анализатора")
        .value();
    assert_eq!(value.as_deref(), Some("192.168.1.507"));
}

#[test]
fn every_screen_has_its_heading() {
    for (screen, heading) in [
        (Screen::Scenarios, "Как анализатор связан с этим ноутбуком?"),
        (
            Screen::Step(SessionMode::TcpServer),
            "Анализатор подключается ко мне",
        ),
        (
            Screen::Step(SessionMode::TcpClient),
            "Я подключаюсь к анализатору",
        ),
        (Screen::Step(SessionMode::Serial), "Кабель COM-порта"),
        (
            Screen::Step(SessionMode::Proxy),
            "Встать между анализатором и ЛИС",
        ),
        (Screen::Recording, "Стоп 00:00"),
    ] {
        let mut harness = Harness::builder()
            .with_size((1200.0, 800.0))
            .build_eframe(move |cc| {
                BytetapApp::new(&cc.egui_ctx, Settings::default(), None, None).with_screen(screen)
            });
        harness.run_steps(2);
        harness.get_by_label(heading);
        for button in ["Свернуть", "Развернуть", "Закрыть"] {
            harness.get_by_label(button);
        }
        // Во время записи «Журналы» нет: открыть журнал нельзя, папка — в меню «…».
        assert_eq!(
            harness.query_by_label("Журналы").is_some(),
            screen != Screen::Recording
        );
    }
}

/// Просмотр журнала: своя шапка с «×», именем файла и кнопкой комментария.
#[test]
fn the_journal_screen_has_its_own_header() {
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(|cc| {
            BytetapApp::new(&cc.egui_ctx, Settings::default(), None, None)
                .with_screen(Screen::Viewing)
        });
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::Button, "Закрыть журнал");
    harness.get_by_role_and_label(Role::Button, "Комментарий");
    for button in ["Свернуть", "Развернуть", "Закрыть"] {
        harness.get_by_label(button);
    }
}

/// Команды окна, которые приложение отправит на событие, кадр за кадром.
fn commands_after(
    harness: &mut Harness<'static, BytetapApp>,
    events: impl IntoIterator<Item = egui::Event>,
) -> Vec<egui::ViewportCommand> {
    let mut commands = Vec::new();
    for event in events {
        harness.event(event);
        harness.step();
        commands.extend(
            harness
                .output()
                .viewport_output
                .values()
                .flat_map(|viewport| viewport.commands.clone()),
        );
    }
    commands
}

/// Стартовый экран с кадром 1/60 с: два клика в соседних кадрах — двойной клик
/// (по умолчанию кадр харнесса — 0.25 с, больше задержки двойного клика).
fn window_harness() -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| {
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None).with_fixtures(fixtures())
        });
    harness.run_steps(2);
    harness
}

fn press_release(pos: egui::Pos2) -> [egui::Event; 3] {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    [egui::Event::PointerMoved(pos), button(true), button(false)]
}

/// Край окна выше кнопок и шапки (U1-AC3): нажатие в углу над «Закрыть» начинает
/// ресайз, а не закрывает окно; двойной клик по верхней кромке не разворачивает.
#[test]
fn the_resize_edge_takes_the_press_from_the_buttons_under_it() {
    use egui::ViewportCommand as Cmd;
    let mut harness = window_harness();
    let (w, _) = WINDOW;
    let commands = commands_after(&mut harness, press_release(egui::pos2(w - 2.0, 2.0)));
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Cmd::BeginResize(egui::ResizeDirection::NorthEast))),
        "{commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Cmd::Close | Cmd::Minimized(_) | Cmd::Maximized(_))),
        "{commands:?}"
    );

    // Двойной клик по верхней кромке над пустым местом шапки.
    let mut harness = window_harness();
    let top = egui::pos2(w / 2.0, 2.0);
    let mut events = press_release(top).to_vec();
    events.extend(press_release(top));
    let commands = commands_after(&mut harness, events);
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Cmd::Maximized(_) | Cmd::StartDrag)),
        "{commands:?}"
    );

    // Чуть ниже кромки двойной клик по шапке по-прежнему разворачивает окно.
    let mut harness = window_harness();
    let below = egui::pos2(w / 2.0, 20.0);
    let mut events = press_release(below).to_vec();
    events.extend(press_release(below));
    let commands = commands_after(&mut harness, events);
    assert!(
        commands.iter().any(|c| matches!(c, Cmd::Maximized(true))),
        "{commands:?}"
    );
}

/// Диалог имени профиля: «Сохранить», «Отмена» слева направо, и Tab идёт так же.
#[test]
fn the_profile_dialog_tabs_in_visual_order() {
    let mut settings = settings(false);
    settings.config.analyzer = "Прибор 1".into();
    let mut harness = plain(settings, None, Screen::Step(SessionMode::Serial));
    harness.get_by_label("Дополнительно").click();
    harness.run_steps(2);
    harness
        .get_all_by_label("Сохранить")
        .next()
        .unwrap()
        .click();
    harness.run_steps(3);
    let save = harness.get_all_by_label("Сохранить").last().unwrap().rect();
    let cancel = harness.get_by_label("Отмена").rect();
    assert!(save.right() < cancel.left(), "{save:?} {cancel:?}");
    harness.key_press(Key::Tab);
    harness.run_steps(2);
    assert!(
        harness
            .get_all_by_label("Сохранить")
            .last()
            .unwrap()
            .is_focused()
    );
    harness.key_press(Key::Tab);
    harness.run_steps(2);
    assert!(harness.get_by_label("Отмена").is_focused());
}

/// Настройки не прочитаны: хранилище до конца сессии ничего не пишет (U1-AC7).
#[test]
fn unread_settings_are_never_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, b"{\"theme\": \"dark\"}").unwrap();
    let store = Store::new(path.clone());
    let warning = LoadWarning::Reset {
        reason: "Access is denied. (os error 5)".into(),
    };
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(
                &cc.egui_ctx,
                Settings::default(),
                Some(warning),
                Some(store),
            )
            .with_fixtures(fixtures())
        });
    harness.run_steps(2);
    assert!(harness.state().settings_read_only());
    harness.get_by_label_contains("Access is denied. (os error 5)");
    harness.get_by_label_contains("изменения не сохранятся");
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    harness.get_by_label("Сценарии").click();
    harness.run_steps(2);
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"theme\": \"dark\"}");
}

/// Испорченный файл настроек — строка под шапкой с путём копии (U1-AC7).
#[test]
fn the_corrupt_settings_warning_is_shown() {
    let copy = std::path::PathBuf::from("C:/bytetap/settings.corrupt-20261021-090507.json");
    let warning = LoadWarning::Corrupt {
        copy: Some(copy.clone()),
    };
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, Settings::default(), Some(warning), None)
        });
    harness.run_steps(2);
    harness.get_by_label_contains(&copy.display().to_string());
    harness.get_by_label("Скрыть предупреждение").click();
    harness.run_steps(2);
    assert!(
        harness
            .query_by_label_contains(&copy.display().to_string())
            .is_none()
    );
}

/// Кириллица, греческий, стрелки направлений и управляющие символы — без «тофу»
/// (U1-AC5). Отсутствующий глиф роняет харнесс (`MissingGlyphPolicy::Panic`).
///
/// Греческого нет в Plex Mono, а лента показывает им единицы измерения («γ-GT
/// μmol/L»): у моноширинных семейств запасной Plex Sans, поэтому глиф есть и там.
#[test]
fn the_fonts_have_every_glyph_the_interface_uses() {
    let sample = "Ёжик съел щи · «Журнал» — №5 … ‹CR›‹LF›‹STX›‹ETX› → ← ↓ ↑ ✓ × \
                  ° ± ² µ ‹x2039›‹x203A› γ μ Ω α ΓΔΘΛΞΠΣΦΨ";
    let mut harness = Harness::builder()
        .with_size((800.0, 200.0))
        .with_context_setup(|ctx| {
            theme::install_fonts(ctx);
            theme::apply(ctx, false);
        })
        .build_ui(|ui| {
            ui.label(sample);
            ui.label(egui::RichText::new(sample).font(theme::mono(13.0)));
            ui.label(egui::RichText::new(sample).font(theme::mono_medium(13.0)));
            ui.label(egui::RichText::new(sample).font(theme::sans_semibold(20.0)));
            ui.label(egui::RichText::new(sample).font(theme::sans_medium(13.0)));
        });
    harness.run();
    // Проверка по семействам, а не только по нарисованным строкам: глиф, которого нет
    // ни в одном семействе, виден здесь по имени, а не «тофу» на снимке.
    let fonts = [
        theme::sans(13.0),
        theme::mono(13.0),
        theme::mono_medium(13.0),
        theme::sans_medium(13.0),
        theme::sans_semibold(20.0),
    ];
    let mut missing = Vec::new();
    harness.ctx.fonts_mut(|fonts_view| {
        for font in &fonts {
            for c in sample.chars() {
                if !fonts_view.has_glyph(font, c) {
                    missing.push(format!("{c:?} в {:?}", font.family));
                }
            }
        }
    });
    assert!(missing.is_empty(), "нет глифов: {missing:?}");
}

/// Строка ленты с греческим в данных, UTF-8: «γ-GT μmol/L» (U1-AC5).
///
/// Моноширинный Plex Mono греческого не содержит, и строка результата анализатора
/// показывала бы «тофу» вместо единиц измерения. Харнесс роняет тест на отсутствующем
/// глифе, а подпись строки в AccessKit проверяется целиком.
#[test]
fn a_feed_row_shows_greek_units_without_tofu() {
    use bytetap_app::settings::Encoding;
    use bytetap_core::journal::{Data, Dir, Event};

    let data = Event::Data(Data::new(
        "2026-10-19T23:27:10.046000+03:00",
        1_000,
        1,
        Dir::AnalyzerToLis,
        false,
        "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019232710||ORU^R01|MSG1|P|2.3.1\rOBX|1|NM|GGT^γ-GT||5.4|μmol/L\r\x1c\r"
            .as_bytes(),
    ));
    let fixture = RecordingFixture {
        events: vec![data],
        stats: bytetap_core::journal::Stats {
            connections: 1,
            bytes_analyzer_to_lis: 1,
            bytes_lis_to_analyzer: 0,
        },
        elapsed: std::time::Duration::from_secs(1),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: None,
        dropped: 0,
    };
    let mut settings = settings(false);
    settings.feed.encoding = Encoding::Utf8;
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .with_theme(Theme::Light)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture)
        });
    harness.run_steps(8);
    // Строка ленты видна целиком: «γ-GT» и «μmol/L» дошли до подписи AccessKit.
    harness.get_by_label_contains("γ-GT");
    harness.get_by_label_contains("μmol/L");
}

/// Масштаб экрана Windows: 100, 125 и 150 % (U1-AC26).
///
/// В egui всё меряется точками, поэтому раскладка от масштаба не зависит; масштаб
/// меняет растр: текст, иконки Lucide и свою рамку окна в 1 физический пиксель.
/// Снимок в 1200×800 точек на 125 % — это 1500×1000 пикселей.
fn scaled_with(
    screen: Screen,
    dark: bool,
    pixels_per_point: f32,
    prepare: impl FnOnce(BytetapApp) -> BytetapApp + 'static,
) -> Harness<'static, BytetapApp> {
    let settings = settings(dark);
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .with_pixels_per_point(pixels_per_point)
        .with_theme(if dark { Theme::Dark } else { Theme::Light })
        .wgpu()
        .build_eframe(move |cc| {
            prepare(
                BytetapApp::new(&cc.egui_ctx, settings, None, None)
                    .with_fixtures(fixtures())
                    .with_screen(screen),
            )
        });
    harness.run_steps(8);
    harness
}

/// Снимок экрана на масштабе `ppp`.
fn scaled_snapshot(
    name: &str,
    screen: Screen,
    ppp: f32,
    prepare: impl FnOnce(BytetapApp) -> BytetapApp + 'static,
) {
    scaled_with(screen, false, ppp, prepare).snapshot_options(name, &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn start_at_125_percent() {
    let _gpu = gpu();
    scaled_snapshot("start-125-light", Screen::Scenarios, 1.25, |app| app);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn start_at_150_percent() {
    let _gpu = gpu();
    scaled_snapshot("start-150-light", Screen::Scenarios, 1.5, |app| app);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_at_125_percent() {
    let _gpu = gpu();
    scaled_snapshot("recording-text-125-light", Screen::Scenarios, 1.25, |app| {
        app.with_recording_fixture(recording_fixture(0))
    });
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_at_150_percent() {
    let _gpu = gpu();
    scaled_snapshot("recording-text-150-light", Screen::Scenarios, 1.5, |app| {
        app.with_recording_fixture(recording_fixture(0))
    });
}

/// На 125 и 150 % у каждого своего виджета та же роль и подпись, что на 100 %,
/// и ни один не вылезает за окно: масштаб не сдвигает раскладку (U1-AC26).
#[test]
fn the_layout_is_the_same_at_every_scale() {
    for pixels_per_point in [1.0, 1.25, 1.5] {
        for screen in [
            Screen::Scenarios,
            Screen::Step(SessionMode::TcpServer),
            Screen::Step(SessionMode::Serial),
        ] {
            let harness = scaled_with(screen, false, pixels_per_point, |app| app);
            let nodes = audit_nodes(&harness, &format!("{screen:?} на {pixels_per_point}"));
            let rect = |label: &str| {
                nodes
                    .iter()
                    .find(|(_, name, _)| name == label)
                    .map(|(_, _, rect)| *rect)
                    .unwrap_or_else(|| panic!("нет «{label}» на {pixels_per_point}"))
            };
            // Шапка и колонка формы стоят на местах в точках: масштаб их не двигает.
            let close = rect("Закрыть");
            assert!(
                (close.right() - WINDOW.0).abs() <= 1.0,
                "«Закрыть» на {pixels_per_point}: {close:?}"
            );
            if matches!(screen, Screen::Step(_)) {
                let start = rect("Старт");
                assert!(
                    (start.right() - start.left() - 512.0).abs() <= 1.0,
                    "«Старт» на {pixels_per_point}: {start:?}"
                );
            }
            for (role, name, rect) in &nodes {
                assert!(
                    rect.left() >= -1.0 && rect.right() <= WINDOW.0 + 1.0,
                    "{role:?} «{name}» за краем окна на {pixels_per_point}: {rect:?}"
                );
            }
        }
    }
}

/// Синтетическая запись как на снимке UI v2: два соединения, сообщения HL7 с кириллицей
/// в Windows-1251, автоответы, замечание и закрытия.
fn recording_fixture(extra: usize) -> RecordingFixture {
    use bytetap_core::journal::{Close, ClosedBy, Data, Dir, Event, InfoEvent, Open, Stats};

    let cp1251 = |text: &str| -> Vec<u8> {
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(text);
        bytes.into_owned()
    };
    let at = |ms: u64| {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-19T23:27:10.046+03:00").unwrap();
        (base + chrono::Duration::milliseconds(ms as i64))
            .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
            .to_string()
    };
    let msg = |id: &str, body: &str| {
        cp1251(&format!(
            "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019232710||ORU^R01|{id}|P|2.3.1\r{body}\x1c\r"
        ))
    };
    let ack = |id: &str| {
        cp1251(&format!(
            "\x0bMSH|^~\\&|LIS||ANALYZER|LAB|20261019232711||ACK^R01|BT{id}|P|2.3.1\rMSA|AA|{id}\r\x1c\r"
        ))
    };
    let data = |ms: u64, conn: u32, dir: Dir, synthetic: bool, bytes: &[u8]| {
        Event::Data(Data::new(at(ms), ms * 1000, conn, dir, synthetic, bytes))
    };
    let mut events = vec![
        Event::Open(Open {
            t: at(0),
            mono_us: 0,
            conn: 1,
            peer: "192.168.1.51:49152".into(),
            local: "0.0.0.0:5000".into(),
            link: None,
        }),
        Event::Open(Open {
            t: at(0),
            mono_us: 0,
            conn: 2,
            peer: "192.168.1.52:49152".into(),
            local: "0.0.0.0:5000".into(),
            link: None,
        }),
        data(
            500,
            1,
            Dir::AnalyzerToLis,
            false,
            &msg(
                "MSG0002",
                "PID|1||000001||PATIENT^TEST\rOBX|1|NM|GLU^Glucose||5.4|mmol/L\r",
            ),
        ),
        data(
            1000,
            1,
            Dir::AnalyzerToLis,
            false,
            &msg(
                "MSG0003",
                "PID|1||000001||ПАЦИЕНТ^ТЕСТ\rOBX|1|NM|GLU^Глюкоза||5.1|ммоль/л\r",
            ),
        ),
        data(1500, 1, Dir::LisToAnalyzer, true, &ack("MSG0004")),
        data(
            2000,
            2,
            Dir::AnalyzerToLis,
            false,
            &msg(
                "MSG0005B",
                "PID|1||000002||PATIENT^TEST\rOBX|1|NM|GLU^Glucose||6.0|mmol/L\r",
            ),
        ),
        data(
            2500,
            1,
            Dir::AnalyzerToLis,
            false,
            &msg("MSG0006", "QRD|20261019|R|I|MSG0002|||10^RD\r"),
        ),
        data(3000, 2, Dir::LisToAnalyzer, true, &ack("MSG0007B")),
        Event::Info(InfoEvent {
            t: at(3500),
            mono_us: 3_500_000,
            conn: None,
            code: None,
            repeats: None,
            message: "Autoresponder: ACK sent".into(),
        }),
    ];
    for index in 0..extra {
        let ms = 3600 + index as u64 * 100;
        events.push(data(
            ms,
            1,
            Dir::AnalyzerToLis,
            false,
            &msg(&format!("MSG1{index:03}"), "PID|1||000001||PATIENT^TEST\r"),
        ));
    }
    let end = 4000 + extra as u64 * 100;
    for conn in [1, 2] {
        events.push(Event::Close(Close {
            t: at(end),
            mono_us: end * 1000,
            conn,
            by: ClosedBy::Analyzer,
            reason: "eof".into(),
        }));
    }
    RecordingFixture {
        events,
        stats: Stats {
            connections: 2,
            bytes_analyzer_to_lis: 629,
            bytes_lis_to_analyzer: 195,
        },
        elapsed: std::time::Duration::from_secs(5),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: Some(format!(r"{LOG_DIR}\bytetap-20261019232710-tcp_server.jsonl").into()),
        dropped: 0,
    }
}

/// Настройки шага прокси: порт прослушивания 50025, цель — вторая сторона (M4-2, M4-3).
fn proxy_settings(dark: bool, role: ProxyMode) -> Settings {
    let mut settings = settings(dark);
    settings.config.mode = SessionMode::Proxy;
    settings.config.proxy_role = role;
    settings.config.listen_port = match role {
        ProxyMode::AnalyzerClient => 50025,
        ProxyMode::AnalyzerServer => 5000,
    };
    match role {
        ProxyMode::AnalyzerClient => {
            settings.config.proxy_host = "10.0.0.5".into();
            settings.config.proxy_port = 5000;
        }
        ProxyMode::AnalyzerServer => {
            settings.config.proxy_host = "192.168.1.51".into();
            settings.config.proxy_port = 5000;
        }
    }
    settings
}

/// Шаг прокси: топология, заполненные поля, «Дополнительно» по флагу, необязательная
/// ошибка у порта прослушивания (порт 5000 занят — макет M4-3).
fn proxy_step(
    dark: bool,
    role: ProxyMode,
    more: bool,
    busy_port: bool,
) -> Harness<'static, BytetapApp> {
    let settings = proxy_settings(dark, role);
    let port = settings.config.listen_port;
    harness_with(
        settings,
        None,
        Screen::Step(SessionMode::Proxy),
        WINDOW,
        move |app| {
            let app = if more { app.with_more() } else { app };
            if busy_port {
                app.with_start_error(
                    StartError::new(ErrorCode::PortInUse, "address in use")
                        .with_param("port", port.to_string()),
                )
            } else {
                app
            }
        },
    )
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_proxy_light() {
    let _gpu = gpu();
    proxy_step(false, ProxyMode::AnalyzerClient, false, false)
        .snapshot_options("step-proxy-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_proxy_dark() {
    let _gpu = gpu();
    proxy_step(true, ProxyMode::AnalyzerClient, false, false)
        .snapshot_options("step-proxy-dark", &options());
}

/// Топология «ЛИС подключается к анализатору»: «Дополнительно» раскрыто, порт 5000
/// занят — ошибка под полем порта, как в макете M4-3.
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_proxy_server_light() {
    let _gpu = gpu();
    proxy_step(false, ProxyMode::AnalyzerServer, true, true)
        .snapshot_options("step-proxy-server-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_proxy_server_dark() {
    let _gpu = gpu();
    proxy_step(true, ProxyMode::AnalyzerServer, true, true)
        .snapshot_options("step-proxy-server-dark", &options());
}

/// Запись прокси: сводка двух сторон в шапке, две пары в ленте, «#1 ЛИС закрыла
/// соединение» (M4-4). `error` — недоступная цель строкой `danger` (M4-6).
fn proxy_recording_fixture(error: bool) -> RecordingFixture {
    use bytetap_core::journal::{
        Close, ClosedBy, Data, Dir, ErrorEvent, Event, Link, LinkRole, Open, Stats,
    };

    let cp1251 = |text: &str| -> Vec<u8> {
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(text);
        bytes.into_owned()
    };
    let at = |ms: u64| {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-22T10:15:03.000+03:00").unwrap();
        (base + chrono::Duration::milliseconds(ms as i64))
            .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
            .to_string()
    };
    let msg = |id: &str| {
        cp1251(&format!(
            "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261022101503||ORU^R01|{id}|P|2.3.1\rPID|1||000123||PATIENT^TEST\rOBX|1|NM|GLU^Glucose||5.4|mmol/L|3.9-6.1|N|||F\r\x1c\r"
        ))
    };
    let ack = |id: &str| {
        cp1251(&format!(
            "\x0bMSH|^~\\&|LIS||ANALYZER|LAB|20261022101503||ACK^R01|BT{id}|P|2.3.1\rMSA|AA|{id}\r\x1c\r"
        ))
    };
    let open = |conn: u32, peer: &str, ms: u64| {
        Event::Open(Open {
            t: at(ms),
            mono_us: ms * 1000,
            conn,
            peer: peer.into(),
            local: "192.168.1.10:50025".into(),
            link: Some(Link {
                role: LinkRole::Lis,
                peer: "10.0.0.5:5000".into(),
                local: "192.168.1.10:50231".into(),
            }),
        })
    };
    let data = |ms: u64, conn: u32, dir: Dir, bytes: &[u8]| {
        Event::Data(Data::new(at(ms), ms * 1000, conn, dir, false, bytes))
    };
    let mut events = Vec::new();
    if error {
        // Цель недоступна до первого соединения: `open` нет, есть только ошибка.
        events.push(Event::Error(ErrorEvent {
            t: at(0),
            mono_us: 0,
            conn: None,
            code: Some("proxy_connect_failed".into()),
            message: "connect failed".into(),
        }));
    }
    events.extend([
        open(1, "192.168.1.51:49152", 3_000),
        data(3_000, 1, Dir::AnalyzerToLis, &msg("MSG0001")),
        data(3_004, 1, Dir::LisToAnalyzer, &ack("MSG0001")),
        Event::Close(Close {
            t: at(6_120),
            mono_us: 6_120_000,
            conn: 1,
            by: ClosedBy::Lis,
            reason: "eof".into(),
        }),
    ]);
    if !error {
        events.extend([
            open(2, "192.168.1.51:49153", 9_460),
            data(9_460, 2, Dir::AnalyzerToLis, &msg("MSG0002")),
        ]);
    }
    let connection = if error { 1 } else { 2 };
    RecordingFixture {
        events,
        stats: Stats {
            connections: connection,
            bytes_analyzer_to_lis: if error { 1_229 } else { 4_710 },
            bytes_lis_to_analyzer: if error { 386 } else { 1_229 },
        },
        elapsed: std::time::Duration::from_secs(41),
        local_addr: "192.168.1.10:50025".into(),
        journal_path: Some(format!("{LOG_DIR}\\bytetap-20261022101503-proxy.jsonl").into()),
        dropped: 0,
    }
}

fn proxy_recording(dark: bool, error: bool, size: (f32, f32)) -> Harness<'static, BytetapApp> {
    let settings = proxy_settings(dark, ProxyMode::AnalyzerClient);
    harness_with(settings, None, Screen::Scenarios, size, move |app| {
        app.with_recording_fixture(proxy_recording_fixture(error))
    })
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_proxy_light() {
    let _gpu = gpu();
    proxy_recording(false, false, WINDOW).snapshot_options("recording-proxy-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_proxy_dark() {
    let _gpu = gpu();
    proxy_recording(true, false, WINDOW).snapshot_options("recording-proxy-dark", &options());
}

/// Цель недоступна: строка ошибки цветом `danger` над первой парой (M4-6).
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_proxy_error_light() {
    let _gpu = gpu();
    proxy_recording(false, true, WINDOW)
        .snapshot_options("recording-proxy-error-light", &options());
}

fn recording(dark: bool, view: FeedView, extra: usize) -> Harness<'static, BytetapApp> {
    let mut settings = settings(dark);
    settings.feed.view = view;
    harness_with(settings, None, Screen::Scenarios, WINDOW, move |app| {
        app.with_recording_fixture(recording_fixture(extra))
    })
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_text_light() {
    let _gpu = gpu();
    recording(false, FeedView::Text, 0).snapshot_options("recording-text-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_text_dark() {
    let _gpu = gpu();
    recording(true, FeedView::Text, 0).snapshot_options("recording-text-dark", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_hex_light() {
    let _gpu = gpu();
    recording(false, FeedView::Hex, 0).snapshot_options("recording-hex-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_hex_dark() {
    let _gpu = gpu();
    recording(true, FeedView::Hex, 0).snapshot_options("recording-hex-dark", &options());
}

/// Прокрутка вверх останавливает догон; пришедшие строки — «↓ N новых» (U1-AC16).
fn scrolled_up(harness: &mut Harness<'static, BytetapApp>) {
    harness.event(egui::Event::PointerMoved(egui::pos2(600.0, 400.0)));
    harness.run_steps(2);
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 600.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(4);
    assert!(
        !harness.state().recording().unwrap().follow(),
        "догон выключен"
    );
    let shared = harness.state().recording().unwrap().shared();
    for event in recording_fixture(5).events.into_iter().skip(9).take(5) {
        if let Some(event) = bytetap_app::feed::FeedEvent::from_session(&event) {
            bytetap_app::live::lock(&shared).push(std::sync::Arc::new(event));
        }
    }
    harness.run_steps(3);
    assert_eq!(harness.state().recording().unwrap().pending(), 5);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_new_rows_light() {
    let _gpu = gpu();
    let mut harness = recording(false, FeedView::Text, 40);
    scrolled_up(&mut harness);
    harness.get_by_label("5 новых");
    harness.snapshot_options("recording-new-rows-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_new_rows_dark() {
    let _gpu = gpu();
    let mut harness = recording(true, FeedView::Text, 40);
    scrolled_up(&mut harness);
    harness.get_by_label("5 новых");
    harness.snapshot_options("recording-new-rows-dark", &options());
}

/// Узкое окно и ноутбук: шапка с длинной сводкой и счётчиками (находка ревью шага 5).
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_text_at_720() {
    let _gpu = gpu();
    long_summary_recording(NARROW).snapshot_options("recording-text-720-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn recording_text_at_1366() {
    let _gpu = gpu();
    long_summary_recording(LAPTOP).snapshot_options("recording-text-1366-light", &options());
}

/// Просмотр журнала с анализатором в 200 символов: шапка не уводит кнопки за край.
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn viewing_text_at_720() {
    let _gpu = gpu();
    long_analyzer_viewing(NARROW).snapshot_options("viewing-text-720-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn viewing_text_at_1366() {
    let _gpu = gpu();
    long_analyzer_viewing(LAPTOP).snapshot_options("viewing-text-1366-light", &options());
}

/// Широкое окно с коротким анализатором: имя файла не режется «…».
///
/// Имя всегда брало не больше 45 % остатка шапки, и короткое имя с короткими метаданными
/// показывалось с «…» на ровном месте (находка контрольного ревью шага 5). Снимок
/// 910 px — эталон вида, тест сверяет ширину подписи с шириной самого текста.
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn a_short_analyzer_keeps_the_journal_name_whole() {
    let _gpu = gpu();
    let mut harness = analyzer_viewing((910.0, 600.0), Some("ANALYZER".to_owned()));
    let name =
        bytetap_app::viewing::file_name(&harness.state().viewing().expect("журнал открыт").path);
    let full = harness.ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                name.clone(),
                theme::mono(bytetap_app::theme::tokens::TEXT_XS),
                egui::Color32::WHITE,
            )
            .size()
            .x
    });
    let width = harness.get_by_label(&name).rect().width();
    assert!(
        (width - full).abs() <= 0.5,
        "имя обрезано: {width} px из {full}"
    );
    harness.snapshot_options("viewing-short-analyzer-910-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journal_failure_light() {
    let _gpu = gpu();
    journal_failure(false).snapshot_options("journal-failure-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journal_failure_dark() {
    let _gpu = gpu();
    journal_failure(true).snapshot_options("journal-failure-dark", &options());
}

/// Стартовый экран после отказа журнала: строка причины и «Открыть записанное».
fn journal_failure(dark: bool) -> Harness<'static, BytetapApp> {
    let failure = JournalFailure {
        code: JournalFailureCode::DiskFull,
        message: "No space left on device (os error 28)".into(),
    };
    let mut harness = harness_with(
        settings(dark),
        None,
        Screen::Scenarios,
        WINDOW,
        move |app| {
            app.with_journal_failure(
                failure,
                Some(format!(r"{LOG_DIR}\bytetap-20261019232710-tcp_server.jsonl").into()),
            )
        },
    );
    harness.get_by_label("Запись в журнал прервана: на диске нет места");
    harness.run_steps(2);
    harness
}

fn plain_recording(view: FeedView, extra: usize) -> Harness<'static, BytetapApp> {
    let mut settings = settings(false);
    settings.feed.view = view;
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(recording_fixture(extra))
        });
    harness.run_steps(3);
    harness
}

/// Шапка записи и строки ленты — с ролями и подписями для AccessKit (U1-AC14, AC15).
#[test]
fn the_recording_header_and_rows_have_names() {
    let harness = plain_recording(FeedView::Text, 0);
    harness.get_by_role_and_label(Role::Button, "Стоп 00:05");
    harness.get_by_role_and_label(Role::Button, "Меню записи");
    harness.get_by_label("подключено · :5000 · HL7 (авто)");
    harness.get_by_label("→ 629\u{a0}Б ← 195\u{a0}Б 2 соед.");
    harness.get_by_role_and_label(Role::ComboBox, "Вид ленты");
    harness.get_by_role_and_label(Role::ComboBox, "Кодировка");
    harness.get_by_role_and_label(Role::Button, "все");
    harness.get_by_role_and_label(Role::Button, "#2");
    harness.get_by_label("23:27:10.046 #1 подключился 192.168.1.51:49152");
    harness.get_by_label_contains("23:27:11.046 → #1 ‹VT›MSH|^~\\&|ANALYZER");
    harness.get_by_label_contains("PID|1||000001||ПАЦИЕНТ^ТЕСТ‹CR›");
    harness.get_by_label("23:27:13.546 Autoresponder: ACK sent");
    harness.get_by_label("23:27:14.046 #1 отключился · анализатор");
}

/// Вид и кодировка ленты меняются в панели и сохраняются в настройках (FR-30).
#[test]
fn the_feed_view_and_encoding_are_saved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::new(dir.path().join("settings.json"));
    let mut harness = harness_plain_store(store.clone());
    harness
        .get_by_role_and_label(Role::ComboBox, "Кодировка")
        .click();
    harness.run_steps(2);
    harness.get_by_label("UTF-8").click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(Role::ComboBox, "Вид ленты")
        .click();
    harness.run_steps(2);
    harness.get_by_label("HEX").click();
    harness.run_steps(3);
    let (saved, _) = store.load("");
    assert_eq!(saved.feed.view, FeedView::Hex);
    assert_eq!(saved.feed.encoding, bytetap_app::settings::Encoding::Utf8);
    // HEX: кодировки нет, строки дампа с полями смещения.
    assert!(
        harness
            .query_by_role_and_label(Role::ComboBox, "Кодировка")
            .is_none()
    );
    assert!(
        harness
            .get_all_by_label_contains("0000 0B 4D 53 48")
            .count()
            >= 1,
        "первая строка дампа фрагмента"
    );
}

fn harness_plain_store(store: Store) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(false), None, Some(store))
                .with_fixtures(fixtures())
                .with_recording_fixture(recording_fixture(0))
        });
    harness.run_steps(3);
    harness
}

/// Фильтр соединения оставляет его строки и события без соединения (FR-26).
#[test]
fn the_connection_filter_keeps_one_connection() {
    let mut harness = plain_recording(FeedView::Text, 0);
    let all = harness.state().recording().unwrap().rows();
    harness.get_by_role_and_label(Role::Button, "#2").click();
    harness.run_steps(2);
    let rows = harness.state().recording().unwrap().rows();
    // Второе соединение: open, два фрагмента, close — и замечание без соединения.
    assert_eq!((all, rows), (11, 5));
    assert!(harness.query_by_label_contains("ПАЦИЕНТ").is_none());
    harness.get_by_role_and_label(Role::Button, "все").click();
    harness.run_steps(2);
    assert_eq!(harness.state().recording().unwrap().rows(), all);
}

/// Контекстное меню строки: копирование текста, HEX и Base64 (U1-AC16).
#[test]
fn a_row_has_a_copy_menu() {
    let mut harness = plain_recording(FeedView::Text, 0);
    harness
        .get_by_label_contains("23:27:11.546 ← #1")
        .click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Скопировать текст");
    harness.get_by_label("Скопировать HEX").click();
    harness.step();
    let copied = harness
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        });
    assert!(copied.is_some_and(|text| text.starts_with("0B 4D 53 48")));
}

/// «Скопировать текст» отдаёт весь фрагмент, а не первые 2048 байт строки ленты
/// (U1-AC16).
#[test]
fn the_copied_text_is_the_whole_fragment() {
    use bytetap_core::journal::{Data, Dir, Event};

    // Фрагмент длиннее предела показа в строке ленты (2048 байт).
    let payload = vec![b'A'; 3000];
    let fixture = RecordingFixture {
        events: vec![Event::Data(Data::new(
            "2026-10-19T23:27:11.546+03:00",
            1_546_000,
            1,
            Dir::AnalyzerToLis,
            false,
            &payload,
        ))],
        stats: bytetap_core::journal::Stats::default(),
        elapsed: std::time::Duration::from_secs(5),
        local_addr: String::new(),
        journal_path: None,
        dropped: 0,
    };
    let mut settings = settings(false);
    settings.feed.view = FeedView::Text;
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture)
        });
    harness.run_steps(3);
    harness
        .get_by_label_contains("23:27:11.546 → ")
        .click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Скопировать текст").click();
    harness.step();
    let copied = harness
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
        .expect("текст скопирован");
    assert_eq!(
        copied.chars().count(),
        3000,
        "скопирован весь фрагмент, а не предел показа"
    );
}

/// «↓ N новых» возвращает ленту вниз и включает догон.
#[test]
fn the_new_rows_pill_returns_to_the_bottom() {
    let mut harness = plain_recording(FeedView::Text, 40);
    scrolled_up(&mut harness);
    harness.get_by_label("5 новых").click();
    harness.run_steps(4);
    let rec = harness.state().recording().unwrap();
    assert!(rec.follow());
    assert_eq!(rec.pending(), 0);
    assert!(harness.query_by_label("5 новых").is_none());
}

/// «Стоп» без сессии (снимки, тесты) возвращает к сценариям.
#[test]
fn stop_returns_to_the_scenarios() {
    let mut harness = plain_recording(FeedView::Text, 0);
    harness
        .get_by_role_and_label(Role::Button, "Стоп 00:05")
        .click();
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
}

/// 10 000 строк в ленте: кадр рисует только видимые (`show_rows`), буфер не растёт
/// сверх предела (U1-AC16).
#[test]
fn ten_thousand_rows_draw_only_the_visible_ones() {
    let mut harness = plain_recording(FeedView::Text, 10_200);
    harness.run_steps(2);
    let rec = harness.state().recording().unwrap();
    assert_eq!(rec.len(), bytetap_app::feed::FEED_CAPACITY);
    assert_eq!(rec.rows(), bytetap_app::feed::FEED_CAPACITY);
    // Подпись каждой строки ленты начинается со времени «23:…».
    let rows = harness.get_all_by_label_contains("23:").count();
    assert!((20..60).contains(&rows), "видимых строк {rows}");
    let started = std::time::Instant::now();
    harness.run_steps(10);
    eprintln!("10 кадров с 10 000 строк: {:?}", started.elapsed());
}

/// Роли AccessKit, по которым человек действует: у каждой должна быть подпись.
const INTERACTIVE: [Role; 10] = [
    Role::Button,
    Role::ComboBox,
    Role::TextInput,
    Role::MultilineTextInput,
    Role::CheckBox,
    Role::RadioButton,
    Role::MenuItem,
    Role::ListBoxOption,
    Role::ScrollBar,
    Role::Link,
];

/// Шаг прокси с раскрытым «Дополнительно» в окне `size`: поля анализатора, комментария,
/// папки журналов и профиля.
fn proxy_more(size: (f32, f32)) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder().with_size(size).build_eframe(move |cc| {
        BytetapApp::new(
            &cc.egui_ctx,
            proxy_settings(false, ProxyMode::AnalyzerClient),
            None,
            None,
        )
        .with_fixtures(fixtures())
        .with_screen(Screen::Step(SessionMode::Proxy))
        .with_more()
    });
    harness.run_steps(2);
    harness
}

/// Экран с раскрытым «Дополнительно»: поля профилей и папки журналов.
fn plain_more(screen: Screen) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(false), None, None)
                .with_fixtures(fixtures())
                .with_screen(screen)
                .with_more()
        });
    harness.run_steps(2);
    harness
}

/// Какие роли и подписи нашлись на экране: `(роль, подпись, прямоугольник)`.
fn audit_nodes(
    harness: &Harness<'static, BytetapApp>,
    screen: &str,
) -> Vec<(Role, String, egui::Rect)> {
    audit_nodes_in(harness, screen, WINDOW)
}

/// То же в окне `size`: раскрытый шаг прокси выше окна 1200×800.
fn audit_nodes_in(
    harness: &Harness<'static, BytetapApp>,
    screen: &str,
    size: (f32, f32),
) -> Vec<(Role, String, egui::Rect)> {
    use egui_kittest::kittest::NodeT as _;
    harness.check_accessibility();
    let mut found = Vec::new();
    let (w, h) = size;
    for node in harness.root().children_recursive() {
        let accesskit = node.accesskit_node();
        if accesskit.is_hidden() || !INTERACTIVE.contains(&accesskit.role()) {
            continue;
        }
        // У строк ленты текст лежит в `value` (роль Label), у кнопок — в `label`.
        let label = accesskit
            .label()
            .or_else(|| accesskit.value())
            .unwrap_or_default();
        // `Node::rect` переводит физические пиксели AccessKit в точки экрана.
        let rect = node.rect();
        assert!(
            !label.trim().is_empty(),
            "{screen}: у {:?} нет подписи ({rect:?})",
            accesskit.role()
        );
        let inside = rect.left() >= -1.0
            && rect.top() >= -1.0
            && rect.right() <= w + 1.0
            && rect.bottom() <= h + 1.0;
        assert!(
            inside,
            "{screen}: {:?} «{label}» вне окна: {rect:?}",
            accesskit.role()
        );
        found.push((accesskit.role(), label, rect));
    }
    assert!(
        !found.is_empty(),
        "{screen}: в дереве нет ни одного виджета"
    );
    found
}

/// Есть ли на экране виджет с такой ролью и подписью.
fn has(nodes: &[(Role, String, egui::Rect)], role: Role, label: &str) -> bool {
    nodes.iter().any(|(r, l, _)| *r == role && l == label)
}

/// Все экраны: у каждого своего виджета — роль и подпись, ничего не обрезано краем
/// окна и не налезает на соседа по тому же месту (U1-AC25).
#[test]
fn every_screen_names_its_widgets() {
    // Стартовый: карточки сценариев, последние записи, шапка окна.
    let harness = plain(settings(false), None, Screen::Scenarios);
    let nodes = audit_nodes(&harness, "сценарии");
    assert!(has(&nodes, Role::Button, "Журналы"));
    assert!(has(&nodes, Role::Button, "Закрыть"));
    assert!(has(&nodes, Role::Button, "Свернуть"));
    assert!(has(&nodes, Role::Button, "Развернуть"));
    assert!(
        nodes
            .iter()
            .filter(|(role, _, _)| *role == Role::Button)
            .count()
            >= 6,
        "шапка, три карточки и записи"
    );

    // Шаг TCP-сервера с «Дополнительно»: поля, списки, кнопки.
    let harness = plain(settings(false), None, Screen::Step(SessionMode::TcpServer));
    let nodes = audit_nodes(&harness, "шаг сервера");
    for (role, label) in [
        (Role::TextInput, "Порт"),
        (Role::ComboBox, "Автоответ"),
        (Role::Button, "Скопировать"),
        (Role::Button, "Старт"),
        (Role::Button, "Дополнительно"),
    ] {
        assert!(has(&nodes, role, label), "нет {role:?} «{label}»");
    }

    // COM с раскрытым «Дополнительно»: списки, флажки, профили, папка журналов.
    let harness = plain_more(Screen::Step(SessionMode::Serial));
    let nodes = audit_nodes(&harness, "шаг COM");
    for (role, label) in [
        (Role::ComboBox, "Порт"),
        (Role::ComboBox, "Скорость"),
        (Role::ComboBox, "Формат"),
        (Role::CheckBox, "DTR"),
        (Role::CheckBox, "RTS"),
        (Role::TextInput, "Анализатор"),
        (Role::TextInput, "Комментарий"),
        (Role::ComboBox, "Профиль"),
        (Role::Button, "Выбрать"),
    ] {
        assert!(has(&nodes, role, label), "нет {role:?} «{label}»");
    }

    // Шаг прокси: радио-пара топологии, порт с интерфейсом, цель, крупный адрес.
    let proxy = plain(
        proxy_settings(false, ProxyMode::AnalyzerClient),
        None,
        Screen::Step(SessionMode::Proxy),
    );
    let nodes = audit_nodes(&proxy, "шаг прокси");
    for (role, label) in [
        (Role::RadioButton, "Анализатор подключается к ЛИС"),
        (Role::RadioButton, "ЛИС подключается к анализатору"),
        (Role::TextInput, "Порт, который слушает bytetap"),
        (Role::ComboBox, "Интерфейс"),
        (Role::TextInput, "Адрес ЛИС"),
        (Role::TextInput, "Порт цели"),
        (Role::Button, "Скопировать"),
        (Role::Button, "Старт"),
    ] {
        assert!(has(&nodes, role, label), "нет {role:?} «{label}»");
    }
    // «Дополнительно» раскрыто: профиль и папка журналов на месте, как у других режимов.
    // Окно выше 800 точек: раскрытый шаг прокси в него не влезает и прокручивается.
    let harness = proxy_more((1200.0, 1000.0));
    let nodes = audit_nodes_in(&harness, "шаг прокси с «Дополнительно»", (1200.0, 1000.0));
    for (role, label) in [
        (Role::TextInput, "Анализатор"),
        (Role::TextInput, "Комментарий"),
        (Role::ComboBox, "Профиль"),
        (Role::Button, "Выбрать"),
    ] {
        assert!(has(&nodes, role, label), "нет {role:?} «{label}»");
    }

    // Запись: «Стоп», меню «…», счётчики, вид и кодировка ленты, фильтр соединений.
    let harness = plain_recording(FeedView::Text, 0);
    let nodes = audit_nodes(&harness, "запись");
    for (role, label) in [
        (Role::Button, "Стоп 00:05"),
        (Role::Button, "Меню записи"),
        (Role::ComboBox, "Вид ленты"),
        (Role::ComboBox, "Кодировка"),
        (Role::Button, "все"),
        (Role::Button, "#2"),
    ] {
        assert!(has(&nodes, role, label), "нет {role:?} «{label}»");
    }
}

/// Раскрытое «Дополнительно» шага прокси в окно 800 точек не помещается и
/// прокручивается: Tab до «Старт» подтягивает кнопку в видимую область (M4-AC13,
/// отступление от макета M4-3).
#[test]
fn tab_brings_the_focused_field_into_view() {
    use egui_kittest::kittest::NodeT as _;
    let mut harness = proxy_more((1200.0, 800.0));
    let mut focused = None;
    for _ in 0..40 {
        harness.key_press(Key::Tab);
        harness.run_steps(2);
        let Some(node) = harness
            .root()
            .children_recursive()
            .find(egui_kittest::Node::is_focused)
        else {
            continue;
        };
        let label = node.accesskit_node().label().unwrap_or_default();
        if label == "Старт" {
            focused = Some(node.rect());
            break;
        }
    }
    let rect = focused.expect("Tab не дошёл до «Старт»");
    assert!(
        rect.bottom() <= 800.0 + 1.0 && rect.top() >= -1.0,
        "«Старт» не в видимой области: {rect:?}"
    );
}

/// Tab обходит экран в порядке чтения: сверху вниз, внутри строки — слева направо
/// (U1-AC25). Обход идёт до возврата к уже пройденному виджету.
fn tab_walk(harness: &mut Harness<'static, BytetapApp>, steps: usize) -> Vec<(String, egui::Rect)> {
    use egui_kittest::kittest::NodeT as _;
    let mut order: Vec<(String, egui::Rect)> = Vec::new();
    for _ in 0..steps {
        harness.key_press(Key::Tab);
        harness.run_steps(2);
        let Some(node) = harness
            .root()
            .children_recursive()
            .find(egui_kittest::Node::is_focused)
        else {
            continue;
        };
        let accesskit = node.accesskit_node();
        let label = accesskit
            .label()
            .or_else(|| accesskit.value())
            .unwrap_or_default();
        assert!(
            !label.trim().is_empty(),
            "Tab попал на {:?} без подписи: {:?}",
            accesskit.role(),
            node.rect()
        );
        let rect = node.rect();
        // Круг замкнулся: egui отдаёт фокус первому виджету.
        if order
            .iter()
            .any(|(seen, seen_rect)| *seen == label && *seen_rect == rect)
        {
            break;
        }
        order.push((label, rect));
    }
    order
}

/// Допуск строки: кнопки окна начинаются от самого верха окна, иконка — на 8 px ниже,
/// а поле с подписью — на 20 px ниже флажка рядом. Всё это одна строка экрана.
const ROW_TOLERANCE: f32 = 22.0;

/// Порядок Tab совпадает с тем, что видит глаз: сверху вниз и слева направо.
fn assert_reading_order(screen: &str, order: &[(String, egui::Rect)]) {
    for pair in order.windows(2) {
        let (above, a) = &pair[0];
        let (below, b) = &pair[1];
        let dy = b.top() - a.top();
        assert!(
            dy >= -ROW_TOLERANCE,
            "{screen}: Tab ушёл вверх: «{above}» {a:?} → «{below}» {b:?}\n{order:#?}"
        );
        if dy.abs() <= ROW_TOLERANCE {
            assert!(
                b.left() > a.left() - 0.5,
                "{screen}: Tab ушёл влево в той же строке: «{above}» {a:?} → «{below}» {b:?}\n{order:#?}"
            );
        }
    }
}

/// Tab идёт по экрану в порядке чтения на каждом экране (U1-AC25).
#[test]
fn tab_visits_every_screen_in_reading_order() {
    let order = tab_walk(&mut plain(settings(false), None, Screen::Scenarios), 24);
    assert_reading_order("сценарии", &order);
    // Шапка, три карточки, записи — и ничего лишнего вроде полосы перетаскивания.
    assert_eq!(
        order.first().map(|(label, _)| label.as_str()),
        Some("Журналы")
    );
    assert!(
        order
            .iter()
            .any(|(label, _)| label.starts_with("Анализатор подключается ко мне")),
        "карточки в обходе: {order:#?}"
    );

    let order = tab_walk(&mut plain_more(Screen::Step(SessionMode::Serial)), 24);
    assert_reading_order("шаг COM", &order);
    let labels: Vec<&str> = order.iter().map(|(label, _)| label.as_str()).collect();
    // Поля формы идут сверху вниз, профили — в конце «Дополнительно».
    let position = |want: &str| labels.iter().position(|label| *label == want);
    let port = position("Порт").expect("порт в обходе");
    let format = position("Формат").expect("формат в обходе");
    let profile = position("Профиль").expect("профиль в обходе");
    assert!(port < format && format < profile, "{labels:#?}");

    // Шаг прокси: радио-пара, порт с интерфейсом, цель, крупный адрес — сверху вниз.
    let order = tab_walk(
        &mut plain(
            proxy_settings(false, ProxyMode::AnalyzerClient),
            None,
            Screen::Step(SessionMode::Proxy),
        ),
        24,
    );
    assert_reading_order("шаг прокси", &order);
    let labels: Vec<&str> = order.iter().map(|(label, _)| label.as_str()).collect();
    let client = position_of(&labels, "Анализатор подключается к ЛИС").expect("радио 1");
    let server = position_of(&labels, "ЛИС подключается к анализатору").expect("радио 2");
    let port = position_of(&labels, "Порт, который слушает bytetap").expect("порт");
    let target = position_of(&labels, "Адрес ЛИС").expect("адрес цели");
    let start = position_of(&labels, "Старт").expect("«Старт»");
    assert!(
        client < server && server < port && port < target && target < start,
        "{labels:#?}"
    );

    let order = tab_walk(&mut plain_recording(FeedView::Text, 0), 24);
    assert_reading_order("запись", &order);
    let labels: Vec<&str> = order.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(labels.first(), Some(&"Стоп 00:05"), "{labels:#?}");
    // Кнопки окна — после своих виджетов шапки и до панели ленты.
    let close = position_of(&labels, "Закрыть").expect("«Закрыть» в обходе");
    let view = position_of(&labels, "Вид ленты").expect("вид ленты в обходе");
    assert!(close < view, "{labels:#?}");
}

fn position_of(labels: &[&str], want: &str) -> Option<usize> {
    labels.iter().position(|label| *label == want)
}

/// Escape закрывает меню шапки и диалог комментария (U1-AC25).
#[test]
fn escape_closes_menus_and_dialogs() {
    let mut harness = plain(settings(false), None, Screen::Scenarios);
    harness.get_by_label("Журналы").click();
    harness.run_steps(2);
    harness.get_by_label("Открыть папку журналов");
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(harness.query_by_label("Открыть папку журналов").is_none());

    // Меню записи «…»: Escape закрывает его, не выбирая пункт.
    let mut harness = plain_recording(FeedView::Text, 0);
    harness.get_by_label("Меню записи").click();
    harness.run_steps(2);
    harness.get_by_label("Скопировать путь к журналу");
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness
            .query_by_label("Скопировать путь к журналу")
            .is_none()
    );

    // Диалог имени профиля: Escape — отмена, профиля нет.
    let mut harness = plain_more(Screen::Step(SessionMode::Serial));
    harness
        .get_all_by_label("Сохранить")
        .next()
        .unwrap()
        .click();
    harness.run_steps(3);
    harness.get_by_role_and_label(Role::TextInput, "Имя профиля");
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Имя профиля")
            .is_none(),
        "Escape не закрыл диалог"
    );
    assert!(harness.state().settings().profiles.is_empty());
}

/// Рамка окна на экране записи (U1-AC3, жалоба владельца: на записи рамка пропадала).
///
/// Экран записи рисует шапку, трассу, панель и ленту в том же слое, что и рамку, поэтому
/// рамка идёт последней и панели её не перекрывают: по середине каждой стороны ровно
/// один физический пиксель цвета `--bt-border`, а следующий за ним — фон `--bt-surface-0`.
/// Обе темы: цвет рамки у них разный.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "рендер проверяется на Linux (lavapipe)"
)]
fn the_recording_screen_keeps_the_window_frame() {
    let _gpu = gpu();
    for (dark, border, surface) in [
        (false, [0xe2, 0xe5, 0xe9], [0xff, 0xff, 0xff]),
        (true, [0x25, 0x2d, 0x36], [0x0d, 0x10, 0x13]),
    ] {
        let mut harness = recording(dark, FeedView::Text, 0);
        harness.run_steps(2);
        let image = harness.render().expect("рендер экрана записи");
        let (w, h) = (image.width() as i32, image.height() as i32);
        let at = |x: i32, y: i32| {
            let pixel = image.get_pixel(x as u32, y as u32);
            [pixel[0], pixel[1], pixel[2]]
        };
        // По середине стороны и в 3 px от неё внутрь: рамка должна быть ровно 1 px.
        let mut edges = Vec::new();
        for (x, y, dx, dy) in [
            (w / 2, 0, 0, 3),
            (w / 2, h - 1, 0, -3),
            (0, h / 2, 3, 0),
            (w - 1, h / 2, -3, 0),
        ] {
            edges.push((at(x, y), at(x + dx, y + dy)));
        }
        for (edge, inner) in edges {
            assert!(
                near(edge, border),
                "тема {dark}: периметр {edge:?} — не цвет --bt-border {border:?}"
            );
            assert!(
                near(inner, surface),
                "тема {dark}: в 3 px внутрь {inner:?} — не фон --bt-surface-0 {surface:?}, \
                 рамка толще одного пикселя"
            );
        }
    }
}

/// Пиксель совпадает с ожидаемым цветом с допуском 8 на канал: снимок идёт через растр
/// lavapipe, и последний разряд канала там гуляет.
fn near(pixel: [u8; 3], want: [u8; 3]) -> bool {
    pixel
        .iter()
        .zip(want)
        .all(|(got, want)| got.abs_diff(want) <= 8)
}
