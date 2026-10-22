//! Задержка и нагрузка прокси (NFR-5, NFR-6, M4-AC4, M4-AC12).
//!
//! Тесты помечены `#[ignore]`: они меряют, а не проверяют поведение на каждом push, и на
//! загруженном раннере могут дать другой результат. Запуск вручную:
//!
//! ```text
//! cargo test -p bytetap-core --test proxy_load -- --ignored --nocapture
//! ```
//!
//! Числа и условия замера записаны в `docs/dev/testing.md`.

use std::time::{Duration, Instant};

use base64::Engine as _;
use bytetap_core::journal::{Dir, Event, JournalReader, Stats};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// Сколько обменов «запрос — ответ» в замере задержки.
const ROUNDS: usize = 2000;

/// Размер одного замера: короткий кадр, как сообщение анализатора.
const FRAME: usize = 64;

/// Сколько байт идёт в каждую сторону в нагрузочном тесте.
const LOAD_BYTES: usize = 8 * 1024 * 1024;

/// Сколько растёт память процесса за нагрузочный тест.
///
/// Пик задаёт не пересылка: у неё на направление один буфер 16 КиБ, — а очередь
/// писателя журнала. Канал писателя неограниченный (ADR 0001), и на быстрой пересылке
/// журнал отстаёт на мегабайты строк. Предел держит поток в разумных рамках, а не
/// обещает постоянную память: замер debug-сборки — около 50 МБ на 8 МиБ в обе стороны.
const MEMORY_BUDGET: u64 = 128 * 1024 * 1024;

/// Предел добавки к задержке на loopback, 99-й процентиль (M4-AC4).
const P99_BUDGET: Duration = Duration::from_millis(2);

/// Предел добавки к задержке на loopback, максимум (M4-AC4).
const MAX_BUDGET: Duration = Duration::from_millis(20);

/// Обмен «запрос — ответ» через соединение: возвращает времена круга.
async fn ping_pong(addr: std::net::SocketAddr, rounds: usize, size: usize) -> Vec<Duration> {
    let mut stream = TcpStream::connect(addr).await.expect("соединение");
    stream.set_nodelay(true).expect("TCP_NODELAY");
    let payload = vec![b'x'; size];
    let mut answer = vec![0u8; size];
    let mut rounds_time = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        stream.write_all(&payload).await.expect("запрос");
        stream.read_exact(&mut answer).await.expect("ответ");
        rounds_time.push(started.elapsed());
    }
    let _ = stream.shutdown().await;
    rounds_time
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

/// Один замер: круг через прокси и круг напрямую к той же ЛИС.
async fn measure() -> (Vec<Duration>, Vec<Duration>) {
    // Напрямую: та же ЛИС-эхо, без прокси.
    let direct_lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Echo);
    let direct_addr = direct_lis.local_addr().expect("адрес");
    let direct_task = tokio::spawn(direct_lis.run());
    let direct = ping_pong(direct_addr, ROUNDS, FRAME).await;
    direct_task.await.expect("ЛИС").expect("обмен");

    // Через прокси: ЛИС та же, между ней и анализатором стоит bytetap.
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Echo);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());
    let handle = Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            lis_addr.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen = handle.local_addr().parse().expect("адрес прокси");
    let through = ping_pong(listen, ROUNDS, FRAME).await;
    lis_task.await.expect("ЛИС").expect("обмен");
    handle.stop().await.expect("остановка");
    (direct, through)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "замер задержки: запускается вручную (M4-AC4)"]
async fn the_forwarding_latency_stays_within_the_budget() {
    let (direct, through) = measure().await;
    let baseline = median(direct.clone());
    // Круг через прокси — два направления: добавка на направление — половина разницы.
    let added: Vec<Duration> = through
        .iter()
        .map(|round| round.saturating_sub(baseline) / 2)
        .collect();
    let p50 = percentile(added.clone(), 50);
    let p99 = percentile(added.clone(), 99);
    let max = added.iter().copied().max().expect("замеры есть");
    eprintln!(
        "задержка пересылки: кругов {ROUNDS} по {FRAME} Б, прямой круг p50 {baseline:?}, \
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
#[ignore = "нагрузка 8 МиБ в обе стороны: запускается вручную (M4-AC12)"]
async fn eight_megabytes_go_both_ways() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Echo)
        .with_timeout(Duration::from_secs(120));
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());
    let handle = Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            lis_addr.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: std::net::SocketAddr = handle.local_addr().parse().expect("адрес");
    let path = handle.journal_path().to_path_buf();

    // Синтетическая запись анализатора, повторённая до 8 МиБ: печатные байты, чтобы
    // журнал не раздувался экранированием.
    let record =
        b"H|\\^&|||ANALYZER|||||||P|E1394-97\rP|1|PATIENT^TEST\rR|1|^^^GLU^1|5.4|mmol/L\rL|1|N\r";
    let payload: Vec<u8> = record.iter().copied().cycle().take(LOAD_BYTES).collect();
    let sent_hash = fnv1a(&payload, 0xcbf2_9ce4_8422_2325);

    let baseline_memory = peak_memory();
    let started = Instant::now();
    let stream = TcpStream::connect(listen).await.expect("анализатор");
    let (mut reader, mut writer) = stream.into_split();
    let chunk = payload.clone();
    let sender = tokio::spawn(async move {
        for part in chunk.chunks(64 * 1024) {
            writer.write_all(part).await.expect("фрагмент нагрузки");
        }
        writer.shutdown().await.expect("FIN анализатора");
    });
    let mut echoed = Vec::with_capacity(LOAD_BYTES);
    reader.read_to_end(&mut echoed).await.expect("эхо 8 МиБ");
    sender.await.expect("отправка");
    let exchange = started.elapsed();
    assert_eq!(echoed.len(), LOAD_BYTES, "эхо целиком");
    assert_eq!(
        fnv1a(&echoed, 0xcbf2_9ce4_8422_2325),
        sent_hash,
        "прокси вернул ровно отправленное"
    );
    let memory = match (baseline_memory, peak_memory()) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };

    let lis_report = lis_task.await.expect("ЛИС").expect("обмен");
    assert_eq!(lis_report.all_received().len(), LOAD_BYTES);
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
    assert_eq!(
        a2l,
        (LOAD_BYTES as u64, sent_hash),
        "журнал: анализатор → ЛИС"
    );
    assert_eq!(
        l2a,
        (LOAD_BYTES as u64, sent_hash),
        "журнал: ЛИС → анализатор"
    );
    assert_eq!(end, Some(stats), "session_end совпадает со счётчиками");

    eprintln!(
        "нагрузка: {LOAD_BYTES} Б в каждую сторону за {exchange:?}, прирост пика памяти: \
         {}",
        memory.map_or_else(|| "не измерен".to_owned(), |bytes| format!("{bytes} Б"))
    );
    if let Some(memory) = memory {
        assert!(
            memory < MEMORY_BUDGET,
            "память выросла на {memory} Б, больше {MEMORY_BUDGET} Б"
        );
    }
}
