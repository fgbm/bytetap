//! `bytetap.exe`: окно без системной рамки на eframe и wgpu.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bytetap_app::settings::{Settings, Store, WindowState};
use bytetap_app::{BytetapApp, WINDOW_MIN_SIZE, WINDOW_SIZE, diag, gpu, t};
use eframe::egui;

fn main() {
    // Обработчик паник и журнал — раньше всего остального: GUI-сборка без консоли.
    diag::start();
    diag::info(format!(
        "старт bytetap {}, ОС {}",
        env!("CARGO_PKG_VERSION"),
        os_info::get()
    ));

    let store = Store::default_location();
    let (settings, warning) = match &store {
        Some(store) => {
            diag::info(format!("настройки: {}", store.path().display()));
            store.load("")
        }
        None => {
            diag::error("каталог конфигурации пользователя не определён: настройки не сохраняются");
            (Settings::default(), None)
        }
    };
    if let Some(warning) = &warning {
        diag::error(format!("настройки сброшены: {warning:?}"));
    }

    let options = eframe::NativeOptions {
        viewport: viewport(settings.window.as_ref()),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: gpu::configuration(),
        centered: settings.window.and_then(|w| w.position).is_none(),
        // Размер и положение окна хранятся в settings.json, а не в хранилище eframe.
        persist_window: false,
        ..Default::default()
    };
    diag::info("создание окна");
    let result = eframe::run_native(
        "bytetap",
        options,
        Box::new(move |cc| {
            Ok(Box::new(BytetapApp::new(
                &cc.egui_ctx,
                settings,
                warning,
                store,
            )))
        }),
    );
    if let Err(err) = result {
        diag::fatal(&t!("diag.startFailed", reason = err));
    }
}

/// Окно без рамки: запомненные размер и положение или 1200×800 по центру.
fn viewport(saved: Option<&WindowState>) -> egui::ViewportBuilder {
    let size = saved.map_or(WINDOW_SIZE, |w| {
        [
            w.size[0].max(WINDOW_MIN_SIZE[0]),
            w.size[1].max(WINDOW_MIN_SIZE[1]),
        ]
    });
    let mut builder = egui::ViewportBuilder::default()
        .with_title("bytetap")
        .with_app_id("bytetap")
        .with_inner_size(size)
        .with_min_inner_size(WINDOW_MIN_SIZE)
        .with_decorations(false)
        .with_resizable(true);
    if let Some(saved) = saved {
        if let Some(position) = saved.position {
            builder = builder.with_position(position);
        }
        builder = builder.with_maximized(saved.maximized);
    }
    builder
}
