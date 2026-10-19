//! Журнал сессии: события, писатель, читатель (FR-19…FR-24).
//!
//! Формат `bytetap-session/1` описан в `docs/reference/session-format.md` и в JSON Schema
//! `docs/public/schema/bytetap-session-1.schema.json`. Решение о писателе — ADR
//! `docs/dev/adr/0001-journal-writer.md`.

mod event;
pub mod reader;
pub mod writer;

use std::time::Instant;

use chrono::{Local, SecondsFormat};

pub use event::{
    Autoresponder, Close, ClosedBy, Data, Dir, ErrorEvent, Event, FORMAT, Host, Mode, Note, Open,
    Session, SessionEnd, Stats,
};
pub use reader::{JournalLine, JournalReader, LineError, LineErrorKind, ReadOutcome, read_all};
pub use writer::{Journal, append_note, default_log_dir, session_file_name};

/// Текущее время по системным часам: RFC 3339 с микросекундами и смещением (FR-22).
#[must_use]
pub fn now_rfc3339() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Micros, false)
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
}
