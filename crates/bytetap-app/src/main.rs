//! `bytetap.exe`: окно без системной рамки на eframe и wgpu.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bytetap_app::settings::{Settings, Store, WindowState};
use bytetap_app::{BytetapApp, WINDOW_MIN_SIZE, WINDOW_SIZE, diag, gpu, placement, t};
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
    // Папка журналов по умолчанию — `Документы\bytetap`, пока пользователь не выбрал свою.
    let log_dir = bytetap_core::journal::default_log_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mut settings, warning) = match &store {
        Some(store) => {
            diag::info(format!("настройки: {}", store.path().display()));
            store.load(&log_dir)
        }
        None => {
            diag::error("каталог конфигурации пользователя не определён: настройки не сохраняются");
            (Settings::with_log_dir(&log_dir), None)
        }
    };
    if settings.config.log_dir.trim().is_empty() {
        settings.config.log_dir = log_dir;
    }
    if let Some(warning) = &warning {
        diag::error(format!("настройки сброшены: {warning:?}"));
    }

    // Положение — только если шапка окна попадает на монитор, иначе окно по центру.
    let position = placement::visible_position(
        settings.window.and_then(|w| w.position),
        placement::on_monitor,
    );
    if let Some(saved) = settings.window.and_then(|w| w.position)
        && position.is_none()
    {
        diag::info(format!(
            "запомненное положение окна {saved:?} вне мониторов: окно по центру"
        ));
    }
    let maximized = settings.window.is_some_and(|w| w.maximized);
    let options = eframe::NativeOptions {
        viewport: viewport(settings.window.as_ref()),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: gpu::configuration(),
        centered: position.is_none(),
        // Размер и положение окна хранятся в settings.json, а не в хранилище eframe.
        persist_window: false,
        ..Default::default()
    };
    diag::info("создание окна");
    let result = eframe::run_native(
        "bytetap",
        options,
        Box::new(move |cc| {
            Ok(Box::new(
                BytetapApp::new(&cc.egui_ctx, settings, warning, store)
                    .with_window_restore(position, maximized),
            ))
        }),
    );
    if let Err(err) = result {
        let wgpu_failed = matches!(err, eframe::Error::Wgpu(_));
        if gpu::retry_with_warp(
            cfg!(windows) && wgpu_failed,
            std::env::var_os(gpu::ADAPTER_ENV).is_some(),
            std::env::var_os(gpu::RETRY_ENV).is_some(),
        ) {
            diag::error(format!(
                "графика не запустилась: {err}; перезапуск с {}={}",
                gpu::ADAPTER_ENV,
                gpu::WARP_ADAPTER
            ));
            relaunch_with_warp();
        }
        diag::fatal(&t!("diag.startFailed", reason = err));
    }
}

/// Перезапуск себя с WARP: тот же exe и аргументы, `WGPU_ADAPTER_NAME=basic render` и
/// метка повтора против цикла. Возвращается, только если запустить не удалось.
fn relaunch_with_warp() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            diag::error(format!("перезапуск с WARP: путь exe не определён: {err}"));
            return;
        }
    };
    let status = std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .env(gpu::ADAPTER_ENV, gpu::WARP_ADAPTER)
        .env(gpu::RETRY_ENV, "1")
        .status();
    match status {
        // Процесс ждёт перезапущенный: код выхода и PID для смоука — те же.
        Ok(status) => {
            diag::info(format!("перезапуск с WARP завершился: {status}"));
            std::process::exit(status.code().unwrap_or(1));
        }
        Err(err) => diag::error(format!(
            "перезапуск с WARP не удался ({}): {err}",
            exe.display()
        )),
    }
}

/// Окно без рамки: запомненный размер или 1200×800. Положение и разворот ставятся в
/// первом кадре (`BytetapApp::with_window_restore`), когда известен масштаб монитора.
fn viewport(saved: Option<&WindowState>) -> egui::ViewportBuilder {
    let size = saved.map_or(WINDOW_SIZE, |w| {
        [
            w.size[0].max(WINDOW_MIN_SIZE[0]),
            w.size[1].max(WINDOW_MIN_SIZE[1]),
        ]
    });
    egui::ViewportBuilder::default()
        .with_title("bytetap")
        .with_app_id("bytetap")
        .with_inner_size(size)
        .with_min_inner_size(WINDOW_MIN_SIZE)
        .with_decorations(false)
        .with_resizable(true)
}
