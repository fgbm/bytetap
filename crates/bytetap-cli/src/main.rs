//! Консольная версия bytetap.
//!
//! `capture` записывает обмен: слушает порт, отвечает анализатору и пишет журнал.
//! `verify` проверяет готовый журнал по схеме и печатает статистику, `note` дописывает
//! комментарий инженера.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use base64::Engine as _;
use bytetap_core::journal::{
    self, Autoresponder, Data, Dir, Event, Host, Journal, JournalFailureReason, LineErrorKind,
    Mode, MonoClock, Note, Session, now_rfc3339,
};
use bytetap_core::session::{Session as CaptureSession, SessionConfig, journal_failure_reason};
use bytetap_core::transport::serial::{Flow, SerialFormat, SerialSettings};
use chrono::DateTime;
use clap::{ArgGroup, Parser, Subcommand};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

/// Печатает строку в stdout и не падает, если вывод уже закрыт.
///
/// `println!` паникует на ошибке записи: `bytetap-cli capture | head -1` закрывает трубу,
/// и паника вместо штатной остановки оставила бы журнал без `session_end`. Ошибка вывода
/// здесь ничего не меняет — сообщать о ней некому.
macro_rules! say {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// То же для stderr: сообщение об ошибке не должно само превращаться в панику.
macro_rules! warn {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// Сколько знаков поля `text` показывает лента `capture`.
const FEED_WIDTH: usize = 120;

/// Сколько процесс ждёт завершения задач runtime после остановки сессии.
///
/// Поток записи журнала может застрять в `sync_data`; выход из программы не должен ждать
/// его вечно. Предел совпадает с ожиданием журнала при остановке сессии.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// JSON Schema формата, вшита в бинарь: проверка не зависит от файлов рядом и от сети (FR-24).
const SCHEMA: &str = include_str!("../../../docs/public/schema/bytetap-session-1.schema.json");

/// Запись «сырого» обмена между лабораторным анализатором и ЛИС.
#[derive(Debug, Parser)]
#[command(name = "bytetap-cli", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Записать обмен: ждать анализатор, подключиться к нему или открыть COM-порт
    Capture(CaptureArgs),
    /// Показать доступные COM-порты с понятным описанием (FR-12)
    Ports,
    /// Проверить журнал сессии по схеме и напечатать статистику
    Verify {
        /// Файл журнала `*.jsonl`
        file: PathBuf,
    },
    /// Дописать комментарий инженера в конец журнала
    Note {
        /// Файл журнала `*.jsonl`
        file: PathBuf,
        /// Текст комментария
        text: String,
    },
    /// Нагрузочная запись журнала для теста устойчивости (T-4)
    #[command(name = "_stress-journal", hide = true)]
    StressJournal {
        /// Файл журнала
        #[arg(long)]
        out: PathBuf,
        /// Сколько секунд писать, прежде чем завершиться штатно
        #[arg(long, default_value_t = 30)]
        max_seconds: u64,
    },
}

/// Параметры записи: ровно один источник — прослушивание, подключение или COM-порт.
#[derive(Debug, clap::Args)]
#[command(group(
    ArgGroup::new("source")
        .required(true)
        .multiple(false)
        .args(["listen", "connect", "serial"])
))]
struct CaptureArgs {
    /// Адрес и порт прослушивания, например `0.0.0.0:5000`
    #[arg(long, value_name = "ADDR", conflicts_with_all = ["connect", "serial"])]
    listen: Option<SocketAddr>,
    /// Адрес анализатора-сервера, к которому подключаться, например `192.168.1.50:5000`
    #[arg(long, value_name = "HOST:PORT", conflicts_with_all = ["listen", "serial"])]
    connect: Option<String>,
    /// Имя COM-порта, например `COM5` или `/dev/ttyUSB0`
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with_all = ["listen", "connect"],
        requires_all = ["baud", "format"]
    )]
    serial: Option<String>,
    /// Скорость порта: 1…4 000 000, типовые 300–115200
    #[arg(
        long,
        value_name = "N",
        requires = "serial",
        value_parser = clap::value_parser!(u32).range(1..=4_000_000)
    )]
    baud: Option<u32>,
    /// Формат порта: `<биты><чётность><стоп>`, например `8N1`, `7E2`, `8M1`
    #[arg(long, value_name = "FORMAT", requires = "serial")]
    format: Option<SerialFormat>,
    /// Управление потоком: `none`, `rtscts`, `xonxoff`
    #[arg(long, value_name = "FLOW", requires = "serial")]
    flow: Option<Flow>,
    /// Начальное состояние DTR: `on` или `off`
    #[arg(long, value_name = "on|off", requires = "serial", value_parser = parse_switch)]
    dtr: Option<bool>,
    /// Начальное состояние RTS: `on` или `off`
    #[arg(long, value_name = "on|off", requires = "serial", value_parser = parse_switch)]
    rts: Option<bool>,
    /// Интервал переподключения к анализатору, секунды: 0…3600 (по умолчанию 5);
    /// пауза короче 100 мс всё равно выдерживается как 100 мс
    #[arg(
        long,
        value_name = "SEC",
        requires = "connect",
        value_parser = clap::value_parser!(u64).range(0..=3600)
    )]
    reconnect_sec: Option<u64>,
    /// Профиль автоответчика: `auto`, `hl7`, `astm` или `none`
    #[arg(long, value_name = "PROFILE", value_parser = ["auto", "hl7", "astm", "none"])]
    responder: String,
    /// ASTM: отвечать `NAK` на неверную контрольную сумму вместо `ACK`
    #[arg(long)]
    astm_nak: bool,
    /// Папка журналов вместо папки по умолчанию
    #[arg(long, value_name = "DIR")]
    log_dir: Option<PathBuf>,
    /// Модель анализатора для заголовка журнала
    #[arg(long, value_name = "TEXT")]
    analyzer: Option<String>,
    /// Комментарий инженера для заголовка журнала
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,
}

/// `--astm-nak` действует только там, где есть ASTM-автомат (FR-17).
const fn astm_nak_applies(responder: Autoresponder) -> bool {
    matches!(responder, Autoresponder::AstmE1381 | Autoresponder::Auto)
}

/// `on`/`off` для DTR и RTS.
fn parse_switch(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("`{other}`: нужно `on` или `off`")),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Capture(args) => capture(args),
        Command::Ports => ports(),
        Command::Verify { file } => verify(&file),
        Command::Note { file, text } => note(&file, &text),
        Command::StressJournal { out, max_seconds } => stress_journal(&out, max_seconds),
    }
}

/// События в порядке, в котором их печатает статистика.
const KINDS: [&str; 8] = [
    "session",
    "open",
    "data",
    "close",
    "error",
    "info",
    "note",
    "session_end",
];

/// `bytetap-cli capture`: ждёт анализатор, подключается к нему или открывает COM-порт,
/// отвечает анализатору, пишет журнал и печатает ленту до Ctrl+C (FR-1, FR-2, FR-9).
fn capture(args: CaptureArgs) -> ExitCode {
    let responder = match args.responder.as_str() {
        "auto" => Autoresponder::Auto,
        "astm" => Autoresponder::AstmE1381,
        "hl7" => Autoresponder::Hl7Mllp,
        _ => Autoresponder::None,
    };
    if args.astm_nak && !astm_nak_applies(responder) {
        warn!(
            "Предупреждение: --astm-nak действует только для профилей astm и auto; профиль {} его не использует.",
            responder.as_str()
        );
    }
    let (mut config, target) = match (&args.listen, &args.connect, &args.serial) {
        (Some(listen), None, None) => (SessionConfig::new(*listen), format!("Слушаю {listen}")),
        (None, Some(connect), None) => {
            let reconnect = Duration::from_secs(args.reconnect_sec.unwrap_or(5));
            (
                SessionConfig::tcp_client(connect.clone(), reconnect),
                format!("Подключаюсь к {connect}"),
            )
        }
        (None, None, Some(serial)) => {
            let format = args.format.unwrap_or(SerialFormat::EIGHT_N_ONE);
            let baud = args.baud.unwrap_or(9600);
            let settings = SerialSettings::new(serial.clone(), baud, format)
                .with_flow(args.flow.unwrap_or(Flow::None))
                .with_lines(args.dtr.unwrap_or(true), args.rts.unwrap_or(true));
            (
                SessionConfig::serial(settings, Duration::from_secs(5)),
                format!("Порт {serial}"),
            )
        }
        // clap требует ровно один источник: сюда не попадает ни один набор аргументов.
        (listen, connect, serial) => {
            warn!(
                "ошибка: нужен ровно один источник соединений — --listen, --connect или --serial (получено: listen={listen:?}, connect={connect:?}, serial={serial:?})"
            );
            return ExitCode::FAILURE;
        }
    };
    config = config
        .with_responder(responder)
        .with_astm_nak(args.astm_nak);
    if let Some(dir) = args.log_dir {
        config = config.with_log_dir(dir);
    }
    if let Some(analyzer) = args.analyzer {
        config = config.with_analyzer(analyzer);
    }
    if let Some(note) = args.note {
        config = config.with_note(note);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!("ошибка: не удалось запустить runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async move {
        // Подписка создаётся до старта приёма: подключение анализатора сразу после
        // запуска не должно разойтись с лентой.
        let (handle, mut events) = CaptureSession::start_with_feed(config)
            .await
            .map_err(CaptureError::Start)?;
        let path = handle.journal_path().to_path_buf();
        let mut failure = handle.journal_failure();
        say!("Журнал: {}", path.display());
        say!("{target} (Ctrl+C — остановка)");
        if let Some(warning) = handle.log_dir_warning() {
            say!("Внимание: {warning}");
        }
        say!("Лента:");

        let interrupt = shutdown_signal();
        tokio::pin!(interrupt);
        loop {
            tokio::select! {
                _ = &mut interrupt => {
                    say!("\nОстановка…");
                    break;
                }
                _ = wait_for_journal_failure(&mut failure) => break,
                received = events.recv() => match received {
                    Ok(event) => print_event(&event),
                    Err(RecvError::Lagged(skipped)) => say!(
                        "… лента отстала: пропущено событий {skipped}, в журнале они есть"
                    ),
                    Err(RecvError::Closed) => break,
                },
            }
        }

        let stats = match handle.stop().await {
            Ok(stats) => stats,
            Err(err) => {
                return Err(match journal_failure_reason(&err) {
                    Some(reason) => CaptureError::Journal(reason.message().to_owned()),
                    None => CaptureError::Stop(err),
                });
            }
        };
        // Сессия успела разослать последние события до остановки — показываем и их.
        while let Ok(event) = events.try_recv() {
            print_event(&event);
        }
        say!(
            "Соединений: {}, байты: {} {}, {} {}",
            stats.connections,
            Dir::AnalyzerToLis.as_str(),
            stats.bytes_analyzer_to_lis,
            Dir::LisToAnalyzer.as_str(),
            stats.bytes_lis_to_analyzer
        );
        say!("Журнал: {}", path.display());
        Ok(())
    });
    // Задачи runtime не должны держать выход: поток записи журнала мог застрять.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(CaptureError::Journal(reason)) => {
            warn!(
                "Запись в журнал прервана: {reason}. Сессия остановлена, анализатор не получил подтверждений после сбоя."
            );
            ExitCode::FAILURE
        }
        Err(CaptureError::Start(err)) => {
            warn!("Не удалось начать сессию: {err}");
            ExitCode::FAILURE
        }
        Err(CaptureError::Stop(err)) => {
            warn!("ошибка: запись прервана: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Чем закончился `capture`.
enum CaptureError {
    /// Сессия не началась: занят порт, недоступна папка журналов.
    Start(std::io::Error),
    /// Журнал отказал: сессия остановлена, подтверждений больше не будет.
    Journal(String),
    /// Другая ошибка остановки.
    Stop(std::io::Error),
}

/// Ждёт отказа журнала, о котором сессия сообщает до остановки.
async fn wait_for_journal_failure(failure: &mut watch::Receiver<Option<JournalFailureReason>>) {
    if failure.borrow_and_update().is_some() {
        return;
    }
    // Ошибка `changed` значит, что сессия выгружена: ждать больше нечего.
    let _ = failure.changed().await;
}

/// Ждёт сигнала штатной остановки.
///
/// Кроме `Ctrl+C` это SIGTERM и SIGHUP на Unix (служба, закрытая консоль) и
/// `ctrl_close`/`ctrl_break` на Windows: консольное окно закрывается так же штатно,
/// и журнал должен получить `session_end`.
///
/// SIGHUP становится остановкой, только если процесс не игнорировал его при старте:
/// `nohup` и демоны оставляют SIGHUP проигнорированным намеренно, и перехват такого
/// сигнала означал бы остановку записи при закрытии терминала, которого программу
/// просили пережить.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut hangup = if sighup_was_ignored() {
            None
        } else {
            signal(SignalKind::hangup()).ok()
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = next_signal(&mut terminate) => {}
            _ = next_signal(&mut hangup) => {}
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;

        match (windows::ctrl_close(), windows::ctrl_break()) {
            (Ok(mut close), Ok(mut interrupt)) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = close.recv() => {}
                    _ = interrupt.recv() => {}
                }
            }
            // Обработчики консольных событий не поставились: остаётся `Ctrl+C`.
            _ => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Ждёт сигнал, если он вообще отслеживается; иначе не наступает никогда.
#[cfg(unix)]
async fn next_signal(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// `true`, если SIGHUP был проигнорирован при старте процесса (`SIG_IGN`).
#[cfg(unix)]
fn sighup_was_ignored() -> bool {
    // SAFETY: `sigaction` только читает текущую раскладку (`null` вместо новой), а
    // нулевая структура — допустимое начальное значение для вывода.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        let read = libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut current);
        read == 0 && current.sa_sigaction == libc::SIG_IGN
    }
}

/// Строка ленты для события (M1-AC15, FR-26).
fn print_event(event: &Event) {
    match event {
        Event::Open(open) => say!(
            "{} ── #{} подключён {}",
            feed_time(&open.t),
            open.conn,
            open.peer
        ),
        Event::Data(data) => {
            let arrow = match data.dir {
                Dir::AnalyzerToLis => "→",
                Dir::LisToAnalyzer => "←",
            };
            let mark = if data.synthetic { " [авто]" } else { "" };
            say!(
                "{} {arrow} #{}{mark} {}",
                feed_time(&data.t),
                data.conn,
                shorten(&data.text)
            );
        }
        Event::Close(close) => say!(
            "{} ── #{} закрыт: {} ({})",
            feed_time(&close.t),
            close.conn,
            close.by,
            close.reason
        ),
        Event::Error(error) => match error.conn {
            Some(conn) => say!("{} !! #{} {}", feed_time(&error.t), conn, error.message),
            None => say!("{} !! {}", feed_time(&error.t), error.message),
        },
        Event::Info(info) => match info.conn {
            Some(conn) => say!("{} .. #{} {}", feed_time(&info.t), conn, info.message),
            None => say!("{} .. {}", feed_time(&info.t), info.message),
        },
        Event::Session(_) | Event::Note(_) | Event::SessionEnd(_) => {}
    }
}

/// Время события для ленты: `HH:MM:SS.mmm`.
fn feed_time(t: &str) -> String {
    DateTime::parse_from_rfc3339(t).map_or_else(
        |_| t.to_owned(),
        |moment| moment.format("%H:%M:%S%.3f").to_string(),
    )
}

/// Обрезает `text` до ширины ленты.
fn shorten(text: &str) -> String {
    if text.chars().count() <= FEED_WIDTH {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(FEED_WIDTH).collect();
    short.push('…');
    short
}

/// `bytetap-cli ports`: доступные COM-порты с описанием (FR-12, M3-AC4).
///
/// Пустой список — не ошибка: на машине без портов команда так и говорит.
fn ports() -> ExitCode {
    match bytetap_core::transport::serial::list_ports() {
        Ok(list) if list.is_empty() => {
            say!("Портов не найдено.");
            ExitCode::SUCCESS
        }
        Ok(list) => {
            for port in list {
                say!("{}", port.description);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            warn!("ошибка: не удалось перечислить порты: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `bytetap-cli verify`: проверка каждой строки по схеме и краткая статистика (M1-AC1, M1-AC15).
fn verify(file: &Path) -> ExitCode {
    let schema: Value = match serde_json::from_str(SCHEMA) {
        Ok(schema) => schema,
        Err(err) => {
            warn!("ошибка: вшитая схема формата не разобралась: {err}");
            return ExitCode::FAILURE;
        }
    };
    let validator = match jsonschema::validator_for(&schema) {
        Ok(validator) => validator,
        Err(err) => {
            warn!("ошибка: вшитая схема формата не принята движком: {err}");
            return ExitCode::FAILURE;
        }
    };

    let reader = match journal::JournalReader::open(file) {
        Ok(reader) => reader,
        Err(err) => {
            warn!("ошибка: не удалось открыть {}: {err}", file.display());
            return ExitCode::FAILURE;
        }
    };

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut bytes: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut connections: BTreeSet<u32> = BTreeSet::new();
    let mut lines = 0usize;
    let mut errors = 0usize;
    let mut first_kind: Option<&'static str> = None;
    let mut sessions = 0usize;
    let mut ended = false;

    for item in reader {
        match item {
            Ok(line) => {
                lines += 1;
                if lines == 1 {
                    first_kind = Some(line.event.kind());
                }
                *counts.entry(line.event.kind()).or_default() += 1;
                if matches!(line.event, Event::Session(_)) {
                    sessions += 1;
                }
                // После итогов сессии файл может дополняться только комментариями.
                if ended && !matches!(line.event, Event::Note(_)) {
                    errors += 1;
                    warn!(
                        "ошибка: строка {}: после session_end допустим только note",
                        line.number
                    );
                }
                match &line.event {
                    Event::Data(data) => {
                        let counter = bytes.entry(data.dir.as_str()).or_default();
                        *counter = counter.saturating_add(data.len);
                        connections.insert(data.conn);
                        if let Err(message) = check_data(data) {
                            errors += 1;
                            warn!("ошибка: строка {}: {message}", line.number);
                        }
                    }
                    Event::Open(open) => {
                        connections.insert(open.conn);
                    }
                    Event::Close(close) => {
                        connections.insert(close.conn);
                    }
                    Event::SessionEnd(_) => ended = true,
                    Event::Session(_) | Event::Error(_) | Event::Info(_) | Event::Note(_) => {}
                }
                if let Err(error) = validator.validate(&line.value) {
                    errors += 1;
                    warn!("ошибка: строка {}: {error}", line.number);
                }
            }
            Err(error) if error.kind == LineErrorKind::Truncated => {
                warn!(
                    "предупреждение: строка {} обрезана — потеря при аварийном завершении",
                    error.line
                );
            }
            Err(error) => {
                errors += 1;
                warn!("ошибка: строка {}: {}", error.line, error.message);
            }
        }
    }

    if first_kind != Some("session") {
        errors += 1;
        warn!("ошибка: первая строка файла должна быть заголовком `session`");
    }
    if sessions != 1 {
        errors += 1;
        warn!("ошибка: в журнале должен быть ровно один заголовок `session`, найдено {sessions}");
    }

    say!("Журнал: {}", file.display());
    say!("Строк: {lines}");
    let kinds: Vec<String> = KINDS
        .iter()
        .map(|kind| format!("{kind} {}", counts.get(kind).copied().unwrap_or(0)))
        .collect();
    say!("События: {}", kinds.join(", "));
    say!(
        "Байты: {} {}, {} {}",
        Dir::AnalyzerToLis.as_str(),
        bytes.get(Dir::AnalyzerToLis.as_str()).copied().unwrap_or(0),
        Dir::LisToAnalyzer.as_str(),
        bytes.get(Dir::LisToAnalyzer.as_str()).copied().unwrap_or(0)
    );
    say!("Соединения: {}", connections.len());

    if errors > 0 {
        warn!("проверка не пройдена: ошибок {errors}");
        return ExitCode::FAILURE;
    }
    say!("Проверка пройдена.");
    ExitCode::SUCCESS
}

/// Проверяет то, чего не видно схеме: `len` и `text` должны выводиться из `b64`.
fn check_data(data: &Data) -> Result<(), String> {
    let payload = base64::engine::general_purpose::STANDARD
        .decode(&data.b64)
        .map_err(|err| format!("b64 не декодируется: {err}"))?;
    let len = u64::try_from(payload.len()).unwrap_or(u64::MAX);
    if len != data.len {
        return Err(format!(
            "len {} не совпадает с длиной декодированного b64 ({len})",
            data.len
        ));
    }
    let expected = bytetap_core::text::escape(&payload);
    if expected != data.text {
        return Err(format!(
            "text не совпадает с экранированием b64: ожидалось {expected:?}"
        ));
    }
    Ok(())
}

/// `bytetap-cli note`: дописывает событие `note` в конец файла, начало не переписывается (FR-23).
fn note(file: &Path, text: &str) -> ExitCode {
    let note = Note {
        t: now_rfc3339(),
        mono_us: None,
        text: text.to_owned(),
    };
    match journal::append_note(file, &note) {
        Ok(()) => {
            say!("Комментарий дописан: {}", file.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            warn!("ошибка: не удалось дописать комментарий: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `_stress-journal`: пишет журнал под нагрузкой, пока процесс не убьют (T-4).
///
/// Команда скрыта из справки: ею пользуется только интеграционный тест устойчивости.
fn stress_journal(out: &Path, max_seconds: u64) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!("ошибка: не удалось запустить runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async {
        let session = Session::new(
            Mode::TcpServer,
            Autoresponder::Hl7Mllp,
            json!({"listen": "127.0.0.1:5000", "stress": true}),
            Host::current(),
        );
        let journal = Journal::create(out, session)?;
        let clock = MonoClock::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_seconds);
        let mut index: u64 = 0;
        loop {
            index += 1;
            let message = format!(
                "\u{b}MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|{index}|P|2.3.1\rPID|1||PATIENT^TEST\rOBX|1|NM|GLU||5.4|mmol/L\r\u{1c}\r"
            );
            let event = Event::Data(Data::new(
                now_rfc3339(),
                clock.now_us(),
                1,
                Dir::AnalyzerToLis,
                false,
                message.as_bytes(),
            ));
            if !journal.record(event) {
                return Err(std::io::Error::other("писатель журнала остановился"));
            }
            if index.is_multiple_of(64) {
                tokio::task::yield_now().await;
                if std::time::Instant::now() >= deadline {
                    // Штатный выход, если тест не успел убить процесс.
                    journal.shutdown().await?;
                    return Ok(());
                }
            }
        }
    });

    // Нагрузочная запись тоже не должна держать выход, если фиксация застряла.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            warn!("ошибка: нагрузочная запись прервана: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_astm_nak_switch_is_only_meaningful_for_astm_profiles() {
        assert!(astm_nak_applies(Autoresponder::AstmE1381));
        assert!(astm_nak_applies(Autoresponder::Auto));
        assert!(
            !astm_nak_applies(Autoresponder::Hl7Mllp),
            "у HL7/MLLP нет контрольной суммы кадра"
        );
        assert!(!astm_nak_applies(Autoresponder::None));
    }

    #[test]
    fn version_matches_core() {
        let version = Cli::command().get_version().map(str::to_owned);
        assert_eq!(version.as_deref(), Some(bytetap_core::version()));
    }

    #[test]
    fn embedded_schema_is_valid() {
        let schema: Value = serde_json::from_str(SCHEMA).expect("схема — корректный JSON");
        assert!(jsonschema::validator_for(&schema).is_ok());
    }

    #[test]
    fn stress_command_is_hidden_from_help() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("_stress-journal"), "{help}");
    }

    #[test]
    fn capture_takes_exactly_one_source() {
        for args in [
            vec!["--listen", "127.0.0.1:0"],
            vec!["--connect", "127.0.0.1:5000"],
            vec!["--serial", "COM5", "--baud", "9600", "--format", "8N1"],
        ] {
            let parsed = Cli::try_parse_from(
                ["bytetap-cli", "capture"]
                    .into_iter()
                    .chain(args)
                    .chain(["--responder", "hl7"]),
            );
            assert!(parsed.is_ok(), "{:?}", parsed.err());
        }

        let none = Cli::try_parse_from(["bytetap-cli", "capture", "--responder", "hl7"]);
        assert!(none.is_err(), "нужен ровно один источник");

        let both = Cli::try_parse_from([
            "bytetap-cli",
            "capture",
            "--listen",
            "127.0.0.1:0",
            "--connect",
            "127.0.0.1:5000",
            "--responder",
            "hl7",
        ]);
        assert!(both.is_err(), "два источника одновременно");

        let serial_without_baud = Cli::try_parse_from([
            "bytetap-cli",
            "capture",
            "--serial",
            "COM5",
            "--format",
            "8N1",
            "--responder",
            "hl7",
        ]);
        assert!(serial_without_baud.is_err(), "--baud обязателен для порта");
    }

    #[test]
    fn capture_takes_all_responder_profiles_and_the_astm_switch() {
        for responder in ["auto", "hl7", "astm", "none"] {
            let parsed = Cli::try_parse_from([
                "bytetap-cli",
                "capture",
                "--listen",
                "127.0.0.1:0",
                "--responder",
                responder,
            ]);
            assert!(parsed.is_ok(), "{responder}: {:?}", parsed.err());
        }
        let astm = Cli::try_parse_from([
            "bytetap-cli",
            "capture",
            "--serial",
            "/dev/ttyUSB0",
            "--baud",
            "115200",
            "--format",
            "7E2",
            "--flow",
            "rtscts",
            "--dtr",
            "off",
            "--rts",
            "off",
            "--responder",
            "astm",
            "--astm-nak",
        ])
        .expect("порт ASTM");
        assert!(matches!(astm.command, Command::Capture(_)));

        let bad_format = Cli::try_parse_from([
            "bytetap-cli",
            "capture",
            "--serial",
            "COM5",
            "--baud",
            "9600",
            "--format",
            "9N1",
            "--responder",
            "hl7",
        ]);
        assert!(bad_format.is_err(), "формат порта проверяется clap");

        let bad_baud = Cli::try_parse_from([
            "bytetap-cli",
            "capture",
            "--serial",
            "COM5",
            "--baud",
            "4000001",
            "--format",
            "8N1",
            "--responder",
            "hl7",
        ]);
        assert!(bad_baud.is_err(), "скорость вне 1…4 000 000");

        let reconnect = Cli::try_parse_from([
            "bytetap-cli",
            "capture",
            "--connect",
            "127.0.0.1:5000",
            "--reconnect-sec",
            "3601",
            "--responder",
            "hl7",
        ]);
        assert!(reconnect.is_err(), "интервал 0…3600");
    }

    #[test]
    fn a_ports_command_is_defined() {
        let parsed = Cli::try_parse_from(["bytetap-cli", "ports"]);
        assert!(parsed.is_ok(), "{:?}", parsed.err());
    }

    #[test]
    fn feed_shows_the_time_and_shortens_long_text() {
        assert_eq!(
            feed_time("2026-10-19T10:15:03.000512+03:00"),
            "10:15:03.000"
        );
        assert_eq!(feed_time("не время"), "не время");
        assert_eq!(shorten("abc"), "abc");
        let long = "x".repeat(FEED_WIDTH + 10);
        let short = shorten(&long);
        assert_eq!(short.chars().count(), FEED_WIDTH + 1);
        assert!(short.ends_with('…'));
    }

    /// SIGHUP, унаследованный проигнорированным, виден проверке, а обычный — нет.
    #[cfg(unix)]
    #[test]
    fn an_ignored_sighup_is_told_from_a_handled_one() {
        // SAFETY: раскладка читается, подменяется на SIG_IGN и возвращается на место
        // до конца теста; других процессов это не касается.
        unsafe {
            let mut original: libc::sigaction = std::mem::zeroed();
            assert_eq!(
                libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut original),
                0,
                "раскладка SIGHUP читается"
            );
            let was_ignored = sighup_was_ignored();

            let mut ignored: libc::sigaction = std::mem::zeroed();
            ignored.sa_sigaction = libc::SIG_IGN;
            assert_eq!(
                libc::sigaction(libc::SIGHUP, &ignored, std::ptr::null_mut()),
                0
            );
            let detected = sighup_was_ignored();

            assert_eq!(
                libc::sigaction(libc::SIGHUP, &original, std::ptr::null_mut()),
                0,
                "исходный обработчик возвращён"
            );
            assert!(detected, "SIG_IGN распознан");
            assert_eq!(
                sighup_was_ignored(),
                was_ignored,
                "исходная раскладка на месте"
            );
        }
    }
}
