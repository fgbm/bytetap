//! Команды моста для интерфейса: имена и типы — по контракту `app/src/api/types.ts`.
//!
//! Здесь только граница Tauri: обработчики проверяют аргументы, вызывают логику из
//! [`crate::bridge`], [`crate::journal`] и [`crate::net`], которую можно проверить без окна
//! (M2-AC17), и превращают ошибки в [`CommandError`] с кодом из контракта.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::ipc::Channel;
use tauri::{AppHandle, Manager as _, State};
use tauri_plugin_dialog::DialogExt as _;
use tauri_plugin_opener::OpenerExt as _;

use crate::AppState;
use crate::api::{
    Interface, JournalSummary, LogDirWarning, RecentJournal, SerialPort, SessionConfig,
    SessionStatus, SessionSummary, StartInfo,
};
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
///
/// Возвращается код и путь: интерфейс переводит код через `ru.json`, а текст ядра
/// (английский, для журнала) на экран не попадает.
#[tauri::command]
pub fn log_dir_warning(dir: String) -> Option<LogDirWarning> {
    let kind = bytetap_core::journal::log_dir_warning_kind(dir.trim())?;
    let params = BTreeMap::from([("dir".to_owned(), dir.trim().to_owned())]);
    Some(LogDirWarning {
        code: kind.as_str().to_owned(),
        params,
    })
}

/// Идёт ли запись и что вернул её старт (M2-AC1).
///
/// Нужно перезагруженной странице: лента после перезагрузки пуста, но сессия в ядре
/// жива, и интерфейс должен предложить «Остановить», а не оставить её сиротой.
#[tauri::command]
pub fn session_status(state: State<'_, AppState>) -> SessionStatus {
    SessionStatus {
        running: state.sessions.is_running(),
        start_info: state.sessions.status(),
    }
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
/// `generation` — поколение сессии из `StartInfo`: страница останавливает только ту сессию,
/// которую запустила сама. Без номера останавливается любая — так закрывают окно.
///
/// # Errors
///
/// `journal`, если журнал отказал: записанное до сбоя сохранено; `io` — если записи не было
/// или поколение чужое (сессию перезапустили).
#[tauri::command]
pub async fn stop_session(
    generation: Option<u64>,
    state: State<'_, AppState>,
) -> Result<SessionSummary, CommandError> {
    state.sessions.stop(generation).await
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

/// Список COM-портов для формы COM (FR-12, M3-AC4).
///
/// Перечисление портов блокирующее, поэтому идёт в [`tokio::task::spawn_blocking`]:
/// окно не замерзает на время опроса адаптеров. Пустой список — не ошибка: на машине
/// без портов форма показывает, что портов нет.
///
/// # Errors
///
/// `serial`: система не смогла перечислить порты.
#[tauri::command]
pub async fn list_serial_ports() -> Result<Vec<SerialPort>, CommandError> {
    serial_ports().await
}

/// Перечисление портов без границы Tauri: проверяется тестами без окна.
async fn serial_ports() -> Result<Vec<SerialPort>, CommandError> {
    let ports = tokio::task::spawn_blocking(bytetap_core::transport::serial::list_ports)
        .await
        .map_err(|err| {
            CommandError::new(
                ErrorCode::Serial,
                format!("перечисление портов упало: {err}"),
            )
        })?
        .map_err(|err| CommandError::from_io_with(ErrorCode::Serial, &err))?;
    Ok(ports
        .into_iter()
        .map(|port| SerialPort {
            name: port.name,
            description: port.description,
        })
        .collect())
}

/// Журналы в папке для стартового экрана: новые сверху, не больше 20 (M3).
///
/// Читаются заголовок и `session_end` каждого файла, а не журнал целиком: папка может
/// содержать сотни мегабайт (см. [`journal::recent`]).
///
/// # Errors
///
/// `log_dir`: путь относительный, папки нет или это не папка.
#[tauri::command]
pub async fn list_recent_journals(
    dir: String,
    limit: usize,
) -> Result<Vec<RecentJournal>, CommandError> {
    let path = log_dir_to_open(&dir)?;
    tokio::task::spawn_blocking(move || journal::recent(&path, limit))
        .await
        .map_err(|err| CommandError::new(ErrorCode::LogDir, format!("чтение папки упало: {err}")))?
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

/// Длины событий журнала в байтах: по ним лента считает высоты HEX-строк (M2-AC5).
///
/// Отдельная команда, а не поле сводки: массив длин у сотен тысяч событий весит больше,
/// чем сама сводка, а нужен только виду HEX.
///
/// # Errors
///
/// `journal`: файла нет или он не читается.
#[tauri::command]
pub async fn journal_lengths(
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<u64>, CommandError> {
    journal::lengths(Path::new(&path), &state.journals).await
}

/// Дописывает комментарий в остановленный журнал (FR-23).
///
/// # Errors
///
/// `journal`: журнал ещё пишется, занят другой программой, оборван, это не `.jsonl`.
#[tauri::command]
pub fn append_note(path: String, text: String) -> Result<(), CommandError> {
    let path = Path::new(&path);
    let extension = path.extension().and_then(|ext| ext.to_str());
    if !extension.is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")) {
        return Err(CommandError::new(
            ErrorCode::Journal,
            "дописать комментарий можно только в файл `.jsonl`",
        )
        .with_param("path", path.to_string_lossy()));
    }
    journal::append_note(path, &text)
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
        .add_filter("bytetap journal", &["jsonl"])
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
/// запустить что угодно (FR-33). Путь обязан быть абсолютным и уже существующим: чужие
/// пути команда не создаёт, а собственную папку по умолчанию — создаёт.
///
/// # Errors
///
/// `log_dir`: путь относительный, папки нет или это не папка.
#[tauri::command]
pub fn open_log_dir(app: AppHandle, dir: String) -> Result<(), CommandError> {
    let path = log_dir_to_open(&dir)?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|err| {
            CommandError::new(ErrorCode::LogDir, err.to_string())
                .with_param("dir", path.to_string_lossy())
        })
}

/// Проверяет путь папки журналов для [`open_log_dir`] и возвращает его в каноническом виде.
///
/// Создаётся только папка по умолчанию: её отсутствие — обычное состояние первого запуска.
/// Любой другой путь должен существовать: создавать чужие каталоги по просьбе интерфейса
/// команда не должна.
fn log_dir_to_open(dir: &str) -> Result<PathBuf, CommandError> {
    let text = dir.trim();
    if text.is_empty() || text.contains("://") {
        return Err(CommandError::new(
            ErrorCode::LogDir,
            format!("`{text}` — не путь к папке журналов"),
        ));
    }
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err(CommandError::new(
            ErrorCode::LogDir,
            format!("`{text}` — не абсолютный путь к папке журналов"),
        )
        .with_param("dir", text));
    }
    if let Some(default) = bytetap_core::journal::default_log_dir()
        && path == default
    {
        std::fs::create_dir_all(path).map_err(|err| {
            CommandError::from_io_with(ErrorCode::LogDir, &err)
                .with_param("dir", path.to_string_lossy())
        })?;
    }
    let resolved = path.canonicalize().map_err(|err| {
        CommandError::from_io_with(ErrorCode::LogDir, &err)
            .with_param("dir", path.to_string_lossy())
    })?;
    let metadata = std::fs::metadata(&resolved).map_err(|err| {
        CommandError::from_io_with(ErrorCode::LogDir, &err)
            .with_param("dir", resolved.to_string_lossy())
    })?;
    if !metadata.is_dir() {
        return Err(
            CommandError::new(ErrorCode::LogDir, format!("`{text}` — не папка"))
                .with_param("dir", text),
        );
    }
    Ok(resolved)
}

/// Копия испорченного `settings.json` рядом с оригиналом (AC11).
///
/// Копия делается до того, как настройки будут перезаписаны значениями по умолчанию:
/// владелец должен найти свой файл, даже если прочитать его не удалось. Возвращается
/// путь копии; `None` — копировать нечего или некуда.
#[tauri::command]
pub fn preserve_corrupt_settings(app: AppHandle) -> Option<String> {
    let dir = app.path().app_data_dir().ok()?;
    let source = dir.join(crate::SETTINGS_FILE);
    std::fs::metadata(&source).ok()?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let copy = dir.join(format!("settings.corrupt-{stamp}.json"));
    std::fs::copy(&source, &copy).ok()?;
    Some(copy.to_string_lossy().into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_existing_absolute_directory_is_opened() {
        let dir = tempfile::tempdir().expect("временная папка");
        let resolved = log_dir_to_open(&dir.path().to_string_lossy()).expect("существующая папка");
        assert_eq!(
            resolved,
            dir.path().canonicalize().expect("канонический путь")
        );

        assert_eq!(
            log_dir_to_open("").expect_err("пустая строка").code,
            ErrorCode::LogDir
        );
        assert_eq!(
            log_dir_to_open("https://example.com/logs")
                .expect_err("URL")
                .code,
            ErrorCode::LogDir
        );
        assert_eq!(
            log_dir_to_open("logs")
                .expect_err("относительный путь")
                .code,
            ErrorCode::LogDir,
            "относительный путь не превращается в папку рабочего каталога"
        );

        let missing = dir.path().join("no-such-dir");
        assert_eq!(
            log_dir_to_open(&missing.to_string_lossy())
                .expect_err("папки нет")
                .code,
            ErrorCode::LogDir,
            "чужие пути команда не создаёт"
        );
        assert!(!missing.exists(), "каталог не создан");

        let file = dir.path().join("journal.jsonl");
        std::fs::write(&file, "{}\n").expect("файл");
        assert_eq!(
            log_dir_to_open(&file.to_string_lossy())
                .expect_err("это файл")
                .code,
            ErrorCode::LogDir
        );
    }

    #[test]
    fn the_default_log_dir_is_created_for_the_first_start() {
        let Some(default) = bytetap_core::journal::default_log_dir() else {
            return;
        };
        if default.exists() {
            // Папка пользователя уже есть: создавать нечего, проверяем обычный путь.
            assert!(log_dir_to_open(&default.to_string_lossy()).is_ok());
            return;
        }
        let resolved = log_dir_to_open(&default.to_string_lossy()).expect("папка по умолчанию");
        assert!(resolved.is_dir());
        let _ = std::fs::remove_dir_all(&default);
    }

    #[test]
    fn a_warning_is_a_code_and_a_path() {
        let warning = log_dir_warning(r"C:\Users\user\OneDrive\Documents\bytetap".to_owned())
            .expect("OneDrive");
        assert_eq!(warning.code, "onedrive");
        assert_eq!(
            warning.params.get("dir").map(String::as_str),
            Some(r"C:\Users\user\OneDrive\Documents\bytetap")
        );
        assert_eq!(
            log_dir_warning(r"\\fileserver\share\bytetap".to_owned())
                .expect("сеть")
                .code,
            "network"
        );
        assert!(log_dir_warning("/home/user/Documents/bytetap".to_owned()).is_none());
    }

    #[test]
    fn notes_are_appended_only_to_jsonl_files() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("notes.jsonl");
        let header = bytetap_core::journal::Event::Session(bytetap_core::journal::Session::new(
            bytetap_core::journal::Mode::TcpServer,
            bytetap_core::journal::Autoresponder::None,
            serde_json::json!({}),
            bytetap_core::journal::Host {
                os: "Linux".to_owned(),
            },
        ));
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&header).unwrap()),
        )
        .expect("журнал");

        assert_eq!(
            append_note(path.to_string_lossy().into_owned(), "проба".to_owned()),
            Ok(()),
            ".jsonl принимается"
        );
        let other = dir.path().join("notes.txt");
        std::fs::write(&other, "привет\n").expect("файл");
        assert_eq!(
            append_note(other.to_string_lossy().into_owned(), "проба".to_owned())
                .expect_err("не журнал")
                .code,
            ErrorCode::Journal
        );
    }

    /// Список портов — не ошибка на машине без портов (M3-AC4).
    #[tokio::test]
    async fn the_serial_port_list_is_never_an_error_without_ports() {
        let ports = serial_ports().await.expect("перечисление портов");
        for port in &ports {
            assert!(!port.name.is_empty(), "у порта есть имя: {port:?}");
            assert!(
                !port.description.is_empty(),
                "у порта есть описание: {port:?}"
            );
        }
    }

    /// Список журналов идёт только по существующей абсолютной папке, как `open_log_dir` (M3).
    #[tokio::test]
    async fn recent_journals_need_an_existing_absolute_directory() {
        let dir = tempfile::tempdir().expect("временная папка");
        assert!(
            list_recent_journals(dir.path().to_string_lossy().into_owned(), 10)
                .await
                .expect("пустая папка")
                .is_empty(),
            "пустая папка — пустой список, а не ошибка"
        );
        assert_eq!(
            list_recent_journals("logs".to_owned(), 10)
                .await
                .expect_err("относительный путь")
                .code,
            ErrorCode::LogDir
        );
        assert_eq!(
            list_recent_journals(
                dir.path()
                    .join("no-such-dir")
                    .to_string_lossy()
                    .into_owned(),
                10
            )
            .await
            .expect_err("папки нет")
            .code,
            ErrorCode::LogDir
        );
    }
}
