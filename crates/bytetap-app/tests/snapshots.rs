//! Снимки экранов в обеих темах и поведение стартового экрана и шагов сценария
//! (U1-AC9…AC12, U1-AC28) через `egui_kittest`.
//!
//! Рендер — wgpu, на Linux программный Vulkan (lavapipe, `mesa-vulkan-drivers`):
//! снимки сравниваются только на Linux, где эталоны и сняты. Обновить эталоны:
//! `UPDATE_SNAPSHOTS=1 cargo test -p bytetap-app --test snapshots`. Глифы проверяются
//! на всех ОС без рендера: отсутствующий глиф роняет харнесс. Окружение (сеть, порты,
//! последние записи, «сейчас») — синтетическое, через `Fixtures`.

use bytetap_app::background::Interface;
use bytetap_app::settings::{LastRun, LoadWarning, SessionMode, Settings, Store, ThemeMode};
use bytetap_app::{BytetapApp, Fixtures, Screen, theme};
use bytetap_core::journal::RecentJournal;
use bytetap_core::transport::Source;
use bytetap_core::transport::serial::PortInfo;
use eframe::egui::{self, Key, Theme, accesskit::Role};
use egui_kittest::{Harness, SnapshotOptions, kittest::Queryable as _};

/// Допуск сравнения: версии lavapipe на машине разработчика и в CI сглаживают немного
/// по-разному.
fn options() -> SnapshotOptions {
    SnapshotOptions::new().threshold(1.0).max_failed_pixels(200)
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
    snapshot("titlebar-light", Screen::Scenarios, false, TITLE_BAR);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn title_bar_dark() {
    snapshot("titlebar-dark", Screen::Scenarios, true, TITLE_BAR);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn close_button_turns_red_on_hover() {
    let mut harness = harness(Screen::Scenarios, false, TITLE_BAR);
    harness.get_by_label("Закрыть").hover();
    harness.run_steps(2);
    harness.snapshot_options("titlebar-close-hover", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn scenarios_light() {
    snapshot("scenarios-light", Screen::Scenarios, false, WINDOW);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn scenarios_dark() {
    snapshot("scenarios-dark", Screen::Scenarios, true, WINDOW);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn journals_menu() {
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
    serial_more(false).snapshot_options("step-serial-more-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_serial_more_dark() {
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
    field_errors(false).snapshot_options("step-errors-light", &options());
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn step_errors_dark() {
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
        (Screen::Recording, "Запись"),
        (Screen::Viewing, "Просмотр журнала"),
    ] {
        let mut harness = Harness::builder()
            .with_size((1200.0, 800.0))
            .build_eframe(move |cc| {
                BytetapApp::new(&cc.egui_ctx, Settings::default(), None, None).with_screen(screen)
            });
        harness.run_steps(2);
        harness.get_by_label(heading);
        for button in ["Свернуть", "Развернуть", "Закрыть", "Журналы"]
        {
            harness.get_by_label(button);
        }
    }
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
