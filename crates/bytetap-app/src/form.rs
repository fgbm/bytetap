//! Логика шага сценария без отрисовки (FR-1, FR-2, FR-9, FR-11, U1-AC10, U1-AC11):
//! проверка полей, ошибки у полей и сборка `SessionConfig` ядра.
//!
//! Правила — те же, что в Vue-версии (`ScenarioForm.vue`, `validation.ts`) и в мосте
//! Tauri (`bridge.rs`, `into_bridge`): ядро получает только поля выбранного режима, а
//! неверные значения отвергаются до создания журнала.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytetap_core::journal::Autoresponder;
use bytetap_core::session::SessionConfig;
use bytetap_core::transport::serial::{
    self as core_serial, MAX_BAUD, MIN_BAUD, SerialFormat, SerialSettings as CoreSerial,
};

use crate::errors::{ErrorCode, ErrorField, StartError};
use crate::settings::{
    AppConfig, BAUD_MAX, BAUD_MIN, BAUD_RATES, Flow, LastRun, Parity, Responder, SerialSettings,
    SessionMode,
};
use crate::t;

/// Пауза между попытками открыть COM-порт после разрыва, секунды.
pub const SERIAL_RECONNECT_SEC: u64 = 5;
/// Предел интервала переподключения TCP-клиента: 0…3600 секунд.
pub const MAX_RECONNECT_SEC: u32 = 3600;

/// Форматы COM-порта в списке формы (FR-11).
pub const FORMATS: [&str; 9] = [
    "8N1", "8E1", "8O1", "8M1", "8S1", "8N2", "7E1", "7O1", "7E2",
];

/// Что не так с портом в поле.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortProblem {
    /// Пусто или не целое число.
    Format,
    /// Вне 1–65535.
    Range,
}

impl PortProblem {
    pub fn text(self) -> &'static str {
        match self {
            Self::Format => t!("errors.port_format"),
            Self::Range => t!("errors.port_range"),
        }
    }
}

/// Порт TCP: целое число 1–65535.
pub fn check_port(text: &str) -> Result<u16, PortProblem> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return Err(PortProblem::Format);
    }
    match text.parse::<u64>() {
        Ok(value) if (1..=u64::from(u16::MAX)).contains(&value) => Ok(value as u16),
        _ => Err(PortProblem::Range),
    }
}

/// Скорость COM-порта «другая…»: целое 1…4 000 000.
pub fn check_baud(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>()
        .ok()
        .filter(|value| (BAUD_MIN..=BAUD_MAX).contains(value))
}

/// Интервал переподключения из поля: не число — 0, как `Number(value) || 0` в Vue.
pub fn parse_reconnect(text: &str) -> u32 {
    text.trim().parse().unwrap_or(0)
}

fn parity_letter(parity: Parity) -> char {
    match parity {
        Parity::None => 'N',
        Parity::Even => 'E',
        Parity::Odd => 'O',
        Parity::Mark => 'M',
        Parity::Space => 'S',
    }
}

/// Формат порта: `8N1`, `7E2` — то же, что `--format` в CLI.
pub fn serial_format(serial: &SerialSettings) -> String {
    format!(
        "{}{}{}",
        serial.data_bits,
        parity_letter(serial.parity),
        serial.stop_bits
    )
}

/// Применяет формат `8N1` к настройкам порта; неверная запись ничего не меняет.
pub fn apply_format(serial: &mut SerialSettings, format: &str) {
    let chars: Vec<char> = format.trim().to_ascii_uppercase().chars().collect();
    let [data, parity, stop] = chars.as_slice() else {
        return;
    };
    let parity = match parity {
        'N' => Parity::None,
        'E' => Parity::Even,
        'O' => Parity::Odd,
        'M' => Parity::Mark,
        'S' => Parity::Space,
        _ => return,
    };
    let (data_bits, stop_bits) = match (data, stop) {
        ('7' | '8', '1' | '2') => (*data as u8 - b'0', *stop as u8 - b'0'),
        _ => return,
    };
    serial.data_bits = data_bits;
    serial.parity = parity;
    serial.stop_bits = stop_bits;
}

/// Последний старт для карточки сценария.
pub fn last_run_of(cfg: &AppConfig) -> LastRun {
    match cfg.mode {
        SessionMode::TcpClient => LastRun {
            mode: cfg.mode,
            port: cfg.connect_port.to_string(),
            host: cfg.connect_host.clone(),
            baud: 0,
        },
        SessionMode::Serial => LastRun {
            mode: cfg.mode,
            port: cfg.serial.port.clone(),
            host: String::new(),
            baud: cfg.serial.baud,
        },
        SessionMode::TcpServer => LastRun {
            mode: cfg.mode,
            port: cfg.listen_port.to_string(),
            host: String::new(),
            baud: 0,
        },
    }
}

/// Поля, которые пользователь уже трогал: своя проверка не кричит раньше времени.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Touched {
    pub port: bool,
    pub address: bool,
    pub serial: bool,
}

/// Текст полей формы: то, что набрано, а не то, что уже принято в настройки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormState {
    pub port_text: String,
    pub connect_host: String,
    pub connect_port_text: String,
    pub reconnect_text: String,
    pub baud_text: String,
    /// Скорость вне типового списка: поле ввода вместо списка.
    pub baud_other: bool,
    /// Раскрыто «Дополнительно».
    pub more: bool,
    /// Нажали «Старт»: только после этого пустые поля краснеют.
    pub attempted: bool,
    pub touched: Touched,
    /// Профиль в списке; пусто — «последние настройки».
    pub selected_profile: String,
}

impl FormState {
    pub fn new(cfg: &AppConfig) -> Self {
        let mut state = Self {
            port_text: String::new(),
            connect_host: String::new(),
            connect_port_text: String::new(),
            reconnect_text: String::new(),
            baud_text: String::new(),
            baud_other: false,
            more: false,
            attempted: false,
            touched: Touched::default(),
            selected_profile: String::new(),
        };
        state.sync(cfg);
        state
    }

    /// Профиль или последние настройки меняют конфигурацию снаружи: поля следуют за ней.
    pub fn sync(&mut self, cfg: &AppConfig) {
        self.port_text = cfg.listen_port.to_string();
        self.connect_host = cfg.connect_host.clone();
        self.connect_port_text = cfg.connect_port.to_string();
        self.reconnect_text = cfg.reconnect_sec.to_string();
        self.baud_text = cfg.serial.baud.to_string();
        self.baud_other = !BAUD_RATES.contains(&cfg.serial.baud);
    }

    fn serial_port_missing(cfg: &AppConfig) -> bool {
        cfg.serial.port.is_empty()
    }

    fn baud_value(&self, cfg: &AppConfig) -> Option<u32> {
        if self.baud_other {
            check_baud(&self.baud_text)
        } else {
            Some(cfg.serial.baud)
        }
    }

    /// Своя проверка поля: текст ошибки или `None`.
    pub fn local_error(&self, cfg: &AppConfig, field: ErrorField) -> Option<&'static str> {
        let shown = self.attempted
            || match field {
                ErrorField::Port => self.touched.port,
                ErrorField::Address => self.touched.address,
                ErrorField::Serial => self.touched.serial,
                ErrorField::LogDir | ErrorField::Start => false,
            };
        if !shown {
            return None;
        }
        match field {
            ErrorField::Port => {
                let text = if cfg.mode == SessionMode::TcpClient {
                    &self.connect_port_text
                } else {
                    &self.port_text
                };
                check_port(text).err().map(PortProblem::text)
            }
            ErrorField::Address => self
                .connect_host
                .trim()
                .is_empty()
                .then(|| t!("form.addressRequired")),
            ErrorField::Serial => {
                if Self::serial_port_missing(cfg) {
                    Some(t!("form.serialRequired"))
                } else if self.baud_value(cfg).is_none() {
                    Some(t!("form.baudRange"))
                } else {
                    None
                }
            }
            ErrorField::LogDir | ErrorField::Start => None,
        }
    }

    /// Поля режима проверены: можно собирать конфигурацию.
    pub fn valid(&self, cfg: &AppConfig) -> bool {
        match cfg.mode {
            SessionMode::TcpServer => check_port(&self.port_text).is_ok(),
            SessionMode::TcpClient => {
                check_port(&self.connect_port_text).is_ok() && !self.connect_host.trim().is_empty()
            }
            SessionMode::Serial => {
                !Self::serial_port_missing(cfg) && self.baud_value(cfg).is_some()
            }
        }
    }

    /// Набранное уходит в настройки: порты, адрес, скорость, переподключение.
    pub fn commit(&self, cfg: &mut AppConfig) {
        if let Ok(port) = check_port(&self.port_text) {
            cfg.listen_port = port;
        }
        if let Ok(port) = check_port(&self.connect_port_text) {
            cfg.connect_port = port;
        }
        cfg.connect_host = self.connect_host.trim().to_owned();
        cfg.reconnect_sec = parse_reconnect(&self.reconnect_text);
        if let Some(baud) = self.baud_value(cfg) {
            cfg.serial.baud = baud;
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn responder(responder: Responder) -> Autoresponder {
    match responder {
        Responder::Auto => Autoresponder::Auto,
        Responder::Hl7 => Autoresponder::Hl7Mllp,
        Responder::Astm => Autoresponder::AstmE1381,
        Responder::None => Autoresponder::None,
    }
}

/// Адрес прослушивания TCP-сервера: пустой узел — все интерфейсы.
fn listen_addr(host: &str, port: u16) -> Result<SocketAddr, StartError> {
    let host = host.trim();
    let ip: Ipv4Addr = if host.is_empty() {
        Ipv4Addr::UNSPECIFIED
    } else {
        host.parse().map_err(|_| {
            StartError::new(
                ErrorCode::AddressUnavailable,
                format!("`{host}` is not an IPv4 address of this computer"),
            )
            .with_param("addr", host)
        })?
    };
    Ok(SocketAddr::from((ip, port)))
}

/// Адрес анализатора `HOST:PORT`: имя узла разрешается ядром при каждой попытке (FR-2).
fn connect_addr(host: &str, port: u16) -> Result<String, StartError> {
    let host = host.trim();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(StartError::new(
            ErrorCode::AddressInvalid,
            "analyzer address is empty or contains spaces",
        ));
    }
    if port == 0 {
        return Err(StartError::new(
            ErrorCode::AddressInvalid,
            "analyzer port is zero",
        ));
    }
    // IPv6 без скобок не разобрался бы как HOST:PORT: двоеточий в адресе больше одного.
    if host.starts_with('[') || host.parse::<Ipv6Addr>().is_err() {
        Ok(format!("{host}:{port}"))
    } else {
        Ok(format!("[{host}]:{port}"))
    }
}

/// Настройки COM-порта ядра; поля проверяются до открытия порта (FR-11).
fn serial_settings(serial: &SerialSettings) -> Result<CoreSerial, StartError> {
    let settings_error = |message: String| {
        StartError::new(ErrorCode::PortSettings, message).with_param("port", serial.port.trim())
    };
    let port = serial.port.trim();
    if port.is_empty() {
        return Err(settings_error("port is not selected".to_owned()));
    }
    if !(MIN_BAUD..=MAX_BAUD).contains(&serial.baud) {
        return Err(settings_error(format!(
            "baud {} is outside {MIN_BAUD}..{MAX_BAUD}",
            serial.baud
        )));
    }
    if !matches!(serial.data_bits, 7 | 8) || !matches!(serial.stop_bits, 1 | 2) {
        return Err(settings_error(format!(
            "format {} is not supported",
            serial_format(serial)
        )));
    }
    let parity = match serial.parity {
        Parity::None => core_serial::Parity::None,
        Parity::Even => core_serial::Parity::Even,
        Parity::Odd => core_serial::Parity::Odd,
        Parity::Mark => core_serial::Parity::Mark,
        Parity::Space => core_serial::Parity::Space,
    };
    let flow = match serial.flow {
        Flow::None => core_serial::Flow::None,
        Flow::Rtscts => core_serial::Flow::RtsCts,
        Flow::Xonxoff => core_serial::Flow::XonXoff,
    };
    let format = SerialFormat {
        data_bits: serial.data_bits,
        parity,
        stop_bits: serial.stop_bits,
    };
    Ok(CoreSerial::new(port, serial.baud, format)
        .with_flow(flow)
        .with_lines(serial.dtr, serial.rts))
}

/// Конфигурация сессии ядра из настроек формы: только поля выбранного режима.
///
/// `default_log_dir` — папка по умолчанию, когда поле пусто; `None` — её не определить.
///
/// # Errors
///
/// Код ошибки и поле, под которым её показать: адрес, порт COM, папка журналов.
pub fn session_config(
    cfg: &AppConfig,
    default_log_dir: Option<PathBuf>,
) -> Result<SessionConfig, StartError> {
    let mut config = match cfg.mode {
        SessionMode::TcpServer => {
            SessionConfig::new(listen_addr(&cfg.listen_host, cfg.listen_port)?)
        }
        SessionMode::TcpClient => {
            if cfg.reconnect_sec > MAX_RECONNECT_SEC {
                return Err(StartError::new(
                    ErrorCode::Io,
                    format!(
                        "reconnect interval {} s is over {MAX_RECONNECT_SEC} s",
                        cfg.reconnect_sec
                    ),
                ));
            }
            SessionConfig::tcp_client(
                connect_addr(&cfg.connect_host, cfg.connect_port)?,
                Duration::from_secs(u64::from(cfg.reconnect_sec)),
            )
        }
        SessionMode::Serial => SessionConfig::serial(
            serial_settings(&cfg.serial)?,
            Duration::from_secs(SERIAL_RECONNECT_SEC),
        ),
    }
    .with_responder(responder(cfg.responder))
    .with_astm_nak(cfg.astm_nak_on_bad_checksum);
    let log_dir = match cfg.log_dir.trim() {
        "" => default_log_dir.ok_or_else(|| {
            StartError::new(ErrorCode::LogDir, "the default log folder is unknown")
        })?,
        dir => PathBuf::from(dir),
    };
    config = config.with_log_dir(log_dir);
    if let Some(analyzer) = non_empty(&cfg.analyzer) {
        config = config.with_analyzer(analyzer);
    }
    if let Some(note) = non_empty(&cfg.note) {
        config = config.with_note(note);
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use bytetap_core::transport::Source;

    use super::*;

    fn config(mode: SessionMode) -> AppConfig {
        let mut cfg = AppConfig::with_log_dir("C:/logs");
        cfg.mode = mode;
        cfg
    }

    #[test]
    fn a_port_is_a_whole_number_from_1_to_65535() {
        assert_eq!(check_port("5000"), Ok(5000));
        assert_eq!(check_port(" 1 "), Ok(1));
        assert_eq!(check_port("65535"), Ok(65535));
        assert_eq!(check_port(""), Err(PortProblem::Format));
        assert_eq!(check_port("50a"), Err(PortProblem::Format));
        assert_eq!(check_port("-1"), Err(PortProblem::Format));
        assert_eq!(check_port("0"), Err(PortProblem::Range));
        assert_eq!(check_port("65536"), Err(PortProblem::Range));
        assert_eq!(check_port("99999999999999999999"), Err(PortProblem::Range));
    }

    #[test]
    fn a_custom_baud_is_within_the_limits() {
        assert_eq!(check_baud("250000"), Some(250_000));
        assert_eq!(check_baud("4000000"), Some(4_000_000));
        assert_eq!(check_baud("4000001"), None);
        assert_eq!(check_baud("0"), None);
        assert_eq!(check_baud("9600.5"), None);
        assert_eq!(check_baud(""), None);
    }

    #[test]
    fn the_format_reads_and_writes_back() {
        let mut serial = SerialSettings::default();
        assert_eq!(serial_format(&serial), "8N1");
        for format in FORMATS {
            apply_format(&mut serial, format);
            assert_eq!(serial_format(&serial), format);
        }
        apply_format(&mut serial, "9X3");
        assert_eq!(
            serial_format(&serial),
            "7E2",
            "неверная запись ничего не меняет"
        );
    }

    #[test]
    fn fields_stay_quiet_until_start_or_input() {
        let mut cfg = config(SessionMode::TcpClient);
        let mut form = FormState::new(&cfg);
        form.connect_host.clear();
        form.connect_port_text = "x".into();
        assert!(!form.valid(&cfg));
        assert_eq!(form.local_error(&cfg, ErrorField::Address), None);
        assert_eq!(form.local_error(&cfg, ErrorField::Port), None);

        form.touched.port = true;
        assert_eq!(
            form.local_error(&cfg, ErrorField::Port),
            Some("Порт — целое число от 1 до 65535.")
        );
        assert_eq!(form.local_error(&cfg, ErrorField::Address), None);

        form.attempted = true;
        assert_eq!(
            form.local_error(&cfg, ErrorField::Address),
            Some("Укажите адрес анализатора")
        );

        cfg.mode = SessionMode::Serial;
        assert_eq!(
            form.local_error(&cfg, ErrorField::Serial),
            Some("Выберите порт")
        );
        cfg.serial.port = "COM5".into();
        form.baud_other = true;
        form.baud_text = "0".into();
        assert_eq!(
            form.local_error(&cfg, ErrorField::Serial),
            Some("Скорость — целое число от 1 до 4 000 000")
        );
        form.baud_text = "250000".into();
        assert_eq!(form.local_error(&cfg, ErrorField::Serial), None);
        assert!(form.valid(&cfg));
    }

    #[test]
    fn the_server_port_is_checked_only_in_its_mode() {
        let cfg = config(SessionMode::Serial);
        let mut form = FormState::new(&cfg);
        form.port_text = "0".into();
        let mut serial = cfg.clone();
        serial.serial.port = "COM1".into();
        assert!(
            form.valid(&serial),
            "порт TCP-сервера в режиме COM не нужен"
        );
        let server = config(SessionMode::TcpServer);
        assert!(!form.valid(&server));
    }

    #[test]
    fn commit_takes_what_was_typed() {
        let mut cfg = config(SessionMode::TcpClient);
        let mut form = FormState::new(&cfg);
        form.port_text = "6000".into();
        form.connect_port_text = "bad".into();
        form.connect_host = "  10.0.0.7 ".into();
        form.reconnect_text = "abc".into();
        form.baud_other = true;
        form.baud_text = "250000".into();
        form.commit(&mut cfg);
        assert_eq!(cfg.listen_port, 6000);
        assert_eq!(cfg.connect_port, 5000, "неверный порт не принят");
        assert_eq!(cfg.connect_host, "10.0.0.7");
        assert_eq!(cfg.reconnect_sec, 0);
        assert_eq!(cfg.serial.baud, 250_000);
        assert_eq!(FormState::new(&cfg).baud_text, "250000");
        assert!(FormState::new(&cfg).baud_other);
    }

    #[test]
    fn the_server_config_listens_on_the_chosen_interface() {
        let mut cfg = config(SessionMode::TcpServer);
        cfg.listen_port = 5001;
        cfg.responder = Responder::Astm;
        cfg.astm_nak_on_bad_checksum = true;
        cfg.analyzer = "  A-1 ".into();
        let core = session_config(&cfg, None).expect("сервер");
        assert_eq!(
            core.source,
            Source::TcpServer {
                listen: "0.0.0.0:5001".parse().unwrap()
            }
        );
        assert_eq!(core.responder, Autoresponder::AstmE1381);
        assert!(core.astm_nak_on_bad_checksum);
        assert_eq!(core.analyzer.as_deref(), Some("A-1"));
        assert_eq!(core.note, None);
        assert_eq!(core.log_dir, Some(PathBuf::from("C:/logs")));

        cfg.listen_host = "192.168.1.10".into();
        let core = session_config(&cfg, None).expect("интерфейс");
        assert_eq!(
            core.source,
            Source::TcpServer {
                listen: "192.168.1.10:5001".parse().unwrap()
            }
        );

        cfg.listen_host = "eth0".into();
        let error = session_config(&cfg, None).expect_err("не IPv4");
        assert_eq!(error.code, ErrorCode::AddressUnavailable);
        assert_eq!(error.field(cfg.mode), ErrorField::Address);
        assert_eq!(error.params["addr"], "eth0");
    }

    #[test]
    fn the_client_config_connects_with_reconnects() {
        let mut cfg = config(SessionMode::TcpClient);
        cfg.connect_host = "lab-analyzer".into();
        cfg.connect_port = 6001;
        cfg.reconnect_sec = 7;
        cfg.note = "проба".into();
        let core = session_config(&cfg, None).expect("клиент");
        assert_eq!(
            core.source,
            Source::TcpClient {
                addr: "lab-analyzer:6001".into(),
                reconnect: Duration::from_secs(7)
            }
        );
        assert_eq!(core.responder, Autoresponder::Auto);
        assert_eq!(core.note.as_deref(), Some("проба"));

        cfg.connect_host = "fe80::1".into();
        let core = session_config(&cfg, None).expect("IPv6");
        assert!(matches!(core.source, Source::TcpClient { addr, .. } if addr == "[fe80::1]:6001"));

        cfg.connect_host = "10.0.0 .1".into();
        let error = session_config(&cfg, None).expect_err("пробел в адресе");
        assert_eq!(error.code, ErrorCode::AddressInvalid);
        assert_eq!(error.field(cfg.mode), ErrorField::Address);

        cfg.connect_host = "10.0.0.1".into();
        cfg.reconnect_sec = 3601;
        let error = session_config(&cfg, None).expect_err("переподключение");
        assert_eq!(error.field(cfg.mode), ErrorField::Start);
    }

    #[test]
    fn the_serial_config_carries_the_port_settings() {
        let mut cfg = config(SessionMode::Serial);
        cfg.serial.port = "COM5".into();
        cfg.serial.baud = 19200;
        apply_format(&mut cfg.serial, "7E2");
        cfg.serial.flow = Flow::Rtscts;
        cfg.serial.dtr = false;
        cfg.responder = Responder::None;
        let core = session_config(&cfg, None).expect("COM");
        let Source::Serial {
            settings,
            reconnect,
        } = &core.source
        else {
            panic!("ожидался COM, а не {:?}", core.source);
        };
        assert_eq!(settings.port, "COM5");
        assert_eq!(settings.baud, 19200);
        assert_eq!(settings.format.to_string(), "7E2");
        assert_eq!(settings.flow, core_serial::Flow::RtsCts);
        assert!(!settings.dtr);
        assert!(settings.rts);
        assert_eq!(*reconnect, Duration::from_secs(SERIAL_RECONNECT_SEC));
        assert_eq!(core.responder, Autoresponder::None);

        cfg.serial.port = String::new();
        let error = session_config(&cfg, None).expect_err("порт не выбран");
        assert_eq!(error.code, ErrorCode::PortSettings);
        assert_eq!(error.field(cfg.mode), ErrorField::Serial);
    }

    #[test]
    fn an_empty_log_dir_takes_the_default_or_fails() {
        let mut cfg = config(SessionMode::TcpServer);
        cfg.log_dir = "  ".into();
        let core = session_config(&cfg, Some(PathBuf::from("/home/u/Documents/bytetap")))
            .expect("папка по умолчанию");
        assert_eq!(
            core.log_dir,
            Some(PathBuf::from("/home/u/Documents/bytetap"))
        );
        let error = session_config(&cfg, None).expect_err("папки нет");
        assert_eq!(error.code, ErrorCode::LogDir);
        assert_eq!(error.field(cfg.mode), ErrorField::LogDir);
    }

    #[test]
    fn the_last_run_shows_the_port_of_the_mode() {
        let mut cfg = config(SessionMode::TcpClient);
        cfg.connect_host = "10.0.0.7".into();
        cfg.connect_port = 6000;
        let run = last_run_of(&cfg);
        assert_eq!((run.port.as_str(), run.host.as_str()), ("6000", "10.0.0.7"));
        cfg.mode = SessionMode::Serial;
        cfg.serial.port = "COM3".into();
        let run = last_run_of(&cfg);
        assert_eq!((run.port.as_str(), run.baud), ("COM3", 9600));
        cfg.mode = SessionMode::TcpServer;
        assert_eq!(last_run_of(&cfg).port, "5000");
    }
}
