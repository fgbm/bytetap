//! Оболочка bytetap на Tauri v2.
//!
//! Логика захвата живёт в `bytetap-core`; здесь — мост к интерфейсу: команды, поток событий
//! пачками через Channel и проверка WebView2 (FR-25…FR-30, NFR-3). Всё, что можно проверить
//! без окна, вынесено в модули [`bridge`], [`batch`], [`journal`], [`feed`] и [`error`].

mod api;
mod batch;
mod bridge;
mod commands;
mod diag;
mod error;
mod feed;
mod journal;
mod net;
mod webview2;

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager as _, RunEvent, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt as _, MessageDialogKind};

/// Имя файла настроек в папке данных приложения; совпадает с `SETTINGS_FILE` фронтенда.
pub const SETTINGS_FILE: &str = "settings.json";

/// Состояние приложения, общее для команд и обработчика закрытия окна.
#[derive(Default)]
pub struct AppState {
    /// Одна активная сессия записи (FR-25).
    pub sessions: bridge::Sessions,
    /// Кеш индексов открытых журналов (FR-29).
    pub journals: journal::JournalCache,
}

/// Метка главного окна в `tauri.conf.json` и в `capabilities/default.json`.
const MAIN_WINDOW: &str = "main";

/// Запускает приложение.
///
/// Ошибки старта не роняют процесс молча: они пишутся в журнал старта [`diag`] и
/// показываются системным окном (NFR-3).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Обработчик паник и журнал — раньше всего остального: GUI-сборка без консоли.
    diag::start();
    diag::info(format!(
        "старт bytetap {}, ОС {}",
        env!("CARGO_PKG_VERSION"),
        os_info::get()
    ));
    // NFR-3: без WebView2 окно не создаётся, поэтому проверка идёт до `tauri::Builder`.
    webview2::ensure_or_exit();

    diag::info("сборка приложения Tauri");
    let built = tauri::Builder::default()
        // Только локальные плагины: диалоги, проводник и настройки. Сети и shell нет (FR-33).
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::list_interfaces,
            commands::list_serial_ports,
            commands::list_recent_journals,
            commands::default_log_dir,
            commands::log_dir_warning,
            commands::session_status,
            commands::start_session,
            commands::stop_session,
            commands::add_session_note,
            commands::open_journal,
            commands::read_journal,
            commands::journal_lengths,
            commands::append_note,
            commands::pick_journal,
            commands::pick_log_dir,
            commands::open_log_dir,
            commands::preserve_corrupt_settings,
            commands::frontend_ready,
        ])
        .setup(|app| {
            create_main_window(app);
            Ok(())
        })
        .on_page_load(|webview, payload| {
            let stage = match payload.event() {
                PageLoadEvent::Started => "началась",
                PageLoadEvent::Finished => "закончилась",
            };
            diag::info(format!(
                "загрузка страницы {stage} в окне {}: {}",
                webview.label(),
                payload.url()
            ));
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                diag::info(format!("закрытие окна {}", window.label()));
                close_after_stopping(window, api);
            }
            WindowEvent::Destroyed => diag::info(format!("окно {} закрыто", window.label())),
            _ => {}
        })
        .build(tauri::generate_context!());
    let app = match built {
        Ok(app) => app,
        Err(err) => diag::fatal(&format!("Не удалось запустить bytetap: {err}")),
    };

    app.run(|app, event| match &event {
        RunEvent::Ready => diag::info("цикл событий запущен"),
        RunEvent::ExitRequested { code, api, .. } => {
            diag::info(format!("запрошен выход, код {code:?}"));
            // Выход, запрошенный самим приложением (`exit`), уже шёл через остановку
            // сессии; перехватывать его второй раз нельзя, иначе окно не закроется.
            if code.is_some() {
                return;
            }
            // Без записи останавливать нечего: выход не задерживается.
            if !session_running(app) {
                return;
            }
            api.prevent_exit();
            stop_then_exit(app.clone());
        }
        RunEvent::Exit => diag::info("выход"),
        _ => {}
    });
}

/// Главное окно создаётся здесь, а не самим Tauri (`"create": false` в конфиге).
///
/// Окно, которое Tauri создаёт из конфига, при ошибке WebView2 роняет процесс паникой
/// «Failed to setup app» сразу после появления рамки. Здесь та же ошибка уходит в журнал
/// и системное окно с понятным текстом (NFR-3).
fn create_main_window(app: &tauri::App) {
    diag::info("setup: создание окна");
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN_WINDOW)
    else {
        diag::fatal("В tauri.conf.json нет окна main");
    };
    match WebviewWindowBuilder::from_config(app.handle(), config).and_then(|builder| builder.build())
    {
        Ok(_) => diag::info("окно main создано"),
        Err(err) => diag::fatal(&format!(
            "Не удалось создать окно bytetap: {err}.

Чаще всего причина — повреждённый или устаревший Microsoft Edge WebView2 Runtime: переустановите его автономным установщиком Evergreen Standalone (x64). Без WebView2 работает bytetap-cli."
        )),
    }
}

/// Идёт ли запись. Без состояния (не должно случаться) — не идёт: выход не задерживается.
fn session_running(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .is_some_and(|state| state.sessions.is_running())
}

/// Закрытие окна во время записи: сначала штатная остановка (NFR-5).
///
/// Закрытие откладывается, пока сессия пишет `session_end` и фиксирует журнал; затем окно
/// закрывается повторно, и вторая попытка проходит уже без активной сессии. Если остановка
/// не уложилась в [`bridge::STOP_WAIT`], приложение всё равно выходит, но пользователь видит,
/// почему журнал мог остаться неполным.
fn close_after_stopping(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    if !session_running(window.app_handle()) {
        return;
    }
    api.prevent_close();
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        let stopped = window.state::<AppState>().sessions.stop_if_running().await;
        if let Err(err) = stopped {
            warn_journal_incomplete(window.app_handle(), &err.message);
            // Остановка не уложилась в предел: `close` снова упёрся бы в этот же
            // обработчик, а `destroy` вызвал бы `ExitRequested` без кода — второе
            // ожидание остановки и второй диалог. `exit` идёт с кодом, и `ExitRequested`
            // его не перехватывает.
            window.app_handle().exit(0);
            return;
        }
        let _ = window.close();
    });
}

/// Выход приложения: остановить запись, предупредить о неполном журнале, выйти.
///
/// `RunEvent::ExitRequested` приходит и после закрытия окна, и по системному завершению;
/// перехват нужен, чтобы сессия успела записать `session_end`, а не оборвалась вместе
/// с процессом (NFR-5).
fn stop_then_exit(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let stopped = app.state::<AppState>().sessions.stop_if_running().await;
        if let Err(err) = stopped {
            warn_journal_incomplete(&app, &err.message);
        }
        app.exit(0);
    });
}

/// Системное окно «журнал мог быть дописан не полностью» перед закрытием (NFR-5).
///
/// Текст собирается из кода ошибки: интерфейс к этому моменту уже не покажет баннер.
fn warn_journal_incomplete(app: &AppHandle, reason: &str) {
    app.dialog()
        .message(format!("Журнал мог быть дописан не полностью: {reason}"))
        .title("bytetap")
        .kind(MessageDialogKind::Warning)
        .blocking_show();
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_and_core_versions_match() {
        assert_eq!(bytetap_core::version(), env!("CARGO_PKG_VERSION"));
    }
}
