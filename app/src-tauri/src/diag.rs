//! Журнал старта и аварийных завершений (NFR-3).
//!
//! Release-сборка на Windows — GUI-подсистема без консоли: паника или ошибка создания окна
//! завершают процесс молча, и пользователь видит только мелькнувшее окно. Поэтому этапы
//! старта, ошибки и паники пишутся в файл `<LOCALAPPDATA>\io.github.fgbm.bytetap\logs\bytetap.log`
//! (на других ОС — та же папка внутри `dirs::data_local_dir`), а фатальные ошибки ещё и
//! показываются системным окном со ссылкой на этот файл.
//!
//! Журнал старта — не журнал сессии: в него не попадают данные приборов, только версии,
//! этапы и тексты ошибок.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Local, TimeZone};

/// Идентификатор приложения из `tauri.conf.json`: папка данных та же, что у Tauri.
pub const IDENTIFIER: &str = "io.github.fgbm.bytetap";

/// Имя файла журнала старта.
pub const LOG_FILE: &str = "bytetap.log";

/// Размер, после которого журнал при старте переименовывается в `bytetap.log.1`.
const ROTATE_AT: u64 = 1024 * 1024;

/// Самое длинное краткое описание паники в системном окне, в символах.
const SUMMARY_LIMIT: usize = 300;

/// Открытый файл журнала; `None` — ещё не открыт или открыть не удалось.
static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Уровень записи журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

impl Level {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Error => "ERROR",
        }
    }
}

/// Путь журнала внутри папки локальных данных пользователя.
#[must_use]
pub fn log_path_in(data_local_dir: &Path) -> PathBuf {
    data_local_dir.join(IDENTIFIER).join("logs").join(LOG_FILE)
}

/// Путь журнала; `None` — папку локальных данных не определить.
#[must_use]
pub fn log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| log_path_in(&dir))
}

/// Строка журнала: время, уровень, сообщение.
///
/// Многострочное сообщение (backtrace) продолжается строками с отступом, чтобы каждая
/// запись начиналась с времени и находилась поиском по началу строки.
#[must_use]
pub fn format_line<Tz: TimeZone>(time: &DateTime<Tz>, level: Level, message: &str) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut lines = message.trim_end().lines();
    let mut out = format!(
        "{} {:<5} {}",
        time.format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
        level.as_str(),
        lines.next().unwrap_or_default()
    );
    for line in lines {
        out.push_str("\n    ");
        out.push_str(line);
    }
    out.push('\n');
    out
}

/// Текст системного окна о панике: кратко и где подробности.
#[must_use]
pub fn crash_message(summary: &str, log: Option<&Path>) -> String {
    let summary = summary.trim();
    let mut short: String = summary.chars().take(SUMMARY_LIMIT).collect();
    if short.len() < summary.len() {
        short.push('…');
    }
    with_details(&format!("bytetap аварийно завершился: {short}"), log)
}

/// Текст с припиской о файле журнала, если он известен.
#[must_use]
pub fn with_details(text: &str, log: Option<&Path>) -> String {
    match log {
        Some(path) => format!("{text}\n\nПодробности: {}", path.display()),
        None => text.to_owned(),
    }
}

/// Ставит обработчик паник и открывает журнал. Вызывается первым в `run`.
pub fn start() {
    install_panic_hook();
    if let Some(path) = log_path() {
        rotate(&path);
    }
    // Открыть заранее: паника не должна зависеть от создания папок.
    with_log(|_| ());
}

/// Запись уровня `INFO`.
pub fn info(message: impl AsRef<str>) {
    write(Level::Info, message.as_ref());
}

/// Запись уровня `ERROR`.
pub fn error(message: impl AsRef<str>) {
    write(Level::Error, message.as_ref());
}

/// Фатальная ошибка старта: журнал, системное окно со ссылкой на журнал и выход с кодом 1.
pub fn fatal(text: &str) -> ! {
    error(text);
    show_error(&with_details(text, log_path().as_deref()));
    std::process::exit(1);
}

/// Системное окно с ошибкой. Вне Windows — строка в stderr.
#[cfg(windows)]
pub fn show_error(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let text: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = "bytetap".encode_utf16().chain([0]).collect();
    // SAFETY: строки с завершающим NUL живут до конца вызова, других требований у функции нет.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
pub fn show_error(text: &str) {
    eprintln!("{text}");
}

fn write(level: Level, message: &str) {
    let line = format_line(&Local::now(), level, message);
    with_log(|file| {
        // Журнал старта — подсказка, а не данные: сбой записи не должен ронять приложение.
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    });
}

/// Вызывает `f` с открытым журналом; если журнал не открыть — молча ничего не делает.
fn with_log(f: impl FnOnce(&mut File)) {
    // Отравленный замок — след паники в другом потоке; журнал при этом по-прежнему годен.
    let mut guard = LOG.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        *guard = log_path().and_then(|path| open(&path));
    }
    if let Some(file) = guard.as_mut() {
        f(file);
    }
}

fn open(path: &Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).ok()?;
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Большой журнал уходит в `bytetap.log.1`, чтобы файл не рос без предела.
fn rotate(path: &Path) {
    if fs::metadata(path).is_ok_and(|meta| meta.len() > ROTATE_AT) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
}

/// Паника пишется в журнал с местом и backtrace; паника главного потока — ещё и окном.
///
/// Главный поток крутит цикл событий окна: его паника завершает процесс. Паники рабочих
/// потоков (команды, задачи tokio) процесс не роняют, поэтому окно о них не нужно.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let summary = panic_summary(info);
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<без имени>").to_owned();
        let location = info.location().map_or_else(
            || "место неизвестно".to_owned(),
            |at| format!("{}:{}:{}", at.file(), at.line(), at.column()),
        );
        let backtrace = std::backtrace::Backtrace::force_capture();
        error(format!(
            "паника в потоке {name} ({location}): {summary}\n{backtrace}"
        ));
        previous(info);
        if name == "main" {
            let text = crash_message(&summary, log_path().as_deref());
            // Окно показывается из отдельного потока: модальный цикл в главном потоке
            // доставлял бы сообщения окну, чей обработчик сейчас паникует.
            let _ = std::thread::spawn(move || show_error(&text)).join();
        }
    }));
}

fn panic_summary(info: &PanicHookInfo<'_>) -> String {
    info.payload_as_str()
        .unwrap_or("паника без текста")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn time() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-21T09:05:07.042+03:00").expect("время теста")
    }

    #[test]
    fn a_line_starts_with_time_and_level() {
        assert_eq!(
            format_line(&time(), Level::Info, "старт bytetap 0.1.0"),
            "2026-10-21T09:05:07.042+03:00 INFO  старт bytetap 0.1.0\n"
        );
        assert_eq!(
            format_line(&time(), Level::Error, "сбой"),
            "2026-10-21T09:05:07.042+03:00 ERROR сбой\n"
        );
    }

    #[test]
    fn a_multiline_message_continues_with_an_indent() {
        assert_eq!(
            format_line(&time(), Level::Error, "паника\n   0: main\n   1: run\n"),
            "2026-10-21T09:05:07.042+03:00 ERROR паника\n       0: main\n       1: run\n"
        );
    }

    #[test]
    fn an_empty_message_still_makes_one_line() {
        assert_eq!(
            format_line(&time(), Level::Info, ""),
            "2026-10-21T09:05:07.042+03:00 INFO  \n"
        );
    }

    #[test]
    fn the_log_lives_next_to_the_app_data() {
        let base = Path::new("C:/Users/u/AppData/Local");
        assert_eq!(
            log_path_in(base),
            base.join("io.github.fgbm.bytetap")
                .join("logs")
                .join("bytetap.log")
        );
    }

    #[test]
    fn the_identifier_matches_the_tauri_config() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(config["identifier"], IDENTIFIER);
    }

    #[test]
    fn the_crash_message_names_the_log() {
        let log = Path::new("C:/logs/bytetap.log");
        let text = crash_message("Failed to setup app: webview", Some(log));
        assert!(text.starts_with("bytetap аварийно завершился: Failed to setup app: webview"));
        assert!(text.ends_with(&format!("Подробности: {}", log.display())));
        assert_eq!(
            crash_message("сбой", None),
            "bytetap аварийно завершился: сбой"
        );
    }

    #[test]
    fn a_long_panic_is_shortened_in_the_window() {
        let text = crash_message(&"ю".repeat(1000), None);
        assert_eq!(text.chars().filter(|c| *c == 'ю').count(), SUMMARY_LIMIT);
        assert!(text.ends_with('…'));
    }
}
