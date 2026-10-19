//! Ошибки команд моста: код для интерфейса, текст для журнала и отладки.
//!
//! Контракт (`app/src/api/types.ts`) задаёт набор кодов: интерфейс переводит ошибку
//! по `code`, а `message` показывает только в подробностях. Поэтому вид системной
//! ошибки превращается в код здесь, один раз, а не в каждом обработчике команды.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use serde::Serialize;

/// Код ошибки для интерфейса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Порт занят другой программой (обычно ЛИС).
    PortInUse,
    /// Адрес нельзя занять: его нет на этой машине.
    AddressUnavailable,
    /// Нет прав: защищённый порт, чужая папка.
    PermissionDenied,
    /// Папка журналов недоступна.
    LogDir,
    /// Запись уже идёт.
    AlreadyRunning,
    /// Журнал не открылся, не читается или отказал.
    Journal,
    /// Всё остальное.
    Io,
}

/// Ошибка команды моста в виде, который сериализуется в интерфейс.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandError {
    /// Код для перевода и выбора действия.
    pub code: ErrorCode,
    /// Текст для журнала и отладки.
    pub message: String,
    /// Подстановки в текст интерфейса: порт, путь.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<BTreeMap<String, String>>,
}

impl CommandError {
    /// Ошибка с кодом и текстом.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            params: None,
        }
    }

    /// Добавляет подстановку для текста интерфейса.
    #[must_use]
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }

    /// Код по виду ошибки ввода-вывода.
    ///
    /// Остальные виды — [`ErrorCode::Io`]: обработчик команды, который знает контекст,
    /// заменяет код сам (`log_dir`, `journal`).
    #[must_use]
    pub fn code_for(kind: io::ErrorKind) -> ErrorCode {
        match kind {
            io::ErrorKind::AddrInUse => ErrorCode::PortInUse,
            io::ErrorKind::AddrNotAvailable => ErrorCode::AddressUnavailable,
            io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::Io,
        }
    }

    /// Ошибка по ошибке ввода-вывода: код из вида, текст из причины.
    #[must_use]
    pub fn from_io(err: &io::Error) -> Self {
        Self::new(Self::code_for(err.kind()), err.to_string())
    }

    /// Ошибка ввода-вывода с заданным кодом: контекст важнее вида ошибки.
    #[must_use]
    pub fn from_io_with(code: ErrorCode, err: &io::Error) -> Self {
        Self::new(code, err.to_string())
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for CommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_is_a_string_from_the_contract() {
        let error = CommandError::new(ErrorCode::PortInUse, "address already in use")
            .with_param("port", "5000");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "code": "port_in_use",
                "message": "address already in use",
                "params": {"port": "5000"}
            })
        );
        let error = CommandError::new(ErrorCode::Io, "boom");
        assert!(
            serde_json::to_value(&error)
                .unwrap()
                .get("params")
                .is_none(),
            "без подстановок поле не пишется"
        );
    }

    #[test]
    fn every_contract_code_has_its_snake_case_name() {
        let codes = [
            (ErrorCode::PortInUse, "port_in_use"),
            (ErrorCode::AddressUnavailable, "address_unavailable"),
            (ErrorCode::PermissionDenied, "permission_denied"),
            (ErrorCode::LogDir, "log_dir"),
            (ErrorCode::AlreadyRunning, "already_running"),
            (ErrorCode::Journal, "journal"),
            (ErrorCode::Io, "io"),
        ];
        for (code, name) in codes {
            assert_eq!(serde_json::to_value(code).unwrap(), name);
        }
    }

    #[test]
    fn io_errors_are_mapped_by_kind() {
        assert_eq!(
            CommandError::from_io(&io::Error::from(io::ErrorKind::AddrInUse)).code,
            ErrorCode::PortInUse
        );
        assert_eq!(
            CommandError::from_io(&io::Error::from(io::ErrorKind::AddrNotAvailable)).code,
            ErrorCode::AddressUnavailable
        );
        assert_eq!(
            CommandError::from_io(&io::Error::from(io::ErrorKind::PermissionDenied)).code,
            ErrorCode::PermissionDenied
        );
        let other = CommandError::from_io(&io::Error::from(io::ErrorKind::UnexpectedEof));
        assert_eq!(other.code, ErrorCode::Io);
        assert!(
            other.message.contains("unexpected end of file"),
            "{}",
            other.message
        );
    }

    #[test]
    fn the_context_code_wins_over_the_kind() {
        let error = CommandError::from_io_with(
            ErrorCode::Journal,
            &io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert_eq!(error.code, ErrorCode::Journal);
    }
}
