//! Читатель журнала: строки файла превращаются в события.
//!
//! Ошибка сообщается с номером строки. Последняя строка может быть обрезана аварийным
//! завершением или отключением питания (FR-20): читатель отличает её от битой строки,
//! чтобы `bytetap-cli verify` считал её предупреждением. Признак обрыва строгий: строка без
//! `\n`, на которой разбор JSON остановился из-за неожиданного конца ввода, или строка без
//! `\n`, оборванная посреди многобайтного символа UTF-8. Завершённая строка с синтаксической
//! ошибкой и строка, где JSON цел, но не является событием формата, — ошибка разбора, где бы
//! они ни стояли.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::journal::event::Event;

/// Одна разобранная строка журнала: исходный JSON и типизированное событие.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalLine {
    /// Номер строки в файле, с 1.
    pub number: usize,
    /// Строка как JSON — для проверки по схеме.
    pub value: Value,
    /// Разобранное событие.
    pub event: Event,
}

/// Ошибка разбора строки журнала.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    /// Номер строки в файле, с 1.
    pub line: usize,
    /// Что не так.
    pub kind: LineErrorKind,
    /// Подробности для человека.
    pub message: String,
}

/// Вид ошибки строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineErrorKind {
    /// Строка не JSON или не событие формата.
    Parse,
    /// Обрезанная последняя строка: потеря при аварийном завершении, а не ошибка.
    Truncated,
    /// Ошибка чтения файла.
    Io,
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "строка {}: {}", self.line, self.message)
    }
}

impl std::error::Error for LineError {}

/// Итератор по строкам журнала.
pub struct JournalReader<R> {
    reader: R,
    line: usize,
    finished: bool,
}

impl JournalReader<BufReader<File>> {
    /// Открывает журнал для чтения.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self::new(BufReader::new(file)))
    }
}

impl<R: BufRead> JournalReader<R> {
    /// Читает журнал из любого буферизованного источника.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line: 0,
            finished: false,
        }
    }

    fn error(&self, kind: LineErrorKind, message: impl Into<String>) -> LineError {
        LineError {
            line: self.line,
            kind,
            message: message.into(),
        }
    }
}

impl<R: BufRead> Iterator for JournalReader<R> {
    type Item = Result<JournalLine, LineError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut buffer = Vec::new();
        match self.reader.read_until(b'\n', &mut buffer) {
            Ok(0) => {
                self.finished = true;
                return None;
            }
            Ok(_) => {}
            Err(err) => {
                self.finished = true;
                self.line += 1;
                return Some(Err(LineError {
                    line: self.line,
                    kind: LineErrorKind::Io,
                    message: err.to_string(),
                }));
            }
        }

        self.line += 1;
        let terminated = buffer.last() == Some(&b'\n');
        if terminated {
            buffer.pop();
        }
        if buffer.is_empty() {
            return Some(Err(self.error(LineErrorKind::Parse, "пустая строка")));
        }
        let text = match std::str::from_utf8(&buffer) {
            Ok(text) => text,
            Err(err) => {
                // Незавершённая последняя строка могла быть оборвана посреди многобайтного
                // символа UTF-8: `error_len() == None` значит «ввод кончился на неполной
                // последовательности», а не «байты не UTF-8».
                let kind = if !terminated && err.error_len().is_none() {
                    LineErrorKind::Truncated
                } else {
                    LineErrorKind::Parse
                };
                return Some(Err(self.error(kind, format!("строка не UTF-8: {err}"))));
            }
        };
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(err) => {
                // Обрыв — только незавершённая строка, на которой кончился ввод.
                let kind = if terminated || !err.is_eof() {
                    LineErrorKind::Parse
                } else {
                    LineErrorKind::Truncated
                };
                return Some(Err(self.error(kind, err.to_string())));
            }
        };
        let event = match Event::deserialize(&value) {
            Ok(event) => event,
            Err(err) => return Some(Err(self.error(LineErrorKind::Parse, err.to_string()))),
        };
        Some(Ok(JournalLine {
            number: self.line,
            value,
            event,
        }))
    }
}

/// Итог разбора журнала целиком.
#[derive(Debug, Default)]
pub struct ReadOutcome {
    /// Разобранные строки в порядке файла.
    pub lines: Vec<JournalLine>,
    /// Строки, которые не разобрались.
    pub problems: Vec<LineError>,
}

impl ReadOutcome {
    /// `true`, если есть ошибки, кроме обрезанной последней строки.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.problems
            .iter()
            .any(|problem| problem.kind != LineErrorKind::Truncated)
    }
}

/// Читает журнал целиком; ошибка открытия — `io::Error`, ошибки строк — в [`ReadOutcome::problems`].
pub fn read_all(path: impl AsRef<Path>) -> io::Result<ReadOutcome> {
    let mut outcome = ReadOutcome::default();
    for line in JournalReader::open(path)? {
        match line {
            Ok(line) => outcome.lines.push(line),
            Err(problem) => outcome.problems.push(problem),
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const SESSION: &str = r#"{"type":"session","format":"bytetap-session/1","app_version":"0.1.0","started_at":"2026-10-19T10:15:00.123456+03:00","mode":"tcp_server","autoresponder":"hl7_mllp","settings":{},"host":{"os":"Linux"}}"#;
    const NOTE: &str =
        r#"{"type":"note","t":"2026-10-19T10:20:00.000000+03:00","text":"прогнали три пробы"}"#;

    fn reader(text: &str) -> JournalReader<Cursor<Vec<u8>>> {
        JournalReader::new(Cursor::new(text.as_bytes().to_vec()))
    }

    #[test]
    fn reads_lines_in_order_with_numbers() {
        let text = format!("{SESSION}\n{NOTE}\n");
        let lines: Vec<JournalLine> = reader(&text).map(Result::unwrap).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].number, 1);
        assert_eq!(lines[0].event.kind(), "session");
        assert_eq!(lines[1].number, 2);
        assert_eq!(lines[1].event.kind(), "note");
        assert_eq!(lines[1].value["text"], "прогнали три пробы");
    }

    #[test]
    fn reports_the_line_number_of_a_broken_line() {
        let text = format!("{SESSION}\n{{ не json\n{NOTE}\n");
        let items: Vec<Result<JournalLine, LineError>> = reader(&text).collect();
        assert_eq!(items.len(), 3);
        assert!(items[0].is_ok());
        let error = items[1].as_ref().unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.kind, LineErrorKind::Parse);
        assert_eq!(items[2].as_ref().unwrap().number, 3);
    }

    #[test]
    fn unknown_event_type_is_a_parse_error() {
        let text = "{\"type\":\"telemetry\"}\n";
        let error = reader(text).next().unwrap().unwrap_err();
        assert_eq!(error.kind, LineErrorKind::Parse);
        assert!(error.message.contains("telemetry"), "{}", error.message);
    }

    #[test]
    fn truncated_last_line_is_reported_as_truncated() {
        let text = format!("{SESSION}\n{{\"type\":\"note\",\"t\":\"2026");
        let items: Vec<Result<JournalLine, LineError>> = reader(&text).collect();
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
        let error = items[1].as_ref().unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.kind, LineErrorKind::Truncated);
    }

    #[test]
    fn broken_json_without_newline_is_an_error_not_a_truncation() {
        // Последняя строка без `\n`, но разбор упал не на конце ввода.
        let text = format!("{SESSION}\n{{ мусор");
        let error = reader(&text).last().unwrap().unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.kind, LineErrorKind::Parse, "{error}");
    }

    #[test]
    fn a_truncated_shape_with_newline_is_an_error() {
        let text = format!("{SESSION}\n{{\"type\":\"note\",\"t\":\"2026\n");
        let error = reader(&text).last().unwrap().unwrap_err();
        assert_eq!(error.kind, LineErrorKind::Parse, "{error}");
    }

    #[test]
    fn a_last_line_cut_inside_a_utf8_character_is_a_truncation() {
        // Строка без `\n`, оборванная посреди символа «П» (0xD0 0x9F): обрыв, а не Parse.
        let mut bytes = format!("{SESSION}\n").into_bytes();
        bytes.extend_from_slice(b"{\"type\":\"note\",\"t\":\"t\",\"text\":\"\xD0");
        let items: Vec<Result<JournalLine, LineError>> =
            JournalReader::new(Cursor::new(bytes)).collect();
        assert_eq!(items.len(), 2);
        let error = items[1].as_ref().unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.kind, LineErrorKind::Truncated, "{error}");
    }

    #[test]
    fn an_unterminated_line_with_broken_utf8_in_the_middle_is_an_error() {
        // Ввод не кончился на неполной последовательности: байт испорчен в середине строки.
        let mut bytes = format!("{SESSION}\n").into_bytes();
        bytes.extend_from_slice(b"{\"type\":\"note\",\"t\":\"t\",\"text\":\"\xD0\xD0\"}");
        let items: Vec<Result<JournalLine, LineError>> =
            JournalReader::new(Cursor::new(bytes)).collect();
        assert_eq!(
            items[1].as_ref().unwrap_err().kind,
            LineErrorKind::Parse,
            "{:?}",
            items[1]
        );
    }

    #[test]
    fn complete_json_that_is_not_an_event_is_a_parse_error() {
        let text = "{\"type\":\"telemetry\"}";
        let error = reader(text).next().unwrap().unwrap_err();
        assert_eq!(error.kind, LineErrorKind::Parse, "{error}");
        assert!(error.message.contains("telemetry"), "{}", error.message);
    }

    #[test]
    fn a_complete_last_line_without_newline_is_fine() {
        let text = format!("{SESSION}\n{NOTE}");
        let items: Vec<Result<JournalLine, LineError>> = reader(&text).collect();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(Result::is_ok));
    }

    #[test]
    fn empty_line_is_an_error() {
        let text = format!("{SESSION}\n\n{NOTE}\n");
        let items: Vec<Result<JournalLine, LineError>> = reader(&text).collect();
        assert_eq!(items[1].as_ref().unwrap_err().line, 2);
        assert_eq!(items[1].as_ref().unwrap_err().kind, LineErrorKind::Parse);
    }

    #[test]
    fn empty_file_has_no_lines() {
        assert!(reader("").next().is_none());
    }

    #[test]
    fn broken_middle_line_makes_outcome_erroneous_truncated_does_not() {
        let text = format!("{SESSION}\n{{\"type\":\"note\",\"t\":\"2026");
        let outcome = ReadOutcome {
            lines: reader(&text).filter_map(Result::ok).collect(),
            problems: reader(&text).filter_map(Result::err).collect(),
        };
        assert_eq!(outcome.lines.len(), 1);
        assert!(!outcome.has_errors(), "{:?}", outcome.problems);

        let text = format!("{SESSION}\n{{ мусор\n{NOTE}\n");
        let outcome = ReadOutcome {
            lines: reader(&text).filter_map(Result::ok).collect(),
            problems: reader(&text).filter_map(Result::err).collect(),
        };
        assert!(outcome.has_errors());
    }
}
