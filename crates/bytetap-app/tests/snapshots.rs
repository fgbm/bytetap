//! Снимки шапки и пустого экрана в обеих темах (U1-AC28 для шага 1) через `egui_kittest`.
//!
//! Рендер — wgpu, на Linux программный Vulkan (lavapipe, `mesa-vulkan-drivers`):
//! снимки сравниваются только на Linux, где эталоны и сняты. Обновить эталоны:
//! `UPDATE_SNAPSHOTS=1 cargo test -p bytetap-app --test snapshots`. Глифы проверяются
//! на всех ОС без рендера: отсутствующий глиф роняет харнесс.

use bytetap_app::settings::{LoadWarning, SessionMode, Settings, ThemeMode};
use bytetap_app::{BytetapApp, Screen, theme};
use eframe::egui::{self, Theme};
use egui_kittest::{Harness, SnapshotOptions, kittest::Queryable as _};

/// Допуск сравнения: версии lavapipe на машине разработчика и в CI сглаживают немного
/// по-разному.
fn options() -> SnapshotOptions {
    SnapshotOptions::new().threshold(1.0).max_failed_pixels(200)
}

fn harness(screen: Screen, dark: bool, size: (f32, f32)) -> Harness<'static, BytetapApp> {
    let settings = Settings {
        theme: if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        ..Settings::default()
    };
    let mut harness = Harness::builder()
        .with_size(size)
        .with_theme(if dark { Theme::Dark } else { Theme::Light })
        .wgpu()
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None).with_screen(screen)
        });
    // Иконки-SVG растеризуются за несколько кадров.
    harness.run_steps(8);
    harness
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
fn empty_screen_light() {
    snapshot("scenarios-light", Screen::Scenarios, false, WINDOW);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn empty_screen_dark() {
    snapshot("scenarios-dark", Screen::Scenarios, true, WINDOW);
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "эталоны сняты на Linux (lavapipe)")]
fn empty_step_screen() {
    snapshot(
        "step-serial-light",
        Screen::Step(SessionMode::Serial),
        false,
        WINDOW,
    );
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
        for button in ["Свернуть", "Развернуть", "Закрыть"] {
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
