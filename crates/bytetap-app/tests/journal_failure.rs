//! Отказ журнала и остановка записи: интерфейс показывает причину (U1-AC17).
//!
//! Писатель журнала ломается посреди записи — как сломанный диск. Ядро останавливает
//! сессию, окно переходит на стартовый экран и показывает строку отказа с «Выбрать
//! другую папку» и «Открыть записанное». Сессия, закончившаяся сама, и падение задачи
//! сессии — тоже видны: окно замечает их и освобождает слот.
//!
//! Хуки `TEST_FAIL_AFTER_EVENTS`, `TEST_END_AFTER_EVENTS` и `TEST_PANIC_AFTER_EVENTS`
//! глобальны для процесса, поэтому тесты живут отдельным бинаром: соседние
//! интеграционные тесты сессии они не задевают. Данные — только синтетические.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytetap_app::live::Phase;
use bytetap_app::settings::Settings;
use bytetap_app::{BytetapApp, Screen};
use bytetap_core::journal::writer::TEST_FAIL_AFTER_EVENTS;
use bytetap_core::session::{TEST_END_AFTER_EVENTS, TEST_PANIC_AFTER_EVENTS};
use bytetap_testkit::emulator::{AnalyzerEmulator, frame};
use egui_kittest::Harness;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

fn hl7(index: usize) -> Vec<u8> {
    frame(
        format!(
            "MSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|MSG{index:05}|P|2.3.1\rPID|1||000001||PATIENT^TEST\r"
        )
        .as_bytes(),
    )
}

/// Кадры харнесса до условия, с паузой: сессия живёт в своём runtime.
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

/// Хуки ядра глобальны для процесса, поэтому тесты этой группы идут по очереди.
static HOOKS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Занимает хуки ядра на время теста: тесты этой группы не идут одновременно.
fn hooks() -> std::sync::MutexGuard<'static, ()> {
    HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Окно с записью TCP-сервера на loopback: пишет журнал в свою папку.
fn recording_harness(
    rt: &tokio::runtime::Runtime,
    log_dir: String,
) -> Harness<'static, BytetapApp> {
    let handle = rt.handle().clone();
    Harness::builder()
        .with_size((1200.0, 800.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, Settings::with_log_dir(&log_dir), None, None)
                .with_runtime(handle)
                .with_test_autostart(&cc.egui_ctx, "tcp_server:127.0.0.1:0")
        })
}

/// Отказ журнала во время записи: окно показывает строку причины и уходит на стартовый
/// экран, записанный журнал остаётся доступным (U1-AC17).
#[test]
fn a_journal_failure_shows_the_reason_on_the_start_screen() {
    let _hooks = hooks();
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let mut harness = recording_harness(&rt, log_dir);
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    let path = harness.state().recorded_path().cloned().expect("журнал");
    let addr: SocketAddr = harness
        .state()
        .recording()
        .unwrap()
        .local_addr()
        .parse()
        .unwrap();

    // Писатель ломается на первом же событии после старта: обмен в журнал не попадёт.
    TEST_FAIL_AFTER_EVENTS.store(0, Ordering::Relaxed);
    // ACK не придёт: сессия остановится на отказе журнала. Это и есть ожидаемое.
    let _ = rt.block_on(AnalyzerEmulator::new(addr).with_messages([hl7(1)]).run());

    // Окно замечает отказ само: сессия ядра закончилась, строка причины на экране.
    step_until(&mut harness, "строка отказа", |h| {
        h.state().journal_failure().is_some()
    });
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
    assert_eq!(harness.state().phase(), Phase::Failed);
    let failure = harness.state().journal_failure().expect("отказ");
    assert!(!failure.message.is_empty(), "причина отказа не пустая");

    // Строка отказа видна на экране вместе с действиями.
    use egui_kittest::kittest::Queryable as _;
    harness.get_by_label_contains("Запись в журнал прервана");
    harness.get_by_label("Выбрать другую папку");
    harness.get_by_label("Открыть записанное");

    // Журнал остался на диске: записанное до отказа можно открыть.
    assert!(path.exists(), "журнал {} есть", path.display());
    let text = std::fs::read_to_string(&path).expect("журнал читается");
    assert!(
        text.contains("\"type\":\"session\""),
        "заголовок сессии на месте"
    );
    assert!(
        !text.contains("\"type\":\"session_end\""),
        "без журнала итогов сессии нет: {text}"
    );

    TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);
}

/// Закрытие окна во время записи, когда журнал отказывает при остановке: окно
/// показывает системное сообщение о неполном журнале, а не закрывается молча (U1-AC17).
#[test]
fn closing_after_a_journal_failure_tells_the_user_about_the_journal() {
    let _hooks = hooks();
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let mut harness = recording_harness(&rt, log_dir);
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    let addr: SocketAddr = harness
        .state()
        .recording()
        .unwrap()
        .local_addr()
        .parse()
        .unwrap();
    // Обмен проходит: журнал пишется, окно закрывать есть что.
    rt.block_on(AnalyzerEmulator::new(addr).with_messages([hl7(1)]).run())
        .expect("эмулятор");

    // Писатель ломается на следующей пачке: отказ придёт при остановке записи.
    TEST_FAIL_AFTER_EVENTS.store(0, Ordering::Relaxed);
    use eframe::egui::{ViewportCommand, ViewportEvent, ViewportId};
    let notice = bytetap_app::CLOSE_NOTICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(notice.is_none(), "до закрытия сообщения не было");

    harness
        .input_mut()
        .viewports
        .get_mut(&ViewportId::ROOT)
        .unwrap()
        .events
        .push(ViewportEvent::Close);
    harness.step();
    let commands: Vec<ViewportCommand> = harness
        .output()
        .viewport_output
        .values()
        .flat_map(|viewport| viewport.commands.clone())
        .collect();
    assert!(
        commands.contains(&ViewportCommand::CancelClose),
        "закрытие отменено до остановки"
    );
    step_until(&mut harness, "закрытие окна", |h| {
        h.output()
            .viewport_output
            .values()
            .flat_map(|viewport| viewport.commands.clone())
            .any(|command| command == ViewportCommand::Close)
    });
    let notice = bytetap_app::CLOSE_NOTICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("окно показало сообщение о неполном журнале");
    assert!(
        notice.contains("Журнал мог быть дописан не полностью"),
        "{notice}"
    );

    TEST_FAIL_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);
}

/// Сессия ядра падает сама: слот освобождается, окно это замечает и показывает ошибку
/// остановки на стартовом экране (как `stop_failed` в UI v2, U1-AC17).
#[test]
fn a_stop_error_is_visible_on_the_start_screen() {
    let _hooks = hooks();
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let mut harness = recording_harness(&rt, log_dir);
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    let addr: SocketAddr = harness
        .state()
        .recording()
        .unwrap()
        .local_addr()
        .parse()
        .unwrap();

    // Задача сессии падает: остановка вернёт ошибку без причины журнала.
    TEST_PANIC_AFTER_EVENTS.store(0, Ordering::Relaxed);
    let _ = rt.block_on(AnalyzerEmulator::new(addr).with_messages([hl7(1)]).run());

    step_until(&mut harness, "ошибка остановки", |h| {
        h.state().stop_error().is_some()
    });
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
    assert!(!harness.state().session_busy(), "слот освобождён");
    use egui_kittest::kittest::Queryable as _;
    harness.get_by_label_contains("Запись остановлена с ошибкой");

    TEST_PANIC_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);
}

/// Сессия ядра закончилась сама без отказа журнала: окно замечает это, останавливает
/// таймер и возвращает стартовый экран (U1-AC17).
#[test]
fn a_session_that_finished_by_itself_is_noticed() {
    let _hooks = hooks();
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime();
    let log_dir = dir.path().to_string_lossy().into_owned();
    let mut harness = recording_harness(&rt, log_dir);
    step_until(&mut harness, "запись", |h| {
        h.state().phase() == Phase::Recording
    });
    let path = harness.state().recorded_path().cloned().expect("журнал");
    let addr: SocketAddr = harness
        .state()
        .recording()
        .unwrap()
        .local_addr()
        .parse()
        .unwrap();

    // Сессия заканчивается сама после первого обмена: ядро пишет `session_end`.
    TEST_END_AFTER_EVENTS.store(1, Ordering::Relaxed);
    let _ = rt.block_on(AnalyzerEmulator::new(addr).with_messages([hl7(1)]).run());

    step_until(&mut harness, "остановка", |h| {
        !h.state().session_busy()
    });
    harness.run_steps(2);
    assert_eq!(harness.state().screen, Screen::Scenarios);
    assert!(
        harness.state().journal_failure().is_none(),
        "отказа журнала не было"
    );
    assert!(
        harness.state().stop_error().is_none(),
        "ошибки остановки нет: сессия закончилась сама"
    );
    // Итоги записи видны на стартовом экране.
    let stats = harness.state().stop_stats().expect("итоги записи");
    assert_eq!(stats.connections, 1, "одно соединение");
    assert!(stats.bytes_analyzer_to_lis > 0, "байты от анализатора");
    use egui_kittest::kittest::Queryable as _;
    harness.get_by_label_contains("Запись закончена");
    let text = std::fs::read_to_string(&path).expect("журнал читается");
    assert!(
        text.contains("\"type\":\"session_end\""),
        "журнал закончен итогами: {text}"
    );

    TEST_END_AFTER_EVENTS.store(u64::MAX, Ordering::Relaxed);
}
