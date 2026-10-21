//! Снимки экранов в обеих темах и поведение стартового экрана и шагов сценария
//! (U1-AC9…AC12, U1-AC28) через `egui_kittest`.
//!
//! Рендер — wgpu, на Linux программный Vulkan (lavapipe, `mesa-vulkan-drivers`):
//! снимки сравниваются только на Linux, где эталоны и сняты. Обновить эталоны:
//! `UPDATE_SNAPSHOTS=1 cargo test -p bytetap-app --test snapshots`. Глифы проверяются
//! на всех ОС без рендера: отсутствующий глиф роняет харнесс. Окружение (сеть, порты,
//! последние записи, «сейчас») — синтетическое, через `Fixtures`.

use bytetap_app::background::Interface;
use bytetap_app::live::{JournalFailure, JournalFailureCode};
use bytetap_app::recording::RecordingFixture;
use bytetap_app::settings::{
    FeedView, LastRun, LoadWarning, SessionMode, Settings, Store, ThemeMode,
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
                addr: "10.0.0.5".into(),
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
        2,
        "вверх с первой — на последнюю"
    );
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Step(SessionMode::Serial));
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
    let viewing = harness.state().viewing().unwrap().to_string_lossy();
    assert!(viewing.ends_with("bytetap-20261018180200-tcp_client.jsonl"));
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
    harness.get_by_label("другая сеть: 10.0.0.5 (Wi-Fi)");
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
        (Screen::Recording, "Стоп 00:00"),
        (Screen::Viewing, "Просмотр журнала"),
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

/// Кириллица, стрелки направлений и управляющие символы — без «тофу» (U1-AC5).
#[test]
fn the_fonts_have_every_glyph_the_interface_uses() {
    let mut harness = Harness::builder()
        .with_size((800.0, 200.0))
        .with_context_setup(|ctx| {
            theme::install_fonts(ctx);
            theme::apply(ctx, false);
        })
        .build_ui(|ui| {
            let sample = "Ёжик съел щи · «Журнал» — №5 … ‹CR›‹LF›‹STX›‹ETX› → ← ↓ ↑ ✓ ×";
            ui.label(sample);
            ui.label(egui::RichText::new(sample).font(theme::mono(13.0)));
            ui.label(egui::RichText::new(sample).font(theme::mono_medium(13.0)));
            ui.label(egui::RichText::new(sample).font(theme::sans_semibold(20.0)));
            ui.label(egui::RichText::new(sample).font(theme::sans_medium(13.0)));
        });
    harness.run();
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
        }),
        Event::Open(Open {
            t: at(0),
            mono_us: 0,
            conn: 2,
            peer: "192.168.1.52:49152".into(),
            local: "0.0.0.0:5000".into(),
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
fn journal_failure_light() {
    let _gpu = gpu();
    let failure = JournalFailure {
        code: JournalFailureCode::DiskFull,
        message: "No space left on device (os error 28)".into(),
    };
    let mut harness = harness_with(
        settings(false),
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
    harness.snapshot_options("journal-failure-light", &options());
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
