//! Журнал сессии: события, писатель, читатель (FR-19…FR-24).
//!
//! Формат `bytetap-session/1` описан в `docs/reference/session-format.md` и в JSON Schema
//! `docs/public/schema/bytetap-session-1.schema.json`. Решение о писателе — ADR
//! `docs/dev/adr/0001-journal-writer.md`.

mod event;
pub mod reader;
pub mod recent;
pub mod writer;

use std::io;
use std::time::Instant;

use chrono::{Local, SecondsFormat};

pub use event::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, FORMAT, Host, InfoEvent, Mode,
    Note, Open, Session, SessionEnd, Stats,
};
pub use reader::{JournalLine, JournalReader, LineError, LineErrorKind, ReadOutcome, read_all};
pub use recent::{MAX_RECENT_JOURNALS, RecentJournal, recent};
pub use writer::{
    Journal, LogDirWarningKind, append_note, default_log_dir, log_dir_warning,
    log_dir_warning_kind, session_file_name,
};

/// Текущее время по системным часам: RFC 3339 с микросекундами и смещением (FR-22).
#[must_use]
pub fn now_rfc3339() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

/// Текст ошибки ввода-вывода для журнала: английский, из вида ошибки и кода ОС.
///
/// `Display` у системной ошибки зависит от языка системы: на русской Windows тот же сбой
/// описан по-русски. Вид ошибки Rust печатает по-английски всегда, а [`io::Error::raw_os_error`]
/// даёт числовой код для поиска: `connection reset (os error 10054)`. Ошибки без кода ОС
/// (свои сообщения программы) остаются как есть: у них `Display` и есть текст.
pub(crate) fn io_message(err: &io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => format!("{} (os error {code})", err.kind()),
        None => err.to_string(),
    }
}

/// Семейство ОС: одинаковые сбои в Windows и Unix имеют разные номера.
///
/// 112 — «диск полон» в Windows и `EBADRQC` в Linux; 5 — «нет доступа» в Windows и `EIO`
/// в Linux. Поэтому код ОС без семейства не говорит, что случилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    /// Windows: коды `GetLastError`.
    Windows,
    /// Unix: номера `errno`.
    Unix,
}

impl OsFamily {
    /// Семейство, на котором работает программа.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Причина отказа журнала: текст для журнала и подробностей, вид ошибки и код ОС.
///
/// Интерфейс переводит причину по коду ОС, а не по тексту: [`io_message`] — английский
/// и живёт в журнале, тогда как вид отказа для баннера нужен структурой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalFailureReason {
    message: String,
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    os: OsFamily,
}

impl JournalFailureReason {
    /// Причина по системной ошибке: вид, код ОС и семейство сохраняются как есть.
    #[must_use]
    pub fn from_error(err: &io::Error) -> Self {
        Self {
            message: io_message(err),
            kind: err.kind(),
            raw_os_error: err.raw_os_error(),
            os: OsFamily::current(),
        }
    }

    /// Причина из текста без кода ОС: свои сообщения программы.
    #[must_use]
    pub fn from_message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: io::ErrorKind::Other,
            raw_os_error: None,
            os: OsFamily::current(),
        }
    }

    /// Причина из вида ошибки и кода ОС: так же строятся сбои в тестах на любой ОС.
    #[must_use]
    pub fn from_code(kind: io::ErrorKind, raw_os_error: Option<i32>, os: OsFamily) -> Self {
        Self {
            message: match raw_os_error {
                Some(code) => format!("{kind} (os error {code})"),
                None => kind.to_string(),
            },
            kind,
            raw_os_error,
            os,
        }
    }

    /// Текст причины для журнала и подробностей баннера.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Вид ошибки ввода-вывода.
    #[must_use]
    pub const fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    /// Числовой код ОС: по нему интерфейс отличает «нет места» от прочих сбоев.
    #[must_use]
    pub const fn raw_os_error(&self) -> Option<i32> {
        self.raw_os_error
    }

    /// Семейство ОС, которому принадлежит [`Self::raw_os_error`].
    #[must_use]
    pub const fn os(&self) -> OsFamily {
        self.os
    }
}

impl std::fmt::Display for JournalFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JournalFailureReason {}

/// Монотонные часы сессии: `mono_us` — микросекунды от её начала (FR-22).
#[derive(Debug, Clone)]
pub struct MonoClock {
    start: Instant,
}

impl MonoClock {
    /// Запускает часы.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Микросекунды от начала сессии.
    #[must_use]
    pub fn now_us(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

impl Default for MonoClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    #[test]
    fn timestamp_is_rfc3339_with_microseconds_and_offset() {
        let text = now_rfc3339();
        assert_eq!(
            text.len(),
            "2026-10-19T10:15:00.123456+03:00".len(),
            "{text}"
        );
        let parsed = DateTime::parse_from_rfc3339(&text).expect("RFC 3339");
        assert_eq!(parsed.format("%:z").to_string().len(), 6);
    }

    #[test]
    fn monotonic_clock_grows() {
        let clock = MonoClock::new();
        let first = clock.now_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(clock.now_us() > first);
    }

    #[test]
    fn format_constant_is_the_expected_version() {
        assert_eq!(FORMAT, "bytetap-session/1");
    }

    #[test]
    fn a_system_error_is_described_in_english_with_its_code() {
        let text = io_message(&io::Error::from_raw_os_error(2));
        assert_eq!(text, format!("{} (os error 2)", io::ErrorKind::NotFound));
        assert!(
            text.is_ascii(),
            "журнал не должен зависеть от языка системы: {text}"
        );
    }

    #[test]
    fn an_error_without_an_os_code_keeps_its_own_text() {
        assert_eq!(
            io_message(&io::Error::from(io::ErrorKind::ConnectionReset)),
            "connection reset",
            "вид ошибки без кода ОС"
        );
        assert_eq!(
            io_message(&io::Error::other(
                "test hook: the journal device refused the write"
            )),
            "test hook: the journal device refused the write"
        );
    }
}
