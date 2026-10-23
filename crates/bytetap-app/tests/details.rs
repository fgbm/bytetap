//! Панель деталей выбранной строки ленты (L1): клик по строке, полный текст без предела
//! 2048 байт, вкладка HEX, Esc, Ctrl+A/C и остановка следования во время записи.
//!
//! Данные синтетические: HL7/MLLP по кодировке CP1251 и управляющие байты.

use std::sync::Arc;
use std::time::Duration;

use bytetap_app::feed::FeedEvent;
use bytetap_app::live;
use bytetap_app::recording::RecordingFixture;
use bytetap_app::settings::{Encoding, FeedView, Settings, Store};
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

/// Приложение с файлом настроек: по нему видно, когда высота панели записана.
fn harness_store(
    settings: Settings,
    fixture: RecordingFixture,
    store: Store,
) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings, None, Some(store))
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture)
        });
    harness.run_steps(3);
    harness
}

/// Запись из `count` событий с разным текстом `EVT0000`…: по нему находится строка.
fn many_events(count: usize) -> RecordingFixture {
    let events = (0..count)
        .map(|index| {
            Event::Data(Data::new(
                "2026-10-19T10:15:03.000+03:00",
                index as u64,
                1,
                Dir::AnalyzerToLis,
                false,
                format!("\x0bMSH|^~\\&|A|B|C|D|E||ORU^R01|EVT{index:04}|P|2.3.1\rOBX|1|NM|GLU||5.4\r\x1c\r").as_bytes(),
            ))
        })
        .collect();
    RecordingFixture {
        events,
        stats: Stats {
            connections: 1,
            ..Stats::default()
        },
        elapsed: Duration::from_secs(3),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: None,
        dropped: 0,
    }
}

/// Строка ленты по её тексту: подпись узла — время, направление и начало фрагмента.
fn row_rect(harness: &Harness<'static, BytetapApp>, needle: &str) -> Option<egui::Rect> {
    harness
        .query_all_by_label_contains(needle)
        .next()
        .map(|node| node.rect())
}

/// Новые события в общий буфер записи: лента принимает их следующим кадром.
fn push_events(harness: &mut Harness<'static, BytetapApp>, texts: &[String]) {
    let shared = harness.state().recording().expect("запись").shared();
    for (index, text) in texts.iter().enumerate() {
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:09.000+03:00",
            6_000_000 + index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            text.as_bytes(),
        ));
        let event: FeedEvent = FeedEvent::from_session(&event).expect("событие ленты");
        live::lock(&shared).push(Arc::new(event));
    }
}

/// Клик по строке ленты с текстом `needle`: так же, как это делает инженер мышью.
///
/// Указатель сперва ставится на строку и только следующим кадром нажимается: иначе
/// прокрутка видит рывок указателя и уезжает по инерции.
fn click_row(harness: &mut Harness<'static, BytetapApp>, needle: &str) {
    let at = harness
        .get_all_by_label_contains(needle)
        .next()
        .unwrap_or_else(|| panic!("нет строки «{needle}»"))
        .rect()
        .center();
    click_at(harness, at, Modifiers::NONE);
    harness.run_steps(1);
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
    // В ленте та же строка обрезана: предел строки остался прежним (FR-26). Подпись
    // строки — её начало (U1-AC16), и в неё не попадает хвост фрагмента.
    let row = harness
        .get_all_by_label_contains("MSGLONG")
        .next()
        .expect("строка ленты")
        .accesskit_node()
        .label()
        .unwrap_or_default();
    assert!(row.ends_with('…'), "строка ленты обрезана: {row}");
    assert!(
        row.chars().count() < text.chars().count(),
        "лента показывает начало фрагмента, панель — весь: {} < {}",
        row.chars().count(),
        text.chars().count()
    );
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
///
/// Поле панели забирает Ctrl+A/C, когда фокус в нём: выделение строк ленты живёт в
/// ленте, и клавиша достаётся тому, с чем сейчас работают (шаг 2, L1-AC6).
#[test]
fn ctrl_a_and_ctrl_c_copy_the_whole_fragment() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .click();
    harness.run_steps(1);
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

/// Журнал из трёх событий для просмотра: номер события — индекс журнала.
fn journal_harness() -> (tempfile::TempDir, Harness<'static, BytetapApp>) {
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
    (dir, harness)
}

/// Панель открывается и в просмотре журнала: номер события — индекс журнала, а не `seq`.
#[test]
fn the_panel_works_in_the_journal_view() {
    let (_dir, mut harness) = journal_harness();
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
    // Поле одно на оба экрана и на все события: выделение, оставшееся от JMSG1, на
    // JMSG2 не переезжает — после возврата «Копировать» кладёт фрагмент целиком.
    click_row(&mut harness, "JMSG1");
    let whole = panel_text(&harness);
    drag_select(&mut harness);
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let part = copied_text(&harness).expect("буфер обмена");
    assert!(
        !part.is_empty() && part.len() < whole.len() && whole.contains(part.as_str()),
        "выделена часть: {} из {}",
        part.chars().count(),
        whole.chars().count()
    );
    click_row(&mut harness, "JMSG2");
    click_row(&mut harness, "JMSG1");
    harness.get_by_label("Копировать").click();
    harness.run_steps(1);
    assert_eq!(
        copied_text(&harness).as_deref(),
        Some(whole.as_str()),
        "«Копировать» кладёт весь фрагмент"
    );
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
    // Панель встаёт на выбранную высоту: её верх после отпускания и сверяется с новым
    // приложением.
    let settled = harness.get_by_label_contains("Детали события").rect().top();
    assert!(settled < top - 40.0, "панель выросла: {top} → {settled}");
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

/// Пока панель открыта, лента стоит: новые события не сдвигают выбранную строку, а ↑/↓
/// подтягивают выбор в видимую область (L1-AC7, L1-AC9).
#[test]
fn an_open_panel_keeps_the_selected_row_where_it_was() {
    let mut harness = harness_with(settings(), many_events(200));
    harness.run_steps(2);
    // Лента догоняет конец: последняя строка на экране.
    harness
        .get_all_by_label_contains("EVT0199")
        .next()
        .expect("последняя строка видна");
    click_row(&mut harness, "EVT0199");
    harness.run_steps(3);
    let before = row_rect(&harness, "EVT0199").expect("выбранная строка видна");
    // 50 новых событий: лента не прилипает к концу, строка стоит, счётчик считает.
    let texts: Vec<String> = (0..50)
        .map(|index| format!("\x0bMSH|^~\\&|A|B|C|D|E||ORU^R01|NEW{index:04}|P|2.3.1\r\x1c\r"))
        .collect();
    push_events(&mut harness, &texts);
    harness.run_steps(3);
    let after = row_rect(&harness, "EVT0199").expect("строка не уехала");
    assert!(
        (after.top() - before.top()).abs() < 0.5,
        "строка осталась на месте: {before:?} → {after:?}"
    );
    harness.get_by_label("50 новых");
    // 40 шагов вверх: выбор уходит на 40 событий вверх и подтягивается в видимую область.
    for _ in 0..40 {
        harness.key_press(Key::ArrowUp);
        harness.run_steps(1);
    }
    let selected = harness
        .get_all_by_label_contains("EVT0159")
        .next()
        .expect("выбранная строка подтянута")
        .rect();
    let panel_top = harness.get_by_label_contains("Детали события").rect().top();
    assert!(
        selected.top() >= 0.0 && selected.bottom() <= panel_top,
        "строка видна над панелью: {selected:?}, панель с {panel_top}"
    );
    assert!(
        panel_text(&harness).contains("EVT0159"),
        "в панели выбранное событие EVT0159"
    );
}

/// Высота панели пишется в настройки один раз — на отпускании мыши, а не на каждом
/// кадре перетаскивания; новое приложение открывает панель той же высоты (L1-AC2).
#[test]
fn the_panel_height_is_written_once_and_kept_after_restart() {
    let dir = tempfile::tempdir().expect("временная папка");
    let store = Store::new(dir.path().join("settings.json"));
    let mut harness = harness_store(settings(), fixture(), store.clone());
    click_row(&mut harness, "MSG0001");
    let before = harness.state().settings().feed.panel_height;
    // Пока разделитель тянут, файла настроек нет: высота записывается на отпускании.
    let _ = std::fs::remove_file(store.path());
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
        assert!(
            !store.path().exists(),
            "на кадре перетаскивания файл не пишется"
        );
    }
    let dragged = harness.get_by_label_contains("Детали события").rect().top();
    harness.event(egui::Event::PointerButton {
        pos: egui::pos2(x, dragged),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(2);
    // Панель встаёт на выбранную высоту: её верх после отпускания и сверяется с новым
    // приложением.
    let settled = harness.get_by_label_contains("Детали события").rect().top();
    assert!(settled < top - 40.0, "панель выросла: {top} → {settled}");
    let (loaded, warning) = store.load("");
    assert_eq!(warning, None);
    let height = loaded.feed.panel_height;
    assert!(
        height > before + 40.0,
        "высота записана: {before} → {height}"
    );
    // Новое приложение с этими настройками открывает панель той же высоты.
    let mut restarted = harness_with(loaded, fixture());
    click_row(&mut restarted, "MSG0001");
    let again = restarted
        .get_by_label_contains("Детали события")
        .rect()
        .top();
    assert!(
        (again - settled).abs() < 1.0,
        "панель той же высоты: {settled} → {again}"
    );
}

/// «Копировать» кладёт фрагмент целиком, даже если в поле осталось выделение с прошлого
/// раза: поле одно на все события, и своё состояние оно получает заново (L1-AC6).
#[test]
fn the_copy_button_takes_the_whole_fragment_after_a_selection() {
    let mut harness = harness();
    // Длинный фрагмент: первая строка поля — заметно меньше всего текста.
    click_row(&mut harness, "MSGLONG");
    let whole = panel_text(&harness);
    // Ставим каретку в поле и выделяем часть Shift+↓.
    harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .expect("поле панели")
        .click();
    harness.run_steps(1);
    // Home и Shift+End выделяют первую строку: часть фрагмента, а не весь он.
    harness.key_press(Key::Home);
    harness.run_steps(1);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::End);
    harness.run_steps(1);
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let selected = copied_text(&harness).expect("буфер обмена");
    assert!(
        !selected.is_empty() && selected.len() < whole.len() && whole.contains(selected.as_str()),
        "выделена часть фрагмента: {} из {} символов: {:?} / {:?}",
        selected.chars().count(),
        whole.chars().count(),
        selected.chars().take(40).collect::<String>(),
        whole.chars().take(40).collect::<String>()
    );
    // Другая строка и обратно: выделение прошлого события к новому не переезжает.
    click_row(&mut harness, "BT0001");
    click_row(&mut harness, "MSGLONG");
    harness.get_by_label("Копировать").click();
    harness.run_steps(1);
    assert_eq!(
        copied_text(&harness).as_deref(),
        Some(whole.as_str()),
        "«Копировать» кладёт весь фрагмент"
    );
}

/// Escape при открытом выпадающем списке закрывает список, а не панель (L1-AC9).
#[test]
fn escape_closes_the_menu_but_not_the_panel() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.get_by_label("Кодировка").click();
    harness.run_steps(2);
    harness.get_by_label("ASCII");
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("ASCII").is_none(),
        "Escape закрыл список"
    );
    assert!(
        harness.query_by_label_contains("Детали события").is_some(),
        "панель осталась открытой"
    );
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label_contains("Детали события").is_none(),
        "второй Escape закрыл панель"
    );
}

/// Строки дампа выделяются кликом и Shift+кликом: Ctrl+C кладёт только их, Ctrl+A —
/// весь дамп (L1-AC5).
#[test]
fn the_hex_tab_copies_only_the_selected_lines() {
    let mut harness = harness();
    click_row(&mut harness, "MSGLONG");
    harness.get_by_label("HEX").click();
    harness.run_steps(2);
    let first = harness
        .get_all_by_label_contains("0000 0B 4D 53 48")
        .next()
        .expect("первая строка дампа")
        .rect();
    let second = harness
        .get_all_by_label_contains("0010 45 52 7C 4C 41")
        .next()
        .expect("вторая строка дампа")
        .rect();
    click_at(&mut harness, first.center(), Modifiers::NONE);
    // Shift держим, как настоящую клавишу: Shift+клик расширяет выделение от якоря.
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    harness.run_steps(1);
    click_at(&mut harness, second.center(), Modifiers::SHIFT);
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(1);
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер обмена");
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(lines.len(), 2, "только две строки: {copied}");
    assert!(lines[0].starts_with("0000 0B 4D 53 48"), "{copied}");
    assert!(lines[1].starts_with("0010 45 52 7C 4C 41"), "{copied}");
    // Ctrl+A — весь дамп: строк дампа много, первая — с начала фрагмента.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(1);
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let all = copied_text(&harness).expect("буфер обмена");
    assert!(
        all.lines().count() > 100,
        "весь дамп: {}",
        all.lines().count()
    );
    assert!(all.starts_with("0000 0B 4D 53 48"), "{all}");
    let last = all.lines().last().expect("строки дампа");
    assert!(
        last.len() > 5 && last.as_bytes()[..4].iter().all(u8::is_ascii_hexdigit),
        "последняя строка дампа: {last}"
    );
}

/// Панель открыта в наименьшем окне 720×560: лента над ней остаётся видимой (L1-AC2).
#[test]
fn the_panel_leaves_the_feed_visible_in_a_small_window() {
    let mut harness = Harness::builder()
        .with_size((720.0, 560.0))
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(many_events(200))
        });
    harness.run_steps(3);
    click_row(&mut harness, "EVT0192");
    harness.run_steps(40);
    let panel_top = harness.get_by_label_contains("Детали события").rect().top();
    let row = harness
        .query_all_by_label_contains("EVT0")
        .map(|node| node.rect())
        .find(|rect| rect.bottom() <= panel_top + 0.5 && rect.height() > 0.0);
    assert!(
        row.is_some(),
        "над панелью с {panel_top} видна строка ленты"
    );
}

/// Доступность панели: регион на всю панель, подпись поля, Tab — панель после строк
/// ленты (L1-AC8).
#[test]
fn the_panel_follows_the_feed_rows_in_the_tab_order() {
    use egui_kittest::kittest::NodeT as _;

    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.check_accessibility();
    // Регион панели — вся панель, а не одна шапка: рамка региона ниже строки ленты.
    let region = harness.get_by_label_contains("Детали события").rect();
    let row = row_rect(&harness, "MSG0001").expect("строка ленты");
    assert!(
        region.top() > row.bottom(),
        "регион {region:?}, строка {row:?}"
    );
    // У поля есть подпись, а не только значение.
    harness.get_by_role_and_label(egui::accesskit::Role::MultilineTextInput, "Фрагмент");
    // Tab идёт по строкам ленты и только потом в панель.
    let mut order = Vec::new();
    for _ in 0..24 {
        harness.key_press(Key::Tab);
        harness.run_steps(1);
        let Some(node) = harness
            .root()
            .children_recursive()
            .find(egui_kittest::Node::is_focused)
        else {
            continue;
        };
        order.push(
            node.accesskit_node()
                .label()
                .or_else(|| node.accesskit_node().value())
                .unwrap_or_default(),
        );
    }
    let copy = order
        .iter()
        .position(|label| label == "Копировать")
        .unwrap_or_else(|| panic!("панель в обходе Tab: {order:#?}"));
    let second = order
        .iter()
        .position(|label| label.contains("BT0001"))
        .unwrap_or_else(|| panic!("вторая строка в обходе: {order:#?}"));
    assert!(second < copy, "панель после строк ленты: {order:#?}");
}

/// Выделение части фрагмента мышью: тянем по полю панели слева направо.
fn drag_select(harness: &mut Harness<'static, BytetapApp>) {
    let field = harness
        .get_all_by_role(egui::accesskit::Role::MultilineTextInput)
        .next()
        .expect("поле панели")
        .rect();
    let y = field.top() + 6.0;
    let from = egui::pos2(field.left() + 40.0, y);
    let to = egui::pos2(field.right() - 40.0, y);
    harness.event(egui::Event::PointerMoved(from));
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(1);
    for step in 1..=4 {
        let x = from.x + (to.x - from.x) * step as f32 / 4.0;
        harness.event(egui::Event::PointerMoved(egui::pos2(x, y)));
        harness.run_steps(1);
    }
    harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(1);
}

/// Клик мышью в точку с модификатором: так Shift+клик выделяет диапазон строк дампа.
fn click_at(harness: &mut Harness<'static, BytetapApp>, at: egui::Pos2, modifiers: Modifiers) {
    harness.event(egui::Event::PointerMoved(at));
    // Указатель постоял, пока прокрутка забудет его скорость: иначе лента уезжает по инерции.
    harness.run_steps(10);
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers,
    });
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers,
    });
    harness.run_steps(1);
}

/// Служебная строка (открытие соединения): вкладка одна, «Копировать» кладёт текст
/// строки ленты, а Ctrl+A выделяет его в поле (L1-AC6).
#[test]
fn a_service_row_copies_its_text() {
    use bytetap_core::journal::Open;

    let mut harness = harness();
    let shared = harness.state().recording().expect("запись").shared();
    let open = Event::Open(Open {
        t: "2026-10-19T10:15:09.000+03:00".into(),
        mono_us: 6_000_000,
        conn: 1,
        peer: "192.168.1.51:49152".into(),
        local: "0.0.0.0:5000".into(),
        link: None,
    });
    let open: FeedEvent = FeedEvent::from_session(&open).expect("событие ленты");
    live::lock(&shared).push(Arc::new(open));
    harness.run_steps(3);
    click_row(&mut harness, "подключился");
    // В шапке панели номер соединения не повторяется: он уже есть в тексте строки.
    let title = harness
        .get_by_label_contains("Детали события")
        .accesskit_node()
        .label()
        .unwrap_or_default();
    assert_eq!(title.matches("#1").count(), 1, "{title}");
    let text = panel_text(&harness);
    assert!(text.contains("подключился 192.168.1.51:49152"), "{text}");
    harness.get_by_label("Копировать").click();
    harness.run_steps(1);
    assert_eq!(copied_text(&harness).as_deref(), Some(text.as_str()));
    // Ctrl+A выделяет текст строки и в служебном событии — когда фокус в поле панели:
    // иначе клавиша принадлежит ленте и выделяет её строки (шаг 2).
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .click();
    harness.run_steps(1);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(1);
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    assert_eq!(copied_text(&harness).as_deref(), Some(text.as_str()));
}

/// Окно снова выросло: панель возвращается к высоте, которую выбрал инженер, а не
/// остаётся ужатой маленьким окном (L1-AC2).
#[test]
fn the_panel_returns_to_its_height_when_the_window_grows() {
    // Панель выше 70 % ленты маленького окна: там её ужимает предел, а не выбор.
    let mut settings = settings();
    settings.feed.panel_height = 400.0;
    let mut harness = harness_with(settings, fixture());
    click_row(&mut harness, "MSG0001");
    harness.run_steps(2);
    let rect = harness.get_by_label_contains("Детали события").rect();
    let height = rect.height();
    assert!(height > 140.0, "панель открыта: {height}");
    harness.set_size(egui::vec2(720.0, 560.0));
    harness.run_steps(4);
    let small = harness
        .get_by_label_contains("Детали события")
        .rect()
        .height();
    assert!(
        small < height,
        "в маленьком окне панель ниже: {small} < {height}"
    );
    harness.set_size(egui::vec2(WINDOW.0, WINDOW.1));
    harness.run_steps(4);
    let again = harness
        .get_by_label_contains("Детали события")
        .rect()
        .height();
    assert!(
        (again - height).abs() < 1.0,
        "панель вернулась к выбранной высоте: {height} → {again}"
    );
}

/// Клик по строке при уже открытой панели только меняет выбор: лента не дёргается
/// и строка остаётся на своём месте (находка контрольного ревью шага 1).
#[test]
fn a_click_with_the_panel_open_keeps_the_row_in_place() {
    let mut harness = harness_with(settings(), many_events(200));
    harness.run_steps(2);
    click_row(&mut harness, "EVT0199");
    harness.run_steps(3);
    // Строка внизу видимой области: её и нажимаем вторым кликом.
    let low = row_rect(&harness, "EVT0197").expect("нижняя видимая строка");
    let panel_top = harness.get_by_label_contains("Детали события").rect().top();
    assert!(
        low.bottom() <= panel_top,
        "строка {low:?} над панелью {panel_top}"
    );
    click_row(&mut harness, "EVT0197");
    harness.run_steps(3);
    let after = row_rect(&harness, "EVT0197").expect("строка на месте");
    assert_eq!(
        after, low,
        "клик при открытой панели не двигает ленту: {low:?} → {after:?}"
    );
    assert!(
        panel_text(&harness).contains("EVT0197"),
        "выбрано новое событие"
    );
}

/// Escape при открытом меню шапки закрывает меню, а не панель (L1-AC9).
#[test]
fn escape_closes_the_header_menu_but_not_the_panel() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.get_by_label("Меню записи").click();
    harness.run_steps(2);
    harness.get_by_label("Скопировать путь к журналу");
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness
            .query_by_label("Скопировать путь к журналу")
            .is_none(),
        "меню закрылось"
    );
    assert!(
        harness.query_by_label_contains("Детали события").is_some(),
        "панель осталась открытой"
    );
}

/// Escape при открытом списке выбора в просмотре журнала закрывает список, а не панель.
#[test]
fn escape_in_the_journal_closes_the_list_but_not_the_panel() {
    let (_dir, mut harness) = journal_harness();
    click_row(&mut harness, "JMSG1");
    harness.get_by_label("Кодировка").click();
    harness.run_steps(2);
    harness.get_by_label("ASCII");
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(harness.query_by_label("ASCII").is_none(), "список закрылся");
    assert!(
        harness.query_by_label_contains("Детали события").is_some(),
        "панель осталась открытой"
    );
}

/// Разделитель панели один — свой, с русской подписью; Tab доходит до него, стрелки
/// меняют высоту, а выбор в ленте и фокус остаются на месте (L1-AC2, L1-AC8).
#[test]
fn the_splitter_is_one_and_its_arrows_change_the_height() {
    use egui_kittest::kittest::NodeT as _;

    let mut harness = harness();
    click_row(&mut harness, "MSG0001");
    harness.run_steps(2);
    let splitters: Vec<String> = harness
        .query_all_by_role(egui::accesskit::Role::Splitter)
        .map(|node| node.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(
        splitters,
        ["Высота панели"],
        "разделитель один и назван по-русски"
    );
    // Tab доходит до разделителя: панель идёт после строк ленты.
    let mut focused = String::new();
    for _ in 0..40 {
        harness.key_press(Key::Tab);
        harness.run_steps(1);
        if let Some(node) = harness
            .root()
            .children_recursive()
            .find(egui_kittest::Node::is_focused)
        {
            focused = node.accesskit_node().label().unwrap_or_default();
        }
        if focused == "Высота панели" {
            break;
        }
    }
    assert_eq!(focused, "Высота панели", "Tab дошёл до разделителя");
    let title = harness
        .get_by_label_contains("Детали события")
        .accesskit_node()
        .label()
        .unwrap_or_default();
    let before = harness
        .get_by_label_contains("Детали события")
        .rect()
        .height();
    harness.key_press(Key::ArrowUp);
    harness.run_steps(1);
    harness.key_press(Key::ArrowUp);
    harness.run_steps(2);
    let after = harness
        .get_by_label_contains("Детали события")
        .rect()
        .height();
    assert!(
        (after - before - 32.0).abs() < 1.0,
        "два шага по 16 точек: {before} → {after}"
    );
    assert!(
        row_rect(&harness, "MSG0001").is_some(),
        "строка ленты не ушла из-за стрелок"
    );
    assert_eq!(
        harness
            .get_by_label_contains("Детали события")
            .accesskit_node()
            .label()
            .unwrap_or_default(),
        title,
        "выбор не переехал"
    );
    let focused = harness
        .root()
        .children_recursive()
        .find(egui_kittest::Node::is_focused)
        .expect("фокус на месте");
    assert_eq!(
        focused.accesskit_node().label().as_deref(),
        Some("Высота панели"),
        "фокус остался на разделителе"
    );
    // Диктор двигает высоту действием Increment — тем же шагом, что и стрелка.
    harness
        .get_by_role(egui::accesskit::Role::Splitter)
        .increment_accesskit();
    harness.run_steps(2);
    let grown = harness
        .get_by_label_contains("Детали события")
        .rect()
        .height();
    assert!(
        (grown - after - 16.0).abs() < 1.0,
        "шаг диктора 16 точек: {after} → {grown}"
    );
}
