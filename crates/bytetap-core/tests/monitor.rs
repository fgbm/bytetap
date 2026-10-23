//! Прослушка двух портов на виртуальной паре (T-2, M5-AC1, M5-AC2, M5-AC8).
//!
//! Виртуальных COM-портов нет ни на `windows-latest`, ни на машине разработчика, поэтому
//! сквозная проверка пересылки идёт парой в процессе: хук ядра
//! [`bytetap_core::transport::virtual_monitor`] отдаёт сессии один конец «порта», а тест
//! держит другой и играет то анализатора, то ЛИС.
//!
//! Проверяется то, что не зависит от драйвера: байты в обе стороны доходят без изменений
//! (ASTM E1381 и HL7/MLLP), направление `data` считается по порту, а не по инициатору,
//! фрагментация не склеивается, `open` несёт обе стороны пары, пропажа одной стороны
//! закрывает пару и переоткрывает оба порта новым `conn`, «Стоп» даёт одно `close`.
//!
//! Хук глобальный для процесса, поэтому тесты файла идут по очереди под одной меткой.
//! Настоящий COM проверяется на pty (`monitor_pty.rs`) и в ручном смоуке на железе.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use bytetap_core::journal::{Autoresponder, Close, ClosedBy, Dir, Event, LinkRole, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig, SessionHandle};
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};
use bytetap_core::transport::{VirtualMonitorEnds, reset_virtual_monitor, virtual_monitor};
use bytetap_testkit::emulator::{Fragmentation, fragment_bounds, frame};
use bytetap_testkit::fixtures_dir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

/// Интервал переоткрытия прослушки в тестах.
const RETRY: Duration = Duration::from_millis(100);

/// Сколько тест ждёт обмена, события или закрытия.
const WAIT: Duration = Duration::from_secs(10);

/// Метка хука: он глобальный для процесса, а тесты идут параллельно.
static HOOK: Mutex<()> = Mutex::const_new(());

/// Снимает виртуальные пары при выходе из теста, даже если проверка упала.
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        reset_virtual_monitor();
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("hl7").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn astm_fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join("astm").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// `ENQ` перед записями: так начинает передачу анализатор (E1381).
const ENQ: u8 = 0x05;

/// `EOT` после записей: передача закончена.
const EOT: u8 = 0x04;

/// Настройки порта анализатора в тестах.
fn analyzer_settings() -> SerialSettings {
    SerialSettings::new("COM3", 9600, SerialFormat::EIGHT_N_ONE)
}

/// Настройки порта ЛИС в тестах.
fn lis_settings() -> SerialSettings {
    SerialSettings::new("COM4", 9600, SerialFormat::EIGHT_N_ONE)
}

/// Сессия прослушки на виртуальной паре: `pairs` — сколько пар готовит тест.
async fn start(dir: &Path, pairs: usize) -> (SessionHandle, Vec<VirtualMonitorEnds>) {
    let (source, ends) =
        virtual_monitor(analyzer_settings(), lis_settings(), RETRY, pairs).expect("хук свободен");
    let handle = Session::start(
        SessionConfig::com_monitor(analyzer_settings(), lis_settings(), RETRY)
            .with_source(source)
            .with_log_dir(dir),
    )
    .await
    .expect("сессия прослушки");
    (handle, ends)
}

/// Отправляет `payload` фрагментами и читает столько же байт с другого конца.
async fn send_and_receive(
    from: &mut tokio::io::DuplexStream,
    to: &mut tokio::io::DuplexStream,
    payload: &[u8],
    fragmentation: Fragmentation,
) -> Vec<u8> {
    for (start, end) in fragment_bounds(payload.len(), fragmentation) {
        from.write_all(&payload[start..end])
            .await
            .expect("фрагмент");
        from.flush().await.expect("сброс фрагмента");
    }
    let mut received = vec![0u8; payload.len()];
    timeout(WAIT, to.read_exact(&mut received))
        .await
        .expect("пересылка дошла")
        .expect("чтение пересланного");
    received
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
            assert!(
                !data.synthetic,
                "в прослушке нет синтетических байтов (FR-10): ACK приходит от ЛИС"
            );
            bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 декодируется"),
            );
        }
    }
    bytes
}

/// Ждёт, пока в журнале появится столько событий `close`.
async fn wait_for_closes(path: &Path, expected: usize) -> Vec<Close> {
    for _ in 0..500 {
        let outcome = read_all(path).expect("журнал читается");
        let closes: Vec<Close> = outcome
            .lines
            .iter()
            .filter_map(|line| match &line.event {
                Event::Close(close) => Some(close.clone()),
                _ => None,
            })
            .collect();
        if closes.len() >= expected {
            return closes;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("{expected} событий close не появились в журнале");
}

/// Ждёт, пока в журнале появится столько событий `open`.
async fn wait_for_opens(path: &Path, expected: usize) {
    for _ in 0..500 {
        let outcome = read_all(path).expect("журнал читается");
        let opens = outcome
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Open(_)))
            .count();
        if opens >= expected {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("{expected} событий open не появились в журнале");
}

/// Номера и направления всех событий `data` журнала: фрагменты не склеиваются.
fn data_events(path: &Path) -> Vec<(u32, Dir, usize)> {
    let outcome = read_all(path).expect("журнал читается");
    outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Data(data) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64");
                Some((data.conn, data.dir, bytes.len()))
            }
            _ => None,
        })
        .collect()
}

/// Прослушка пересылает обмен в обе стороны без изменений и пишет пару как одно
/// соединение журнала (M5-AC1, M5-AC2, M5-AC6).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pair_forwards_both_directions_byte_for_byte() {
    let _hook = HOOK.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let (handle, mut ends) = start(dir.path(), 1).await;
    let path = handle.journal_path().to_path_buf();
    assert_eq!(
        handle.local_addr(),
        "COM3",
        "строка состояния — порт анализатора"
    );

    let mut analyzer = ends.pop().expect("пара");
    let mut sent = vec![ENQ];
    sent.extend(astm_fixture("analyzer_patient_result.astm"));
    let mut stream = sent.clone();
    stream.push(EOT);

    // Анализатор передаёт `ENQ`, запись и `EOT` по одному байту: журнал собирает
    // фрагменты по факту чтения и не теряет ни байта.
    let received = send_and_receive(
        &mut analyzer.analyzer,
        &mut analyzer.lis,
        &stream,
        Fragmentation::ByteByByte,
    )
    .await;
    assert_eq!(received, stream, "ASTM доходит до ЛИС без изменений");

    // ЛИС отвечает `ACK` и `ACK` на `ENQ` и запись, плюс HL7/MLLP-подтверждение: идёт
    // обратное направление, и оно тоже не меняется.
    let mut answer = vec![0x06u8, 0x06];
    let ack = frame(&fixture("lis_ack_aa.hl7"));
    answer.extend(&ack);
    let back = send_and_receive(
        &mut analyzer.lis,
        &mut analyzer.analyzer,
        &answer,
        Fragmentation::Random { seed: 7, max: 5 },
    )
    .await;
    assert_eq!(
        back, answer,
        "ответы ЛИС доходят до анализатора без изменений"
    );

    // Случайная нарезка входящего: фрагменты журнала равны тому, что отправлено.
    let noise: Vec<u8> = (0..300u32)
        .map(|at| u8::try_from(at % 251).expect("меньше 251"))
        .collect();
    let echoed = send_and_receive(
        &mut analyzer.analyzer,
        &mut analyzer.lis,
        &noise,
        Fragmentation::Random { seed: 3, max: 17 },
    )
    .await;
    assert_eq!(echoed, noise, "любая нарезка доходит целиком");

    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, 1, "пара — одно соединение");

    let outcome = read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(header.mode, Mode::ComMonitor);
    assert_eq!(
        header.autoresponder,
        Autoresponder::None,
        "автоответчика нет"
    );
    assert_eq!(header.settings["analyzer"]["port"], "COM3");
    assert_eq!(header.settings["analyzer"]["baud"], 9600);
    assert_eq!(header.settings["analyzer"]["format"], "8N1");
    assert_eq!(header.settings["analyzer"]["flow"], "none");
    assert_eq!(header.settings["analyzer"]["dtr"], serde_json::json!(true));
    assert_eq!(header.settings["analyzer"]["rts"], serde_json::json!(true));
    assert_eq!(header.settings["lis"]["port"], "COM4");
    assert_eq!(header.settings["lis"]["baud"], 9600);
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
    assert_eq!(open.peer, "COM3", "peer — порт анализатора");
    assert_eq!(open.local, "", "у порта нет локального адреса");
    let link = open.link.as_ref().expect("link — вторая сторона пары");
    assert_eq!(link.role, LinkRole::Lis);
    assert_eq!(link.peer, "COM4");
    assert_eq!(link.local, "");
    assert_eq!(open.conn, 1);

    let mut sent_all = stream;
    sent_all.extend(&noise);
    assert_eq!(
        recorded_bytes(&path, 1, Dir::AnalyzerToLis),
        sent_all,
        "направление анализатора записано байт в байт"
    );
    assert_eq!(
        recorded_bytes(&path, 1, Dir::LisToAnalyzer),
        answer,
        "направление ЛИС записано байт в байт"
    );
    // Фрагментация не склеена в одно событие: журнал пишет то, что прочитано за раз.
    let frames = data_events(&path);
    assert!(
        frames.len() > 1,
        "фрагменты журнала не склеены: {} событий data",
        frames.len()
    );
    assert!(
        frames.iter().all(|(conn, _, _)| *conn == 1),
        "все фрагменты — первого соединения: {frames:?}"
    );

    let Event::SessionEnd(end) = &outcome.lines.last().expect("последняя строка").event
    else {
        panic!("последняя строка — session_end");
    };
    assert_eq!(end.stats.connections, 1);
    assert_eq!(
        end.stats.bytes_analyzer_to_lis,
        u64::try_from(sent_all.len()).expect("длина")
    );
    assert_eq!(
        end.stats.bytes_lis_to_analyzer,
        u64::try_from(answer.len()).expect("длина")
    );
}

/// Пропажа адаптера одной стороны закрывает пару, источник переоткрывает оба порта и
/// пишет новое `open` со следующим `conn` (M5-AC2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_side_closes_the_pair_and_the_monitor_reopens() {
    let _hook = HOOK.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let (handle, mut pairs) = start(dir.path(), 2).await;
    let path = handle.journal_path().to_path_buf();

    let mut first = pairs.remove(0);
    let _ = send_and_receive(
        &mut first.analyzer,
        &mut first.lis,
        b"ENQ",
        Fragmentation::Whole,
    )
    .await;

    // Отключён USB-адаптер стороны ЛИС: её конец выгружается, и сессия видит конец данных
    // (на настоящем порту — ошибку чтения или записи) на порту ЛИС.
    drop(first.lis);

    let closes = wait_for_closes(&path, 1).await;
    assert_eq!(closes.len(), 1, "close один на пару");
    assert_eq!(closes[0].by, ClosedBy::Lis, "порт пропал у ЛИС");
    assert!(
        matches!(closes[0].reason.as_str(), "eof" | "error"),
        "причина закрытия: {}",
        closes[0].reason
    );
    drop(first.analyzer);

    // Второе открытие: новый pty занимает тот же путь, сессия открывает его заново.
    wait_for_opens(&path, 2).await;
    let mut second = pairs.remove(0);
    let _ = send_and_receive(
        &mut second.analyzer,
        &mut second.lis,
        b"ACK",
        Fragmentation::Whole,
    )
    .await;

    let stats = handle.stop().await.expect("остановка");
    assert!(stats.connections >= 2, "новая пара — новый conn");
    let outcome = read_all(&path).expect("журнал читается");
    let conns: Vec<u32> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Open(open) => Some(open.conn),
            _ => None,
        })
        .collect();
    assert_eq!(conns, vec![1, 2], "нумерация сквозная и без пропусков");
    assert_eq!(
        recorded_bytes(&path, 2, Dir::AnalyzerToLis),
        b"ACK",
        "обмен второй пары записан байт в байт"
    );
}

/// «Стоп» закрывает пару один раз и называет bytetap (M5-AC2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_closes_the_pair_once_and_names_bytetap() {
    let _hook = HOOK.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let (handle, mut pairs) = start(dir.path(), 1).await;
    let path = handle.journal_path().to_path_buf();
    let mut pair = pairs.remove(0);

    let _ = send_and_receive(
        &mut pair.analyzer,
        &mut pair.lis,
        b"ENQ",
        Fragmentation::Whole,
    )
    .await;
    handle.stop().await.expect("остановка");

    let outcome = read_all(&path).expect("журнал читается");
    let closes: Vec<&Close> = outcome
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Close(close) => Some(close),
            _ => None,
        })
        .collect();
    assert_eq!(closes.len(), 1, "close один на пару");
    assert_eq!(closes[0].by, ClosedBy::Bytetap);
    assert_eq!(closes[0].reason, "stop");
    assert!(
        outcome
            .lines
            .iter()
            .any(|line| matches!(line.event, Event::SessionEnd(_))),
        "штатная остановка пишет session_end"
    );
}

/// Прослушка перестаёт отвечать автоответчиком, даже если профиль задан настройками:
/// в заголовке `none`, и `synthetic` в журнале нет (M5-AC4).
#[test]
fn the_monitor_config_has_no_autoresponder() {
    let config = SessionConfig::com_monitor(
        SerialSettings::new("COM3", 9600, SerialFormat::EIGHT_N_ONE),
        SerialSettings::new("COM4", 19200, SerialFormat::EIGHT_N_ONE),
        RETRY,
    )
    .with_responder(Autoresponder::AstmE1381);
    assert_eq!(config.header_responder(), Autoresponder::None);
    assert_eq!(
        config.source.settings(""),
        serde_json::json!({
            "analyzer": {
                "port": "COM3",
                "baud": 9600,
                "format": "8N1",
                "flow": "none",
                "dtr": true,
                "rts": true,
            },
            "lis": {
                "port": "COM4",
                "baud": 19200,
                "format": "8N1",
                "flow": "none",
                "dtr": true,
                "rts": true,
            },
        })
    );
}
