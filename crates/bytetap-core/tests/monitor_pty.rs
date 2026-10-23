//! Прослушка двух COM-портов на двух парах pty (T-3, M5-AC1, M5-AC2, M5-AC8).
//!
//! `socat pty,raw,echo=0,link=A pty,raw,echo=0,link=B` создаёт пару связанных
//! псевдотерминалов и стабильные имена для них. Прослушка открывает `A` первой пары
//! (сторона анализатора) и `A` второй (сторона ЛИС), тест держит `B` обеих и играет
//! анализатора и ЛИС по ASTM E1381 и HL7/MLLP.
//!
//! Убитый `socat` закрывает ведущую сторону pty: чтение ведомой отдаёт `EIO` — та же
//! ситуация, что отключённый USB-адаптер. Пара закрывается, источник переоткрывает оба
//! порта, а второй `socat` с теми же именами изображает подключённый обратно адаптер:
//! в журнале новое `open` со следующим `conn`.
//!
//! Тест помечен `#[ignore = "requires socat"]`: обычный прогон его не запускает, CI
//! запускает отдельно с `BYTETAP_REQUIRE_SOCAT=1` (M3-AC11, M5-AC8). Если `socat` нужен,
//! а его нет, тест падает; без переменной — печатает явный пропуск.
#![cfg(unix)]

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytetap_core::journal::{Close, ClosedBy, Dir, Event, LinkRole, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::MonitorSide;
use bytetap_core::transport::monitor_side;
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};
use bytetap_testkit::emulator::frame;
use bytetap_testkit::fixtures_dir;

/// Интервал переоткрытия прослушки: быстрее ручного прогона, но не цикл без паузы.
const RETRY: Duration = Duration::from_millis(200);

/// Сколько тест ждёт обмена, события или закрытия.
const WAIT: Duration = Duration::from_secs(10);

/// `socat` обязателен: CI ставит его и передаёт переменную, и молча зелёный тест там
/// означал бы, что прослушка не проверена вовсе (M5-AC8).
const REQUIRE_SOCAT: &str = "BYTETAP_REQUIRE_SOCAT";

/// `ENQ` — анализатор просит разрешения передавать (E1381).
const ENQ: u8 = 0x05;

/// `ACK` — подтверждение приёма (E1381).
const ACK: u8 = 0x06;

/// `EOT` — передача закончена.
const EOT: u8 = 0x04;

/// Пара pty от `socat`; дочерний процесс живёт, пока живёт значение.
struct Socat {
    child: Child,
    first: PathBuf,
    second: PathBuf,
}

impl Socat {
    /// Запускает `socat` с двумя именами в `dir`: `first` — сторона bytetap, `second` — теста.
    fn start(dir: &Path, name: &str) -> Option<Self> {
        let first = dir.join(format!("{name}-bytetap"));
        let second = dir.join(format!("{name}-test"));
        let link = |path: &Path| format!("pty,raw,echo=0,link={}", path.display());
        let child = match Command::new("socat")
            .args([link(&first), link(&second)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if socat_required() {
                    panic!("{REQUIRE_SOCAT}=1, но `socat` не найден: установите пакет `socat`");
                }
                eprintln!(
                    "тест пропущен: `socat` не найден; для теста прослушки установите его \
                     или запустите прогон с {REQUIRE_SOCAT}=1"
                );
                return None;
            }
            Err(err) => panic!("socat не запустился: {err}"),
        };
        let socat = Self {
            child,
            first,
            second,
        };
        // Имена появляются чуть позже запуска процесса.
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if socat.first.exists() && socat.second.exists() {
                return Some(socat);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("socat не создал имена pty за {WAIT:?}");
    }
}

impl Drop for Socat {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // `kill` не даёт `socat` убрать имена: следующая пара с теми же именами не встанет.
        let _ = std::fs::remove_file(&self.first);
        let _ = std::fs::remove_file(&self.second);
    }
}

/// `socat` требуется этим прогоном: CI передаёт переменную, локальный прогон — нет.
fn socat_required() -> bool {
    std::env::var(REQUIRE_SOCAT).is_ok_and(|value| value == "1")
}

/// Открывает конец pty для эмулятора: блокирующий порт с таймаутом.
fn open_end(path: &Path) -> io::Result<Box<dyn serialport::SerialPort>> {
    serialport::new(path.to_string_lossy(), 9600)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_secs(5))
        .open()
        .map_err(io::Error::from)
}

/// Читает из порта один байт.
fn read_byte(port: &mut dyn serialport::SerialPort) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    port.read_exact(&mut byte)?;
    Ok(byte[0])
}

/// Читает кадр ASTM до `LF` включительно.
fn read_frame(port: &mut dyn serialport::SerialPort) -> io::Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let byte = read_byte(port)?;
        frame.push(byte);
        if byte == b'\n' {
            return Ok(frame);
        }
        if frame.len() > 4096 {
            return Err(io::Error::other("кадр ASTM не закончился"));
        }
    }
}

/// Кадр ASTM E1381 с верной контрольной суммой, номер кадра `'1'` — первый после `ENQ`.
fn astm_frame() -> Vec<u8> {
    let body = b"1H|\\^&||||||||||P|E1394-97|20261026101500\r";
    let checksum = body
        .iter()
        .chain(std::iter::once(&0x03u8))
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    let mut frame = vec![0x02];
    frame.extend_from_slice(body);
    frame.push(0x03);
    frame.extend(format!("{checksum:02X}").bytes());
    frame.extend_from_slice(b"\r\n");
    frame
}

/// Подтверждение ЛИС в HL7/MLLP: рамка вокруг синтетической фикстуры.
fn hl7_ack() -> Vec<u8> {
    let message = std::fs::read(fixtures_dir().join("hl7").join("lis_ack_aa.hl7"))
        .expect("фикстура lis_ack_aa.hl7");
    frame(&message)
}

/// Что записалось и что прошло в одном обмене.
struct Exchange {
    /// Что анализатор отправил в ЛИС.
    sent: Vec<u8>,
    /// Что анализатор получил от ЛИС.
    answers: Vec<u8>,
    /// Что ЛИС получила от анализатора.
    received: Vec<u8>,
}

/// Обмен «анализатор ↔ ЛИС» через прослушку: `full` — `ENQ`, запись ASTM и `EOT` против
/// одного `ENQ`.
///
/// Оба конца pty — блокирующие порты, поэтому половины идут в двух потоках: анализатор
/// не может ждать ответа, который ЛИС отдаст только после его же байтов.
fn exchange(analyzer_end: &Path, lis_end: &Path, full: bool) -> io::Result<Exchange> {
    let lis_path = lis_end.to_path_buf();
    let lis_thread = std::thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut port = open_end(&lis_path)?;
        let mut received = vec![read_byte(port.as_mut())?];
        port.write_all(&[ACK])?;
        if full {
            received.extend(read_frame(port.as_mut())?);
            // `EOT` приходит сразу за кадром и подтверждения не ждёт.
            received.push(read_byte(port.as_mut())?);
            port.write_all(&[ACK])?;
            port.write_all(&hl7_ack())?;
        }
        port.flush()?;
        Ok(received)
    });

    let mut port = open_end(analyzer_end)?;
    let mut sent = vec![ENQ];
    if full {
        sent.extend(astm_frame());
        sent.push(EOT);
    }
    port.write_all(&sent)?;
    port.flush()?;
    let mut answers = vec![read_byte(port.as_mut())?];
    if full {
        answers.push(read_byte(port.as_mut())?);
        let mut ack = vec![0u8; hl7_ack().len()];
        port.read_exact(&mut ack)?;
        answers.extend(ack);
    }
    let received = lis_thread.join().expect("поток ЛИС")?;
    Ok(Exchange {
        sent,
        answers,
        received,
    })
}

/// Сколько обменов «запрос — ответ» в замере задержки на pty.
const ROUNDS: usize = 2000;

/// Размер одного замера задержки: короткий кадр, как сообщение анализатора.
const FRAME: usize = 64;

/// Предел добавки к задержке на направление на pty, 99-й процентиль (M5-AC5).
const P99_BUDGET: Duration = Duration::from_millis(2);

/// Предел добавки к задержке на направление на pty, максимум (M5-AC5).
const MAX_BUDGET: Duration = Duration::from_millis(20);

/// Круги «запрос — ответ» между двумя концами pty: `answer` отвечает тем же.
fn round_trips(ask: &Path, answer: &Path, rounds: usize, size: usize) -> Vec<Duration> {
    let answer_path = answer.to_path_buf();
    let echo = std::thread::spawn(move || -> io::Result<()> {
        let mut port = open_end(&answer_path)?;
        let mut buffer = vec![0u8; size];
        for _ in 0..rounds {
            port.read_exact(&mut buffer)?;
            port.write_all(&buffer)?;
            port.flush()?;
        }
        Ok(())
    });
    let mut port = open_end(ask).expect("конец запроса");
    let payload = vec![b'x'; size];
    let mut back = vec![0u8; size];
    let mut times = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        port.write_all(&payload).expect("запрос");
        port.flush().expect("сброс запроса");
        port.read_exact(&mut back).expect("ответ");
        times.push(started.elapsed());
    }
    echo.join().expect("поток ответа").expect("обмен");
    times
}

/// Медиана длительностей.
fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

/// Процентиль длительностей: значение, ниже которого лежит `percent` процентов.
fn percentile(mut values: Vec<Duration>, percent: usize) -> Duration {
    values.sort_unstable();
    let index = (values.len() - 1) * percent / 100;
    values[index]
}

/// Замер задержки прослушки на pty (NFR-6, M5-AC5): добавка на направление против такого
/// же круга через одну пару pty без bytetap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires socat"]
async fn the_monitor_latency_on_pty_stays_within_the_budget() {
    let dir = tempfile::tempdir().expect("временная папка");
    let direct = match Socat::start(dir.path(), "direct") {
        Some(socat) => socat,
        None => return,
    };
    let direct_times = tokio::task::spawn_blocking({
        let ask = direct.second.clone();
        let answer = direct.first.clone();
        move || round_trips(&ask, &answer, ROUNDS, FRAME)
    })
    .await
    .expect("замер напрямую");
    drop(direct);
    std::thread::sleep(Duration::from_millis(200));

    let analyzer = Socat::start(dir.path(), "analyzer").expect("пара анализатора");
    let lis = Socat::start(dir.path(), "lis").expect("пара ЛИС");
    let settings = |port: &Path| {
        SerialSettings::new(port.to_string_lossy(), 9600, SerialFormat::EIGHT_N_ONE)
            .with_lines(false, false)
    };
    let handle = Session::start(
        SessionConfig::com_monitor(settings(&analyzer.first), settings(&lis.first), RETRY)
            .with_log_dir(dir.path()),
    )
    .await
    .expect("оба порта открываются");
    let through = tokio::task::spawn_blocking({
        let ask = analyzer.second.clone();
        let answer = lis.second.clone();
        move || round_trips(&ask, &answer, ROUNDS, FRAME)
    })
    .await
    .expect("замер через прослушку");
    handle.stop().await.expect("остановка");

    let baseline = median(direct_times.clone());
    // Круг через прослушку — два направления: добавка на направление — половина разницы.
    let added: Vec<Duration> = through
        .iter()
        .map(|round| round.saturating_sub(baseline) / 2)
        .collect();
    let p50 = percentile(added.clone(), 50);
    let p99 = percentile(added.clone(), 99);
    let max = added.iter().copied().max().expect("замеры есть");
    eprintln!(
        "задержка прослушки на pty: кругов {ROUNDS} по {FRAME} Б, прямой круг p50 {baseline:?}, \
         добавка на направление p50 {p50:?}, p99 {p99:?}, max {max:?}"
    );
    assert!(
        p99 <= P99_BUDGET,
        "p99 добавки {p99:?} больше {P99_BUDGET:?}"
    );
    assert!(
        max <= MAX_BUDGET,
        "max добавки {max:?} больше {MAX_BUDGET:?}"
    );
}

/// Байты одного направления одного соединения, склеенные из `b64`.
fn recorded_bytes(path: &Path, conn: u32, dir: Dir) -> Vec<u8> {
    let outcome = read_all(path).expect("журнал читается");
    let mut bytes = Vec::new();
    for line in outcome.lines {
        if let Event::Data(data) = line.event
            && data.conn == conn
            && data.dir == dir
        {
            assert!(!data.synthetic, "в прослушке нет синтетических байтов");
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 декодируется"),
            );
        }
    }
    bytes
}

/// Ждёт, пока в журнале окажется `len` байт одного направления: журнал асинхронный.
async fn wait_for_bytes(path: &Path, conn: u32, dir: Dir, len: usize) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if recorded_bytes(path, conn, dir).len() >= len {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("в журнале не дождались {len} байт направления {dir} за {WAIT:?}");
}

/// Ждёт в журнале условие: журнал пишется асинхронно.
async fn wait_for(path: &Path, what: &str, found: impl Fn(&[Event]) -> bool) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let outcome = read_all(path).expect("журнал читается");
        let events: Vec<Event> = outcome
            .lines
            .iter()
            .map(|line| line.event.clone())
            .collect();
        if found(&events) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{what} не появилось в журнале за {WAIT:?}");
}

/// Событие `close` с причиной `error` и стороной ЛИС: адаптер второй стороны отключён.
fn is_lis_error(events: &[Event]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            Event::Close(Close { by: ClosedBy::Lis, reason, .. }) if reason == "error"
        )
    })
}

/// Прослушка на двух парах pty: обмен в обе стороны, пропажа порта ЛИС и переоткрытие
/// новым `conn` (M5-AC1, M5-AC2, M5-AC8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires socat"]
async fn a_monitor_pair_forwards_astm_and_hl7_over_pty() {
    let dir = tempfile::tempdir().expect("временная папка");
    let analyzer = match Socat::start(dir.path(), "analyzer") {
        Some(socat) => socat,
        None => return,
    };
    let lis = Socat::start(dir.path(), "lis").expect("вторая пара pty");

    let settings = |port: &Path| {
        SerialSettings::new(port.to_string_lossy(), 9600, SerialFormat::EIGHT_N_ONE)
            .with_lines(false, false)
    };
    let handle = Session::start(
        SessionConfig::com_monitor(settings(&analyzer.first), settings(&lis.first), RETRY)
            .with_log_dir(dir.path()),
    )
    .await
    .expect("оба порта открываются");
    let path = handle.journal_path().to_path_buf();
    let analyzer_port = analyzer.first.to_string_lossy().into_owned();
    let lis_port = lis.first.to_string_lossy().into_owned();
    assert_eq!(
        handle.local_addr(),
        analyzer_port,
        "строка состояния — порт анализатора"
    );

    // Обмен: анализатор шлёт `ENQ`, запись и `EOT`, ЛИС подтверждает и отвечает HL7/MLLP.
    let analyzer_end = analyzer.second.clone();
    let lis_end = lis.second.clone();
    let first = tokio::task::spawn_blocking(move || exchange(&analyzer_end, &lis_end, true))
        .await
        .expect("задача обмена")
        .expect("обмен через pty");
    assert_eq!(
        first.received, first.sent,
        "ASTM доходит до ЛИС без изменений"
    );
    let mut expected_answers = vec![ACK, ACK];
    expected_answers.extend(hl7_ack());
    assert_eq!(
        first.answers, expected_answers,
        "ответы ЛИС доходят до анализатора без изменений"
    );

    // Журнал асинхронный: `open` и `data` обмена доходят до файла не мгновенно.
    wait_for_bytes(&path, 1, Dir::AnalyzerToLis, first.sent.len()).await;
    wait_for_bytes(&path, 1, Dir::LisToAnalyzer, first.answers.len()).await;

    let outcome = read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(header.mode, Mode::ComMonitor);
    assert_eq!(
        header.autoresponder,
        bytetap_core::journal::Autoresponder::None,
        "автоответчика в прослушке нет"
    );
    assert_eq!(header.settings["analyzer"]["port"], analyzer_port);
    assert_eq!(header.settings["lis"]["port"], lis_port);
    assert_eq!(header.settings["lis"]["baud"], 9600);
    assert_eq!(header.settings["lis"]["format"], "8N1");
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("-com_monitor.jsonl")),
        "имя файла режима: {}",
        path.display()
    );

    let mut opens = outcome.lines.iter().filter_map(|line| match &line.event {
        Event::Open(open) => Some(open),
        _ => None,
    });
    let open = opens.next().expect("open пары");
    assert!(opens.next().is_none(), "open один на пару");
    assert_eq!(open.peer, analyzer_port);
    assert_eq!(open.local, "");
    let link = open.link.as_ref().expect("link — вторая сторона пары");
    assert_eq!(link.role, LinkRole::Lis);
    assert_eq!(link.peer, lis_port);
    assert_eq!(link.local, "");
    assert_eq!(
        recorded_bytes(&path, 1, Dir::AnalyzerToLis),
        first.sent,
        "направление анализатора записано байт в байт"
    );
    assert_eq!(
        recorded_bytes(&path, 1, Dir::LisToAnalyzer),
        first.answers,
        "направление ЛИС записано байт в байт"
    );

    // Отключён адаптер стороны ЛИС: `socat` убит, ведущая сторона pty закрыта, чтение
    // ведомой отдаёт `EIO`.
    drop(lis);
    wait_for(&path, "закрытие пары после пропажи порта", is_lis_error).await;
    let outcome = read_all(&path).expect("журнал читается");
    assert!(
        outcome.lines.iter().any(|line| matches!(
            &line.event,
            Event::Error(error)
                if matches!(
                    error.code.as_deref(),
                    Some("connection_read_failed" | "connection_write_failed")
                )
        )),
        "ошибка обмена с кодом отказа"
    );

    // Адаптер подключён обратно: второй `socat` с теми же именами, источник открывает порты
    // заново и пишет новое `open` со следующим `conn`.
    let lis = Socat::start(dir.path(), "lis").expect("вторая пара pty снова");
    wait_for(&path, "второе open", |events| {
        events
            .iter()
            .filter(|event| matches!(event, Event::Open(_)))
            .count()
            >= 2
            && events
                .iter()
                .any(|event| matches!(event, Event::Open(open) if open.conn == 2))
    })
    .await;

    let analyzer_end = analyzer.second.clone();
    let lis_end = lis.second.clone();
    let second = tokio::task::spawn_blocking(move || exchange(&analyzer_end, &lis_end, false))
        .await
        .expect("задача второго обмена")
        .expect("обмен после переоткрытия");
    assert_eq!(
        second.received, second.sent,
        "порт ЛИС работает после переоткрытия"
    );
    assert_eq!(second.answers, vec![ACK], "ЛИС ответила");
    wait_for_bytes(&path, 2, Dir::AnalyzerToLis, second.sent.len()).await;
    assert_eq!(
        recorded_bytes(&path, 2, Dir::AnalyzerToLis),
        second.sent,
        "обмен второй пары записан байт в байт"
    );

    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, 2, "две пары — два соединения");
}

/// Ошибка старта на стороне ЛИС: порт анализатора настоящий, порта ЛИС нет (M5-AC3).
///
/// Проверяется на pty, потому что нужен настоящий открывающийся порт: ошибка стороны
/// видна только тогда, когда первый порт уже открыт.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires socat"]
async fn a_missing_lis_port_is_a_start_error_on_the_lis_side() {
    let dir = tempfile::tempdir().expect("временная папка");
    let logs = dir.path().join("logs");
    std::fs::create_dir(&logs).expect("папка журналов");
    let analyzer = match Socat::start(dir.path(), "analyzer") {
        Some(socat) => socat,
        None => return,
    };
    let err = Session::start(
        SessionConfig::com_monitor(
            SerialSettings::new(
                analyzer.first.to_string_lossy(),
                9600,
                SerialFormat::EIGHT_N_ONE,
            )
            .with_lines(false, false),
            SerialSettings::new("/dev/bytetap-no-such-port", 9600, SerialFormat::EIGHT_N_ONE),
            RETRY,
        )
        .with_log_dir(&logs),
    )
    .await
    .expect_err("порта ЛИС нет");
    assert_eq!(
        monitor_side(&err),
        Some(MonitorSide::Lis),
        "ошибка называет сторону ЛИС: {err}"
    );
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    assert!(
        err.to_string().contains("/dev/bytetap-no-such-port"),
        "ошибка называет порт: {err}"
    );
    assert_eq!(
        std::fs::read_dir(&logs).expect("папка").count(),
        0,
        "журнал не создан"
    );
}
