//! `bytetap.exe`: окно без системной рамки на eframe и wgpu.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bytetap_app::settings::{Settings, Store, WindowState};
use bytetap_app::{
    AUTOSTART_ENV, BytetapApp, FORCED_EXIT, HOOKS_ENV, SCREEN_ENV, WINDOW_MIN_SIZE, WINDOW_SIZE,
    diag, gpu, placement, t,
};
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
    // Один runtime на приложение: сессия записи и приёмник ленты живут в нём, окно не
    // ждёт ввод-вывод.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("bytetap-session")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => diag::fatal(&t!("diag.startFailed", reason = err)),
    };
    let handle = runtime.handle().clone();
    let autostart = std::env::var(AUTOSTART_ENV).ok();
    // Снимки для согласования (CI, U1-AC21): экран по имени, только вместе с ключом
    // хуков. Переменная окружения сама по себе приложение не переключает.
    let screen = hooks_enabled()
        .then(|| std::env::var(SCREEN_ENV).ok())
        .flatten();
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
            let mut app = BytetapApp::new(&cc.egui_ctx, settings, warning, store)
                .with_window_restore(position, maximized)
                .with_runtime(handle);
            if let Some(spec) = &screen {
                app = app.with_test_screen(&cc.egui_ctx, spec);
            }
            if let Some(spec) = autostart {
                app = app.with_test_autostart(&cc.egui_ctx, &spec);
            }
            Ok(Box::new(app))
        }),
    );
    // Сессия, которую окно не дождалось, получает сигнал остановки; ждать её дольше
    // нельзя — пользователь уже закрыл окно.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    if FORCED_EXIT.load(std::sync::atomic::Ordering::Relaxed) {
        diag::show_error(t!("record.closeTimeout"));
    }
    // Журнал мог быть дописан не полностью: окно уходит молча, а система говорит об
    // этом после его закрытия (U1-AC17).
    if let Some(notice) = bytetap_app::take_close_notice() {
        diag::show_error(&notice);
    }
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

/// Тестовые хуки включены: `BYTETAP_TEST_HOOKS=1`. Без этого ключа
/// `BYTETAP_TEST_SCREEN` не читается: снимок экрана в чужой оболочке не должен
/// менять поведение приложения.
fn hooks_enabled() -> bool {
    std::env::var(HOOKS_ENV).is_ok_and(|value| value == "1")
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
