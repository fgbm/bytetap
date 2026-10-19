//! COM-порт вместо ЛИС (FR-9, FR-11, FR-12).
//!
//! Крейт `serialport` даёт открытие, настройки и перечисление портов, но интерфейс у него
//! блокирующий. Поэтому вокруг порта живут два потока: читатель с коротким таймаутом и
//! писатель со своей очередью команд (`try_clone`). Чтение не задерживает ответ, а ответ
//! не задерживает чтение; `flush`/`tcdrain` после записи не вызываются — они ждут, пока
//! драйвер вытолкнет байты в линию. Асинхронная сторона — [`SerialConnection`]:
//! [`tokio::io::AsyncRead`] отдаёт прочитанные фрагменты, а [`tokio::io::AsyncWrite`]
//! возвращает фактический результат записи в порт. Решение и его обоснование — ADR 0002.
//!
//! Соединение — одно открытие порта. Ошибка чтения или записи закрывает соединение:
//! сессия пишет `close` и `error`, а источник переоткрывает порт с интервалом (NFR-5).
//! Переоткрытие ждёт закрытия прежнего дескриптора асинхронно: `Drop` не соединяет потоки,
//! иначе зависшая в драйвере запись держала бы «Стоп».
//!
//! Чётности `mark`/`space` в `serialport` нет: после открытия они выставляются напрямую —
//! `CMSPAR` в termios на Linux и `MARKPARITY`/`SPACEPARITY` в DCB на Windows. На pty
//! управляющих линий нет, поэтому `ENOTTY` от DTR/RTS не считается ошибкой (M3-AC11).

use std::collections::HashMap;
use std::fmt;
use std::future::Future as _;
use std::io;
use std::io::Read as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc as sync_mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serialport::SerialPort as _;
use serialport::{DataBits, FlowControl, StopBits};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

/// Наименьшая допустимая скорость (FR-11, M3-AC3).
pub const MIN_BAUD: u32 = 1;

/// Наибольшая допустимая скорость (FR-11, M3-AC3).
pub const MAX_BAUD: u32 = 4_000_000;

/// Сколько ждёт поток порта одного чтения, прежде чем проверить остановку.
///
/// Меньше — чаще просыпается и быстрее останавливается, больше — меньше нагрузки.
/// Пятьдесят миллисекунд незаметны для протокола и не мешают переоткрыть порт.
const READ_POLL: Duration = Duration::from_millis(50);

/// Сколько фрагментов держит очередь от порта к сессии.
///
/// Сессия читает сразу, очередь нужна только на разнице планировщиков. Полная очередь
/// не теряет данные: поток ждёт места и проверяет остановку.
const READ_QUEUE: usize = 64;

/// Сколько ждёт одна запись в порт, прежде чем ответ считается неотправленным (FR-9).
///
/// Драйвер может не принять байты сразу — `Ok(0)` или таймаут порта не значат потерю:
/// запись повторяется, пока не примут или не выйдет этот срок. Дольше ждать незачем:
/// анализатор к тому времени уже не дождётся `ACK`, и честнее закрыть соединение.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Пауза перед повтором записи, которую порт не принял: цикл не крутится вхолостую.
const WRITE_PAUSE: Duration = Duration::from_millis(5);

/// Сколько ждёт поток места в полной очереди к сессии, прежде чем проверить остановку.
const QUEUE_PAUSE: Duration = Duration::from_millis(2);

/// Чётность (FR-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Parity {
    /// Без бита чётности.
    None,
    /// Чётная.
    Even,
    /// Нечётная.
    Odd,
    /// Бит чётности всегда 1.
    Mark,
    /// Бит чётности всегда 0.
    Space,
}

impl Parity {
    /// Значение в `settings.parity` и в списке CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Even => "even",
            Self::Odd => "odd",
            Self::Mark => "mark",
            Self::Space => "space",
        }
    }

    /// Буква в записи `8N1` (FR-11, M3-AC3).
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::None => 'N',
            Self::Even => 'E',
            Self::Odd => 'O',
            Self::Mark => 'M',
            Self::Space => 'S',
        }
    }

    /// Чётность по букве записи `8N1`; регистр не важен.
    #[must_use]
    pub fn from_letter(letter: char) -> Option<Self> {
        match letter.to_ascii_uppercase() {
            'N' => Some(Self::None),
            'E' => Some(Self::Even),
            'O' => Some(Self::Odd),
            'M' => Some(Self::Mark),
            'S' => Some(Self::Space),
            _ => None,
        }
    }
}

impl fmt::Display for Parity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Управление потоком (FR-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flow {
    /// Нет.
    None,
    /// Аппаратное: RTS/CTS.
    RtsCts,
    /// Программное: XON/XOFF.
    XonXoff,
}

impl Flow {
    /// Значение в `settings.flow` и в CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RtsCts => "rtscts",
            Self::XonXoff => "xonxoff",
        }
    }

    /// Управление потоком по имени из CLI.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "rtscts" => Some(Self::RtsCts),
            "xonxoff" => Some(Self::XonXoff),
            _ => None,
        }
    }

    fn serialport(self) -> FlowControl {
        match self {
            Self::None => FlowControl::None,
            Self::RtsCts => FlowControl::Hardware,
            Self::XonXoff => FlowControl::Software,
        }
    }
}

impl std::str::FromStr for Flow {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
            .ok_or_else(|| format!("`{text}` — не управление потоком: none, rtscts или xonxoff"))
    }
}

impl fmt::Display for Flow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Скорость, данные, чётность и стоп-биты в записи `<биты><чётность><стоп>` (FR-11).
///
/// `8N1` — восемь бит данных, без чётности, один стоп-бит; `7E2` и `8M1` — то же
/// в разборе и обратной записи (M3-AC3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SerialFormat {
    /// Биты данных: 7 или 8.
    pub data_bits: u8,
    /// Чётность.
    pub parity: Parity,
    /// Стоп-биты: 1 или 2.
    pub stop_bits: u8,
}

impl SerialFormat {
    /// Восемь бит данных, без чётности, один стоп-бит.
    pub const EIGHT_N_ONE: Self = Self {
        data_bits: 8,
        parity: Parity::None,
        stop_bits: 1,
    };

    /// Разбирает запись вида `8N1`, `7E2`, `8M1`.
    ///
    /// # Errors
    ///
    /// Не три знака, биты данных не 7/8, неизвестная чётность, стоп-биты не 1/2.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let mut chars = text.chars();
        let (Some(data), Some(parity), Some(stop), None) =
            (chars.next(), chars.next(), chars.next(), chars.next())
        else {
            return Err(format!(
                "`{text}` — не формат порта: нужны три знака, например `8N1`"
            ));
        };
        let data_bits = match data {
            '7' => 7,
            '8' => 8,
            other => {
                return Err(format!(
                    "`{other}` в формате `{text}`: битов данных может быть 7 или 8"
                ));
            }
        };
        let Some(parity) = Parity::from_letter(parity) else {
            return Err(format!(
                "`{parity}` в формате `{text}`: чётность — N (нет), E (чётная), O (нечётная), M (mark) или S (space)"
            ));
        };
        let stop_bits = match stop {
            '1' => 1,
            '2' => 2,
            other => {
                return Err(format!(
                    "`{other}` в формате `{text}`: стоп-битов может быть 1 или 2"
                ));
            }
        };
        Ok(Self {
            data_bits,
            parity,
            stop_bits,
        })
    }

    fn data_bits_sp(self) -> DataBits {
        match self.data_bits {
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        }
    }

    fn stop_bits_sp(self) -> StopBits {
        match self.stop_bits {
            2 => StopBits::Two,
            _ => StopBits::One,
        }
    }
}

impl std::str::FromStr for SerialFormat {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for SerialFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.data_bits,
            self.parity.letter(),
            self.stop_bits
        )
    }
}

/// Настройки COM-порта (FR-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialSettings {
    /// Имя порта: `COM5` или `/dev/ttyUSB0`.
    pub port: String,
    /// Скорость: 1…4 000 000; типовые 300–115200.
    pub baud: u32,
    /// Биты данных, чётность и стоп-биты.
    pub format: SerialFormat,
    /// Управление потоком.
    pub flow: Flow,
    /// Начальное состояние DTR.
    pub dtr: bool,
    /// Начальное состояние RTS.
    pub rts: bool,
}

impl SerialSettings {
    /// Настройки порта: `9600 8N1`, без управления потоком, DTR и RTS подняты.
    #[must_use]
    pub fn new(port: impl Into<String>, baud: u32, format: SerialFormat) -> Self {
        Self {
            port: port.into(),
            baud,
            format,
            flow: Flow::None,
            dtr: true,
            rts: true,
        }
    }

    /// Управление потоком.
    #[must_use]
    pub fn with_flow(mut self, flow: Flow) -> Self {
        self.flow = flow;
        self
    }

    /// Начальные DTR и RTS.
    #[must_use]
    pub fn with_lines(mut self, dtr: bool, rts: bool) -> Self {
        self.dtr = dtr;
        self.rts = rts;
        self
    }

    /// Запись формата для журнала и интерфейса: `8N1`.
    #[must_use]
    pub fn format(&self) -> SerialFormat {
        self.format
    }

    /// Проверяет настройки до открытия порта и создания журнала (M3-AC3).
    ///
    /// # Errors
    ///
    /// Порт не указан, скорость вне 1…4 000 000, неподдерживаемое сочетание.
    pub fn validate(&self) -> io::Result<()> {
        if self.port.trim().is_empty() {
            return Err(invalid("порт не указан"));
        }
        if !(MIN_BAUD..=MAX_BAUD).contains(&self.baud) {
            return Err(invalid(format!(
                "скорость {} вне диапазона {MIN_BAUD}…{MAX_BAUD}",
                self.baud
            )));
        }
        if !matches!(self.format.data_bits, 7 | 8) {
            return Err(invalid(format!(
                "битов данных {}: допустимо 7 или 8",
                self.format.data_bits
            )));
        }
        if !matches!(self.format.stop_bits, 1 | 2) {
            return Err(invalid(format!(
                "стоп-битов {}: допустимо 1 или 2",
                self.format.stop_bits
            )));
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
        if matches!(self.format.parity, Parity::Mark | Parity::Space) {
            return Err(invalid(
                "чётность mark/space не поддерживается на этой операционной системе",
            ));
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Порт для формы COM и команды `ports` (FR-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    /// Имя порта: `COM5`, `/dev/ttyUSB0`.
    pub name: String,
    /// Понятное описание: `USB-SERIAL CH340 (COM5)` или имя порта.
    pub description: String,
}

/// Перечисляет доступные порты с понятным описанием (FR-12, M3-AC4).
///
/// Перечисление блокирующее: мост вызывает его через `spawn_blocking`. Пустой список —
/// не ошибка: на машине без портов форма COM показывает, что портов нет.
///
/// # Errors
///
/// Система не смогла перечислить порты.
pub fn list_ports() -> io::Result<Vec<PortInfo>> {
    let ports = serialport::available_ports().map_err(io::Error::from)?;
    let mut list: Vec<PortInfo> = ports
        .iter()
        .map(|port| PortInfo {
            name: port.port_name.clone(),
            description: describe(&port.port_name, &port.port_type),
        })
        .collect();
    // COM2 раньше COM10: номер сравнивается как число, а не как текст (M3-AC4).
    list.sort_by(|left, right| natural_cmp(&left.name, &right.name));
    Ok(list)
}

/// Описание порта по типу подключения.
fn describe(name: &str, port_type: &serialport::SerialPortType) -> String {
    match port_type {
        serialport::SerialPortType::UsbPort(info) => describe_usb(
            name,
            info.product.as_deref(),
            info.manufacturer.as_deref(),
            info.vid,
            info.pid,
        ),
        _ => name.to_owned(),
    }
}

/// Описание USB-порта: продукт, иначе производитель, иначе VID:PID (FR-12).
///
/// Некоторые драйверы Windows уже кладут имя порта в строку продукта: «USB-SERIAL CH340
/// (COM5)». Тогда второй раз `(COM5)` не дописывается — иначе описание дублировало бы порт.
fn describe_usb(
    name: &str,
    product: Option<&str>,
    manufacturer: Option<&str>,
    vid: u16,
    pid: u16,
) -> String {
    let label = [product, manufacturer]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|label| !label.is_empty())
        .map_or_else(|| format!("USB {vid:04X}:{pid:04X}"), str::to_owned);
    let suffix = format!("({name})");
    if label
        .to_ascii_lowercase()
        .ends_with(&suffix.to_ascii_lowercase())
    {
        label
    } else {
        format!("{label} {suffix}")
    }
}

/// Сравнение имён портов по номеру: `COM2` раньше `COM10` (FR-12).
fn natural_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut left = left.as_bytes();
    let mut right = right.as_bytes();
    loop {
        match (left.first(), right.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                if a.is_ascii_digit() && b.is_ascii_digit() {
                    let left_len = digits(left);
                    let right_len = digits(right);
                    let left_number = trim_zeros(&left[..left_len]);
                    let right_number = trim_zeros(&right[..right_len]);
                    let order = left_number
                        .len()
                        .cmp(&right_number.len())
                        .then_with(|| left_number.cmp(right_number));
                    if order != Ordering::Equal {
                        return order;
                    }
                    left = &left[left_len..];
                    right = &right[right_len..];
                } else {
                    let order = a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
                    if order != Ordering::Equal {
                        return order;
                    }
                    left = &left[1..];
                    right = &right[1..];
                }
            }
        }
    }
}

fn digits(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(bytes.len())
}

fn trim_zeros(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(bytes.len());
    &bytes[start..]
}

/// Сколько ждёт открытие, пока прежнее соединение этого порта отпустит дескриптор.
///
/// Порт закрывается, когда вышел последний его поток. Ожидание асинхронное: оно не держит
/// ни ответ анализатору, ни «Стоп». Две секунды — с запасом на медленную машину; дольше
/// держать переоткрытие незачем — следующая попытка придёт по интервалу источника.
const PORT_CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Сколько байт читает порт за один вызов.
const READ_BUFFER: usize = 4096;

/// Открывает порт и запускает его потоки чтения и записи (FR-9).
///
/// # Errors
///
/// Настройки неверны, порт не найден, занят или операционная система отказала
/// в настройке. Файл журнала к этому моменту ещё не создан (M3-AC3).
pub async fn open(settings: &SerialSettings) -> io::Result<SerialConnection> {
    settings.validate()?;
    // Прежнее соединение могло ещё не отпустить дескриптор: ждём его асинхронно, чтобы
    // переоткрытие не получило «порт занят» от собственного прошлого открытия.
    wait_for_close(&settings.port).await;
    let settings = settings.clone();
    tokio::task::spawn_blocking(move || open_blocking(&settings))
        .await
        .map_err(|err| io::Error::other(format!("the port open thread failed: {err}")))?
}

fn open_blocking(settings: &SerialSettings) -> io::Result<SerialConnection> {
    let mut port = serialport::new(settings.port.clone(), settings.baud)
        .data_bits(settings.format.data_bits_sp())
        // mark/space в `serialport` нет: выставим их после открытия (FR-11).
        .parity(serialport_parity(settings.format.parity))
        .stop_bits(settings.format.stop_bits_sp())
        .flow_control(settings.flow.serialport())
        .timeout(READ_POLL)
        .dtr_on_open(settings.dtr)
        .open_native()
        .map_err(|err| port_error(&settings.port, &err))?;

    apply_platform_settings(&mut port, settings).map_err(|err| io_error(&settings.port, &err))?;

    // Писатель получает свой дескриптор: блокирующее чтение с таймаутом не задерживает
    // ответ, а ответ не задерживает чтение (ADR 0002).
    let writer = port
        .try_clone()
        .map_err(|err| port_error(&settings.port, &err))?;
    start_threads(Box::new(port), writer, &settings.port)
}

fn serialport_parity(parity: Parity) -> serialport::Parity {
    match parity {
        Parity::None | Parity::Mark | Parity::Space => serialport::Parity::None,
        Parity::Even => serialport::Parity::Even,
        Parity::Odd => serialport::Parity::Odd,
    }
}

#[cfg(unix)]
fn apply_platform_settings(
    port: &mut serialport::TTYPort,
    settings: &SerialSettings,
) -> io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    let fd = port.as_raw_fd();
    apply_parity(fd, settings.format.parity)?;
    set_modem_line(fd, libc::TIOCM_DTR, settings.dtr)?;
    set_modem_line(fd, libc::TIOCM_RTS, settings.rts)?;
    Ok(())
}

#[cfg(windows)]
fn apply_platform_settings(
    port: &mut serialport::COMPort,
    settings: &SerialSettings,
) -> io::Result<()> {
    apply_parity(port, settings.format.parity)?;
    port.write_data_terminal_ready(settings.dtr)
        .map_err(io::Error::from)?;
    port.write_request_to_send(settings.rts)
        .map_err(io::Error::from)?;
    Ok(())
}

/// `ENOTTY` — у pty нет управляющих линий: выставлять нечего, это не ошибка (M3-AC11).
#[cfg(unix)]
fn set_modem_line(fd: std::os::unix::io::RawFd, line: libc::c_int, level: bool) -> io::Result<()> {
    let request = if level {
        libc::TIOCMBIS
    } else {
        libc::TIOCMBIC
    };
    // SAFETY: `fd` открыт портом, `request` — ioctl с указателем на `c_int`.
    let result = unsafe { libc::ioctl(fd, request, &line) };
    if result == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOTTY) {
        return Ok(());
    }
    Err(err)
}

/// Чётность mark/space через `CMSPAR`: в `serialport` таких значений нет (FR-11).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn apply_parity(fd: std::os::unix::io::RawFd, parity: Parity) -> io::Result<()> {
    if !matches!(parity, Parity::Mark | Parity::Space) {
        return Ok(());
    }
    // `termios2`, а не `termios`: при произвольной скорости (250 000) настройки
    // читаются и пишутся только этой структурой, иначе скорость потеряется.
    let mut termios = std::mem::MaybeUninit::<libc::termios2>::uninit();
    // SAFETY: `fd` открыт портом; `TCGETS2` заполняет переданную структуру.
    if unsafe { libc::ioctl(fd, libc::TCGETS2 as _, termios.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `TCGETS2` вернул 0, структура заполнена.
    let mut termios = unsafe { termios.assume_init() };
    termios.c_cflag |= libc::PARENB | libc::CMSPAR;
    match parity {
        Parity::Mark => termios.c_cflag |= libc::PARODD,
        Parity::Space => termios.c_cflag &= !libc::PARODD,
        Parity::None | Parity::Even | Parity::Odd => {}
    }
    // SAFETY: структура получена из `TCGETS2` того же порта.
    if unsafe { libc::ioctl(fd, libc::TCSETS2 as _, &termios) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn apply_parity(_fd: std::os::unix::io::RawFd, parity: Parity) -> io::Result<()> {
    if matches!(parity, Parity::Mark | Parity::Space) {
        return Err(invalid(
            "чётность mark/space не поддерживается на этой операционной системе",
        ));
    }
    Ok(())
}

/// Чётность mark/space через поле `Parity` структуры DCB (FR-11).
#[cfg(windows)]
fn apply_parity(port: &serialport::COMPort, parity: Parity) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Devices::Communication::{
        DCB, GetCommState, MARKPARITY, SPACEPARITY, SetCommState,
    };
    use windows_sys::Win32::Foundation::HANDLE;

    if !matches!(parity, Parity::Mark | Parity::Space) {
        return Ok(());
    }
    let handle = port.as_raw_handle() as HANDLE;
    // SAFETY: `DCB` — структура без указателей, нулевое значение допустимо.
    let mut dcb: DCB = unsafe { std::mem::zeroed() };
    dcb.DCBlength = u32::try_from(std::mem::size_of::<DCB>()).unwrap_or(0);
    // SAFETY: `handle` открыт портом, `dcb` — валидная структура.
    if unsafe { GetCommState(handle, &mut dcb) } == 0 {
        return Err(io::Error::last_os_error());
    }
    dcb.Parity = if parity == Parity::Mark {
        MARKPARITY
    } else {
        SPACEPARITY
    };
    // Бит 1 `_bitfield` — `fParity`: проверка чётности включена.
    dcb._bitfield |= 1 << 1;
    // SAFETY: `handle` открыт портом, `dcb` заполнена `GetCommState`.
    if unsafe { SetCommState(handle, &dcb) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Запускает потоки чтения и записи вокруг открытого порта.
///
/// Оба выходят по флагу остановки; последний вышедший отмечает, что порт закрыт, и будит
/// переоткрытие. Потоки не соединяются при остановке: [`SerialConnection`] только
/// сигналит, а ждать их никто не обязан (NFR-5).
fn start_threads(
    reader: Box<dyn serialport::SerialPort>,
    writer: Box<dyn serialport::SerialPort>,
    name: &str,
) -> io::Result<SerialConnection> {
    let (read_tx, read_rx) = mpsc::channel(READ_QUEUE);
    let (write_tx, write_rx) = sync_mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let alive = Arc::new(AtomicUsize::new(2));
    let closed = Arc::new(PortClosed::new());
    if let Ok(mut ports) = closing_ports().lock() {
        ports.insert(name.to_owned(), Arc::downgrade(&closed));
    }

    let reader_name = name.to_owned();
    let reader_thread = std::thread::Builder::new()
        .name(format!("serial-read-{name}"))
        .spawn({
            let stop = Arc::clone(&stop);
            let alive = Arc::clone(&alive);
            let closed = Arc::clone(&closed);
            move || {
                run_reader(reader, &reader_name, read_tx, &stop);
                finish_thread(&alive, &closed);
            }
        })
        .map_err(|err| io::Error::other(format!("the port thread did not start: {err}")))?;

    let writer_name = name.to_owned();
    let writer_thread = std::thread::Builder::new()
        .name(format!("serial-write-{name}"))
        .spawn({
            let stop = Arc::clone(&stop);
            let alive = Arc::clone(&alive);
            let closed = Arc::clone(&closed);
            move || {
                run_writer(writer, &writer_name, write_rx, &stop);
                finish_thread(&alive, &closed);
            }
        });

    let writer_thread = match writer_thread {
        Ok(thread) => thread,
        Err(err) => {
            // Читатель уже запущен: флаг остановки отпустит его без ожидания.
            stop.store(true, Ordering::Relaxed);
            // Соединение не создаётся: отдаём ошибку, а поток дойдёт сам.
            drop(reader_thread);
            return Err(io::Error::other(format!(
                "the port thread did not start: {err}"
            )));
        }
    };
    // Потоки отсоединяются: `Drop` не ждёт их, иначе зависшая запись держала бы «Стоп».
    drop(reader_thread);
    drop(writer_thread);

    Ok(SerialConnection {
        name: name.to_owned(),
        reads: read_rx,
        writes: Some(write_tx),
        pending: None,
        leftover: Vec::new(),
        stop,
    })
}

/// Порт отпустил дескриптор: последний его поток вышел.
///
/// Пока жив хоть один дескриптор, открыть порт заново нельзя. Сигнал ждут асинхронно,
/// поэтому переоткрытие не блокирует runtime.
#[derive(Debug)]
struct PortClosed {
    closed: AtomicBool,
    /// Разрешение на одного ждущего: переоткрытие одно.
    notify: tokio::sync::Notify,
}

impl PortClosed {
    fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Отмечает закрытие и будит ждущего переоткрытия.
    fn mark_closed(&self) {
        self.closed.store(true, Ordering::Release);
        // `notify_one` запоминает разрешение, если ждущего ещё нет: сигнал не потеряется.
        self.notify.notify_one();
    }

    async fn wait(&self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.notify.notified().await;
    }
}

/// Сигналы закрытия портов по имени: переоткрытие ждёт прежнее соединение того же порта.
fn closing_ports() -> &'static Mutex<HashMap<String, Weak<PortClosed>>> {
    static PORTS: OnceLock<Mutex<HashMap<String, Weak<PortClosed>>>> = OnceLock::new();
    PORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ждёт, пока прежнее соединение этого порта отпустит дескриптор.
///
/// Сигнал слабый: если потоков уже нет, ждать нечего. Ожидание ограничено
/// [`PORT_CLOSE_WAIT`] — зависшая в драйвере запись не должна останавливать переоткрытие
/// навсегда: следующая попытка придёт по интервалу источника.
async fn wait_for_close(port: &str) {
    let signal = closing_ports()
        .lock()
        .ok()
        .and_then(|ports| ports.get(port).and_then(Weak::upgrade));
    let Some(signal) = signal else {
        return;
    };
    let _ = tokio::time::timeout(PORT_CLOSE_WAIT, signal.wait()).await;
}

/// Последний вышедший поток отмечает, что дескриптор порта отпущен.
fn finish_thread(alive: &AtomicUsize, closed: &PortClosed) {
    if alive.fetch_sub(1, Ordering::AcqRel) == 1 {
        closed.mark_closed();
    }
}

/// Кладёт в очередь к сессии, дожидаясь места: полная очередь не теряет ни фрагмент,
/// ни ошибку чтения.
///
/// `make` вызывается на каждую попытку: `io::Error` не клонируется, а очередь принимает
/// значение только целиком. Возвращает `false`, если поток остановлен или канал закрыт.
fn send_waiting<T>(queue: &mpsc::Sender<T>, stop: &AtomicBool, make: impl Fn() -> T) -> bool {
    while !stop.load(Ordering::Relaxed) {
        match queue.try_send(make()) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(_)) => std::thread::sleep(QUEUE_PAUSE),
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
    false
}

/// Поток чтения: читает с таймаутом и отправляет фрагменты сессии.
fn run_reader(
    mut port: Box<dyn serialport::SerialPort>,
    name: &str,
    reads: mpsc::Sender<io::Result<Vec<u8>>>,
    stop: &AtomicBool,
) {
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match port.read(&mut buffer) {
            // `Ok(0)` на порту не означает конец потока: ждём, но не крутим цикл.
            Ok(0) => std::thread::sleep(Duration::from_millis(10)),
            Ok(len) => {
                let chunk = buffer[..len].to_vec();
                // Полная очередь не теряет фрагмент: ждём места, но замечаем остановку.
                if !send_waiting(&reads, stop, || Ok(chunk.clone())) {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
            // Отключённый адаптер: ошибка идёт сессии, соединение закрывается. Место в
            // очереди ждём так же, как для данных: иначе ошибка пропала бы, а сессия
            // увидела бы только конец потока.
            Err(err) => {
                let _ = send_waiting(&reads, stop, || Err(io_error(name, &err)));
                return;
            }
        }
    }
}

/// Команда записи: байты ответа и канал, куда вернуть результат.
struct WriteCommand {
    bytes: Vec<u8>,
    done: tokio::sync::oneshot::Sender<io::Result<()>>,
}

/// Пишет байты в порт, повторяя попытку, пока драйвер их не примет.
///
/// `Ok(0)` и таймаут порта — не потеря, а занятый драйвер: попытка повторяется, пока не
/// выйдет [`WRITE_TIMEOUT`] или не выставят флаг остановки. Таймаут порта не говорит,
/// сколько байт драйвер всё же взял: повтор может продублировать ответ, но зависший ответ
/// хуже — анализатор не получит `ACK` и не узнает, что с ним стало.
fn write_bytes(
    port: &mut dyn serialport::SerialPort,
    name: &str,
    bytes: &[u8],
    stop: &AtomicBool,
) -> io::Result<()> {
    let deadline = std::time::Instant::now() + WRITE_TIMEOUT;
    let mut written = 0;
    while written < bytes.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!(
                    "serial port {name}: the write is stopped at {written} of {} bytes",
                    bytes.len()
                ),
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "serial port {name} did not take the answer in {} s: {written} of {} bytes written",
                    WRITE_TIMEOUT.as_secs(),
                    bytes.len()
                ),
            ));
        }
        match port.write(&bytes[written..]) {
            Ok(0) => std::thread::sleep(WRITE_PAUSE),
            Ok(len) => written += len,
            Err(err) if err.kind() == io::ErrorKind::TimedOut => std::thread::sleep(WRITE_PAUSE),
            Err(err) => return Err(io_error(name, &err)),
        }
    }
    Ok(())
}

/// Поток записи: пишет ответы в порт и возвращает результат записи [`SerialConnection`].
///
/// `flush`/`tcdrain` не вызываются: `write` отдаёт байты драйверу, а `tcdrain` ждёт, пока
/// их вытолкнут в линию, — на медленном порту это задержка ответа и остановки.
fn run_writer(
    mut port: Box<dyn serialport::SerialPort>,
    name: &str,
    writes: sync_mpsc::Receiver<WriteCommand>,
    stop: &AtomicBool,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match writes.recv_timeout(READ_POLL) {
            Ok(command) => {
                let result = write_bytes(port.as_mut(), name, &command.bytes, stop);
                let failed = result.is_err();
                // Результат уходит в `poll_write`: ответ не считается отправленным, пока
                // порт не принял байты.
                let _ = command.done.send(result);
                if failed {
                    // Ошибка записи закрывает соединение: источник переоткроет порт.
                    return;
                }
            }
            Err(sync_mpsc::RecvTimeoutError::Timeout) => {}
            Err(sync_mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Асинхронная сторона COM-порта: одно открытие порта (FR-9).
pub struct SerialConnection {
    name: String,
    reads: mpsc::Receiver<io::Result<Vec<u8>>>,
    /// Очередь команд записи; `None` после остановки.
    writes: Option<sync_mpsc::Sender<WriteCommand>>,
    /// Результат записи, которую ждёт `poll_write`, и длина её буфера.
    ///
    /// Длина нужна, чтобы не спутать результат отменённой записи с новой: `poll_write`
    /// по контракту повторяет тот же буфер, пока не вернул `Ready`.
    pending: Option<(usize, tokio::sync::oneshot::Receiver<io::Result<()>>)>,
    /// Остаток последнего прочитанного куска, не влезший в буфер вызывающего.
    leftover: Vec<u8>,
    stop: Arc<AtomicBool>,
}

impl SerialConnection {
    /// Имя порта, к которому подключено соединение.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Сигнал остановки потокам порта; очередь записи закрывается, ответов больше нет.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.writes = None;
    }
}

impl fmt::Debug for SerialConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SerialConnection")
            .field("port", &self.name)
            .finish()
    }
}

impl AsyncRead for SerialConnection {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        // Кусок из канала может быть больше буфера вызывающего: остаток отдаётся следующим
        // вызовом, а не теряется (иначе сессия записала бы в журнал не всё прочитанное).
        if !self.leftover.is_empty() {
            let take = self.leftover.len().min(buf.remaining());
            buf.put_slice(&self.leftover[..take]);
            self.leftover.drain(..take);
            return std::task::Poll::Ready(Ok(()));
        }
        match self.reads.poll_recv(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Some(Ok(chunk))) => {
                let take = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..take]);
                if take < chunk.len() {
                    self.leftover.extend_from_slice(&chunk[take..]);
                }
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Some(Err(err))) => std::task::Poll::Ready(Err(err)),
            std::task::Poll::Ready(None) => {
                if self.stop.load(Ordering::Relaxed) {
                    // Штатная остановка: порт закрыт, это конец потока.
                    std::task::Poll::Ready(Ok(()))
                } else {
                    std::task::Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the serial port thread has finished",
                    )))
                }
            }
        }
    }
}

impl AsyncWrite for SerialConnection {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        use std::task::Poll;

        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if self.stop.load(Ordering::Relaxed) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the serial port is closed: the answer is not written",
            )));
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|(len, _)| *len != buf.len())
        {
            // Буфер сменился: прошлая запись была отменена вместе со своим будущим,
            // а её результат уже никому не нужен.
            self.pending = None;
        }
        if self.pending.is_none() {
            let Some(writes) = self.writes.as_ref() else {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the serial port writer has stopped",
                )));
            };
            let (done, wait) = tokio::sync::oneshot::channel();
            if writes
                .send(WriteCommand {
                    bytes: buf.to_vec(),
                    done,
                })
                .is_err()
            {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the serial port writer has stopped",
                )));
            }
            self.pending = Some((buf.len(), wait));
        }
        // Ждём фактический результат записи: ошибка порта не должна выглядеть отправкой.
        let outcome = match self.pending.as_mut() {
            Some((_, pending)) => std::pin::Pin::new(pending).poll(cx),
            None => Poll::Ready(Ok(Ok(()))),
        };
        match outcome {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Ok(()))) => {
                self.pending = None;
                Poll::Ready(Ok(buf.len()))
            }
            Poll::Ready(Ok(Err(err))) => {
                self.pending = None;
                Poll::Ready(Err(err))
            }
            Poll::Ready(Err(_)) => {
                self.pending = None;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the serial port writer stopped before the answer was written",
                )))
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        // Буфера на стороне соединения нет: байты уходят в поток писателя сразу.
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.stop();
        std::task::Poll::Ready(Ok(()))
    }
}

impl Drop for SerialConnection {
    fn drop(&mut self) {
        // Потоки не соединяются: зависшая в драйвере запись не должна держать «Стоп» или
        // runtime. Порт закрывается, когда они выйдут, а переоткрытие ждёт этого асинхронно
        // (`wait_for_close`).
        self.stop();
    }
}

/// Ошибка ввода-вывода порта по-английски: язык ОС в журнал не попадает.
///
/// Код ОС остаётся в тексте: он нужен инженеру, а `Display` ошибки Windows локализован.
fn io_error(port: &str, err: &io::Error) -> io::Error {
    let message = match err.raw_os_error() {
        Some(code) => format!(
            "serial port {port} {} (os error {code})",
            describe_kind(err.kind())
        ),
        None => format!("serial port {port} {}", describe_kind(err.kind())),
    };
    io::Error::new(err.kind(), message)
}

/// Занятость порта в описании отказа: `EBUSY` крейт отдаёт как `NoDevice`.
fn mentions_busy(description: &str) -> bool {
    description.to_ascii_lowercase().contains("busy")
}

/// Описание отказа по виду ошибки: журнал английский независимо от языка ОС.
fn describe_kind(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "is not found",
        io::ErrorKind::PermissionDenied => "access is denied",
        io::ErrorKind::InvalidInput => "does not support the settings",
        io::ErrorKind::TimedOut => "did not answer in time",
        io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected => "is disconnected",
        io::ErrorKind::AddrInUse | io::ErrorKind::ResourceBusy => "is busy",
        _ => "refused the operation",
    }
}

/// Ошибка открытия порта: английский текст по виду отказа, а не `Display` крейта.
///
/// `serialport` отдаёт `NoDevice` и для отсутствующего порта, и для занятого (`EBUSY`):
/// различить их можно только по описанию отказа, поэтому занятость видна в тексте, а вид
/// остаётся `NotFound` — по нему мост различает занятость по списку портов (`port_busy`).
fn port_error(port: &str, err: &serialport::Error) -> io::Error {
    match err.kind {
        serialport::ErrorKind::NoDevice if mentions_busy(&err.description) => io::Error::new(
            io::ErrorKind::NotFound,
            format!("serial port {port} is busy: it is opened by another program"),
        ),
        serialport::ErrorKind::NoDevice => io::Error::new(
            io::ErrorKind::NotFound,
            format!("serial port {port} is not found, is busy or access is denied"),
        ),
        serialport::ErrorKind::InvalidInput => io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("serial port {port}: the settings are not supported"),
        ),
        serialport::ErrorKind::Io(kind) => {
            io::Error::new(kind, format!("serial port {port} {}", describe_kind(kind)))
        }
        serialport::ErrorKind::Unknown => io::Error::other(format!(
            "serial port {port}: the operating system refused the operation"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::collections::VecDeque;

    use super::*;

    #[test]
    fn format_parses_and_formats_back() {
        for (text, data_bits, parity, stop_bits) in [
            ("8N1", 8, Parity::None, 1),
            ("7E2", 7, Parity::Even, 2),
            ("8M1", 8, Parity::Mark, 1),
            ("7S2", 7, Parity::Space, 2),
            ("8O1", 8, Parity::Odd, 1),
        ] {
            let format = SerialFormat::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"));
            assert_eq!(format.data_bits, data_bits, "{text}");
            assert_eq!(format.parity, parity, "{text}");
            assert_eq!(format.stop_bits, stop_bits, "{text}");
            assert_eq!(format.to_string(), text.to_uppercase(), "обратная запись");
        }
        assert_eq!(
            SerialFormat::parse("8n1")
                .expect("регистр буквы не важен")
                .to_string(),
            "8N1"
        );
    }

    #[test]
    fn format_rejects_broken_values() {
        for text in ["", "8N", "8N12", "5N1", "8X1", "8N3"] {
            assert!(
                SerialFormat::parse(text).is_err(),
                "`{text}` не должен разбираться"
            );
        }
    }

    #[test]
    fn settings_check_the_baud_range_before_the_port_opens() {
        let mut settings = SerialSettings::new("COM5", 9600, SerialFormat::EIGHT_N_ONE);
        settings.validate().expect("типовые настройки");
        for baud in [MIN_BAUD, 300, 115_200, MAX_BAUD] {
            settings.baud = baud;
            settings
                .validate()
                .unwrap_or_else(|err| panic!("{baud}: {err}"));
        }
        for baud in [0, MAX_BAUD + 1] {
            settings.baud = baud;
            let err = settings.validate().expect_err("скорость вне диапазона");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("диапазона"), "{err}");
        }
    }

    #[test]
    fn an_empty_port_name_is_rejected() {
        let settings = SerialSettings::new("  ", 9600, SerialFormat::EIGHT_N_ONE);
        let err = settings.validate().expect_err("порт не указан");
        assert!(err.to_string().contains("порт не указан"), "{err}");
    }

    #[test]
    fn a_usb_port_is_described_by_product_and_name() {
        assert_eq!(
            describe_usb(
                "COM5",
                Some("USB-SERIAL CH340"),
                Some("wch.cn"),
                0x1A86,
                0x7523
            ),
            "USB-SERIAL CH340 (COM5)"
        );
        assert_eq!(
            describe_usb("COM7", None, Some("FTDI"), 0x0403, 0x6001),
            "FTDI (COM7)"
        );
        assert_eq!(
            describe_usb("COM9", None, None, 0x1A86, 0x7523),
            "USB 1A86:7523 (COM9)"
        );
        // Фикстура Windows: драйвер уже вписал имя порта в строку продукта.
        assert_eq!(
            describe_usb(
                "COM5",
                Some("USB-SERIAL CH340 (COM5)"),
                Some("wch.cn"),
                0x1A86,
                0x7523
            ),
            "USB-SERIAL CH340 (COM5)",
            "имя порта не дублируется"
        );
        assert_eq!(
            describe_usb(
                "COM5",
                Some("USB-SERIAL CH340 (com5)"),
                None,
                0x1A86,
                0x7523
            ),
            "USB-SERIAL CH340 (com5)",
            "регистр имени порта не важен"
        );
    }

    #[test]
    fn a_non_usb_port_keeps_its_name() {
        assert_eq!(
            describe("COM1", &serialport::SerialPortType::PciPort),
            "COM1"
        );
    }

    #[test]
    fn a_busy_port_error_is_named_busy() {
        // `EBUSY` крейт отдаёт как `NoDevice`: занятость видна только по описанию.
        let busy =
            serialport::Error::new(serialport::ErrorKind::NoDevice, "Device or resource busy");
        let error = port_error("COM5", &busy);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("is busy"), "{error}");

        let missing =
            serialport::Error::new(serialport::ErrorKind::NoDevice, "No such file or directory");
        let error = port_error("COM7", &missing);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            error
                .to_string()
                .contains("is not found, is busy or access is denied"),
            "английский текст по виду отказа: {error}"
        );
    }

    #[test]
    fn ports_are_sorted_by_number_not_as_text() {
        let mut names = ["COM10", "COM2", "COM1", "/dev/ttyUSB10", "/dev/ttyUSB2"];
        names.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(
            names,
            ["/dev/ttyUSB2", "/dev/ttyUSB10", "COM1", "COM2", "COM10"]
        );
        assert_eq!(natural_cmp("COM02", "COM2"), Ordering::Equal);
    }

    #[test]
    fn an_empty_port_list_is_not_an_error() {
        let ports = list_ports().expect("перечисление портов не падает");
        for pair in ports.windows(2) {
            assert_ne!(
                natural_cmp(&pair[0].name, &pair[1].name),
                Ordering::Greater,
                "{} и {}",
                pair[0].name,
                pair[1].name
            );
        }
    }

    /// Пара pty проверяет адаптер без `socat`; интеграционный тест COM — в `tests/serial.rs`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_port_is_read_and_written_through_the_adapter() {
        use std::io::Write as _;

        use serialport::SerialPort as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::time::timeout;

        let (mut analyzer, slave) = serialport::TTYPort::pair().expect("пара pty");
        let name = slave.name().expect("имя pty");
        drop(slave);

        let settings =
            SerialSettings::new(name, 9600, SerialFormat::EIGHT_N_ONE).with_lines(false, false);
        let mut port = open(&settings).await.expect("порт открывается");

        analyzer.write_all(b"\x05").expect("запрос");
        let mut buffer = [0u8; 8];
        let read = timeout(Duration::from_secs(2), port.read(&mut buffer))
            .await
            .expect("чтение не зависает")
            .expect("чтение");
        assert_eq!(&buffer[..read], b"\x05");

        port.write_all(b"\x06").await.expect("ответ");
        let read = tokio::task::spawn_blocking(move || {
            let mut buffer = [0u8; 8];
            let read = std::io::Read::read(&mut analyzer, &mut buffer).expect("чтение");
            buffer[..read].to_vec()
        })
        .await
        .expect("задача чтения");
        assert_eq!(read, b"\x06");
    }

    /// Что делает фейковая запись: висит, отказывает или не берёт байты с первого раза.
    #[derive(Clone, Copy)]
    enum FakeWrite {
        /// Висит, пока тест не разрешит: так ведёт себя зависший драйвер.
        Block,
        /// Отказывает, как выдернутый адаптер.
        Fail,
        /// Сначала `Ok(0)`, потом таймаут порта, дальше пишет: занятый драйвер.
        Busy,
    }

    /// Фейковый порт поверх `serialport::SerialPort`: без железа и без pty.
    struct FakePort {
        state: Arc<FakeState>,
        write: FakeWrite,
    }

    /// Общее состояние клонов фейкового порта.
    struct FakeState {
        released: Mutex<bool>,
        ready: std::sync::Condvar,
        written: Mutex<Vec<u8>>,
        /// Сколько раз порт позвали на запись: по счёту отвечает [`FakeWrite::Busy`].
        attempts: Mutex<usize>,
        /// Заданные чтения по порядку: фрагмент или ошибка; когда очередь пуста, порт молчит.
        reads: Mutex<VecDeque<io::Result<Vec<u8>>>>,
    }

    impl FakeState {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                released: Mutex::new(false),
                ready: std::sync::Condvar::new(),
                written: Mutex::new(Vec::new()),
                attempts: Mutex::new(0),
                reads: Mutex::new(VecDeque::new()),
            })
        }

        /// Задаёт следующее чтение: фрагмент или отказ порта.
        fn script_read(&self, result: io::Result<Vec<u8>>) {
            self.reads.lock().expect("замок").push_back(result);
        }

        /// Разрешает зависшую запись: поток писателя дописывает байты и выходит.
        fn release(&self) {
            *self.released.lock().expect("замок") = true;
            self.ready.notify_all();
        }

        /// Ждёт разрешения: так запись висит, пока тест не отпустит порт.
        fn blocked(&self) {
            let mut released = self.released.lock().expect("замок");
            while !*released {
                released = self.ready.wait(released).expect("ожидание разрешения");
            }
        }
    }

    fn fake_pair(
        state: &Arc<FakeState>,
        write: FakeWrite,
    ) -> (
        Box<dyn serialport::SerialPort>,
        Box<dyn serialport::SerialPort>,
    ) {
        let port = || {
            Box::new(FakePort {
                state: Arc::clone(state),
                write,
            }) as Box<dyn serialport::SerialPort>
        };
        (port(), port())
    }

    impl io::Read for FakePort {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let scripted = self.state.reads.lock().expect("замок").pop_front();
            match scripted {
                Some(Ok(chunk)) => {
                    let len = chunk.len().min(buf.len());
                    buf[..len].copy_from_slice(&chunk[..len]);
                    Ok(len)
                }
                Some(Err(err)) => Err(err),
                // Как порт с таймаутом: управление возвращается, данных нет.
                None => {
                    std::thread::sleep(READ_POLL);
                    Err(io::Error::from(io::ErrorKind::TimedOut))
                }
            }
        }
    }

    impl io::Write for FakePort {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self.write {
                FakeWrite::Fail => Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the adapter is gone",
                )),
                FakeWrite::Busy => {
                    let mut attempts = self.state.attempts.lock().expect("замок");
                    *attempts += 1;
                    match *attempts {
                        1 => Ok(0),
                        2 => Err(io::Error::from(io::ErrorKind::TimedOut)),
                        _ => {
                            drop(attempts);
                            self.state
                                .written
                                .lock()
                                .expect("замок")
                                .extend_from_slice(buf);
                            Ok(buf.len())
                        }
                    }
                }
                FakeWrite::Block => {
                    self.state.blocked();
                    self.state
                        .written
                        .lock()
                        .expect("замок")
                        .extend_from_slice(buf);
                    Ok(buf.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            // Вечный `flush`: код, который звал бы его после записи, не отпустил бы порт.
            self.state.blocked();
            Ok(())
        }
    }

    impl serialport::SerialPort for FakePort {
        fn name(&self) -> Option<String> {
            None
        }

        fn baud_rate(&self) -> serialport::Result<u32> {
            Ok(9600)
        }

        fn data_bits(&self) -> serialport::Result<DataBits> {
            Ok(DataBits::Eight)
        }

        fn flow_control(&self) -> serialport::Result<FlowControl> {
            Ok(FlowControl::None)
        }

        fn parity(&self) -> serialport::Result<serialport::Parity> {
            Ok(serialport::Parity::None)
        }

        fn stop_bits(&self) -> serialport::Result<StopBits> {
            Ok(StopBits::One)
        }

        fn timeout(&self) -> Duration {
            READ_POLL
        }

        fn set_baud_rate(&mut self, _baud_rate: u32) -> serialport::Result<()> {
            Ok(())
        }

        fn set_data_bits(&mut self, _data_bits: DataBits) -> serialport::Result<()> {
            Ok(())
        }

        fn set_flow_control(&mut self, _flow_control: FlowControl) -> serialport::Result<()> {
            Ok(())
        }

        fn set_parity(&mut self, _parity: serialport::Parity) -> serialport::Result<()> {
            Ok(())
        }

        fn set_stop_bits(&mut self, _stop_bits: StopBits) -> serialport::Result<()> {
            Ok(())
        }

        fn set_timeout(&mut self, _timeout: Duration) -> serialport::Result<()> {
            Ok(())
        }

        fn write_request_to_send(&mut self, _level: bool) -> serialport::Result<()> {
            Ok(())
        }

        fn write_data_terminal_ready(&mut self, _level: bool) -> serialport::Result<()> {
            Ok(())
        }

        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }

        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }

        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }

        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }

        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Ok(0)
        }

        fn bytes_to_write(&self) -> serialport::Result<u32> {
            Ok(0)
        }

        fn clear(&self, _buffer_to_clear: serialport::ClearBuffer) -> serialport::Result<()> {
            Ok(())
        }

        fn try_clone(&self) -> serialport::Result<Box<dyn serialport::SerialPort>> {
            Ok(Box::new(FakePort {
                state: Arc::clone(&self.state),
                write: self.write,
            }))
        }

        fn set_break(&self) -> serialport::Result<()> {
            Ok(())
        }

        fn clear_break(&self) -> serialport::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_port_that_does_not_take_the_answer_at_once_is_written_again() {
        use tokio::io::AsyncWriteExt as _;

        // `Ok(0)` и таймаут порта — не потеря: ответ дописывается целиком.
        let state = FakeState::new();
        let (reader, writer) = fake_pair(&state, FakeWrite::Busy);
        let mut connection = start_threads(reader, writer, "FAKE-BUSY").expect("потоки порта");

        connection
            .write_all(b"answer")
            .await
            .expect("запись повторена");
        assert_eq!(
            *state.written.lock().expect("замок"),
            b"answer",
            "байты дописаны целиком"
        );
        connection.shutdown().await.expect("остановка");
    }

    #[test]
    fn a_read_error_is_not_lost_when_the_queue_is_full() {
        // Очередь уже полна: без ожидания места ошибка чтения пропала бы, и сессия
        // увидела бы только конец потока.
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(Ok(b"filler".to_vec())).expect("очередь полна");
        let state = FakeState::new();
        state.script_read(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the adapter is gone",
        )));
        let (reader, _writer) = fake_pair(&state, FakeWrite::Fail);
        let stop = AtomicBool::new(false);

        std::thread::scope(|scope| {
            let reading = scope.spawn(move || run_reader(reader, "FAKE-READ-ERR", tx, &stop));
            // Забираем заполнитель: место освобождается, и ошибка доходит до сессии.
            let filler = rx
                .blocking_recv()
                .expect("первый элемент")
                .expect("фрагмент");
            assert_eq!(filler, b"filler");
            let error = rx
                .blocking_recv()
                .expect("ошибка не потеряна")
                .expect_err("ошибка порта");
            assert!(
                error.to_string().contains("serial port FAKE-READ-ERR"),
                "ошибка порта по-английски: {error}"
            );
            reading.join().expect("поток чтения");
        });
    }

    #[tokio::test]
    async fn the_stop_does_not_wait_for_a_stuck_port_write() {
        use tokio::io::AsyncWriteExt as _;

        let state = FakeState::new();
        let (reader, writer) = fake_pair(&state, FakeWrite::Block);
        let mut connection = start_threads(reader, writer, "FAKE-STOP").expect("потоки порта");

        {
            // Ответ висит в порту: поток писателя заблокирован в `write`.
            let write = connection.write_all(b"answer");
            tokio::pin!(write);
            tokio::select! {
                result = &mut write => panic!("запись не должна завершиться: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }

        // Остановка только сигналит: зависшая в драйвере запись её не держит (NFR-5).
        let started = std::time::Instant::now();
        connection.shutdown().await.expect("остановка не ждёт порт");
        drop(connection);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "остановка ждала порт: {elapsed:?}"
        );

        // Порт закрывается, когда запись отпущена: переоткрытие ждёт этого асинхронно.
        state.release();
        tokio::time::timeout(Duration::from_secs(2), wait_for_close("FAKE-STOP"))
            .await
            .expect("порт закрылся после разрешения записи");
    }

    #[tokio::test]
    async fn a_port_write_error_comes_back_from_the_connection() {
        use tokio::io::AsyncWriteExt as _;

        let state = FakeState::new();
        let (reader, writer) = fake_pair(&state, FakeWrite::Fail);
        let mut connection = start_threads(reader, writer, "FAKE-FAIL").expect("потоки порта");

        let error = connection
            .write_all(b"answer")
            .await
            .expect_err("запись отказала");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(
            error.to_string().contains("serial port FAKE-FAIL"),
            "ошибка порта по-английски: {error}"
        );
        connection.shutdown().await.expect("остановка");
    }

    #[tokio::test]
    async fn the_reopen_waits_for_the_stuck_port_to_be_released() {
        use tokio::io::AsyncWriteExt as _;

        let state = FakeState::new();
        let (reader, writer) = fake_pair(&state, FakeWrite::Block);
        let mut connection = start_threads(reader, writer, "FAKE-REOPEN").expect("потоки порта");

        {
            // Писатель заходит в `write` и висит там: дескриптор не отпущен.
            let write = connection.write_all(b"answer");
            tokio::pin!(write);
            tokio::select! {
                result = &mut write => panic!("запись не должна завершиться: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        drop(connection);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), wait_for_close("FAKE-REOPEN"))
                .await
                .is_err(),
            "порт ещё занят: переоткрытие ждёт"
        );

        state.release();
        tokio::time::timeout(Duration::from_secs(2), wait_for_close("FAKE-REOPEN"))
            .await
            .expect("порт закрылся после разрешения записи");
    }

    #[tokio::test]
    async fn a_read_chunk_bigger_than_the_buffer_is_not_lost() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        // Потоки порта не нужны: проверяется только выдача прочитанного куска.
        let (read_tx, read_rx) = mpsc::channel(READ_QUEUE);
        let (write_tx, _write_rx) = sync_mpsc::channel();
        let mut connection = SerialConnection {
            name: "FAKE-READ".to_owned(),
            reads: read_rx,
            writes: Some(write_tx),
            pending: None,
            leftover: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
        };
        read_tx
            .send(Ok(b"abcdef".to_vec()))
            .await
            .expect("фрагмент");

        // Буфер меньше куска: остаток отдаётся следующим чтением, а не теряется.
        let mut buffer = [0u8; 2];
        let mut received = Vec::new();
        for _ in 0..3 {
            let read = connection.read(&mut buffer).await.expect("чтение");
            received.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(received, b"abcdef", "остаток куска не потерян");
        let _ = connection.shutdown().await;
    }
}
