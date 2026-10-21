//! Ввод кириллицы, IME и буфер обмена в полях приложения (FR-31, U1-AC27).
//!
//! Русская раскладка приходит обычными `Event::Text`, композиция Windows — событиями
//! `Event::Ime`. egui-winit сам включает IME у поля в фокусе и ставит область
//! кандидатов по `PlatformOutput::ime`; тест проверяет, что приложение отдаёт эту
//! область у поля, и что текст композиции доходит до поля.

use bytetap_app::background::Interface;
use bytetap_app::settings::{SessionMode, Settings};
use bytetap_app::{BytetapApp, Fixtures, Screen};
use eframe::egui::{self, Key, Role};
use egui_kittest::{Harness, kittest::Queryable as _};

const LOG_DIR: &str = r"C:\Users\operator\Documents\bytetap";
const WINDOW: (f32, f32) = (1200.0, 800.0);

fn settings() -> Settings {
    let mut settings = Settings::with_log_dir(LOG_DIR);
    settings.config.mode = SessionMode::Serial;
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

/// COM-шаг с раскрытым «Дополнительно»: поля «Анализатор» и «Комментарий» на экране.
fn harness() -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder().with_size(WINDOW).build_eframe(|cc| {
        BytetapApp::new(&cc.egui_ctx, settings(), None, None)
            .with_fixtures(fixtures())
            .with_screen(Screen::Step(SessionMode::Serial))
            .with_more()
    });
    harness.run_steps(3);
    harness
}

/// Кириллица русской раскладки приходит в поле событиями текста.
#[test]
fn cyrillic_text_reaches_the_analyzer_field() {
    let mut harness = harness();
    harness
        .get_by_role_and_label(Role::TextInput, "Анализатор")
        .focus();
    harness.run_steps(1);
    harness
        .get_by_role_and_label(Role::TextInput, "Анализатор")
        .type_text("Анализатор «Ёж» №5");
    harness.run_steps(2);
    assert_eq!(
        harness.state().settings().config.analyzer,
        "Анализатор «Ёж» №5"
    );
}

/// Композиция Windows: предварительный текст виден, `Commit` дописывает слово в поле,
/// а приложение отдаёт системе область кандидатов у курсора поля (U1-AC27).
#[test]
fn an_ime_composition_fills_the_field_and_publishes_its_candidate_area() {
    let mut harness = harness();
    harness
        .get_by_role_and_label(Role::TextInput, "Анализатор")
        .focus();
    harness.run_steps(2);
    let field = harness
        .get_by_role_and_label(Role::TextInput, "Анализатор")
        .rect();

    // Композиция: «Прибор» набрано, но ещё не подтверждено.
    harness.event(egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "Прибор".into(),
        active_range_chars: Some(0..6),
    }));
    harness.run_steps(2);
    let ime = harness
        .output()
        .platform_output
        .ime
        .expect("поле в фокусе отдаёт область кандидатов IME");
    assert!(
        field.contains_rect(ime.cursor_rect),
        "курсор композиции {ime:?} вне поля {field:?}"
    );
    assert_eq!(
        ime.purpose,
        egui::IMEPurpose::Normal,
        "обычный ввод, не пароль"
    );

    // Подтверждение: текст ложится в поле и сохраняется в настройках.
    harness.event(egui::Event::Ime(egui::ImeEvent::Commit("Прибор".into())));
    harness.run_steps(3);
    assert_eq!(harness.state().settings().config.analyzer, "Прибор");
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "Анализатор")
            .value()
            .as_deref(),
        Some("Прибор")
    );
}

/// Вставка из буфера обмена (`Ctrl+V` приходит событием `Paste`) и «Комментарий» с
/// кириллицей (U1-AC27).
#[test]
fn pasted_text_and_a_cyrillic_comment_reach_their_fields() {
    let mut harness = harness();
    harness
        .get_by_role_and_label(Role::TextInput, "Анализатор")
        .focus();
    harness.run_steps(1);
    harness.event(egui::Event::Paste("Анализатор X".into()));
    harness.run_steps(2);
    assert_eq!(harness.state().settings().config.analyzer, "Анализатор X");
    // Ctrl+A и Del: очистить поле перед новым текстом.
    harness.event(egui::Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    harness.run_steps(1);
    harness
        .get_by_role_and_label(Role::TextInput, "Комментарий")
        .focus();
    harness.run_steps(1);
    harness
        .get_by_role_and_label(Role::TextInput, "Комментарий")
        .type_text("прогон с утра");
    harness.run_steps(2);
    assert_eq!(harness.state().settings().config.note, "прогон с утра");
}
