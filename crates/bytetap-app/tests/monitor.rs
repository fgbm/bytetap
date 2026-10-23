//! Прослушка из интерфейса без железа (M5-AC9, M5-AC11): шаг сценария запускает сессию
//! на виртуальной паре портов ядра (`virtual_monitor` под `test-hooks`), тест играет
//! «анализатор» и «ЛИС», лента показывает `open` пары с `link`, `data` обеих сторон и
//! `close`, журнал несёт `session_end`. Данные — только синтетические (`ANALYZER-3`,
//! `PATIENT^TEST`), как в макете M5-4.

use std::path::Path;
use std::time::{Duration, Instant};

use bytetap_app::feed::FeedKind;
use bytetap_app::live::Phase;
use bytetap_app::settings::{SessionMode, Settings};
use bytetap_app::{BytetapApp, Screen};
use bytetap_core::journal::Dir;
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

/// Кадры столько, сколько нужно условию: сессия живёт в своём runtime.
fn step_until(
    harness: &mut Harness<'static, BytetapApp>,
    what: &str,
    mut done: impl FnMut(&mut Harness<'static, BytetapApp>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        harness.step();
        if done(harness) {
            return;
        }
        assert!(Instant::now() < deadline, "не дождались: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Строки журнала: тип каждой строки по порядку.
fn journal_types(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("журнал читается")
        .lines()
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("строка журнала — JSON");
            value["type"].as_str().unwrap_or_default().to_owned()
        })
        .collect()
}

/// Настройки шага прослушки: два порта, как их открывает виртуальная пара.
fn monitor_settings(log_dir: &str) -> Settings {
    let mut settings = Settings::with_log_dir(log_dir);
    settings.config.mode = SessionMode::ComMonitor;
    settings.config.serial.port = "COM3".into();
    settings.config.monitor_lis.port = "COM4".into();
    settings
}

/// Кадр ASTM E1381 с контрольной суммой: `STX…ETX` и `<сумма>CR LF`.
fn astm_frame(text: &str) -> Vec<u8> {
    let mut frame = vec![0x02];
    frame.extend_from_slice(text.as_bytes());
    frame.push(0x03);
    let sum = text
        .bytes()
        .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(byte)))
        % 256;
    frame.extend_from_slice(format!("{sum:02X}\r\n").as_bytes());
    frame
}

/// Прослушка из формы на виртуальной паре: обмен ASTM в обе стороны, лента и журнал
/// (M5-AC9, M5-AC11).
#[test]
fn a_monitor_from_the_form_records_both_ports() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let rt = runtime();
    let format = SerialFormat::EIGHT_N_ONE;
    let (_, mut ends) = bytetap_core::transport::virtual_monitor(
        SerialSettings::new("COM3", 9600, format),
        SerialSettings::new("COM4", 9600, format),
        Duration::from_secs(1),
        1,
    )
    .expect("виртуальная пара");
    let mut end = ends.remove(0);

    let handle = rt.handle().clone();
    let settings = monitor_settings(&log_dir);
    let mut harness = Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_runtime(handle)
                .with_screen(Screen::Step(SessionMode::ComMonitor))
        });
    harness.run_steps(2);
    harness.get_by_label("Старт").click();
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    assert_eq!(harness.state().screen, Screen::Recording);
    let path = harness.state().recorded_path().cloned().expect("журнал");

    // «Анализатор» шлёт ENQ и кадр, «ЛИС» отвечает ACK: обмен одного сеанса прибора.
    rt.block_on(async {
        let mut buffer = [0u8; 4096];
        end.analyzer.write_all(b"\x05").await.expect("ENQ");
        end.lis.read_exact(&mut buffer[..1]).await.expect("ENQ у ЛИС");
        end.lis.write_all(b"\x06").await.expect("ACK");
        end.analyzer
            .read_exact(&mut buffer[..1])
            .await
            .expect("ACK у анализатора");
        let frame = astm_frame(
            "H|\\^&|||ANALYZER-3^1.0|||||||P|E1394-97|20261026101503\rP|1|000123|PATIENT^TEST\rL|1|N\r",
        );
        end.analyzer.write_all(&frame).await.expect("кадр");
        let read = end.lis.read(&mut buffer).await.expect("кадр у ЛИС");
        assert!(read > 0, "ЛИС получила кадр");
        end.lis.write_all(b"\x06").await.expect("ACK на кадр");
        let read = end.analyzer.read(&mut buffer).await.expect("ACK на кадр");
        assert!(read > 0, "анализатор получил ACK");
        end.analyzer.write_all(b"\x04").await.expect("EOT");
        let read = end.lis.read(&mut buffer).await.expect("EOT у ЛИС");
        assert!(read > 0, "ЛИС получила EOT");
    });

    // Лента: `open` пары несёт порт ЛИС, `data` — обе стороны (M5-В4, M5-В8).
    let feed = harness.state().recording().unwrap().shared();
    step_until(
        &mut harness,
        "фрагменты обеих сторон",
        |_| {
            let dirs: Vec<Dir> = bytetap_app::live::lock(&feed)
                .iter()
                .filter_map(|event| event.data().map(|data| data.dir))
                .collect();
            dirs.contains(&Dir::AnalyzerToLis) && dirs.contains(&Dir::LisToAnalyzer)
        },
    );
    let ring = bytetap_app::live::lock(&feed);
    let open = ring.iter().find_map(|event| match &event.kind {
        FeedKind::Open { peer, link } => Some((peer.clone(), link.clone())),
        _ => None,
    });
    let (peer, link) = open.expect("open пары в ленте");
    assert_eq!(peer, "COM3", "принятая сторона — порт анализатора");
    let link = link.expect("у пары есть вторая сторона");
    assert_eq!(link.role, bytetap_core::journal::LinkRole::Lis);
    assert_eq!(link.peer, "COM4", "вторая сторона — порт ЛИС");
    drop(ring);

    // «Стоп»: пара закрывается, журнал получает `session_end`.
    harness.get_by_label_contains("Стоп 00:").click();
    step_until(&mut harness, "остановка", |h| {
        h.state().phase() == Phase::Idle
    });
    let stats = harness.state().stop_stats().expect("итоги записи");
    assert_eq!(stats.connections, 1, "пара портов — одно соединение");
    assert!(stats.bytes_analyzer_to_lis > 0 && stats.bytes_lis_to_analyzer > 0);

    let types = journal_types(&path);
    assert_eq!(types.first().map(String::as_str), Some("session"));
    assert_eq!(types.last().map(String::as_str), Some("session_end"));
    let text = std::fs::read_to_string(&path).expect("журнал");
    let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(header["mode"], "com_monitor");
    assert_eq!(header["autoresponder"], "none");
    assert_eq!(header["settings"]["analyzer"]["port"], "COM3");
    assert_eq!(header["settings"]["lis"]["port"], "COM4");
    let open_line = text
        .lines()
        .find(|line| line.contains("\"type\":\"open\""))
        .expect("open в журнале");
    let value: serde_json::Value = serde_json::from_str(open_line).unwrap();
    assert_eq!(value["peer"], "COM3");
    assert_eq!(value["link"]["role"], "lis");
    assert_eq!(value["link"]["peer"], "COM4");
    // Закрытие — сторона порта и причина: у порта нет соединения, которое кто-то закрыл.
    let close_line = text
        .lines()
        .find(|line| line.contains("\"type\":\"close\""))
        .expect("close в журнале");
    let close: serde_json::Value = serde_json::from_str(close_line).unwrap();
    assert_eq!(close["by"], "bytetap");
    assert_eq!(close["reason"], "stop");

    bytetap_core::transport::reset_virtual_monitor();
}
