//! Логика шага сценария без отрисовки (FR-1, FR-2, FR-9, FR-11, U1-AC10, U1-AC11):
//! проверка полей, ошибки у полей и сборка `SessionConfig` ядра.
//!
//! Правила — те же, что в Vue-версии v0.1.0 (`app/src/**/ScenarioForm.vue`, `validation.ts`)
//! и в мосте Tauri (`app/src-tauri/src/bridge.rs`, `into_bridge`): ядро получает только
//! поля выбранного режима, а неверные значения отвергаются до создания журнала.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bytetap_core::journal::Autoresponder;
use bytetap_core::session::SessionConfig;
use bytetap_core::transport::serial::{
    self as core_serial, MAX_BAUD, MIN_BAUD, SerialFormat, SerialSettings as CoreSerial,
};
use bytetap_core::transport::{ProxyRole, peer_fits_listener, unmapped_ip};

use crate::errors::{ErrorCode, ErrorField, StartError};
use crate::settings::{
    AppConfig, BAUD_MAX, BAUD_MIN, BAUD_RATES, Flow, LastRun, Parity, ProxyMode, Responder,
    SerialSettings, SessionMode,
};
use crate::t;

/// Пауза между попытками открыть COM-порт после разрыва, секунды.
pub const SERIAL_RECONNECT_SEC: u64 = 5;
/// Предел интервала переподключения TCP-клиента: 0…3600 секунд.
pub const MAX_RECONNECT_SEC: u32 = 3600;
/// Пауза прокси после неудачного подключения к цели (FR-5, M4-AC6).
///
/// Переподключения у прокси нет — вторая сторона приходит сама, — и пауза нужна только
/// затем, чтобы недоступная цель не крутила `accept` без задержки. Сто миллисекунд —
/// нижняя граница ядра, дальше серия ошибок схлопывается в журнале и ленте. Те же
/// сто миллисекунд держит `bytetap-cli capture --proxy`.
pub const PROXY_RETRY: Duration = Duration::from_millis(100);

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

/// Что не так с адресом фильтра прокси.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressProblem {
    /// Ни IPv4, ни IPv6.
    Format,
}

impl AddressProblem {
    pub fn text(self) -> &'static str {
        match self {
            Self::Format => t!("errors.allow_from_invalid"),
        }
    }
}

/// Адрес, с которого прокси принимает входящие (FR-8).
///
/// Пустая строка — фильтра нет, `Ok(None)`: так поле и открывается у нового профиля.
/// Принимается IPv4 и IPv6 без порта: порт входящего выбирает клиент, и задавать его
/// в настройке бессмысленно. `::ffff:a.b.c.d` — тот же IPv4: двойной стек отдаёт
/// входящие IPv4 так, и фильтр в этой записи должен совпасть с IPv4-слушателем.
///
/// # Errors
///
/// Строка не разбирается как IP-адрес.
pub fn check_allow_from(text: &str) -> Result<Option<IpAddr>, AddressProblem> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<IpAddr>()
        .map(|ip| Some(unmapped_ip(ip)))
        .map_err(|_| AddressProblem::Format)
}

/// Что не так с адресом цели прокси: текст ошибки у поля (FR-5, FR-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetProblem {
    /// Порт вписан в адрес: для порта у формы своё поле.
    PortInside,
    /// Скобки не парой или не вокруг IPv6-адреса.
    Brackets,
    /// Зона адреса (`%eth0`): такую цель ядро не разрешит.
    Zone,
}

impl TargetProblem {
    fn text(self) -> &'static str {
        match self {
            Self::PortInside => t!("form.targetPortInside"),
            Self::Brackets => t!("form.targetBrackets"),
            Self::Zone => t!("form.targetZone"),
        }
    }
}

/// Что не так с непустым адресом цели прокси (FR-5, FR-8).
///
/// Двоеточие, которое не делает строку IPv6-литералом, означает порт внутри адреса: без
/// этой проверки цель стала бы `10.0.0.5:5000:5000`. Скобки допустимы только парой и
/// только вокруг IPv6-адреса — иначе адрес не разберётся как HOST:PORT: `[fe80::1`,
/// `fe80::1]`, `[[fe80::1]]` и `[10.0.0.5]` дают ошибку у поля, а не порт в адресе. Зона
/// (`fe80::1%eth0`) в строке адреса не разбирается вовсе, и к такой цели ядро не
/// подключилось бы.
pub(crate) fn target_host_problem(host: &str) -> Option<TargetProblem> {
    let host = host.trim();
    let address = match host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        Some(inner) => {
            // Скобки внутри скобок: `[[fe80::1]]`.
            if inner.contains(['[', ']']) {
                return Some(TargetProblem::Brackets);
            }
            inner
        }
        None => {
            // Скобка врозь: половина пары или лишняя.
            if host.contains('[') || host.contains(']') {
                return Some(TargetProblem::Brackets);
            }
            host
        }
    };
    if address.contains('%') {
        return Some(TargetProblem::Zone);
    }
    if address.parse::<Ipv6Addr>().is_ok() {
        return None;
    }
    if host != address {
        // Скобки были, а внутри не IPv6-литерал: `[10.0.0.5]`, `[lis.example]`.
        return Some(TargetProblem::Brackets);
    }
    host.contains(':').then_some(TargetProblem::PortInside)
}

/// Адрес без скобок: скобки допустимы только вокруг IPv6, а разбор адреса ждёт его как
/// есть.
pub(crate) fn unbracketed(host: &str) -> &str {
    let host = host.trim();
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
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
        // У прослушки сторон две, и обе — порты: анализатор и ЛИС (M5-AC11).
        SessionMode::ComMonitor => LastRun {
            mode: cfg.mode,
            port: cfg.serial.port.clone(),
            host: cfg.monitor_lis.port.clone(),
            baud: cfg.serial.baud,
        },
        // У прокси сторона две: порт прослушивания и цель — карточка показывает обе.
        // Пустая цель не показывается: карточка говорит только о порте.
        SessionMode::Proxy => LastRun {
            mode: cfg.mode,
            port: cfg.listen_port.to_string(),
            host: if cfg.proxy_host.trim().is_empty() || cfg.proxy_port == 0 {
                String::new()
            } else {
                format!("{}:{}", cfg.proxy_host, cfg.proxy_port)
            },
            baud: 0,
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
    /// Второй порт прослушки: своё поле — своя ошибка (M5-AC3).
    pub serial_b: bool,
    /// Порт цели прокси: своё поле — своя ошибка.
    pub target_port: bool,
    /// Фильтр по адресу: своё поле — своя ошибка.
    pub allow_from: bool,
}

/// Текст полей формы: то, что набрано, а не то, что уже принято в настройки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormState {
    pub port_text: String,
    pub connect_host: String,
    pub connect_port_text: String,
    /// Адрес цели прокси (FR-5): ЛИС в топологии «анализатор — клиент» и анализатор в
    /// топологии «анализатор — сервер».
    pub proxy_host: String,
    /// Порт цели прокси.
    pub proxy_port_text: String,
    /// Адрес, с которого прокси принимает входящие; пусто — с любого (FR-8).
    pub proxy_allow_from: String,
    /// Адреса этого компьютера: интерфейсы и loopback. По ним проверяется, что порт цели
    /// не указывает на собственный порт bytetap — иначе bytetap подключится сам к себе.
    pub own_addresses: Vec<String>,
    pub reconnect_text: String,
    pub baud_text: String,
    /// Скорость вне типового списка: поле ввода вместо списка.
    pub baud_other: bool,
    /// Скорость второго порта прослушки: у него своя (M5-В2).
    pub lis_baud_text: String,
    /// Скорость второго порта вне типового списка.
    pub lis_baud_other: bool,
    /// Второй порт ещё повторяет скорость и формат первого: правка первого переносится во
    /// второй, пока его самого не тронули (M5-В2).
    pub lis_follows: bool,
    /// Радио-паре топологии вернуть фокус на следующем кадре: стрелка переключает пару,
    /// но egui успевает увести фокус по своей навигации до того, как пара запрётся.
    pub radio_focus: Option<crate::settings::ProxyMode>,
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
            proxy_host: String::new(),
            proxy_port_text: String::new(),
            proxy_allow_from: String::new(),
            own_addresses: Vec::new(),
            reconnect_text: String::new(),
            baud_text: String::new(),
            baud_other: false,
            lis_baud_text: String::new(),
            lis_baud_other: false,
            lis_follows: true,
            radio_focus: None,
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
        self.proxy_host = cfg.proxy_host.clone();
        self.proxy_port_text = if cfg.proxy_port == 0 {
            // Порт цели не набран: поле пустое, подсказка в нём — пример, а не значение.
            String::new()
        } else {
            cfg.proxy_port.to_string()
        };
        self.proxy_allow_from = cfg.proxy_allow_from.clone();
        self.reconnect_text = cfg.reconnect_sec.to_string();
        self.baud_text = cfg.serial.baud.to_string();
        self.baud_other = !BAUD_RATES.contains(&cfg.serial.baud);
        self.lis_baud_text = cfg.monitor_lis.baud.to_string();
        self.lis_baud_other = !BAUD_RATES.contains(&cfg.monitor_lis.baud);
        // Второй порт повторяет первый, пока их скорость и формат совпадают: так форма
        // открывается у нового профиля и так же после правки профиля (M5-В2).
        self.lis_follows = cfg.monitor_lis.baud == cfg.serial.baud
            && serial_format(&cfg.monitor_lis) == serial_format(&cfg.serial);
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

    /// Скорость второго порта прослушки.
    fn lis_baud_value(&self, cfg: &AppConfig) -> Option<u32> {
        if self.lis_baud_other {
            check_baud(&self.lis_baud_text)
        } else {
            Some(cfg.monitor_lis.baud)
        }
    }

    /// Второй порт не выбран.
    fn lis_port_missing(cfg: &AppConfig) -> bool {
        cfg.monitor_lis.port.trim().is_empty()
    }

    /// Первый порт изменили: пока второй не трогали, он повторяет скорость и формат.
    pub fn analyzer_settings_changed(&mut self, cfg: &mut AppConfig) {
        if self.lis_follows {
            cfg.monitor_lis.baud = cfg.serial.baud;
            apply_format(&mut cfg.monitor_lis, &serial_format(&cfg.serial));
            self.lis_baud_text = cfg.monitor_lis.baud.to_string();
            self.lis_baud_other = !BAUD_RATES.contains(&cfg.monitor_lis.baud);
        }
    }

    /// Цель прокси ведёт на собственный порт bytetap: адрес второй стороны — этот же
    /// компьютер (loopback, все интерфейсы или свой адрес из списка), а порт совпадает с
    /// портом прослушивания. Тогда bytetap на каждое входящее соединение подключался б
    /// сам к себе, и пара замкнулась бы на себя.
    pub fn target_is_own_port(&self, cfg: &AppConfig) -> bool {
        let Ok(port) = check_port(&self.proxy_port_text) else {
            return false;
        };
        if cfg.mode != SessionMode::Proxy || port != cfg.listen_port {
            return false;
        }
        let host = self
            .proxy_host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        const LOOPBACK: [&str; 4] = ["0.0.0.0", "127.0.0.1", "localhost", "::1"];
        LOOPBACK.contains(&host.as_str())
            || self
                .own_addresses
                .iter()
                .any(|own| own.trim().eq_ignore_ascii_case(&host))
    }

    /// Что не так с адресом цели прокси (FR-5, FR-8).
    ///
    /// Пустой адрес просит его вписать; остальное — [`target_host_problem`].
    fn target_problem(&self, cfg: &AppConfig) -> Option<&'static str> {
        let host = self.proxy_host.trim();
        if host.is_empty() {
            return Some(match cfg.proxy_role {
                ProxyMode::AnalyzerClient => t!("form.proxyLisRequired"),
                ProxyMode::AnalyzerServer => t!("form.proxyAnalyzerRequired"),
            });
        }
        target_host_problem(host).map(TargetProblem::text)
    }

    /// Что не так с фильтром по адресу входящего (FR-8).
    ///
    /// Неразборчивый адрес просит адрес без порта; адрес другого семейства, чем
    /// слушатель, не совпадёт ни с одним входящим — входящее приходит с адресом того
    /// семейства, на котором слушает bytetap.
    fn allow_from_problem(&self, cfg: &AppConfig) -> Option<&'static str> {
        match check_allow_from(&self.proxy_allow_from) {
            Err(problem) => Some(problem.text()),
            Ok(Some(allowed))
                if Self::listener_addr(cfg)
                    .is_some_and(|listen| !peer_fits_listener(listen, allowed)) =>
            {
                Some(t!("errors.allow_from_family"))
            }
            _ => None,
        }
    }

    /// Слушатель прокси как адрес. Форма выбирает интерфейс из IPv4-адресов машины,
    /// поэтому `::` приходит только из файла настроек; пустой интерфейс — все адреса.
    fn listener_addr(cfg: &AppConfig) -> Option<SocketAddr> {
        let host = cfg.listen_host.trim();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Some(SocketAddr::from((Ipv4Addr::UNSPECIFIED, cfg.listen_port)));
        }
        host.parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, cfg.listen_port))
    }

    /// Своя проверка поля: текст ошибки или `None`.
    pub fn local_error(&self, cfg: &AppConfig, field: ErrorField) -> Option<&'static str> {
        let shown = self.attempted
            || match field {
                ErrorField::Port => self.touched.port,
                ErrorField::Address => self.touched.address,
                ErrorField::TargetPort => self.touched.target_port,
                ErrorField::AllowFrom => self.touched.allow_from,
                ErrorField::Serial => self.touched.serial,
                ErrorField::SerialA => self.touched.serial,
                ErrorField::SerialB => self.touched.serial_b,
                // Ошибка интерфейса приходит только от старта; своя проверка не нужна.
                ErrorField::Interface | ErrorField::LogDir | ErrorField::Start => false,
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
            ErrorField::Address => {
                if cfg.mode == SessionMode::Proxy {
                    self.target_problem(cfg)
                } else {
                    self.connect_host
                        .trim()
                        .is_empty()
                        .then(|| t!("form.addressRequired"))
                }
            }
            ErrorField::TargetPort => match check_port(&self.proxy_port_text) {
                Err(problem) => Some(problem.text()),
                Ok(_) => self
                    .target_is_own_port(cfg)
                    .then(|| t!("form.targetPortOwn")),
            },
            ErrorField::AllowFrom => self.allow_from_problem(cfg),
            ErrorField::Serial => {
                if Self::serial_port_missing(cfg) {
                    Some(t!("form.serialRequired"))
                } else if self.baud_value(cfg).is_none() {
                    Some(t!("form.baudRange"))
                } else {
                    None
                }
            }
            // Порт анализатора в прослушке: своё поле со своим текстом (M5-AC3).
            ErrorField::SerialA => {
                if Self::serial_port_missing(cfg) {
                    Some(t!("form.serialRequired"))
                } else if self.baud_value(cfg).is_none() {
                    Some(t!("form.baudRange"))
                } else {
                    None
                }
            }
            // Порт ЛИС: не выбран, повторяет первый порт или скорость вне границ.
            ErrorField::SerialB => {
                let port = cfg.monitor_lis.port.trim();
                if port.is_empty() {
                    Some(t!("form.serialLisRequired"))
                } else if monitor_same_port(cfg) {
                    // Порт уже виден в самом поле: текст говорит, что делать.
                    Some(t!("form.monitorSamePort"))
                } else if self.lis_baud_value(cfg).is_none() {
                    Some(t!("form.baudRange"))
                } else {
                    None
                }
            }
            ErrorField::Interface | ErrorField::LogDir | ErrorField::Start => None,
        }
    }

    /// Поля режима проверены: можно собирать конфигурацию.
    pub fn valid(&self, cfg: &AppConfig) -> bool {
        match cfg.mode {
            SessionMode::TcpServer => check_port(&self.port_text).is_ok(),
            SessionMode::TcpClient => {
                check_port(&self.connect_port_text).is_ok() && !self.connect_host.trim().is_empty()
            }
            SessionMode::Proxy => {
                check_port(&self.port_text).is_ok()
                    && check_port(&self.proxy_port_text).is_ok()
                    && self.target_problem(cfg).is_none()
                    && self.allow_from_problem(cfg).is_none()
                    && !self.target_is_own_port(cfg)
            }
            SessionMode::Serial => {
                !Self::serial_port_missing(cfg) && self.baud_value(cfg).is_some()
            }
            // Прослушка: два порта, у каждого своя скорость, и порты должны быть разными
            // (M5-AC3).
            SessionMode::ComMonitor => {
                !Self::serial_port_missing(cfg)
                    && self.baud_value(cfg).is_some()
                    && !Self::lis_port_missing(cfg)
                    && self.lis_baud_value(cfg).is_some()
                    && !monitor_same_port(cfg)
            }
        }
    }

    /// Набранное уходит в настройки: порты, адреса, скорость, переподключение.
    pub fn commit(&self, cfg: &mut AppConfig) {
        if let Ok(port) = check_port(&self.port_text) {
            cfg.listen_port = port;
        }
        if let Ok(port) = check_port(&self.connect_port_text) {
            cfg.connect_port = port;
        }
        if let Ok(port) = check_port(&self.proxy_port_text) {
            cfg.proxy_port = port;
        }
        cfg.connect_host = self.connect_host.trim().to_owned();
        cfg.proxy_host = self.proxy_host.trim().to_owned();
        cfg.proxy_allow_from = self.proxy_allow_from.trim().to_owned();
        cfg.reconnect_sec = parse_reconnect(&self.reconnect_text);
        if let Some(baud) = self.baud_value(cfg) {
            cfg.serial.baud = baud;
        }
        if let Some(baud) = self.lis_baud_value(cfg) {
            cfg.monitor_lis.baud = baud;
        }
    }
}

/// Один и тот же порт с двух сторон прослушки: в разрыве нужны два разных (M5-AC3).
///
/// Имена COM сравниваются без регистра, а пробелы по краям не порт: `COM3` и `com3` —
/// один порт, и второй его открыть не даст.
pub fn monitor_same_port(cfg: &AppConfig) -> bool {
    let analyzer = cfg.serial.port.trim();
    !analyzer.is_empty() && analyzer.eq_ignore_ascii_case(cfg.monitor_lis.port.trim())
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

/// Топология прокси в виде роли ядра: экран и журнал называют её одним словом.
fn proxy_role(role: ProxyMode) -> ProxyRole {
    match role {
        ProxyMode::AnalyzerClient => ProxyRole::AnalyzerClient,
        ProxyMode::AnalyzerServer => ProxyRole::AnalyzerServer,
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

/// Ключ цели прокси «адрес:порт»: пустой адрес и ненабранный порт ключа не дают.
///
/// По ключу адреса из записи и из фоновой пробы привязаны к цели: к другой цели они не
/// относятся (FR-8).
pub fn target_key(host: &str, port: u16) -> Option<String> {
    let host = host.trim();
    (!host.is_empty() && port != 0).then(|| format!("{host}:{port}"))
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

/// Настройки COM-порта ядра со стороной прослушки в ошибке (M5-AC3).
///
/// Сторона уходит подстановкой ошибки старта: интерфейс по ней выбирает поле — «Порт
/// анализатора» или «Порт ЛИС».
fn serial_settings_side(serial: &SerialSettings, side: &str) -> Result<CoreSerial, StartError> {
    serial_settings(serial).map_err(|error| error.with_param("side", side))
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
        // Прослушка — пятый источник (FR-10): два порта, байты идут без изменений.
        // Скорость и формат у портов свои, поток и линии общие — из блока анализатора
        // (M5-В2). Автоответчика здесь нет, и `with_responder` на прослушку не действует.
        SessionMode::ComMonitor => {
            if cfg.reconnect_sec > MAX_RECONNECT_SEC {
                return Err(StartError::new(
                    ErrorCode::Io,
                    format!(
                        "reconnect interval {} s is over {MAX_RECONNECT_SEC} s",
                        cfg.reconnect_sec
                    ),
                ));
            }
            if monitor_same_port(cfg) {
                return Err(StartError::new(
                    ErrorCode::MonitorSamePort,
                    "the monitor needs two ports",
                )
                .with_param("port", cfg.monitor_lis.port.trim())
                .with_param("side", "lis"));
            }
            let lis = SerialSettings {
                flow: cfg.serial.flow,
                dtr: cfg.serial.dtr,
                rts: cfg.serial.rts,
                ..cfg.monitor_lis.clone()
            };
            SessionConfig::com_monitor(
                serial_settings_side(&cfg.serial, "analyzer")?,
                serial_settings_side(&lis, "lis")?,
                Duration::from_secs(u64::from(cfg.reconnect_sec)),
            )
        }
        // Прокси — четвёртый источник (FR-5): слушать одну сторону и подключаться к
        // другой. Автоответчика в нём нет, `with_responder` ниже на прокси не действует.
        SessionMode::Proxy => {
            // Форма не пустит «Старт» с неразборчивым адресом; здесь — тот же разбор
            // для настроек, пришедших из файла.
            // Отказ фильтра встаёт под своим полем, а не под адресом цели (FR-8).
            let allow_from = check_allow_from(&cfg.proxy_allow_from).map_err(|_| {
                StartError::new(
                    ErrorCode::AllowFromInvalid,
                    "the peer filter is not an IP address without a port",
                )
            })?;
            SessionConfig::proxy(
                listen_addr(&cfg.listen_host, cfg.listen_port)?,
                connect_addr(&cfg.proxy_host, cfg.proxy_port)?,
                proxy_role(cfg.proxy_role),
                PROXY_RETRY,
            )
            .with_allow_from(allow_from)
        }
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
        assert_eq!(error.field(cfg.mode), ErrorField::Interface);
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

    /// Прокси: поля обеих сторон проверяются до старта; сборка `SessionConfig` несёт
    /// топологию, цель и паузу источника (M4-AC1, M4-AC2, M4-AC13).
    #[test]
    fn the_proxy_config_listens_and_connects_by_topology() {
        let mut cfg = config(SessionMode::Proxy);
        cfg.listen_host = "192.168.1.10".into();
        cfg.listen_port = 50025;
        cfg.proxy_host = " 10.0.0.5 ".into();
        cfg.proxy_port = 5000;
        cfg.analyzer = "A-1".into();
        let core = session_config(&cfg, None).expect("прокси");
        assert_eq!(
            core.source,
            Source::Proxy {
                listen: "192.168.1.10:50025".parse().unwrap(),
                target: "10.0.0.5:5000".into(),
                role: ProxyRole::AnalyzerClient,
                retry: PROXY_RETRY,
                allow_from: None,
            }
        );
        // Автоответчика в прокси нет всегда, какой бы профиль ни стоял в настройках:
        // в заголовок журнала идёт «без ответов», автомат не создаётся (FR-6).
        assert_eq!(core.header_responder(), Autoresponder::None);
        assert_eq!(core.analyzer.as_deref(), Some("A-1"));

        cfg.proxy_role = ProxyMode::AnalyzerServer;
        cfg.proxy_host = "192.168.1.51".into();
        cfg.listen_host = String::new();
        let core = session_config(&cfg, None).expect("анализатор — сервер");
        assert_eq!(
            core.source,
            Source::Proxy {
                listen: "0.0.0.0:50025".parse().unwrap(),
                target: "192.168.1.51:5000".into(),
                role: ProxyRole::AnalyzerServer,
                retry: PROXY_RETRY,
                allow_from: None,
            }
        );

        // Имя цели разрешает ядро при каждой попытке: форма отдаёт его как есть.
        cfg.proxy_host = "lis.example".into();
        let core = session_config(&cfg, None).expect("имя цели");
        assert!(
            matches!(&core.source, Source::Proxy { target, .. } if target == "lis.example:5000")
        );

        // IPv6 в цели — в скобках, иначе адрес не разберётся как HOST:PORT.
        cfg.proxy_host = "fe80::1".into();
        let core = session_config(&cfg, None).expect("IPv6");
        assert!(matches!(&core.source, Source::Proxy { target, .. } if target == "[fe80::1]:5000"));

        cfg.proxy_host = "10.0.0 .5".into();
        let error = session_config(&cfg, None).expect_err("пробел в адресе");
        assert_eq!(error.code, ErrorCode::AddressInvalid);
        assert_eq!(error.field(cfg.mode), ErrorField::Address);
    }

    /// Фильтр по адресу (FR-8): пусто — принимать с любого, неразборчивый адрес — своя
    /// ошибка у своего поля, а не у цели или интерфейса.
    #[test]
    fn the_peer_filter_takes_one_ip_address() {
        let mut cfg = config(SessionMode::Proxy);
        cfg.proxy_host = "10.0.0.5".into();
        cfg.proxy_port = 5000;
        let mut form = FormState::new(&cfg);
        // Пустое поле — фильтра нет, «Старт» не ждёт адреса.
        assert_eq!(check_allow_from(""), Ok(None));
        assert_eq!(check_allow_from("  "), Ok(None));
        assert!(form.valid(&cfg));
        assert_eq!(form.local_error(&cfg, ErrorField::AllowFrom), None);

        // Порт в фильтре не принимается: порт входящего выбирает клиент.
        assert_eq!(
            form.local_error(&cfg, ErrorField::AllowFrom),
            None,
            "ошибка молчит, пока поле не трогали"
        );
        form.proxy_allow_from = "192.168.1.51:5000".into();
        form.touched.allow_from = true;
        assert_eq!(
            form.local_error(&cfg, ErrorField::AllowFrom),
            Some("Адрес — IPv4 или IPv6 без порта, например 192.168.1.51.")
        );
        assert!(
            !form.valid(&cfg),
            "с таким адресом «Старт» не собирает сессию"
        );

        form.proxy_allow_from = " 192.168.1.51 ".into();
        assert_eq!(form.local_error(&cfg, ErrorField::AllowFrom), None);
        assert!(form.valid(&cfg));

        // Найдено в настройки и в конфигурацию ядра — тем же адресом.
        form.commit(&mut cfg);
        assert_eq!(cfg.proxy_allow_from, "192.168.1.51");
        let core = session_config(&cfg, None).expect("прокси с фильтром");
        assert_eq!(
            core.source,
            Source::Proxy {
                listen: "0.0.0.0:5000".parse().unwrap(),
                target: "10.0.0.5:5000".into(),
                role: ProxyRole::AnalyzerClient,
                retry: PROXY_RETRY,
                allow_from: Some("192.168.1.51".parse().unwrap()),
            }
        );

        // IPv6 годится как есть, без скобок: это адрес, а не HOST:PORT.
        form.proxy_allow_from = "fe80::1".into();
        form.commit(&mut cfg);
        let core = session_config(&cfg, None).expect("прокси с IPv6");
        assert!(matches!(
            &core.source,
            Source::Proxy {
                allow_from: Some(IpAddr::V6(_)),
                ..
            }
        ));
    }

    /// Поля прокси: порт прослушивания, адрес цели и порт цели — каждое со своей
    /// ошибкой, и ни одно не краснеет до ввода или «Старт» (M4-AC13).
    #[test]
    fn the_proxy_fields_are_checked_one_by_one() {
        let mut cfg = config(SessionMode::Proxy);
        let mut form = FormState::new(&cfg);
        form.port_text = "0".into();
        form.proxy_host.clear();
        form.proxy_port_text = "70000".into();
        assert!(!form.valid(&cfg));
        assert_eq!(form.local_error(&cfg, ErrorField::Port), None);
        assert_eq!(form.local_error(&cfg, ErrorField::Address), None);
        assert_eq!(form.local_error(&cfg, ErrorField::TargetPort), None);

        form.touched.port = true;
        form.touched.target_port = true;
        assert_eq!(
            form.local_error(&cfg, ErrorField::Port),
            Some("Порт — целое число от 1 до 65535.")
        );
        assert_eq!(
            form.local_error(&cfg, ErrorField::TargetPort),
            Some("Порт — целое число от 1 до 65535.")
        );
        assert_eq!(
            form.local_error(&cfg, ErrorField::Address),
            None,
            "адрес краснеет только после «Старт»"
        );

        form.attempted = true;
        assert_eq!(
            form.local_error(&cfg, ErrorField::Address),
            Some("Укажите адрес ЛИС")
        );
        cfg.proxy_role = ProxyMode::AnalyzerServer;
        assert_eq!(
            form.local_error(&cfg, ErrorField::Address),
            Some("Укажите адрес анализатора")
        );

        form.proxy_host = "192.168.1.51".into();
        form.proxy_port_text = "5000".into();
        form.port_text = "50025".into();
        assert!(form.valid(&cfg));
        form.commit(&mut cfg);
        assert_eq!(cfg.proxy_host, "192.168.1.51");
        assert_eq!(cfg.proxy_port, 5000);
        assert_eq!(cfg.listen_port, 50025);
    }

    /// Порт в адресе цели — своя ошибка у поля адреса: для порта есть отдельное поле, а
    /// цель `10.0.0.5:5000:5000` ядро не разобрало бы (M4-AC13).
    #[test]
    fn a_port_inside_the_target_address_is_refused() {
        let cfg = config(SessionMode::Proxy);
        let mut form = FormState::new(&cfg);
        form.proxy_port_text = "5000".into();
        form.attempted = true;
        form.proxy_host = "10.0.0.5:5000".into();
        assert_eq!(
            form.local_error(&cfg, ErrorField::Address),
            Some(
                "Порт — в отдельном поле: в адресе цели только адрес, например 10.0.0.5, а порт впишите в «Порт цели»."
            )
        );
        assert!(!form.valid(&cfg), "с портом в адресе сессия не собирается");
        // Имя узла с двоеточием — тоже порт в адресе.
        form.proxy_host = "lis.example:5000".into();
        assert!(form.local_error(&cfg, ErrorField::Address).is_some());

        // IPv6-литерал — адрес, а не порт: со скобками и без.
        form.proxy_host = "fe80::1".into();
        assert_eq!(form.local_error(&cfg, ErrorField::Address), None);
        assert!(form.valid(&cfg));
        form.proxy_host = "[fe80::1]".into();
        assert_eq!(form.local_error(&cfg, ErrorField::Address), None);
        assert!(form.valid(&cfg));
    }

    /// Скобки в адресе цели — только парой и только вокруг IPv6: половина пары, лишняя
    /// пара и скобки вокруг IPv4 дают ошибку у поля, а не «порт в адресе» (M4-AC13).
    #[test]
    fn odd_brackets_and_a_zone_are_refused_with_their_own_text() {
        let mut cfg = config(SessionMode::Proxy);
        let mut form = FormState::new(&cfg);
        form.proxy_port_text = "5000".into();
        form.attempted = true;
        let brackets = "Скобки — только парой и только вокруг IPv6-адреса, например [fe80::1]. Впишите адрес без лишних скобок: 10.0.0.5.";
        for host in [
            "[fe80::1",
            "fe80::1]",
            "[[fe80::1]]",
            "[10.0.0.5]",
            "fe80::1[",
            "]fe80::1[",
        ] {
            form.proxy_host = host.into();
            assert_eq!(
                form.local_error(&cfg, ErrorField::Address),
                Some(brackets),
                "{host}"
            );
            assert!(!form.valid(&cfg), "{host}: сессия не собирается");
        }

        // Зона адреса: ядро такую цель не разрешит, и текст должен говорить о зоне, а не
        // о порте в адресе.
        let zone = "Зона адреса (например, %eth0) не поддерживается: впишите адрес без зоны.";
        for host in ["fe80::1%eth0", "[fe80::1%eth0]"] {
            form.proxy_host = host.into();
            assert_eq!(
                form.local_error(&cfg, ErrorField::Address),
                Some(zone),
                "{host}"
            );
        }

        // Порт в адресе остаётся портом в адресе.
        form.proxy_host = "10.0.0.5:5000".into();
        assert_eq!(
            form.local_error(&cfg, ErrorField::Address),
            Some(
                "Порт — в отдельном поле: в адресе цели только адрес, например 10.0.0.5, а порт впишите в «Порт цели»."
            )
        );

        // Пара вокруг IPv6 доходит до ядра как HOST:PORT.
        form.proxy_host = "[fe80::1]".into();
        form.commit(&mut cfg);
        let core = session_config(&cfg, None).expect("IPv6 в скобках");
        assert!(
            matches!(&core.source, Source::Proxy { target, .. } if target == "[fe80::1]:5000"),
            "{:?}",
            core.source
        );
        assert_eq!(unbracketed("[fe80::1]"), "fe80::1", "проба идёт без скобок");
        assert_eq!(unbracketed(" 10.0.0.5 "), "10.0.0.5");
    }

    /// IPv6-адрес в фильтре при IPv4-слушателе не пропустит никого: входящее приходит с
    /// адресом того семейства, на котором слушает bytetap (FR-8).
    #[test]
    fn an_ipv6_filter_under_an_ipv4_listener_is_an_error() {
        let mut cfg = config(SessionMode::Proxy);
        cfg.proxy_host = "10.0.0.5".into();
        cfg.proxy_port = 5000;
        let mut form = FormState::new(&cfg);
        form.attempted = true;
        form.proxy_allow_from = "fe80::1".into();
        assert_eq!(
            form.local_error(&cfg, ErrorField::AllowFrom),
            Some(
                "Адрес другого семейства, чем слушатель bytetap: такое входящее не совпадёт ни с кем. Слушатель IPv4 — впишите IPv4 (192.168.1.51), слушатель IPv6 — IPv6."
            )
        );
        assert!(!form.valid(&cfg), "с таким фильтром сессия не собирается");

        // IPv4 в фильтре при том же слушателе — норма.
        form.proxy_allow_from = "192.168.1.51".into();
        assert_eq!(form.local_error(&cfg, ErrorField::AllowFrom), None);
        assert!(form.valid(&cfg));

        // `::ffff:a.b.c.d` — тот же IPv4: двойной стек отдаёт так входящие IPv4.
        form.proxy_allow_from = "::ffff:192.168.1.51".into();
        assert_eq!(form.local_error(&cfg, ErrorField::AllowFrom), None);
        form.commit(&mut cfg);
        let core = session_config(&cfg, None).expect("прокси с mapped-фильтром");
        assert!(matches!(
            &core.source,
            Source::Proxy {
                allow_from: Some(IpAddr::V4(addr)),
                ..
            } if *addr == Ipv4Addr::new(192, 168, 1, 51)
        ));

        // IPv6-слушатель из файла настроек: семейства совпадают.
        form.proxy_allow_from = "fe80::1".into();
        let mut ipv6 = cfg.clone();
        ipv6.listen_host = "::".into();
        assert_eq!(form.local_error(&ipv6, ErrorField::AllowFrom), None);
        assert!(form.valid(&ipv6));
        // Двойной стек принимает и IPv4 — фильтр IPv4 под `[::]` тоже годится.
        form.proxy_allow_from = "192.168.1.51".into();
        assert_eq!(form.local_error(&ipv6, ErrorField::AllowFrom), None);
    }

    /// Цель прокси ведёт на собственный порт bytetap: адрес второй стороны — этот же
    /// компьютер, и пара замкнулась бы на себя (M4-AC13).
    #[test]
    fn a_target_on_the_own_listen_port_is_an_error() {
        let mut cfg = config(SessionMode::Proxy);
        cfg.listen_port = 50025;
        let mut form = FormState::new(&cfg);
        form.own_addresses = vec!["192.168.1.10".into()];
        form.attempted = true;
        for host in ["127.0.0.1", "localhost", "0.0.0.0", "::1", "192.168.1.10"] {
            form.proxy_host = host.into();
            form.proxy_port_text = "50025".into();
            assert!(
                !form.valid(&cfg),
                "{host}:50025 — собственный порт прослушивания"
            );
            assert_eq!(
                form.local_error(&cfg, ErrorField::TargetPort),
                Some(
                    "Порт цели совпадает с портом bytetap на этом же адресе: bytetap подключится сам к себе. Укажите порт второй стороны."
                ),
                "{host}"
            );
        }

        // Чужой порт или чужая машина — не петля.
        form.proxy_port_text = "5000".into();
        assert!(form.valid(&cfg), "свой адрес, но порт второй стороны");
        form.proxy_port_text = "50025".into();
        form.proxy_host = "192.168.1.51".into();
        assert!(form.valid(&cfg), "другая машина, тот же номер порта");
        assert_eq!(form.local_error(&cfg, ErrorField::TargetPort), None);
        // Пустой адрес — про него своя ошибка у поля «Адрес ЛИС», а не про петлю.
        form.proxy_host.clear();
        assert!(!form.target_is_own_port(&cfg));
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

    /// Прослушка: два порта, у каждого своя скорость и формат, а поток и линии общие —
    /// из блока анализатора (M5-AC3, М5-В2).
    #[test]
    fn the_monitor_config_carries_both_ports() {
        let mut cfg = config(SessionMode::ComMonitor);
        cfg.serial.port = "COM3".into();
        cfg.serial.baud = 19200;
        apply_format(&mut cfg.serial, "7E2");
        cfg.monitor_lis.port = "COM4".into();
        cfg.monitor_lis.baud = 9600;
        apply_format(&mut cfg.monitor_lis, "8N1");
        cfg.serial.flow = Flow::Rtscts;
        cfg.serial.dtr = false;
        cfg.serial.rts = true;
        cfg.reconnect_sec = 7;
        let core = session_config(&cfg, None).expect("прослушка");
        let Source::ComMonitor {
            analyzer,
            lis,
            retry,
        } = &core.source
        else {
            panic!("ожидалась прослушка, а не {:?}", core.source);
        };
        assert_eq!(analyzer.port, "COM3");
        assert_eq!(analyzer.baud, 19200);
        assert_eq!(analyzer.format.to_string(), "7E2");
        assert_eq!(lis.port, "COM4");
        assert_eq!(lis.baud, 9600, "у ЛИС своя скорость");
        assert_eq!(lis.format.to_string(), "8N1", "у ЛИС свой формат");
        // Поток и линии в разрыве общие: у второго порта те же значения (М5-В2).
        assert_eq!(lis.flow, core_serial::Flow::RtsCts);
        assert!(!lis.dtr);
        assert!(lis.rts);
        assert_eq!(*retry, Duration::from_secs(7));
        // Автоответчика в прослушке нет, какой бы профиль ни стоял в настройках
        // (FR-10, M5-AC4).
        assert_eq!(core.header_responder(), Autoresponder::None);
    }

    /// Два порта прослушки: пустой порт — ошибка у своего поля, одинаковые порты —
    /// у «Порт ЛИС», ошибка ядра — под полем отказавшей стороны (M5-AC3).
    #[test]
    fn the_monitor_needs_two_different_ports() {
        let mut cfg = config(SessionMode::ComMonitor);
        let mut form = FormState::new(&cfg);
        form.attempted = true;
        assert!(!form.valid(&cfg));
        assert_eq!(
            form.local_error(&cfg, ErrorField::SerialA),
            Some("Выберите порт")
        );
        assert_eq!(
            form.local_error(&cfg, ErrorField::SerialB),
            Some("Выберите порт ЛИС")
        );

        cfg.serial.port = "COM3".into();
        cfg.monitor_lis.port = "com3".into();
        assert!(!form.valid(&cfg), "регистр в имени COM не значит ничего");
        assert_eq!(
            form.local_error(&cfg, ErrorField::SerialB),
            Some(
                "Этот порт указан и у анализатора. В прослушке нужны два разных порта: анализатор на одном, ЛИС на другом."
            )
        );

        cfg.monitor_lis.port = "COM4".into();
        assert!(form.valid(&cfg));
        form.commit(&mut cfg);
        let core = session_config(&cfg, None).expect("прослушка");
        assert!(matches!(
            &core.source,
            Source::ComMonitor { analyzer, lis, .. }
                if analyzer.port == "COM3" && lis.port == "COM4"
        ));

        // Ядро тоже отвергает одинаковые порты, и поле — у второго.
        let mut same = cfg.clone();
        same.monitor_lis.port = "COM3".into();
        let error = session_config(&same, None).expect_err("один порт с двух сторон");
        assert_eq!(error.code, ErrorCode::MonitorSamePort);
        assert_eq!(error.field(SessionMode::ComMonitor), ErrorField::SerialB);

        // Порт ЛИС не выбран — настройка порта, и поле тоже своё.
        let mut missing = cfg.clone();
        missing.monitor_lis.port = String::new();
        let error = session_config(&missing, None).expect_err("порт ЛИС не выбран");
        assert_eq!(error.code, ErrorCode::PortSettings);
        assert_eq!(error.field(SessionMode::ComMonitor), ErrorField::SerialB);

        // Порт анализатора не выбран — под первым полем.
        let mut missing = cfg.clone();
        missing.serial.port = String::new();
        let error = session_config(&missing, None).expect_err("порт анализатора не выбран");
        assert_eq!(error.code, ErrorCode::PortSettings);
        assert_eq!(error.field(SessionMode::ComMonitor), ErrorField::SerialA);
    }

    /// Второй порт прослушки повторяет скорость и формат первого, пока его самого не
    /// тронули; профиль с одинаковыми значениями включает повтор снова (М5-В2).
    #[test]
    fn the_second_port_repeats_the_first_until_it_is_changed() {
        let cfg = config(SessionMode::ComMonitor);
        let mut form = FormState::new(&cfg);
        assert!(form.lis_follows, "новый профиль повторяет первый порт");

        let mut changed = cfg.clone();
        changed.serial.baud = 19200;
        apply_format(&mut changed.serial, "7E2");
        form.analyzer_settings_changed(&mut changed);
        assert_eq!(changed.monitor_lis.baud, 19200);
        assert_eq!(serial_format(&changed.monitor_lis), "7E2");
        assert_eq!(form.lis_baud_text, "19200");

        // Скорость второго порта поменяли — дальше он живёт своей.
        form.lis_follows = false;
        changed.serial.baud = 4800;
        form.analyzer_settings_changed(&mut changed);
        assert_eq!(changed.monitor_lis.baud, 19200, "у ЛИС своя скорость");

        // Профиль вернул одинаковые значения — повтор включается снова.
        let mut same = changed.clone();
        same.monitor_lis.baud = same.serial.baud;
        apply_format(&mut same.monitor_lis, &serial_format(&same.serial));
        assert!(FormState::new(&same).lis_follows);
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
        // Прослушка: карточка показывает оба порта — анализатор и ЛИС (M5-AC11).
        cfg.mode = SessionMode::ComMonitor;
        cfg.monitor_lis.port = "COM4".into();
        let run = last_run_of(&cfg);
        assert_eq!(
            (run.port.as_str(), run.host.as_str()),
            ("COM3", "COM4"),
            "порт анализатора и порт ЛИС"
        );
        cfg.mode = SessionMode::TcpServer;
        assert_eq!(last_run_of(&cfg).port, "5000");
        // Прокси: порт прослушивания и цель — на карточке обе стороны.
        cfg.mode = SessionMode::Proxy;
        cfg.listen_port = 50025;
        cfg.proxy_host = "10.0.0.5".into();
        cfg.proxy_port = 5000;
        let run = last_run_of(&cfg);
        assert_eq!(
            (run.port.as_str(), run.host.as_str()),
            ("50025", "10.0.0.5:5000")
        );
    }
}
