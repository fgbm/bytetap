//! Ошибки старта и формы по коду (U1-AC11): текст из каталога и поле, под которым он
//! показывается, — как `app/src/session/errors.ts` в Vue-версии.
//!
//! Текст ошибки ядра (`message`) остаётся журналу запуска и подстановке `{message}`;
//! на экран идёт фраза каталога по коду. UI v2: ошибка — красный текст 12px под своим
//! полем, без баннеров.

use std::collections::BTreeMap;
use std::io;

use crate::settings::{AppConfig, SessionMode};
use crate::{i18n, t};

/// Код ошибки — тот же контракт, что у моста Tauri-версии.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// Порт занят другой программой (обычно ЛИС).
    PortInUse,
    /// Адрес нельзя занять: его нет на этой машине.
    AddressUnavailable,
    /// Адрес анализатора не разбирается.
    AddressInvalid,
    /// Нет прав: защищённый порт, чужая папка.
    PermissionDenied,
    /// Папка журналов недоступна.
    LogDir,
    /// Запись уже идёт.
    AlreadyRunning,
    /// Запись в журнал отказала.
    Journal,
    /// Журнал не открылся или не читается.
    JournalOpen,
    /// COM-порта нет.
    PortNotFound,
    /// COM-порт занят.
    PortBusy,
    /// Настройки COM-порта неверны.
    PortSettings,
    /// Прочая ошибка COM-порта.
    Serial,
    /// Всё остальное.
    Io,
}

impl ErrorCode {
    pub const ALL: [Self; 13] = [
        Self::PortInUse,
        Self::AddressUnavailable,
        Self::AddressInvalid,
        Self::PermissionDenied,
        Self::LogDir,
        Self::AlreadyRunning,
        Self::Journal,
        Self::JournalOpen,
        Self::PortNotFound,
        Self::PortBusy,
        Self::PortSettings,
        Self::Serial,
        Self::Io,
    ];

    /// Строка каталога с подстановками `{port}`, `{addr}`, `{dir}`, `{message}`.
    fn template(self) -> &'static str {
        match self {
            Self::PortInUse => t!("errors.port_in_use"),
            Self::AddressUnavailable => t!("errors.address_unavailable"),
            Self::AddressInvalid => t!("errors.address_invalid"),
            Self::PermissionDenied => t!("errors.permission_denied"),
            Self::LogDir => t!("errors.log_dir"),
            Self::AlreadyRunning => t!("errors.already_running"),
            Self::Journal => t!("errors.journal"),
            Self::JournalOpen => t!("errors.journal_open"),
            Self::PortNotFound => t!("errors.port_not_found"),
            Self::PortBusy => t!("errors.port_busy"),
            Self::PortSettings => t!("errors.port_settings"),
            Self::Serial => t!("errors.serial"),
            Self::Io => t!("errors.io"),
        }
    }

    /// Код по виду ошибки ввода-вывода; остальное — [`ErrorCode::Io`].
    pub fn from_io_kind(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::AddrInUse => Self::PortInUse,
            io::ErrorKind::AddrNotAvailable => Self::AddressUnavailable,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            _ => Self::Io,
        }
    }
}

/// Поле формы, под которым показывается ошибка; `Start` — под кнопкой «Старт».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorField {
    Port,
    Address,
    Serial,
    LogDir,
    Start,
}

/// Куда отнести ошибку: порт TCP у сервера и порт COM — разные поля, поэтому нужен и
/// режим.
pub fn error_field(code: ErrorCode, mode: SessionMode) -> ErrorField {
    let serial = mode == SessionMode::Serial;
    match code {
        ErrorCode::PortInUse | ErrorCode::PermissionDenied if serial => ErrorField::Serial,
        ErrorCode::PortInUse | ErrorCode::PermissionDenied => ErrorField::Port,
        ErrorCode::AddressUnavailable | ErrorCode::AddressInvalid => ErrorField::Address,
        ErrorCode::PortNotFound
        | ErrorCode::PortBusy
        | ErrorCode::PortSettings
        | ErrorCode::Serial => ErrorField::Serial,
        ErrorCode::LogDir | ErrorCode::JournalOpen => ErrorField::LogDir,
        ErrorCode::AlreadyRunning | ErrorCode::Journal | ErrorCode::Io => ErrorField::Start,
    }
}

/// Ошибка старта: код, текст ядра и подстановки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartError {
    pub code: ErrorCode,
    /// Текст для журнала запуска и подстановки `{message}`.
    pub message: String,
    pub params: BTreeMap<String, String>,
}

impl StartError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            params: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_param(mut self, key: &str, value: impl Into<String>) -> Self {
        self.params.insert(key.to_owned(), value.into());
        self
    }

    pub fn field(&self, mode: SessionMode) -> ErrorField {
        error_field(self.code, mode)
    }

    /// Текст для экрана: подстановки ядра важнее текущей конфигурации.
    pub fn text(&self, cfg: &AppConfig) -> String {
        let mut args: BTreeMap<&str, String> = BTreeMap::from([
            ("port", cfg.listen_port.to_string()),
            (
                "addr",
                if cfg.listen_host.is_empty() {
                    "0.0.0.0".to_owned()
                } else {
                    cfg.listen_host.clone()
                },
            ),
            ("host", cfg.connect_host.clone()),
            ("connectPort", cfg.connect_port.to_string()),
            ("dir", cfg.log_dir.clone()),
            ("path", cfg.log_dir.clone()),
            ("message", self.message.clone()),
        ]);
        if cfg.mode == SessionMode::Serial {
            args.insert("port", cfg.serial.port.clone());
        }
        for (key, value) in &self.params {
            args.insert(key.as_str(), value.clone());
        }
        let args: Vec<(&str, String)> = args.into_iter().collect();
        i18n::fill(self.code.template(), &args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_catalog_text() {
        for code in ErrorCode::ALL {
            let text = code.template();
            assert!(!text.starts_with("errors."), "{code:?}: {text}");
        }
    }

    #[test]
    fn codes_are_bound_to_their_fields() {
        use ErrorCode::*;
        use SessionMode::{Serial as Com, TcpClient, TcpServer};
        let cases = [
            (PortInUse, TcpServer, ErrorField::Port),
            (PortInUse, Com, ErrorField::Serial),
            (PermissionDenied, TcpServer, ErrorField::Port),
            (PermissionDenied, Com, ErrorField::Serial),
            (AddressUnavailable, TcpServer, ErrorField::Address),
            (AddressInvalid, TcpClient, ErrorField::Address),
            (PortNotFound, Com, ErrorField::Serial),
            (PortBusy, Com, ErrorField::Serial),
            (PortSettings, Com, ErrorField::Serial),
            (Serial, Com, ErrorField::Serial),
            (LogDir, TcpClient, ErrorField::LogDir),
            (JournalOpen, TcpServer, ErrorField::LogDir),
            (AlreadyRunning, TcpServer, ErrorField::Start),
            (Journal, Com, ErrorField::Start),
            (Io, TcpClient, ErrorField::Start),
        ];
        for (code, mode, field) in cases {
            assert_eq!(error_field(code, mode), field, "{code:?} в {mode:?}");
        }
    }

    #[test]
    fn the_text_takes_the_core_params_first() {
        let mut cfg = AppConfig::with_log_dir("C:/logs");
        cfg.listen_port = 5000;
        let error = StartError::new(ErrorCode::PortInUse, "address in use");
        assert_eq!(
            error.text(&cfg),
            "Порт 5000 уже занят другой программой (возможно, ЛИС). Выберите другой."
        );
        let error = StartError::new(ErrorCode::LogDir, "denied").with_param("dir", "D:/x");
        assert_eq!(
            error.text(&cfg),
            "Не удалось писать в папку журналов D:/x. Выберите другую папку."
        );
        let error = StartError::new(ErrorCode::Io, "broken pipe");
        assert_eq!(error.text(&cfg), "Ошибка ввода-вывода: broken pipe");
    }

    #[test]
    fn a_serial_error_names_the_com_port() {
        let mut cfg = AppConfig::with_log_dir("");
        cfg.mode = SessionMode::Serial;
        cfg.serial.port = "COM5".into();
        let error = StartError::new(ErrorCode::PortBusy, "busy");
        assert_eq!(error.text(&cfg), "Порт COM5 занят другой программой.");
    }

    #[test]
    fn io_kinds_map_to_codes() {
        assert_eq!(
            ErrorCode::from_io_kind(io::ErrorKind::AddrInUse),
            ErrorCode::PortInUse
        );
        assert_eq!(
            ErrorCode::from_io_kind(io::ErrorKind::AddrNotAvailable),
            ErrorCode::AddressUnavailable
        );
        assert_eq!(
            ErrorCode::from_io_kind(io::ErrorKind::PermissionDenied),
            ErrorCode::PermissionDenied
        );
        assert_eq!(ErrorCode::from_io_kind(io::ErrorKind::Other), ErrorCode::Io);
    }
}
