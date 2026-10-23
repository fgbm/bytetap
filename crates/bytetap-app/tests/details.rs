//! Панель деталей выбранной строки ленты (L1): клик по строке, полный текст без предела
//! 2048 байт, вкладка HEX, Esc, Ctrl+A/C и остановка следования во время записи.
//!
//! Данные синтетические: HL7/MLLP по кодировке CP1251 и управляющие байты.

use std::sync::Arc;
use std::time::Duration;

use bytetap_app::feed::FeedEvent;
use bytetap_app::live;
use bytetap_app::recording::RecordingFixture;
use bytetap_app::settings::{Encoding, FeedView, Settings};
use bytetap_app::{BytetapApp, Fixtures, Screen};
use bytetap_core::journal::{Data, Dir, Event, Stats};
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::{Harness, kittest::NodeT as _, kittest::Queryable as _};

const LOG_DIR: &str = r"C:\Users\operator\Documents\bytetap";
const WINDOW: (f32, f32) = (1200.0, 800.0);

/// Фрагмент длиннее предела строки ленты: 3 000 байт — панель показывает все.
const LONG: usize = 3000;

fn fixtures() -> Fixtures {
    Fixtures {
        interfaces: Vec::new(),
        ports: Vec::new(),
        recents: Vec::new(),
        now: chrono::DateTime::parse_from_rfc3339("2026-10-19T12:00:00+03:00").unwrap(),
    }
}

fn settings() -> Settings {
    let mut settings = Settings::with_log_dir(LOG_DIR);
    settings.feed.encoding = Encoding::Cp1251;
    settings
}

/// Запись: два коротких фрагмента HL7 и длинный ORU без предела строки ленты.
fn fixture() -> RecordingFixture {
    let at = |ms: u64| {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-19T10:15:03.000+03:00").unwrap();
        (base + chrono::Duration::milliseconds(ms as i64))
            .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
            .to_string()
    };
    let cp1251 = |text: &str| -> Vec<u8> {
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(text);
        bytes.into_owned()
    };
    let mut long =
        String::from("\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101503||ORU^R01|MSGLONG|P|2.3.1\r");
    while long.len() < LONG {
        long.push_str("OBX|1|NM|GLU^Glucose||5.4|mmol/L|3.9-6.1|N|||F\r");
    }
    long.push_str("\x1c\r");
    let events = vec![
        Event::Data(Data::new(
            at(0),
            0,
            1,
            Dir::AnalyzerToLis,
            false,
            &cp1251(
                "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101503||ORU^R01|MSG0001|P|2.3.1\rPID|1||000001||PATIENT^TEST\r\x1c\r",
            ),
        )),
        Event::Data(Data::new(
            at(500),
            500_000,
            1,
            Dir::LisToAnalyzer,
            true,
            &cp1251(
                "\x0bMSH|^~\\&|LIS||ANALYZER|LAB|20261019101503||ACK^R01|BT0001|P|2.3.1\rMSA|AA|MSG0001\r\x1c\r",
            ),
        )),
        Event::Data(Data::new(
            at(1000),
            1_000_000,
            1,
            Dir::AnalyzerToLis,
            false,
            &cp1251(&long),
        )),
    ];
    RecordingFixture {
        events,
        stats: Stats {
            connections: 1,
            bytes_analyzer_to_lis: 629,
            bytes_lis_to_analyzer: 195,
        },
        elapsed: Duration::from_secs(3),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: Some(format!(r"{LOG_DIR}\bytetap-20261019101503-tcp_server.jsonl").into()),
        dropped: 0,
    }
}

fn harness_with(settings: Settings, fixture: RecordingFixture) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture)
        });
    harness.run_steps(3);
    harness
}

fn harness() -> Harness<'static, BytetapApp> {
    harness_with(settings(), fixture())
}

/// Клик по строке ленты с текстом `needle`: так же, как это делает инженер мышью.
fn click_row(harness: &mut Harness<'static, BytetapApp>, needle: &str) {
    let node = harness
        .get_all_by_label_contains(needle)
        .next()
        .unwrap_or_else(|| panic!("нет строки «{needle}»"));
    node.click();
    harness.run_steps(2);
}

/// Полный текст фрагмента из панели: значение поля только для чтения.
fn panel_text(harness: &Harness<'static, BytetapApp>) -> String {
    let field = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .expect("поле панели");
    field.accesskit_node().value().unwrap_or_default()
}

/// Клик по строке выбирает событие и открывает панель: в шапке — время, направление,
/// номер, тип HL7, размер и кодировка, в поле — полный фрагмент (L1-AC1, L1-AC4).
#[test]
fn a_click_on_a_row_opens_the_details_panel() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    let panel = harness.get_by_label_contains("Детали события");
    let node = panel.accesskit_node();
    let title = node.label().unwrap_or_default();
    assert!(title.contains("10:15:03.000 → #1"), "{title}");
    assert!(title.contains("ORU^R01"), "{title}");
    assert!(title.contains("CP1251"), "{title}");
    let text = panel_text(&harness);
    assert!(text.starts_with("‹VT›MSH|^~\\&|ANALYZER"), "{text}");
    assert!(text.ends_with("‹FS›‹CR›"), "{text}");
}

/// Длинный фрагмент виден целиком: строка ленты обрезана «…» на 2048 байтах, а панель
/// показывает все 3 000 байт (L1-AC1).
#[test]
fn the_panel_shows_the_whole_long_fragment() {
    let mut harness = harness();
    click_row(&mut harness, "MSGLONG");
    let text = panel_text(&harness);
    assert!(text.chars().count() > LONG, "{}", text.chars().count());
    assert!(!text.ends_with('…'), "без предела показа");
    assert!(text.contains("OBX|1|NM|GLU^Glucose||5.4|mmol/L|3.9-6.1|N|||F"));
    // В ленте та же строка обрезана: предел строки остался прежним (FR-26).
    let row = harness
        .get_all_by_label_contains("MSGLONG")
        .next()
        .expect("строка ленты")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(row.ends_with('…'), "строка ленты обрезана: {row}");
}

/// Esc закрывает панель и снимает выбор; строка больше не подсвечена (L1-AC9).
#[test]
fn escape_closes_the_panel_and_clears_the_selection() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    assert!(harness.query_by_label_contains("Детали события").is_some());
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label_contains("Детали события").is_none(),
        "панель закрыта"
    );
    let selected = harness
        .get_all_by_label_contains("MSG0001")
        .next()
        .expect("строка на месте")
        .accesskit_node()
        .is_selected();
    assert_ne!(selected, Some(true), "выбор снят");
}

/// «×» панели закрывает её так же, как Esc.
#[test]
fn the_close_button_closes_the_panel() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.get_by_label("Закрыть панель").click();
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Детали события").is_none());
}

/// Во время записи открытая панель останавливает следование: новые события копятся в
/// «↓ N новых», а выбранная строка стоит на месте; Esc возвращает следование (Л-В9).
#[test]
fn an_open_panel_stops_the_follow() {
    let mut harness = harness();
    // До выбора лента догоняет конец.
    assert!(harness.state().recording().expect("запись").follow());
    click_row(&mut harness, "MSG0001");
    let shared = harness.state().recording().expect("запись").shared();
    for index in 0..3 {
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:09.000+03:00",
            6_000_000 + index,
            1,
            Dir::AnalyzerToLis,
            false,
            b"\x0bMSH|^~\\&|A|B|C|D|E||ORU^R01|MSGNEW|P|2.3.1\r\x1c\r",
        ));
        let event: FeedEvent = FeedEvent::from_session(&event).expect("событие ленты");
        live::lock(&shared).push(Arc::new(event));
    }
    harness.run_steps(3);
    let rec = harness.state().recording().expect("запись");
    assert!(!rec.follow(), "лента стоит, пока панель открыта");
    assert_eq!(rec.pending(), 3, "новые события копятся");
    harness.get_by_label("3 новых");
    // Esc — выбор снят, следование вернулось, счётчик сброшен.
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    let rec = harness.state().recording().expect("запись");
    assert!(rec.follow(), "следование вернулось");
    assert_eq!(rec.pending(), 0);
}

/// Ctrl+A выделяет весь фрагмент, Ctrl+C кладёт его в буфер обмена (L1-AC1).
#[test]
fn ctrl_a_and_ctrl_c_copy_the_whole_fragment() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    let text = panel_text(&harness);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    // Ctrl+C приходит событием, как его шлёт обвязка окна; команда копирования живёт
    // один кадр, поэтому читается сразу после него.
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let copied = copied_text(&harness);
    assert_eq!(copied.as_deref(), Some(text.as_str()));
}

/// Вкладка HEX: полный дамп, «Копировать» кладёт строки дампа (L1-AC5).
#[test]
fn the_hex_tab_copies_the_dump() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.get_by_label("HEX").click();
    harness.run_steps(2);
    harness.get_by_label("Копировать").click();
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер");
    // Первая строка дампа: смещение 0000 и байты VT, M, S, H; вторая — продолжение.
    assert!(
        copied.starts_with("0000 0B 4D 53 48 7C 5E 7E 5C 26"),
        "{copied}"
    );
    assert!(copied.contains("  .MSH|^~\\&"), "{copied}");
    assert!(
        copied.lines().count() > 1,
        "весь фрагмент, а не одна строка"
    );
    // Последняя строка дампа — до конца фрагмента.
    let last = copied.lines().last().expect("строки дампа");
    assert!(last.starts_with("00"), "{last}");
}

/// В ленте вид «HEX»: клик по строке дампа выбирает событие целиком, панель — та же.
#[test]
fn a_click_in_the_hex_view_selects_the_event() {
    let mut settings = settings();
    settings.feed.view = FeedView::Hex;
    // Один короткий фрагмент: весь дамп помещается в ленту, первая строка видна.
    let mut fixture = fixture();
    fixture.events.truncate(1);
    let mut harness = harness_with(settings, fixture);
    click_row(&mut harness, "0B 4D 53 48");
    assert!(harness.query_by_label_contains("Детали события").is_some());
    let rows = harness.get_all_by_label_contains("MSG0001").count();
    assert!(rows >= 1, "строка ленты в HEX-виде");
}

/// ↑/↓ ходят по ленте и меняют выбранное событие: подпись панели называет соседнее.
#[test]
fn arrow_keys_move_the_selection() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.key_press(Key::ArrowDown);
    harness.run_steps(2);
    let title = harness
        .get_by_label_contains("Детали события")
        .accesskit_node()
        .label()
        .unwrap_or_default();
    assert!(title.contains("ACK^R01"), "следующее событие: {title}");
    harness.key_press(Key::ArrowUp);
    harness.run_steps(2);
    let title = harness
        .get_by_label_contains("Детали события")
        .accesskit_node()
        .label()
        .unwrap_or_default();
    assert!(title.contains("ORU^R01"), "обратно: {title}");
}

/// Текст, который приложение положило в буфер обмена за последний кадр.
fn copied_text(harness: &Harness<'static, BytetapApp>) -> Option<String> {
    harness
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
}

/// Панель открывается и в просмотре журнала: номер события — индекс журнала, а не `seq`.
#[test]
fn the_panel_works_in_the_journal_view() {
    use bytetap_app::journal::{JournalCache, JournalOpen};

    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("bytetap-20261019101500-tcp_server.jsonl");
    let header = bytetap_core::journal::Session::new(
        bytetap_core::journal::Mode::TcpServer,
        bytetap_core::journal::Autoresponder::Hl7Mllp,
        serde_json::json!({ "listen": "127.0.0.1:5000" }),
        bytetap_core::journal::Host {
            os: "Windows 11 23H2".to_owned(),
        },
    );
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    for index in 0..3 {
        let data = Event::Data(Data::new(
            "2026-10-19T10:15:01.000+03:00",
            index,
            1,
            Dir::AnalyzerToLis,
            false,
            format!("\x0bMSH|^~\\&|A|B|C|D|E||ORU^R01|JMSG{index}|P|2.3.1\r\x1c\r").as_bytes(),
        ));
        text.push_str(&serde_json::to_string(&data).expect("событие"));
        text.push('\n');
    }
    std::fs::write(&path, text).expect("журнал");
    let cache = JournalCache::default();
    let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
    let mut view = bytetap_app::viewing::JournalView::new(
        opened.path.clone(),
        opened.summary.clone(),
        opened.lengths.clone(),
        opened.first_event.clone(),
    );
    let events = cache
        .read(&opened.path, 0, bytetap_app::journal::PAGE_SIZE)
        .expect("страница");
    let generation = view.source.request_page(0);
    view.source.store_page(0, generation, events);
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    harness.run_steps(3);
    click_row(&mut harness, "JMSG1");
    let title = harness
        .get_by_label_contains("Детали события")
        .accesskit_node()
        .label()
        .unwrap_or_default();
    assert!(title.contains("ORU^R01"), "{title}");
    let text = panel_text(&harness);
    assert!(text.contains("JMSG1"), "{text}");
    // ↓ переключает на следующее событие журнала.
    harness.key_press(Key::ArrowDown);
    harness.run_steps(2);
    let text = panel_text(&harness);
    assert!(text.contains("JMSG2"), "{text}");
    // Esc закрывает панель и в просмотре.
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Детали события").is_none());
    assert_eq!(harness.state().screen, Screen::Viewing);
}

/// Высота панели тянется за разделитель и запоминается в настройках (L1-AC2).
#[test]
fn the_panel_height_is_dragged_and_remembered() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    let before = harness.state().settings().feed.panel_height;
    // Верхняя граница панели — разделитель: тянем её вверх на 60 точек.
    let top = harness.get_by_label_contains("Детали события").rect().top();
    let x = 600.0;
    harness.event(egui::Event::PointerMoved(egui::pos2(x, top + 3.0)));
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: egui::pos2(x, top + 3.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(1);
    for step in 1..=6 {
        harness.event(egui::Event::PointerMoved(egui::pos2(
            x,
            top + 3.0 - 10.0 * step as f32,
        )));
        harness.run_steps(1);
    }
    let dragged = harness.get_by_label_contains("Детали события").rect().top();
    harness.event(egui::Event::PointerButton {
        pos: egui::pos2(x, dragged),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(2);
    let after = harness.state().settings().feed.panel_height;
    assert!(dragged < top - 40.0, "панель выросла: {top} → {dragged}");
    assert!(after > before + 40.0, "{before} → {after}");
}

/// Вытеснение выбранного события из кольца: панель остаётся с прочитанным текстом и
/// говорит об этом подписью (Л-В6, L1-AC7).
#[test]
fn an_evicted_event_keeps_its_text() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    let text = panel_text(&harness);
    assert!(text.contains("MSG0001"), "{text}");
    // Лента на 10 000 событий: столько же новых вытесняет выбранное.
    let shared = harness.state().recording().expect("запись").shared();
    for index in 0..bytetap_app::feed::FEED_CAPACITY {
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:09.000+03:00",
            6_000_000 + index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            b"\x06",
        ));
        let event = FeedEvent::from_session(&event).expect("событие ленты");
        live::lock(&shared).push(Arc::new(event));
    }
    harness.run_steps(3);
    harness.get_by_label_contains("Событие вытеснено из ленты");
    assert_eq!(panel_text(&harness), text, "прочитанный текст на месте");
}
