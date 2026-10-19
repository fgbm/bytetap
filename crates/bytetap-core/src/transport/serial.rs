//! COM-порт вместо ЛИС (FR-9, FR-11, FR-12).
//!
//! Крейт `serialport` даёт открытие, настройки и перечисление портов, но интерфейс у него
//! блокирующий. Поэтому порт живёт в отдельном потоке: он читает с коротким таймаутом,
//! пишет по очереди из асинхронной стороны и замечает остановку между чтениями. Асинхронная
//! сторона — [`SerialConnection`]: [`tokio::io::AsyncRead`] отдаёт прочитанные фрагменты,
//! [`tokio::io::AsyncWrite`] кладёт ответ в очередь. Решение и его обоснование — ADR 0002.
//!
//! Соединение — одно открытие порта. Ошибка чтения или записи закрывает соединение:
//! сессия пишет `close` и `error`, а источник переоткрывает порт с интервалом (NFR-5).
//!
//! Чётности `mark`/`space` в `serialport` нет: после открытия они выставляются напрямую —
//! `CMSPAR` в termios на Linux и `MARKPARITY`/`SPACEPARITY` в DCB на Windows. На pty
//! управляющих линий нет, поэтому `ENOTTY` от DTR/RTS не считается ошибкой (M3-AC11).

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as sync_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(windows)]
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
    format!("{label} ({name})")
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

/// Открывает порт и запускает его поток (FR-9).
///
/// # Errors
///
/// Настройки неверны, порт не найден, занят или операционная система отказала
/// в настройке. Файл журнала к этому моменту ещё не создан (M3-AC3).
pub async fn open(settings: &SerialSettings) -> io::Result<SerialConnection> {
    settings.validate()?;
    let settings = settings.clone();
    tokio::task::spawn_blocking(move || open_blocking(&settings))
        .await
        .map_err(|err| io::Error::other(format!("поток открытия порта упал: {err}")))?
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
        .map_err(io::Error::from)?;

    apply_platform_settings(&mut port, settings)?;

    let (read_tx, read_rx) = mpsc::channel(READ_QUEUE);
    let (write_tx, write_rx) = sync_mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let name = settings.port.clone();
    let thread_name = format!("serial-{name}");
    let thread = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_port(port, write_rx, read_tx, thread_stop))
        .map_err(|err| io::Error::other(format!("поток порта не запустился: {err}")))?;

    Ok(SerialConnection {
        name,
        reads: read_rx,
        writes: write_tx,
        stop,
        thread: Some(thread),
    })
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

/// Поток порта: читает с таймаутом и отправляет накопленные записи.
fn run_port(
    mut port: impl serialport::SerialPort + 'static,
    writes: sync_mpsc::Receiver<Vec<u8>>,
    reads: mpsc::Sender<io::Result<Vec<u8>>>,
    stop: Arc<AtomicBool>,
) {
    let mut buffer = vec![0u8; 4096];
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        while let Ok(bytes) = writes.try_recv() {
            if let Err(err) = port.write_all(&bytes).and_then(|()| port.flush()) {
                let _ = reads.try_send(Err(err));
                return;
            }
        }
        match port.read(&mut buffer) {
            // `Ok(0)` на порту не означает конец потока: ждём, но не крутим цикл.
            Ok(0) => std::thread::sleep(Duration::from_millis(10)),
            Ok(len) => {
                let chunk = buffer[..len].to_vec();
                // Полная очередь не теряет фрагмент: ждём места, но замечаем остановку.
                while !stop.load(Ordering::Relaxed) {
                    match reads.try_send(Ok(chunk.clone())) {
                        Ok(()) => break,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
            // Отключённый адаптер: отдаём ошибку сессии, соединение закрывается.
            Err(err) => {
                let _ = reads.try_send(Err(err));
                return;
            }
        }
    }
}

/// Асинхронная сторона COM-порта: одно открытие порта (FR-9).
pub struct SerialConnection {
    name: String,
    reads: mpsc::Receiver<io::Result<Vec<u8>>>,
    writes: sync_mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SerialConnection {
    /// Имя порта, к которому подключено соединение.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
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
        match self.reads.poll_recv(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Some(Ok(chunk))) => {
                let take = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..take]);
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
                        "поток порта завершился",
                    )))
                }
            }
        }
    }
}

impl AsyncWrite for SerialConnection {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        if buf.is_empty() {
            return std::task::Poll::Ready(Ok(0));
        }
        // Очередь неограниченная: ответ уходит в поток порта сразу, а ошибка записи
        // вернётся чтением (`run_port` кладёт её в тот же канал).
        match self.writes.send(buf.to_vec()) {
            Ok(()) => std::task::Poll::Ready(Ok(buf.len())),
            Err(_) => std::task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "поток порта остановлен",
            ))),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.stop();
        std::task::Poll::Ready(Ok(()))
    }
}

impl Drop for SerialConnection {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            // Поток просыпается не позже `READ_POLL`; блокировка короткая, но порт
            // должен быть закрыт до переоткрытия (FR-9).
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

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
    }

    #[test]
    fn a_non_usb_port_keeps_its_name() {
        assert_eq!(
            describe("COM1", &serialport::SerialPortType::PciPort),
            "COM1"
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
}
