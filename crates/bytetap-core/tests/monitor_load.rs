//! Задержка и нагрузка прослушки двух портов (NFR-5, NFR-6, M5-AC5, M5-AC10).
//!
//! Тесты помечены `#[ignore]`: они меряют, а не проверяют поведение на каждом push, и на
//! загруженном раннере могут дать другой результат. Запуск вручную:
//!
//! ```text
//! cargo test -p bytetap-core --test monitor_load -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Пара «портов» здесь виртуальная (хук `test-hooks`), поэтому замер показывает добавку
//! самой пересылки, без драйвера порта. Замер на pty — в `monitor_pty.rs`; числа и условия
//! записаны в `docs/dev/testing.md`.

use std::time::{Duration, Instant};

use base64::Engine as _;
use bytetap_core::journal::{Dir, Event, JournalReader, Stats};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};
use bytetap_core::transport::{reset_virtual_monitor, virtual_monitor};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

/// Сколько обменов «запрос — ответ» в замере задержки.
const ROUNDS: usize = 2000;

/// Размер одного замера: короткий кадр, как сообщение анализатора.
const FRAME: usize = 64;

/// Сколько байт идёт в каждую сторону в нагрузочном тесте.
const LOAD_BYTES: usize = 8 * 1024 * 1024;

/// Предел прироста пиковой памяти на нагрузке (M5-AC10, как у прокси).
const MEMORY_BUDGET: u64 = 128 * 1024 * 1024;

/// Предел добавки к задержке на направление, 99-й процентиль (M5-AC5).
const P99_BUDGET: Duration = Duration::from_millis(2);

/// Предел добавки к задержке на направление, максимум (M5-AC5).
const MAX_BUDGET: Duration = Duration::from_millis(20);

/// Метка хука: он глобальный для процесса.
static HOOK: Mutex<()> = Mutex::const_new(());

/// Снимает виртуальные пары при выходе из теста, даже если проверка упала.
struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        reset_virtual_monitor();
    }
}

/// Сессия прослушки на виртуальной паре: источник, конец теста и журнал.
async fn start(
    dir: &std::path::Path,
) -> (
    bytetap_core::session::SessionHandle,
    DuplexStream,
    DuplexStream,
) {
    let (source, mut ends) = virtual_monitor(
        SerialSettings::new("COM3", 9600, SerialFormat::EIGHT_N_ONE),
        SerialSettings::new("COM4", 9600, SerialFormat::EIGHT_N_ONE),
        Duration::from_millis(100),
        1,
    )
    .expect("хук свободен");
    let handle = Session::start(
        SessionConfig::com_monitor(
            SerialSettings::new("COM3", 9600, SerialFormat::EIGHT_N_ONE),
            SerialSettings::new("COM4", 9600, SerialFormat::EIGHT_N_ONE),
            Duration::from_millis(100),
        )
        .with_source(source)
        .with_log_dir(dir),
    )
    .await
    .expect("сессия прослушки");
    let pair = ends.pop().expect("пара");
    (handle, pair.analyzer, pair.lis)
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

/// Обмен «запрос — ответ» через два конца виртуальной пары без bytetap: базовая линия.
async fn ping_pong_direct(rounds: usize, size: usize) -> Vec<Duration> {
    let (mut first, mut second) = tokio::io::duplex(64 * 1024);
    let echo = tokio::spawn(async move {
        let mut buffer = vec![0u8; size];
        while second.read_exact(&mut buffer).await.is_ok() {
            if second.write_all(&buffer).await.is_err() {
                return;
            }
        }
    });
    let payload = vec![b'x'; size];
    let mut answer = vec![0u8; size];
    let mut rounds_time = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        first.write_all(&payload).await.expect("запрос");
        first.read_exact(&mut answer).await.expect("ответ");
        rounds_time.push(started.elapsed());
    }
    echo.abort();
    rounds_time
}

/// Обмен «запрос — ответ» через прослушку: анализатор спрашивает, ЛИС отвечает.
async fn ping_pong_monitor(
    analyzer: &mut DuplexStream,
    lis: &mut DuplexStream,
    rounds: usize,
    size: usize,
) -> Vec<Duration> {
    let payload = vec![b'x'; size];
    let mut request = vec![0u8; size];
    let mut answer = vec![0u8; size];
    let mut rounds_time = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        analyzer
            .write_all(&payload)
            .await
            .expect("запрос анализатора");
        lis.read_exact(&mut request).await.expect("запрос у ЛИС");
        lis.write_all(&request).await.expect("ответ ЛИС");
        analyzer
            .read_exact(&mut answer)
            .await
            .expect("ответ у анализатора");
        rounds_time.push(started.elapsed());
    }
    rounds_time
}

/// Читает ровно `len` байт, не закрывая конец: закрытие конца закрыло бы пару.
async fn read_exactly(reader: &mut ReadHalf<DuplexStream>, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    reader.read_exact(&mut out).await.expect("эхо целиком");
    out
}

/// Пишет поток кусками по 64 КиБ, не закрывая конец: у пары нет полузакрытия.
async fn write_all_chunks(writer: &mut WriteHalf<DuplexStream>, payload: &[u8]) {
    for part in payload.chunks(64 * 1024) {
        writer.write_all(part).await.expect("фрагмент нагрузки");
    }
}

/// Занятая память процесса: пик (VmHWM) на Linux, иначе `None`.
fn peak_memory() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut peak = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            peak = Some(kib * 1024);
        }
    }
    peak
}

/// FNV-1a: быстрый хеш для сравнения больших потоков без второй копии в памяти.
fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "замер задержки: запускается вручную (M5-AC5)"]
async fn the_monitor_latency_stays_within_the_budget() {
    let _hook = HOOK.lock().await;
    let _guard = HookGuard;
    let direct = ping_pong_direct(ROUNDS, FRAME).await;

    let dir = tempfile::tempdir().expect("временная папка");
    let (handle, mut analyzer, mut lis) = start(dir.path()).await;
    let through = ping_pong_monitor(&mut analyzer, &mut lis, ROUNDS, FRAME).await;
    handle.stop().await.expect("остановка");

    let baseline = median(direct.clone());
    // Круг через прослушку — два направления: добавка на направление — половина разницы.
    let added: Vec<Duration> = through
        .iter()
        .map(|round| round.saturating_sub(baseline) / 2)
        .collect();
    let p50 = percentile(added.clone(), 50);
    let p99 = percentile(added.clone(), 99);
    let max = added.iter().copied().max().expect("замеры есть");
    eprintln!(
        "задержка прослушки на виртуальной паре: кругов {ROUNDS} по {FRAME} Б, \
         прямой круг p50 {baseline:?}, добавка на направление p50 {p50:?}, \
         p99 {p99:?}, max {max:?}"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "нагрузка 8 МиБ в обе стороны: запускается вручную (M5-AC10)"]
async fn eight_megabytes_go_both_ways() {
    let _hook = HOOK.lock().await;
    let _guard = HookGuard;
    let dir = tempfile::tempdir().expect("временная папка");
    let (handle, analyzer, lis) = start(dir.path()).await;
    let path = handle.journal_path().to_path_buf();

    // Синтетическая запись, повторённая до 8 МиБ: печатные байты, чтобы журнал
    // не раздувался экранированием.
    let record =
        b"H|\\^&|||ANALYZER|||||||P|E1394-97\rP|1|PATIENT^TEST\rR|1|^^^GLU^1|5.4|mmol/L\rL|1|N\r";
    let payload: Vec<u8> = record.iter().copied().cycle().take(LOAD_BYTES).collect();
    let hash = fnv1a(&payload, 0xcbf2_9ce4_8422_2325);

    let baseline_memory = peak_memory();
    let started = Instant::now();

    // Оба направления идут одновременно, как в настоящем обмене: анализатор передаёт
    // записи, ЛИС отвечает тем же потоком.
    let (mut analyzer_read, mut analyzer_write) = tokio::io::split(analyzer);
    let (mut lis_read, mut lis_write) = tokio::io::split(lis);
    let to_lis = payload.clone();
    let to_analyzer = payload.clone();
    let analyzer_task = tokio::spawn(async move {
        let send = write_all_chunks(&mut analyzer_write, &to_lis);
        let receive = read_exactly(&mut analyzer_read, LOAD_BYTES);
        let ((), received) = tokio::join!(send, receive);
        received
    });
    let lis_task = tokio::spawn(async move {
        let send = write_all_chunks(&mut lis_write, &to_analyzer);
        let receive = read_exactly(&mut lis_read, LOAD_BYTES);
        let ((), received) = tokio::join!(send, receive);
        received
    });
    let at_analyzer = analyzer_task.await.expect("направление ЛИС → анализатор");
    let at_lis = lis_task.await.expect("направление анализатор → ЛИС");
    let exchange = started.elapsed();
    assert_eq!(
        fnv1a(&at_lis, 0xcbf2_9ce4_8422_2325),
        hash,
        "ЛИС получила ровно отправленное анализатором"
    );
    assert_eq!(
        fnv1a(&at_analyzer, 0xcbf2_9ce4_8422_2325),
        hash,
        "анализатор получил ровно отправленное ЛИС"
    );
    let memory = match (baseline_memory, peak_memory()) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };

    let stats = handle.stop().await.expect("остановка");
    assert_eq!(
        stats,
        Stats {
            connections: 1,
            bytes_analyzer_to_lis: LOAD_BYTES as u64,
            bytes_lis_to_analyzer: LOAD_BYTES as u64,
        }
    );

    // Журнал читается потоком: 8 МиБ в base64 не должны грузиться в память целиком.
    let mut a2l = (0u64, 0xcbf2_9ce4_8422_2325u64);
    let mut l2a = (0u64, 0xcbf2_9ce4_8422_2325u64);
    let mut end = None;
    for line in JournalReader::open(&path).expect("журнал читается") {
        let line = line.expect("строка журнала");
        match line.event {
            Event::Data(data) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64 декодируется");
                let slot = match data.dir {
                    Dir::AnalyzerToLis => &mut a2l,
                    Dir::LisToAnalyzer => &mut l2a,
                };
                slot.0 += bytes.len() as u64;
                slot.1 = fnv1a(&bytes, slot.1);
            }
            Event::SessionEnd(event) => end = Some(event.stats),
            _ => {}
        }
    }
    assert_eq!(a2l, (LOAD_BYTES as u64, hash), "журнал: анализатор → ЛИС");
    assert_eq!(l2a, (LOAD_BYTES as u64, hash), "журнал: ЛИС → анализатор");
    assert_eq!(end, Some(stats), "session_end совпадает со счётчиками");

    eprintln!(
        "нагрузка прослушки: {LOAD_BYTES} Б в каждую сторону за {exchange:?}, \
         прирост пика памяти: {}",
        memory.map_or_else(|| "не измерен".to_owned(), |bytes| format!("{bytes} Б"))
    );
    if let Some(memory) = memory {
        assert!(
            memory < MEMORY_BUDGET,
            "память выросла на {memory} Б, больше {MEMORY_BUDGET} Б"
        );
    }
}
