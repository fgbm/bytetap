//! Команды моста для интерфейса: имена и типы — по контракту `app/src/api/types.ts`.
//!
//! Здесь только граница Tauri: обработчики проверяют аргументы, вызывают логику из
//! [`crate::bridge`], [`crate::journal`] и [`crate::net`], которую можно проверить без окна
//! (M2-AC17), и превращают ошибки в [`CommandError`] с кодом из контракта.

use std::path::Path;
use std::sync::Arc;

use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt as _;
use tauri_plugin_opener::OpenerExt as _;

use crate::AppState;
use crate::api::{Interface, JournalSummary, SessionConfig, SessionSummary, StartInfo};
use crate::error::{CommandError, ErrorCode};
use crate::feed::{FeedBatch, FeedEvent};
use crate::{journal, net};

/// IPv4-адреса ноутбука для подсказки анализатору (M2-AC2).
#[tauri::command]
pub fn list_interfaces() -> Vec<Interface> {
    net::list_interfaces()
}

/// Папка журналов по умолчанию (FR-19).
///
/// # Errors
///
/// Домашняя папка не определилась: указать каталог придётся вручную.
#[tauri::command]
pub fn default_log_dir() -> Result<String, CommandError> {
    bytetap_core::journal::default_log_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .ok_or_else(|| {
            CommandError::new(
                ErrorCode::LogDir,
                "не удалось определить папку журналов: укажите каталог явно",
            )
        })
}

/// Предупреждение о папке журналов: OneDrive или сетевой путь (FR-19).
#[tauri::command]
pub fn log_dir_warning(dir: String) -> Option<String> {
    bytetap_core::journal::log_dir_warning(dir)
}

/// Начинает запись и отдаёт события ленты в канал пачками (FR-25, M2-AC16).
///
/// # Errors
///
/// `already_running`, `port_in_use`, `address_unavailable`, `permission_denied`,
/// `log_dir`, `journal`.
#[tauri::command]
pub async fn start_session(
    cfg: SessionConfig,
    channel: Channel<FeedBatch>,
    state: State<'_, AppState>,
) -> Result<StartInfo, CommandError> {
    state.sessions.start(cfg, Arc::new(channel)).await
}

/// Останавливает запись и возвращает итоги (FR-25, NFR-5).
///
/// # Errors
///
/// `journal`, если журнал отказал: записанное до сбоя сохранено; `io` — если записи не было.
#[tauri::command]
pub async fn stop_session(state: State<'_, AppState>) -> Result<SessionSummary, CommandError> {
    state.sessions.stop().await
}

/// Комментарий в идущую сессию (FR-23).
///
/// # Errors
///
/// `journal`, если сессия остановлена или журнал отказал; `io` — если записи нет.
#[tauri::command]
pub fn add_session_note(text: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.sessions.note(text)
}

/// Сводка открытого журнала: заголовок, итоги и плотность всей сессии (FR-29).
///
/// # Errors
///
/// `journal`: файла нет, он не читается или это не журнал bytetap.
#[tauri::command]
pub async fn open_journal(
    path: String,
    state: State<'_, AppState>,
) -> Result<JournalSummary, CommandError> {
    journal::open(Path::new(&path), &state.journals).await
}

/// Страница событий журнала по индексу строк (FR-29).
///
/// # Errors
///
/// `journal`: файла нет или он не читается.
#[tauri::command]
pub async fn read_journal(
    path: String,
    from: usize,
    count: usize,
    state: State<'_, AppState>,
) -> Result<Vec<FeedEvent>, CommandError> {
    journal::read_page(Path::new(&path), from, count, &state.journals).await
}

/// Дописывает комментарий в остановленный журнал (FR-23).
///
/// # Errors
///
/// `journal`: журнал ещё пишется, занят другой программой или оборван.
#[tauri::command]
pub fn append_note(path: String, text: String) -> Result<(), CommandError> {
    journal::append_note(Path::new(&path), &text)
}

/// Выбор журнала для просмотра; `null` — диалог закрыт (FR-29).
///
/// # Errors
///
/// Диалог не открылся.
#[tauri::command]
pub async fn pick_journal(app: AppHandle) -> Result<Option<String>, CommandError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Журнал bytetap", &["jsonl"])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    receive_pick(receiver).await
}

/// Выбор папки журналов; `null` — диалог закрыт (FR-19).
///
/// # Errors
///
/// Диалог не открылся.
#[tauri::command]
pub async fn pick_log_dir(app: AppHandle) -> Result<Option<String>, CommandError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = sender.send(path);
    });
    receive_pick(receiver).await
}

/// Открывает папку журналов в проводнике (FR-29).
///
/// Открывается только путь: URL и файлы не принимаются, чтобы команда не стала способом
/// запустить что угодно (FR-33).
///
/// # Errors
///
/// `log_dir`: папку нельзя создать или открыть.
#[tauri::command]
pub fn open_log_dir(app: AppHandle, dir: String) -> Result<(), CommandError> {
    let path = Path::new(dir.trim());
    if path.as_os_str().is_empty() || dir.contains("://") {
        return Err(CommandError::new(
            ErrorCode::LogDir,
            format!("`{dir}` — не путь к папке журналов"),
        ));
    }
    std::fs::create_dir_all(path).map_err(|err| {
        CommandError::from_io_with(ErrorCode::LogDir, &err)
            .with_param("dir", path.to_string_lossy())
    })?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|err| {
            CommandError::new(ErrorCode::LogDir, err.to_string())
                .with_param("dir", path.to_string_lossy())
        })
}

/// Путь из диалога, дождавшись его закрытия.
async fn receive_pick(
    receiver: tokio::sync::oneshot::Receiver<Option<tauri_plugin_dialog::FilePath>>,
) -> Result<Option<String>, CommandError> {
    let picked = receiver
        .await
        .map_err(|_| CommandError::new(ErrorCode::Io, "диалог закрылся, не вернув выбор"))?;
    Ok(picked
        .and_then(|file| file.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned()))
}
