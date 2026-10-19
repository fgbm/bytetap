//! Журнал сессии: события, писатель, читатель (FR-19…FR-24).
//!
//! Формат `bytetap-session/1` описан в `docs/reference/session-format.md` и в JSON Schema
//! `docs/public/schema/bytetap-session-1.schema.json`. Решение о писателе — ADR
//! `docs/dev/adr/0001-journal-writer.md`.

mod event;
pub mod reader;
pub mod writer;

use std::io;
use std::time::Instant;

use chrono::{Local, SecondsFormat};

pub use event::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, FORMAT, Host, InfoEvent, Mode,
    Note, Open, Session, SessionEnd, Stats,
};
pub use reader::{JournalLine, JournalReader, LineError, LineErrorKind, ReadOutcome, read_all};
pub use writer::{Journal, append_note, default_log_dir, log_dir_warning, session_file_name};

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
