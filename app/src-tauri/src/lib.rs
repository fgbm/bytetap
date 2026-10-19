//! Оболочка bytetap на Tauri v2.
//!
//! Логика захвата живёт в `bytetap-core`; здесь — мост к интерфейсу: команды, поток событий
//! пачками через Channel и проверка WebView2 (FR-25…FR-30, NFR-3). Всё, что можно проверить
//! без окна, вынесено в модули [`bridge`], [`batch`], [`journal`], [`feed`] и [`error`].

mod api;
mod batch;
mod bridge;
mod commands;
mod error;
mod feed;
mod journal;
mod net;
mod webview2;

use tauri::{Manager as _, WindowEvent};

/// Состояние приложения, общее для команд и обработчика закрытия окна.
#[derive(Default)]
pub struct AppState {
    /// Одна активная сессия записи (FR-25).
    pub sessions: bridge::Sessions,
    /// Кеш индексов открытых журналов (FR-29).
    pub journals: journal::JournalCache,
}

/// Запускает приложение.
///
/// # Panics
///
/// Паникует, если Tauri не смог запустить цикл событий: без окна приложение бесполезно.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // NFR-3: без WebView2 окно не создаётся, поэтому проверка идёт до `tauri::Builder`.
    webview2::ensure_or_exit();

    tauri::Builder::default()
        // Только локальные плагины: диалоги, проводник и настройки. Сети и shell нет (FR-33).
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::list_interfaces,
            commands::default_log_dir,
            commands::log_dir_warning,
            commands::start_session,
            commands::stop_session,
            commands::add_session_note,
            commands::open_journal,
            commands::read_journal,
            commands::append_note,
            commands::pick_journal,
            commands::pick_log_dir,
            commands::open_log_dir,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                close_after_stopping(window, api);
            }
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить bytetap");
}

/// Закрытие окна во время записи: сначала штатная остановка (NFR-5).
///
/// Закрытие откладывается, пока сессия пишет `session_end` и фиксирует журнал; затем окно
/// закрывается повторно, и вторая попытка проходит уже без активной сессии.
fn close_after_stopping(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    if !window.state::<AppState>().sessions.is_running() {
        return;
    }
    api.prevent_close();
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        window.state::<AppState>().sessions.stop_if_running().await;
        let _ = window.close();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_and_core_versions_match() {
        assert_eq!(bytetap_core::version(), env!("CARGO_PKG_VERSION"));
    }
}
