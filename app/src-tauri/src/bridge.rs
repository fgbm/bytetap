//! Мост к ядру: старт и стоп сессии, состояние приложения и поток событий (FR-25, NFR-5).
//!
//! Всё, что можно проверить без окна, живёт здесь: [`Sessions`] хранит одну активную
//! сессию, [`start_session`] поднимает её на настроенном источнике, а [`batch::pump`] пересылает
//! события в приёмник. Обработчики Tauri-команд в [`crate::commands`] — тонкие обёртки:
//! `Channel` в них превращается в [`FeedSink`], поэтому тесты подставляют простой приёмник.

use std::io;
use std::io::Read as _;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytetap_core::journal::{Autoresponder, Stats, default_log_dir};
use bytetap_core::session::{
    Session, SessionConfig as CoreConfig, SessionHandle, journal_failure_reason,
};
use bytetap_core::transport::Source;
use bytetap_core::transport::serial::{
    Flow, MAX_BAUD, MIN_BAUD, Parity, SerialFormat, SerialSettings,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::api::{SerialConfig, SessionConfig, SessionStats, SessionSummary, StartInfo};
use crate::batch::{self, FLUSH_INTERVAL, FeedSink, PumpEnd};
use crate::error::{CommandError, ErrorCode};

/// Пауза между попытками по умолчанию: подключение и переоткрытие порта (M3-AC1).
pub const DEFAULT_RECONNECT_SEC: u64 = 5;

/// Предел интервала переподключения из формы: 0…3600 секунд (M3-AC1).
pub const MAX_RECONNECT_SEC: u64 = 3600;

/// Настройки запуска после проверки полей формы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Откуда берутся соединения: TCP-сервер, TCP-клиент или COM-порт.
    pub source: Source,
    /// Профиль автоответчика.
    pub responder: Autoresponder,
    /// Отвечать `NAK` на неверную контрольную сумму ASTM (FR-17).
    pub astm_nak_on_bad_checksum: bool,
    /// Папка журналов.
    pub log_dir: PathBuf,
    /// Модель анализатора для заголовка журнала.
    pub analyzer: Option<String>,
    /// Комментарий инженера для заголовка журнала.
    pub note: Option<String>,
}

impl SessionConfig {
    /// Проверяет поля формы и превращает их в настройки моста.
    ///
    /// Нереализованные режимы, чужие поля и неверные значения отвергаются здесь,
    /// до создания файла журнала: интерфейс может прислать их из заглушки или из
    /// общей формы настроек, но сессия не должна начаться наполовину.
    pub fn into_bridge(self) -> Result<BridgeConfig, CommandError> {
        let mode = self.mode.trim();
        let source = match mode {
            "tcp_server" => {
                refuse_foreign(&[
                    ("connectHost", self.connect_host.is_some()),
                    ("connectPort", self.connect_port.is_some()),
                    ("reconnectSec", self.reconnect_sec.is_some()),
                    ("serial", self.serial.is_some()),
                ])?;
                Source::TcpServer {
                    listen: listen_addr(self.listen_host.as_deref(), self.listen_port)?,
                }
            }
            "tcp_client" => {
                refuse_foreign(&[
                    ("listenHost", self.listen_host.is_some()),
                    ("listenPort", self.listen_port.is_some()),
                    ("serial", self.serial.is_some()),
                ])?;
                Source::TcpClient {
                    addr: connect_addr(self.connect_host.as_deref(), self.connect_port)?,
                    reconnect: Duration::from_secs(reconnect_sec(self.reconnect_sec)?),
                }
            }
            "serial" => {
                refuse_foreign(&[
                    ("listenHost", self.listen_host.is_some()),
                    ("listenPort", self.listen_port.is_some()),
                    ("connectHost", self.connect_host.is_some()),
                    ("connectPort", self.connect_port.is_some()),
                    ("reconnectSec", self.reconnect_sec.is_some()),
                ])?;
                let settings = self.serial.clone().ok_or_else(|| {
                    field_error(
                        ErrorCode::PortSettings,
                        "serial",
                        "настройки порта не переданы",
                    )
                })?;
                Source::Serial {
                    settings: serial_settings(settings)?,
                    reconnect: Duration::from_secs(DEFAULT_RECONNECT_SEC),
                }
            }
            other => {
                return Err(CommandError::new(
                    ErrorCode::Io,
                    format!(
                        "режим `{other}` недоступен: выберите tcp_server, tcp_client или serial"
                    ),
                ));
            }
        };
        let responder = match self.responder.trim() {
            "auto" => Autoresponder::Auto,
            "hl7" => Autoresponder::Hl7Mllp,
            "astm" => Autoresponder::AstmE1381,
            "none" => Autoresponder::None,
            other => {
                return Err(CommandError::new(
                    ErrorCode::Io,
                    format!("автоответчик `{other}` неизвестен: auto, hl7, astm или none"),
                ));
            }
        };
        let log_dir = match self.log_dir.trim() {
            "" => default_log_dir().ok_or_else(|| {
                CommandError::new(
                    ErrorCode::LogDir,
                    "не удалось определить папку журналов: укажите каталог явно",
                )
            })?,
            dir => PathBuf::from(dir),
        };
        Ok(BridgeConfig {
            source,
            responder,
            astm_nak_on_bad_checksum: self.astm_nak_on_bad_checksum,
            log_dir,
            analyzer: non_empty(self.analyzer),
            note: non_empty(self.note),
        })
    }
}

/// Строка формы: пустая или из пробелов — «не указано».
fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Ошибка поля формы: `params.field` называет поле, к которому относится текст (M3).
fn field_error(code: ErrorCode, field: &str, message: impl Into<String>) -> CommandError {
    CommandError::new(code, message).with_param("field", field)
}

/// Поле чужого режима в настройках: интерфейс шлёт только поля своего режима.
fn refuse_foreign(fields: &[(&str, bool)]) -> Result<(), CommandError> {
    match fields.iter().find(|(_, set)| *set) {
        Some((name, _)) => Err(field_error(
            ErrorCode::Io,
            name,
            format!("поле `{name}` не относится к выбранному режиму"),
        )),
        None => Ok(()),
    }
}

/// Адрес прослушивания TCP-сервера: пустой узел — все интерфейсы, порт `0` — любой.
fn listen_addr(host: Option<&str>, port: Option<u16>) -> Result<SocketAddr, CommandError> {
    let host = host.unwrap_or("").trim();
    let ip: Ipv4Addr = if host.is_empty() {
        Ipv4Addr::UNSPECIFIED
    } else {
        host.parse().map_err(|_| {
            CommandError::new(
                ErrorCode::AddressUnavailable,
                format!("`{host}` — не IPv4-адрес этого компьютера"),
            )
            .with_param("addr", host)
            .with_param("field", "listenHost")
        })?
    };
    let port = port.ok_or_else(|| {
        // Форма всегда присылает порт: сюда попадает только сломанный запрос, и поле
        // `port` интерфейса тут не при чём — ошибка относится к форме целиком.
        field_error(ErrorCode::Io, "listenPort", "порт прослушивания не указан")
    })?;
    Ok(SocketAddr::from((ip, port)))
}

/// Адрес анализатора `HOST:PORT`: имя узла разрешается ядром при каждой попытке (FR-2).
fn connect_addr(host: Option<&str>, port: Option<u16>) -> Result<String, CommandError> {
    let host = host.unwrap_or("").trim();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(field_error(
            ErrorCode::AddressInvalid,
            "connectHost",
            "адрес анализатора пуст или содержит пробелы",
        ));
    }
    let port = port.filter(|port| *port != 0).ok_or_else(|| {
        field_error(
            ErrorCode::AddressInvalid,
            "connectPort",
            "порт анализатора не указан или равен нулю",
        )
    })?;
    // IPv6 без скобок не разобрался бы как HOST:PORT: двоеточий в адресе больше одного.
    if host.starts_with('[') || host.parse::<Ipv6Addr>().is_err() {
        Ok(format!("{host}:{port}"))
    } else {
        Ok(format!("[{host}]:{port}"))
    }
}

/// Интервал переподключения из формы: по умолчанию 5 секунд, предел 0…3600 (M3-AC1).
fn reconnect_sec(sec: Option<u64>) -> Result<u64, CommandError> {
    let sec = sec.unwrap_or(DEFAULT_RECONNECT_SEC);
    if sec > MAX_RECONNECT_SEC {
        return Err(field_error(
            ErrorCode::Io,
            "reconnectSec",
            format!("интервал переподключения {sec} с больше предела {MAX_RECONNECT_SEC} с"),
        ));
    }
    Ok(sec)
}

/// Настройки COM-порта из формы; поля проверяются до открытия порта (FR-11).
fn serial_settings(config: SerialConfig) -> Result<SerialSettings, CommandError> {
    let port = config.port.trim();
    if port.is_empty() {
        return Err(field_error(
            ErrorCode::PortSettings,
            "port",
            "порт не выбран",
        ));
    }
    if !(MIN_BAUD..=MAX_BAUD).contains(&config.baud) {
        return Err(field_error(
            ErrorCode::PortSettings,
            "baud",
            format!(
                "скорость {} вне диапазона {MIN_BAUD}…{MAX_BAUD}",
                config.baud
            ),
        ));
    }
    if !matches!(config.data_bits, 7 | 8) {
        return Err(field_error(
            ErrorCode::PortSettings,
            "dataBits",
            format!("битов данных {}: допустимо 7 или 8", config.data_bits),
        ));
    }
    let parity = match config.parity.trim() {
        "none" => Parity::None,
        "even" => Parity::Even,
        "odd" => Parity::Odd,
        "mark" => Parity::Mark,
        "space" => Parity::Space,
        other => {
            return Err(field_error(
                ErrorCode::PortSettings,
                "parity",
                format!("чётность `{other}`: none, even, odd, mark или space"),
            ));
        }
    };
    if !matches!(config.stop_bits, 1 | 2) {
        return Err(field_error(
            ErrorCode::PortSettings,
            "stopBits",
            format!("стоп-битов {}: допустимо 1 или 2", config.stop_bits),
        ));
    }
    let flow = Flow::parse(&config.flow).ok_or_else(|| {
        field_error(
            ErrorCode::PortSettings,
            "flow",
            format!(
                "управление потоком `{}` неизвестно: none, rtscts или xonxoff",
                config.flow.trim()
            ),
        )
    })?;
    let format = SerialFormat {
        data_bits: config.data_bits,
        parity,
        stop_bits: config.stop_bits,
    };
    Ok(SerialSettings::new(port, config.baud, format)
        .with_flow(flow)
        .with_lines(config.dtr, config.rts))
}

/// Код ошибки старта по ошибке ядра.
///
/// Ядро сначала занимает адрес или открывает порт, потом создаёт журнал, поэтому всё,
/// что не про адрес и не про порт, — про журнал: папка недоступна или файл не создан.
fn start_error(err: &io::Error, source: &Source) -> CommandError {
    match source {
        Source::TcpServer { listen } => tcp_error(err, listen.port()),
        Source::TcpClient { .. } => CommandError::from_io_with(ErrorCode::Io, err),
        Source::Serial { settings, .. } => serial_error(err, settings),
    }
}

/// Ошибка старта TCP-сервера: занятый порт и отказ журнала (M1).
fn tcp_error(err: &io::Error, port: u16) -> CommandError {
    match CommandError::code_for(err.kind()) {
        ErrorCode::PortInUse => CommandError::new(ErrorCode::PortInUse, err.to_string())
            .with_param("port", port.to_string()),
        ErrorCode::Io => CommandError::new(ErrorCode::Journal, err.to_string()),
        code => CommandError::new(code, err.to_string()),
    }
}

/// Ошибка старта COM-порта: нет порта, занят, нет прав, неверные настройки (M3-AC3).
///
/// `serialport` отдаёт `NoDevice` и для отсутствующего порта, и для занятого: различие —
/// по списку портов, который уже умеет перечислять (M3-AC4). Вызов блокирующий, но он
/// бывает только на отказе старта, когда окно всё равно ждёт ошибку.
fn serial_error(err: &io::Error, settings: &SerialSettings) -> CommandError {
    let code = match err.kind() {
        io::ErrorKind::InvalidInput => ErrorCode::PortSettings,
        io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        io::ErrorKind::NotFound => {
            if port_listed(&settings.port) {
                ErrorCode::PortBusy
            } else {
                ErrorCode::PortNotFound
            }
        }
        _ => ErrorCode::Serial,
    };
    CommandError::from_io_with(code, err).with_param("port", &settings.port)
}

/// Порт есть в списке доступных, значит отказ — это занятость, а не отсутствие порта.
fn port_listed(port: &str) -> bool {
    bytetap_core::transport::serial::list_ports().is_ok_and(|ports| {
        ports
            .iter()
            .any(|info| info.name.eq_ignore_ascii_case(port))
    })
}

/// Начало сессии из заголовка, который ядро уже записало в журнал.
fn started_at_of(path: &Path) -> Option<String> {
    let mut line = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_string(&mut line)
        .ok()?;
    let first = line.lines().next()?;
    let header: serde_json::Value = serde_json::from_str(first).ok()?;
    Some(header.get("started_at")?.as_str()?.to_owned())
}

/// Запущенная сессия вместе с задачей пересылки событий.
pub struct ActiveSession {
    handle: SessionHandle,
    pump: JoinHandle<PumpEnd>,
    sink: Arc<dyn FeedSink>,
    journal_path: PathBuf,
    /// То, что мост вернул на старт: переживает перезагрузку страницы (M2-AC1).
    info: StartInfo,
}

impl ActiveSession {
    /// Комментарий в идущую сессию (FR-23).
    fn note(&self, text: String) -> Result<(), CommandError> {
        self.handle
            .note(text)
            .map_err(|err| CommandError::from_io_with(ErrorCode::Journal, &err))
    }

    /// Останавливает сессию: журнал получает `session_end`, лента — `stopped`.
    ///
    /// Сначала ядро закрывает журнал, потом лента дописывает последние события и только
    /// затем уходит `stopped`: интерфейс не должен увидеть остановку раньше её событий.
    /// Отказ на финальной фиксации заканчивает ленту той же причиной, что и ошибка команды:
    /// интерфейс покажет баннер AC15, а не молча потеряет хвост журнала (NFR-5).
    async fn stop(self) -> Result<SessionSummary, CommandError> {
        let Self {
            handle,
            pump,
            sink,
            journal_path,
            ..
        } = self;
        let path = journal_path.to_string_lossy().into_owned();
        let result = handle.stop().await;
        // Задача пересылки заканчивается сама: сессия выгрузила отправителя событий.
        let end = pump.await.unwrap_or(PumpEnd::Closed);
        if end == PumpEnd::Closed {
            match result.as_ref().err().and_then(journal_failure_reason) {
                Some(reason) => sink.send(batch::journal_failed_batch(reason)),
                None => sink.send(batch::user_stopped_batch()),
            }
        }
        let stats = result.map_err(|err| match journal_failure_reason(&err) {
            Some(reason) => CommandError::new(ErrorCode::Journal, reason.to_string()).with_param(
                "failure",
                crate::feed::JournalFailureCode::from_failure(reason).as_str(),
            ),
            None => CommandError::from_io(&err),
        })?;
        Ok(SessionSummary {
            stats: stats.into(),
            journal_path: path,
        })
    }
}

impl From<Stats> for SessionStats {
    fn from(stats: Stats) -> Self {
        Self {
            connections: stats.connections,
            bytes_analyzer_to_lis: stats.bytes_analyzer_to_lis,
            bytes_lis_to_analyzer: stats.bytes_lis_to_analyzer,
        }
    }
}

/// Запускает сессию на настроенном источнике и начинает пересылку событий.
///
/// Папка журналов создаётся до старта: недоступная папка — ошибка `log_dir`, а не
/// непонятный отказ журнала посреди работы (FR-19).
pub async fn start_session(
    config: BridgeConfig,
    sink: Arc<dyn FeedSink>,
    flush: Duration,
    generation: u64,
) -> Result<(ActiveSession, StartInfo), CommandError> {
    std::fs::create_dir_all(&config.log_dir).map_err(|err| {
        CommandError::from_io_with(ErrorCode::LogDir, &err)
            .with_param("dir", config.log_dir.to_string_lossy())
    })?;
    let mut core = match config.source.clone() {
        Source::TcpServer { listen } => CoreConfig::new(listen),
        Source::TcpClient { addr, reconnect } => CoreConfig::tcp_client(addr, reconnect),
        Source::Serial {
            settings,
            reconnect,
        } => CoreConfig::serial(settings, reconnect),
    }
    .with_responder(config.responder)
    .with_astm_nak(config.astm_nak_on_bad_checksum)
    .with_log_dir(&config.log_dir);
    if let Some(analyzer) = config.analyzer.clone() {
        core = core.with_analyzer(analyzer);
    }
    if let Some(note) = config.note.clone() {
        core = core.with_note(note);
    }

    let (handle, feed) = Session::start_with_feed(core)
        .await
        .map_err(|err| start_error(&err, &config.source))?;
    let journal_path = handle.journal_path().to_path_buf();
    let started_at =
        started_at_of(&journal_path).unwrap_or_else(bytetap_core::journal::now_rfc3339);
    let failure = handle.journal_failure();
    let pump = tokio::spawn(batch::pump(
        feed,
        failure,
        handle.counters(),
        Arc::clone(&sink),
        flush,
    ));
    let info = StartInfo {
        generation,
        journal_path: journal_path.to_string_lossy().into_owned(),
        local_addr: handle.local_addr().to_string(),
        started_at,
    };
    Ok((
        ActiveSession {
            handle,
            pump,
            sink,
            journal_path,
            info: info.clone(),
        },
        info,
    ))
}

/// Что сейчас с записью.
#[derive(Default)]
enum Slot {
    /// Сессии нет.
    #[default]
    Idle,
    /// Сессия запускается или останавливается: второй старт уже запрещён.
    Busy,
    /// Сессия идёт.
    Running(Box<ActiveSession>),
}

/// Предел ожидания остановки при закрытии окна (NFR-5).
///
/// У писателя журнала свой предел в десять секунд; пятнадцать секунд здесь — запас на
/// очередь и на медленную машину. По истечении окно закрывается принудительно, но
/// пользователь об этом узнаёт: журнал мог быть дописан не полностью.
pub const STOP_WAIT: Duration = Duration::from_secs(15);

/// Одна активная сессия приложения.
///
/// Замок здесь обычный, [`std::sync::Mutex`]: он не держится через `await`, поэтому
/// команда и обработчик закрытия окна не могут заклинить друг друга. Пока сессия
/// запускается или останавливается, слот занят: повторный старт получает
/// [`ErrorCode::AlreadyRunning`], а не второй журнал. После отказа журнала мост
/// останавливает сессию сам и освобождает слот (NFR-5).
#[derive(Default, Clone)]
pub struct Sessions {
    slot: Arc<Mutex<Slot>>,
    /// Счётчик поколений: каждая сессия получает свой номер, растущий на каждом старте.
    generation: Arc<AtomicU64>,
}

/// Сброс `Busy` при панике: замок не должен остаться занятым навсегда.
struct BusyGuard<'a> {
    slot: &'a Mutex<Slot>,
    armed: bool,
}

impl<'a> BusyGuard<'a> {
    fn new(slot: &'a Mutex<Slot>) -> Self {
        Self { slot, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Slot::Idle;
        }
    }
}

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Идёт ли запись. `true` и пока сессия останавливается: закрывать окно до конца
    /// остановки нельзя, иначе журнал не получит `session_end` (NFR-5).
    pub fn is_running(&self) -> bool {
        !matches!(*self.lock(), Slot::Idle)
    }

    /// Идущая сессия: то, что вернул её старт. `None` — записи нет.
    ///
    /// Нужно перезагруженной странице: она не получала `StartInfo`, но может предложить
    /// «Остановить» и не оставить сессию сиротой (M2-AC1).
    #[must_use]
    pub fn status(&self) -> Option<StartInfo> {
        match &*self.lock() {
            Slot::Running(session) => Some(session.info.clone()),
            _ => None,
        }
    }

    /// Запускает сессию, если другой нет.
    ///
    /// Слот, занятый завершившейся сессией (отказ журнала), освобождается: ядро уже
    /// ничего не пишет, и новый старт не должен получать `already_running`.
    ///
    /// # Errors
    ///
    /// `already_running`, если запись идёт или останавливается; ошибки старта ядра.
    pub async fn start(
        &self,
        config: SessionConfig,
        sink: Arc<dyn FeedSink>,
    ) -> Result<StartInfo, CommandError> {
        let stale = {
            let mut slot = self.lock();
            match std::mem::replace(&mut *slot, Slot::Busy) {
                Slot::Idle => None,
                Slot::Running(session) if session.handle.is_finished() => Some(session),
                other => {
                    *slot = other;
                    return Err(already_running());
                }
            }
        };
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let mut guard = BusyGuard::new(&self.slot);
        if let Some(session) = stale {
            // Дожидаемся ленты старой сессии в фоне: `stopped` должна уйти в её канал.
            tokio::spawn(async move {
                let _ = session.stop().await;
            });
        }
        let started = match config.into_bridge() {
            Ok(config) => start_session(config, sink, FLUSH_INTERVAL, generation).await,
            Err(err) => Err(err),
        };
        let mut slot = self.lock();
        match started {
            Ok((session, info)) => {
                let failure = session.handle.journal_failure();
                *slot = Slot::Running(Box::new(session));
                guard.disarm();
                drop(slot);
                self.stop_after_failure(failure);
                Ok(info)
            }
            Err(err) => {
                *slot = Slot::Idle;
                guard.disarm();
                Err(err)
            }
        }
    }

    /// Останавливает сессию сама, как только писатель журнала откажет (NFR-5).
    ///
    /// Слот освобождается без участия интерфейса: перезагруженная страница не должна
    /// оставлять мост занятым. Вызов `stop_session` из интерфейса после этого безвреден.
    fn stop_after_failure(
        &self,
        mut failure: watch::Receiver<Option<bytetap_core::journal::JournalFailureReason>>,
    ) {
        let sessions = self.clone();
        tokio::spawn(async move {
            loop {
                if failure.borrow_and_update().is_some() {
                    break;
                }
                if failure.changed().await.is_err() {
                    // Сессия закончилась штатно: останавливать нечего.
                    return;
                }
            }
            let _ = sessions.stop(None).await;
        });
    }

    /// Останавливает сессию и возвращает итоги.
    ///
    /// `generation` — номер поколения из [`StartInfo`] того, кто просит остановку. `None`
    /// останавливает любую сессию: так работает закрытие окна. Чужое поколение — это
    /// страница, которая пережила перезапуск сессии, и она не должна останавливать новую.
    ///
    /// # Errors
    ///
    /// `journal`, если журнал отказал: записанное до сбоя сохранено, но `session_end`
    /// в файле может не быть. `io` — если записи не было или поколение чужое.
    pub async fn stop(&self, generation: Option<u64>) -> Result<SessionSummary, CommandError> {
        let (session, mut guard) = {
            let mut slot = self.lock();
            match std::mem::replace(&mut *slot, Slot::Busy) {
                Slot::Running(session) if matches_generation(&session.info, generation) => {
                    (session, BusyGuard::new(&self.slot))
                }
                Slot::Running(session) => {
                    // Чужое поколение: новая сессия остаётся работать, слот возвращается.
                    *slot = Slot::Running(session);
                    return Err(CommandError::new(
                        ErrorCode::Io,
                        "эта сессия уже закончилась: остановлена другая",
                    ));
                }
                other => {
                    let error = match other {
                        Slot::Busy => {
                            CommandError::new(ErrorCode::Io, "сессия ещё запускается: повторите")
                        }
                        _ => CommandError::new(ErrorCode::Io, "сессия не запущена"),
                    };
                    *slot = match other {
                        Slot::Busy => Slot::Busy,
                        _ => Slot::Idle,
                    };
                    return Err(error);
                }
            }
        };
        let result = session.stop().await;
        *self.lock() = Slot::Idle;
        guard.disarm();
        result
    }

    /// Останавливает сессию, если она идёт: закрытие окна во время записи (NFR-5).
    ///
    /// Если остановка уже начата командой «Стоп», окно ждёт её окончания, а не выходит
    /// посреди записи журнала. Дольше [`STOP_WAIT`] не ждёт: зависшая фиксация не должна
    /// держать окно вечно, и вызывающий закрывает окно принудительно с предупреждением.
    ///
    /// # Errors
    ///
    /// `io`: остановка не уложилась в [`STOP_WAIT`].
    pub async fn stop_if_running(&self) -> Result<(), CommandError> {
        let deadline = tokio::time::Instant::now() + STOP_WAIT;
        loop {
            let state = match &*self.lock() {
                Slot::Idle => 0,
                Slot::Running(_) => 1,
                Slot::Busy => 2,
            };
            match state {
                0 => return Ok(()),
                1 => {
                    let _ = self.stop(None).await;
                    return Ok(());
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CommandError::new(
                    ErrorCode::Io,
                    "сессия не остановилась за отведённое время",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Комментарий в идущую сессию (FR-23).
    ///
    /// # Errors
    ///
    /// `journal`, если сессия уже остановлена или журнал отказал.
    pub fn note(&self, text: impl Into<String>) -> Result<(), CommandError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(CommandError::new(
                ErrorCode::Io,
                "пустой комментарий не записывается",
            ));
        }
        match &*self.lock() {
            Slot::Running(session) => session.note(text),
            _ => Err(CommandError::new(
                ErrorCode::Io,
                "сессия не идёт: комментарий некуда добавить",
            )),
        }
    }
}

/// Просит ли остановку владелец этого поколения сессии.
///
/// `None` — остановка без номера: закрытие окна и выход приложения останавливают любую.
fn matches_generation(info: &StartInfo, generation: Option<u64>) -> bool {
    generation.is_none_or(|asked| asked == info.generation)
}

/// Ошибка повторного старта.
fn already_running() -> CommandError {
    CommandError::new(
        ErrorCode::AlreadyRunning,
        "запись уже идёт: сначала остановите текущую сессию",
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use bytetap_core::journal::{Event, read_all};
    use bytetap_testkit::emulator::AnalyzerEmulator;
    use bytetap_testkit::fixtures_dir;
    use tokio::io::AsyncWriteExt as _;

    use super::*;
    use crate::batch::test_sink::TestSink;
    use crate::feed::{FeedDir, FeedEvent, StopReason};
    use crate::journal::{JournalCache, open, read_page};

    fn config(dir: &Path, port: u16) -> SessionConfig {
        SessionConfig {
            mode: "tcp_server".to_owned(),
            listen_host: Some("127.0.0.1".to_owned()),
            listen_port: Some(port),
            connect_host: None,
            connect_port: None,
            reconnect_sec: None,
            serial: None,
            responder: "none".to_owned(),
            astm_nak_on_bad_checksum: false,
            analyzer: " ANALYZER ".to_owned(),
            note: String::new(),
            log_dir: dir.to_string_lossy().into_owned(),
        }
    }

    /// Настройки клиента: мост подключается к анализатору-серверу (FR-2).
    fn client_config(dir: &Path, addr: SocketAddr) -> SessionConfig {
        SessionConfig {
            mode: "tcp_client".to_owned(),
            listen_host: None,
            listen_port: None,
            connect_host: Some(addr.ip().to_string()),
            connect_port: Some(addr.port()),
            reconnect_sec: Some(1),
            serial: None,
            responder: "hl7".to_owned(),
            astm_nak_on_bad_checksum: false,
            analyzer: "ANALYZER".to_owned(),
            note: String::new(),
            log_dir: dir.to_string_lossy().into_owned(),
        }
    }

    /// Настройки COM-порта по умолчанию: 9600 8N1, DTR и RTS подняты (FR-11).
    fn serial_config(dir: &Path, port: &str) -> SessionConfig {
        SessionConfig {
            mode: "serial".to_owned(),
            listen_host: None,
            listen_port: None,
            connect_host: None,
            connect_port: None,
            reconnect_sec: None,
            serial: Some(SerialConfig {
                port: port.to_owned(),
                baud: 9600,
                data_bits: 8,
                parity: "none".to_owned(),
                stop_bits: 1,
                flow: "none".to_owned(),
                dtr: true,
                rts: true,
            }),
            responder: "auto".to_owned(),
            astm_nak_on_bad_checksum: false,
            analyzer: String::new(),
            note: String::new(),
            log_dir: dir.to_string_lossy().into_owned(),
        }
    }

    async fn wait_for(mut ready: impl FnMut() -> bool) {
        for _ in 0..200 {
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("событие не дошло до интерфейса за две секунды");
    }

    #[tokio::test]
    async fn start_and_stop_record_a_session_and_end_the_feed() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес");
        assert_ne!(addr.port(), 0, "порт 0 заменён фактическим");
        assert!(Path::new(&info.journal_path).exists(), "журнал создан");
        assert!(sessions.is_running());

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("подключение анализатора");
        client
            .write_all(b"\x0bMSH|^~\\&\r\x1c\r")
            .await
            .expect("фрагмент");
        drop(client);
        wait_for(|| {
            sink.events()
                .iter()
                .any(|event| matches!(event, FeedEvent::Data { conn: 1, .. }))
        })
        .await;

        sessions.note("прогнали три пробы").expect("комментарий");
        wait_for(|| {
            sink.events()
                .iter()
                .any(|event| matches!(event, FeedEvent::Note { .. }))
        })
        .await;
        let summary = sessions.stop(None).await.expect("остановка");

        assert_eq!(summary.stats.connections, 1);
        assert_eq!(summary.stats.bytes_analyzer_to_lis, 12);
        assert_eq!(summary.stats.bytes_lis_to_analyzer, 0);
        assert!(!sessions.is_running());

        let events = sink.events();
        assert_eq!(
            events.last(),
            Some(&FeedEvent::Stopped {
                reason: StopReason::User,
                code: None,
                message: None,
            }),
            "лента заканчивается штатной остановкой"
        );

        let outcome = read_all(&summary.journal_path).expect("чтение журнала");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let kinds: Vec<&str> = outcome.lines.iter().map(|line| line.event.kind()).collect();
        assert_eq!(kinds.first(), Some(&"session"));
        assert_eq!(kinds.last(), Some(&"session_end"));
        assert!(kinds.contains(&"note"), "{kinds:?}");
        assert!(kinds.contains(&"open") && kinds.contains(&"data") && kinds.contains(&"close"));
        let Event::Session(header) = &outcome.lines[0].event else {
            panic!("первая строка — заголовок");
        };
        assert_eq!(header.mode.as_str(), "tcp_server");
        assert_eq!(header.analyzer.as_deref(), Some("ANALYZER"));
        assert_eq!(
            header.settings["listen"].as_str(),
            Some(info.local_addr.as_str())
        );

        // После остановки запись можно начать снова: слот свободен.
        let again = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("повторный старт");
        sessions.stop(None).await.expect("повторная остановка");
        assert_ne!(again.journal_path, info.journal_path, "новый файл журнала");
    }

    #[tokio::test]
    async fn a_second_start_while_running_is_refused() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");

        let error = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect_err("второй старт запрещён");
        assert_eq!(error.code, ErrorCode::AlreadyRunning);

        sessions.stop(None).await.expect("остановка");
        assert!(!sessions.is_running());
    }

    /// «Стоп» из чужой страницы не трогает сессию, запущенную после её перезагрузки (NFR-5).
    #[tokio::test]
    async fn a_stale_generation_does_not_stop_a_newer_session() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let first = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("первый старт");
        sessions
            .stop(Some(first.generation))
            .await
            .expect("своё поколение останавливает");
        let second = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("второй старт");
        assert!(
            second.generation > first.generation,
            "номер поколения растёт: {} → {}",
            first.generation,
            second.generation
        );

        let error = sessions
            .stop(Some(first.generation))
            .await
            .expect_err("старое поколение не останавливает новую сессию");
        assert_eq!(error.code, ErrorCode::Io);
        assert!(sessions.is_running(), "новая сессия продолжает писать");
        assert_eq!(
            sessions.status().map(|info| info.generation),
            Some(second.generation),
            "состояние отдаёт поколение своей сессии"
        );

        sessions
            .stop(Some(second.generation))
            .await
            .expect("своё поколение останавливает и вторую");
        assert!(!sessions.is_running());
    }

    /// Сквозная проверка M3: TCP-клиент через мост к анализатору-серверу (T-2, M3-AC1).
    ///
    /// Эмулятор из `bytetap-testkit` работает сервером: мост подключается сам, лента моста
    /// получает `open` с адресом анализатора, `data` обоих направлений и `close`, а строка
    /// состояния показывает адрес анализатора.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tcp_client_session_shows_open_data_and_close_in_the_feed() {
        use bytetap_testkit::server::AnalyzerServer;

        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let server = AnalyzerServer::bind("127.0.0.1:0".parse().expect("адрес"))
            .await
            .expect("сервер анализатора");
        let addr = server.local_addr().expect("адрес сервера");
        let message = std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7"))
            .expect("фикстура analyzer_oru_r01.hl7");
        let server = tokio::spawn(
            server
                .with_connection([message])
                .with_timeout(Duration::from_secs(10))
                .run(),
        );

        let info = sessions
            .start(client_config(dir.path(), addr), sink.clone())
            .await
            .expect("старт без доступного анализатора");
        assert_eq!(
            info.local_addr,
            addr.to_string(),
            "строка состояния показывает, куда идём"
        );

        let report = tokio::time::timeout(Duration::from_secs(20), server)
            .await
            .expect("сервер успел принять соединение")
            .expect("задача сервера")
            .expect("обмен с анализатором");
        assert_eq!(report.connections, 1);
        assert_eq!(report.acks.len(), 1, "мост подтвердил сообщение");
        wait_for(|| {
            sink.events()
                .iter()
                .any(|event| matches!(event, FeedEvent::Close { conn: 1, .. }))
        })
        .await;

        let summary = sessions.stop(None).await.expect("остановка");
        assert_eq!(summary.stats.connections, 1);
        let events = sink.events();
        assert!(
            events.iter().any(|event| matches!(
                event,
                FeedEvent::Open { conn: 1, peer, .. } if peer == &addr.to_string()
            )),
            "open пишется после подключения, peer — адрес анализатора: {events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            FeedEvent::Data {
                conn: 1,
                dir: FeedDir::A2l,
                synthetic: false,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            FeedEvent::Data {
                conn: 1,
                dir: FeedDir::L2a,
                synthetic: true,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            FeedEvent::Close {
                conn: 1,
                reason,
                ..
            } if reason == "eof"
        )));

        let outcome = read_all(&summary.journal_path).expect("чтение журнала");
        let Event::Session(header) = &outcome.lines[0].event else {
            panic!("первая строка — заголовок");
        };
        assert_eq!(header.mode.as_str(), "tcp_client");
        assert_eq!(header.autoresponder, Autoresponder::Hl7Mllp);
        assert_eq!(header.settings["connect"], addr.to_string());
        assert_eq!(header.settings["reconnect_sec"], 1);
    }

    #[tokio::test]
    async fn a_busy_port_is_reported_as_port_in_use() {
        let _serial = serial().await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("занятый порт");
        let port = listener.local_addr().expect("адрес").port();
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let error = sessions
            .start(config(dir.path(), port), sink.clone())
            .await
            .expect_err("порт занят");
        assert_eq!(error.code, ErrorCode::PortInUse);
        assert_eq!(
            error.params.as_ref().and_then(|params| params.get("port")),
            Some(&port.to_string())
        );
        assert!(
            std::fs::read_dir(dir.path())
                .expect("папка журналов")
                .next()
                .is_none(),
            "файл журнала не создан"
        );

        // Неудачный старт не занимает слот: следующая попытка проходит.
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("свободный порт");
        assert!(!info.local_addr.is_empty());
        sessions.stop(None).await.expect("остановка");
    }

    #[tokio::test]
    async fn an_unknown_responder_does_not_start_a_session() {
        let _serial = serial().await;
        let sessions = Sessions::default();
        let mut cfg = config(Path::new("/tmp"), 0);
        cfg.responder = "mllp".to_owned();
        let error = sessions
            .start(cfg, Arc::new(TestSink::default()))
            .await
            .expect_err("неизвестный профиль");
        assert_eq!(error.code, ErrorCode::Io);
        assert!(!sessions.is_running(), "слот освобождён после отказа");
    }

    /// Три режима M3 превращаются в источники ядра, поля не путаются между режимами.
    #[test]
    fn form_fields_are_validated_before_start() {
        let valid = config(Path::new("/tmp/bytetap"), 5000)
            .into_bridge()
            .expect("поля");
        assert_eq!(
            valid.source,
            Source::TcpServer {
                listen: "127.0.0.1:5000".parse().unwrap()
            }
        );
        assert_eq!(valid.responder, Autoresponder::None);
        assert_eq!(valid.analyzer.as_deref(), Some("ANALYZER"));
        assert_eq!(valid.note, None);
        assert!(!valid.astm_nak_on_bad_checksum, "NAK по умолчанию выключен");

        let mut all = config(Path::new("/tmp/bytetap"), 5000);
        all.listen_host = Some(String::new());
        all.responder = "hl7".to_owned();
        let all = all.into_bridge().expect("все интерфейсы");
        assert_eq!(
            all.source,
            Source::TcpServer {
                listen: "0.0.0.0:5000".parse().unwrap()
            }
        );
        assert_eq!(all.responder, Autoresponder::Hl7Mllp);

        let mut wrong_host = config(Path::new("/tmp/bytetap"), 5000);
        wrong_host.listen_host = Some("localhost".to_owned());
        assert_eq!(
            wrong_host.into_bridge().expect_err("не IPv4").code,
            ErrorCode::AddressUnavailable
        );

        let mut no_port = config(Path::new("/tmp/bytetap"), 5000);
        no_port.listen_port = None;
        let error = no_port.into_bridge().expect_err("порт не указан");
        assert_eq!(error.code, ErrorCode::Io);
        assert_eq!(field_of(&error), Some("listenPort"));

        let mut proxy = config(Path::new("/tmp/bytetap"), 5000);
        proxy.mode = "proxy".to_owned();
        assert!(proxy.into_bridge().is_err(), "режим M4 не стартует");

        // Поле чужого режима отвергается, а не теряется молча (контракт M3).
        let mut foreign = config(Path::new("/tmp/bytetap"), 5000);
        foreign.connect_port = Some(5000);
        let error = foreign.into_bridge().expect_err("поле клиента в сервере");
        assert_eq!(field_of(&error), Some("connectPort"));
    }

    /// TCP-клиент: адрес анализатора собирается из узла и порта, пауза по умолчанию 5 с.
    #[test]
    fn client_fields_become_a_core_source() {
        let mut cfg = client_config(Path::new("/tmp/bytetap"), "10.0.0.5:5000".parse().unwrap());
        cfg.responder = "auto".to_owned();
        cfg.astm_nak_on_bad_checksum = true;
        let bridge = cfg.into_bridge().expect("поля клиента");
        assert_eq!(
            bridge.source,
            Source::TcpClient {
                addr: "10.0.0.5:5000".to_owned(),
                reconnect: Duration::from_secs(1),
            }
        );
        assert_eq!(bridge.responder, Autoresponder::Auto);
        assert!(bridge.astm_nak_on_bad_checksum);

        let mut default_pause = client_config(Path::new("/tmp"), "10.0.0.5:5000".parse().unwrap());
        default_pause.reconnect_sec = None;
        assert_eq!(
            default_pause
                .into_bridge()
                .expect("пауза по умолчанию")
                .source,
            Source::TcpClient {
                addr: "10.0.0.5:5000".to_owned(),
                reconnect: Duration::from_secs(DEFAULT_RECONNECT_SEC),
            }
        );

        let mut no_host = client_config(Path::new("/tmp"), "10.0.0.5:5000".parse().unwrap());
        no_host.connect_host = Some("  ".to_owned());
        assert_eq!(
            field_of(&no_host.into_bridge().expect_err("пустой адрес")),
            Some("connectHost")
        );

        let mut zero_port = client_config(Path::new("/tmp"), "10.0.0.5:5000".parse().unwrap());
        zero_port.connect_port = Some(0);
        assert_eq!(
            field_of(&zero_port.into_bridge().expect_err("порт 0")),
            Some("connectPort")
        );

        let mut slow = client_config(Path::new("/tmp"), "10.0.0.5:5000".parse().unwrap());
        slow.reconnect_sec = Some(MAX_RECONNECT_SEC + 1);
        assert_eq!(
            field_of(&slow.into_bridge().expect_err("интервал велик")),
            Some("reconnectSec")
        );

        // IPv6 без скобок разобрался бы как два порта: мост добавляет скобки.
        let mut v6 = client_config(Path::new("/tmp"), "10.0.0.5:5000".parse().unwrap());
        v6.connect_host = Some("::1".to_owned());
        assert_eq!(
            v6.into_bridge().expect("IPv6").source,
            Source::TcpClient {
                addr: "[::1]:5000".to_owned(),
                reconnect: Duration::from_secs(1),
            }
        );
    }

    /// COM: настройки формы доходят до ядра целиком, неверные — ошибкой поля (FR-11).
    #[test]
    fn serial_fields_become_the_port_settings() {
        let mut cfg = serial_config(Path::new("/tmp/bytetap"), "COM5");
        cfg.serial = Some(SerialConfig {
            port: " COM5 ".to_owned(),
            baud: 115_200,
            data_bits: 7,
            parity: "even".to_owned(),
            stop_bits: 2,
            flow: "rtscts".to_owned(),
            dtr: false,
            rts: true,
        });
        cfg.responder = "astm".to_owned();
        let bridge = cfg.into_bridge().expect("поля порта");
        assert_eq!(
            bridge.source,
            Source::Serial {
                settings: SerialSettings::new(
                    "COM5",
                    115_200,
                    SerialFormat {
                        data_bits: 7,
                        parity: Parity::Even,
                        stop_bits: 2,
                    },
                )
                .with_flow(Flow::RtsCts)
                .with_lines(false, true),
                reconnect: Duration::from_secs(DEFAULT_RECONNECT_SEC),
            }
        );
        assert_eq!(bridge.responder, Autoresponder::AstmE1381);
        assert_eq!(bridge.analyzer, None);

        let cases = [
            (
                "port",
                SerialConfig {
                    port: " ".to_owned(),
                    ..serial_default()
                },
            ),
            (
                "baud",
                SerialConfig {
                    baud: 0,
                    ..serial_default()
                },
            ),
            (
                "dataBits",
                SerialConfig {
                    data_bits: 5,
                    ..serial_default()
                },
            ),
            (
                "parity",
                SerialConfig {
                    parity: "always".to_owned(),
                    ..serial_default()
                },
            ),
            (
                "stopBits",
                SerialConfig {
                    stop_bits: 3,
                    ..serial_default()
                },
            ),
            (
                "flow",
                SerialConfig {
                    flow: "hard".to_owned(),
                    ..serial_default()
                },
            ),
        ];
        for (field, settings) in cases {
            let mut cfg = serial_config(Path::new("/tmp"), "COM5");
            cfg.serial = Some(settings);
            let error = cfg.into_bridge().expect_err(field);
            assert_eq!(error.code, ErrorCode::PortSettings, "{field}");
            assert_eq!(field_of(&error), Some(field));
        }

        let mut no_settings = serial_config(Path::new("/tmp"), "COM5");
        no_settings.serial = None;
        assert_eq!(
            field_of(&no_settings.into_bridge().expect_err("нет настроек")),
            Some("serial")
        );
    }

    fn serial_default() -> SerialConfig {
        serial_config(Path::new("/tmp"), "COM5")
            .serial
            .expect("настройки порта в фикстуре")
    }

    /// Отсутствующий порт — ошибка `port_not_found` до создания журнала, не паника (M3-AC4).
    #[tokio::test]
    async fn a_missing_serial_port_is_reported_before_the_journal() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sessions = Sessions::default();
        let error = sessions
            .start(
                serial_config(dir.path(), "/dev/bytetap-no-such-port"),
                Arc::new(TestSink::default()),
            )
            .await
            .expect_err("порта нет");
        assert_eq!(error.code, ErrorCode::PortNotFound);
        assert_eq!(
            error.params.as_ref().and_then(|params| params.get("port")),
            Some(&"/dev/bytetap-no-such-port".to_owned())
        );
        assert!(
            std::fs::read_dir(dir.path())
                .expect("папка журналов")
                .next()
                .is_none(),
            "файл журнала не создан"
        );
        assert!(!sessions.is_running(), "слот свободен");
    }

    fn field_of(error: &CommandError) -> Option<&str> {
        error
            .params
            .as_ref()
            .and_then(|params| params.get("field"))
            .map(String::as_str)
    }

    #[tokio::test]
    async fn closing_the_window_stops_a_running_session() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");

        sessions
            .stop_if_running()
            .await
            .expect("остановка при закрытии");

        assert!(!sessions.is_running());
        let outcome = read_all(&info.journal_path).expect("чтение журнала");
        assert_eq!(
            outcome.lines.last().map(|line| line.event.kind()),
            Some("session_end"),
            "журнал закрыт штатно, а не оборван"
        );
        sessions
            .stop_if_running()
            .await
            .expect("без сессии ничего не делает");
    }

    /// Сквозная проверка M2: запись через мост, затем чтение того же журнала командами.
    ///
    /// Эмулятор анализатора шлёт синтетические HL7/MLLP-сообщения, лента получает `open`,
    /// `data` обоих направлений (ответ автоответчика — `synthetic`), `close` и `stopped`,
    /// а `open_journal` с `read_journal` возвращают те же события из файла.
    #[tokio::test]
    async fn a_recorded_session_is_read_back_through_the_journal_commands() {
        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();

        let mut cfg = config(dir.path(), 0);
        cfg.responder = "hl7".to_owned();
        cfg.analyzer = "ANALYZER".to_owned();
        cfg.note = "три пробы".to_owned();

        let info = sessions.start(cfg, sink.clone()).await.expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес слушателя");
        let messages: Vec<Vec<u8>> = ["analyzer_oru_r01.hl7", "analyzer_qry_q02.hl7"]
            .iter()
            .map(|name| {
                let path = fixtures_dir().join("hl7").join(name);
                std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            })
            .collect();
        let report = AnalyzerEmulator::new(addr)
            .with_messages(messages)
            .run()
            .await
            .expect("обмен с анализатором");
        assert_eq!(
            report.acks.len(),
            2,
            "автоответчик подтвердил оба сообщения"
        );

        let summary = sessions.stop(None).await.expect("остановка");
        assert_eq!(summary.stats.connections, 1);
        assert!(summary.stats.bytes_analyzer_to_lis > 0);
        assert!(summary.stats.bytes_lis_to_analyzer > 0);

        let events = sink.events();
        let count = |predicate: fn(&FeedEvent) -> bool| {
            events.iter().filter(|event| predicate(event)).count()
        };
        assert_eq!(count(|event| matches!(event, FeedEvent::Open { .. })), 1);
        assert!(
            count(|event| matches!(
                event,
                FeedEvent::Data {
                    dir: FeedDir::A2l,
                    synthetic: false,
                    ..
                }
            )) >= 2
        );
        assert!(
            count(|event| matches!(
                event,
                FeedEvent::Data {
                    dir: FeedDir::L2a,
                    synthetic: true,
                    ..
                }
            )) >= 2
        );
        assert_eq!(count(|event| matches!(event, FeedEvent::Close { .. })), 1);
        assert_eq!(
            events.last(),
            Some(&FeedEvent::Stopped {
                reason: StopReason::User,
                code: None,
                message: None,
            })
        );

        let path = Path::new(&summary.journal_path);
        let cache = JournalCache::default();
        let journal = open(path, &cache).await.expect("сводка журнала");
        assert_eq!(journal.header["analyzer"].as_str(), Some("ANALYZER"));
        assert_eq!(journal.header["note"].as_str(), Some("три пробы"));
        assert_eq!(
            journal.stats, summary.stats,
            "итоги журнала и сессии совпали"
        );
        assert_eq!(journal.density.a2l.len(), crate::journal::DENSITY_BUCKETS);

        let page = read_page(path, 0, 10_000, &cache)
            .await
            .expect("страница журнала");
        assert_eq!(page.len(), journal.events, "журнал читается целиком");
        assert_eq!(
            page.iter()
                .filter(|event| matches!(event, FeedEvent::Data { .. }))
                .count(),
            count(|event| matches!(event, FeedEvent::Data { .. })),
            "лента и журнал содержат одни и те же фрагменты"
        );
        assert!(matches!(page.first(), Some(FeedEvent::Open { .. })));
        assert!(matches!(page.last(), Some(FeedEvent::Close { .. })));
    }

    #[test]
    fn started_at_is_read_from_the_header() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("empty.jsonl");
        let mut file = std::fs::File::create(&path).expect("файл");
        file.write_all(b"not a journal\n").expect("запись");
        assert!(started_at_of(&path).is_none());
        assert!(started_at_of(&dir.path().join("missing.jsonl")).is_none());
    }

    /// Хуки ядра — глобальные статики: тесты моста идут по одному, включая те,
    /// что хуки включают. Иначе отказ журнала одного теста ломает соседний.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        SERIAL.lock().await
    }

    /// Отказ журнала освобождает слот без участия интерфейса (NFR-5).
    ///
    /// `stop_session` после отказа возвращает ошибку, но и без него новый старт проходит:
    /// мост сам остановил сессию в фоне и освободил слот.
    #[tokio::test]
    async fn a_journal_failure_frees_the_slot_and_a_new_start_passes() {
        use std::sync::atomic::Ordering;

        use bytetap_core::journal::writer::TEST_FAIL_AFTER_EVENTS;

        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let info = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес");

        // Писатель отказывает на первой же пачке событий.
        TEST_FAIL_AFTER_EVENTS.store(0, Ordering::Relaxed);
        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("анализатор");
        client
            .write_all(b"\x0bMSH|^~\\&\r\x1c\r")
            .await
            .expect("фрагмент");
        drop(client);

        wait_for(|| {
            sink.events().iter().any(|event| {
                matches!(
                    event,
                    FeedEvent::Stopped {
                        reason: StopReason::JournalFailure,
                        ..
                    }
                )
            })
        })
        .await;
        for _ in 0..200 {
            if !sessions.is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);

        assert!(!sessions.is_running(), "мост освободил слот сам");
        assert!(sessions.status().is_none());
        // Вызов интерфейса после отказа безвреден: ошибка ожидаема.
        assert!(
            sessions.stop(None).await.is_err(),
            "останавливать уже нечего"
        );

        let again = sessions
            .start(config(dir.path(), 0), sink.clone())
            .await
            .expect("новый старт после отказа");
        assert_ne!(again.journal_path, info.journal_path, "новый файл журнала");
        sessions.stop(None).await.expect("остановка");
    }

    /// Отказ на финальной фиксации: лента заканчивается `journal_failure`, команда — ошибкой.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_flush_at_the_stop_ends_the_feed_with_a_journal_failure() {
        use std::sync::atomic::Ordering;

        use bytetap_core::journal::writer::{TEST_SYNC_DELAY_MS, TEST_SYNC_DELAYS};
        use bytetap_core::session::TEST_JOURNAL_SHUTDOWN_MS;

        let _serial = serial().await;
        let dir = tempfile::tempdir().expect("временная папка");
        let sink = Arc::new(TestSink::default());
        let sessions = Sessions::default();
        let mut cfg = config(dir.path(), 0);
        cfg.responder = "hl7".to_owned();
        let info = sessions.start(cfg, sink.clone()).await.expect("старт");
        let addr: SocketAddr = info.local_addr.parse().expect("адрес");
        let message = std::fs::read(fixtures_dir().join("hl7").join("analyzer_oru_r01.hl7"))
            .expect("фикстура analyzer_oru_r01.hl7");

        TEST_SYNC_DELAYS.store(0, Ordering::Relaxed);
        TEST_SYNC_DELAY_MS.store(1_000, Ordering::Relaxed);
        TEST_JOURNAL_SHUTDOWN_MS.store(50, Ordering::Relaxed);

        AnalyzerEmulator::new(addr)
            .with_messages([message])
            .run()
            .await
            .expect("обмен с анализатором");
        let waiting = std::time::Instant::now();
        while TEST_SYNC_DELAYS.load(Ordering::Relaxed) == 0 {
            assert!(
                waiting.elapsed() < Duration::from_secs(5),
                "писатель журнала так и не был замедлен"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let error = sessions
            .stop(None)
            .await
            .expect_err("остановка не дождалась журнала");
        TEST_SYNC_DELAY_MS.store(0, Ordering::Relaxed);
        TEST_JOURNAL_SHUTDOWN_MS.store(0, Ordering::Relaxed);

        assert_eq!(error.code, ErrorCode::Journal);
        assert_eq!(
            error
                .params
                .as_ref()
                .and_then(|params| params.get("failure")),
            Some(&"timeout".to_owned())
        );
        assert!(!sessions.is_running(), "слот освобождён");
        let events = sink.events();
        assert!(
            matches!(
                events.last(),
                Some(FeedEvent::Stopped {
                    reason: StopReason::JournalFailure,
                    code: Some(crate::feed::JournalFailureCode::Timeout),
                    ..
                })
            ),
            "лента закончилась отказом журнала: {:?}",
            events.last()
        );
    }
}
