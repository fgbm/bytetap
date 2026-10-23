//! Выделение строк ленты блоком (L1, шаг 2; вариант А без горизонтальной прокрутки):
//! клик, Shift+клик, Ctrl+клик, Shift+↑/↓, Ctrl+A, Ctrl+C, контекстное меню выделения,
//! Esc и остановка следования.
//!
//! Данные синтетические: HL7/MLLP по кодировке CP1251.

use std::sync::Arc;
use std::time::Duration;

use bytetap_app::feed::FeedEvent;
use bytetap_app::journal::{JournalCache, JournalOpen, PAGE_SIZE};
use bytetap_app::live;
use bytetap_app::recording::RecordingFixture;
use bytetap_app::settings::{Encoding, Settings};
use bytetap_app::viewing::JournalView;
use bytetap_app::{BytetapApp, Fixtures};
use bytetap_core::journal::{Data, Dir, Event, Stats};
use eframe::egui::{self, Key, Modifiers};
use egui_kittest::{Harness, kittest::NodeT as _, kittest::Queryable as _};

const LOG_DIR: &str = r"C:\Users\operator\Documents\bytetap";
const WINDOW: (f32, f32) = (1200.0, 800.0);

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

fn at(ms: u64) -> String {
    let base = chrono::DateTime::parse_from_rfc3339("2026-10-19T10:15:03.000+03:00").unwrap();
    (base + chrono::Duration::milliseconds(ms as i64))
        .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
        .to_string()
}

fn cp1251(text: &str) -> Vec<u8> {
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(text);
    bytes.into_owned()
}

fn message(id: &str) -> Vec<u8> {
    cp1251(&format!(
        "\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101503||ORU^R01|{id}|P|2.3.1\rOBX|1|NM|GLU^Glucose||5.4|mmol/L\r\x1c\r"
    ))
}

/// Запись из трёх фрагментов: у каждого своё сообщение, по нему находится строка.
fn fixture() -> RecordingFixture {
    let events = (1..=3)
        .map(|index| {
            Event::Data(Data::new(
                at(index * 500),
                index * 500_000,
                1,
                Dir::AnalyzerToLis,
                false,
                &message(&format!("MSG000{index}")),
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

/// Запись с двумя соединениями вперемешку: у фильтра соединений есть что прятать.
fn two_conns() -> RecordingFixture {
    let events = (0..6)
        .map(|index| {
            Event::Data(Data::new(
                at(index * 100),
                index * 100_000,
                if index % 2 == 0 { 1 } else { 2 },
                Dir::AnalyzerToLis,
                false,
                &message(&format!("F{index:02}")),
            ))
        })
        .collect();
    RecordingFixture {
        events,
        stats: Stats {
            connections: 2,
            ..Stats::default()
        },
        elapsed: Duration::from_secs(3),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: None,
        dropped: 0,
    }
}

fn harness_of(fixture: RecordingFixture) -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture)
        });
    harness.run_steps(3);
    harness
}

fn harness() -> Harness<'static, BytetapApp> {
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                .with_fixtures(fixtures())
                .with_recording_fixture(fixture())
        });
    harness.run_steps(3);
    harness
}

/// Клик по строке ленты с текстом `needle`: указатель сперва ставится на строку.
fn click_row(harness: &mut Harness<'static, BytetapApp>, needle: &str, modifiers: Modifiers) {
    let at = harness
        .get_all_by_label_contains(needle)
        .next()
        .unwrap_or_else(|| panic!("нет строки «{needle}»"))
        .rect()
        .center();
    click_at(harness, at, modifiers);
}

fn click_at(harness: &mut Harness<'static, BytetapApp>, at: egui::Pos2, modifiers: Modifiers) {
    harness.event(egui::Event::PointerMoved(at));
    // Указатель постоял, пока прокрутка забудет его скорость: иначе лента уезжает по инерции.
    harness.run_steps(10);
    // Shift и Ctrl приходят сменой модификаторов, как их шлёт обвязка окна: в самом
    // событии кнопки они для `input.modifiers` не значат ничего.
    harness.event(egui::Event::ModifiersChanged(modifiers));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
        harness.run_steps(1);
    }
    harness.event(egui::Event::ModifiersChanged(Modifiers::default()));
    // Счётчик выделения живёт в панели фильтров, а она рисуется до ленты: свой кадр.
    harness.run_steps(2);
}

/// Правый клик по строке: контекстное меню строки.
fn right_click(harness: &mut Harness<'static, BytetapApp>, needle: &str) {
    let at = harness
        .get_all_by_label_contains(needle)
        .next()
        .unwrap_or_else(|| panic!("нет строки «{needle}»"))
        .rect()
        .center();
    harness.event(egui::Event::PointerMoved(at));
    harness.run_steps(10);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    harness.run_steps(2);
}

/// Текст фрагмента в поле панели: значение поля только для чтения.
fn panel_text(harness: &Harness<'static, BytetapApp>) -> String {
    harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput)
        .accesskit_node()
        .value()
        .unwrap_or_default()
}

/// Текст, который приложение положило в буфер обмена за последний кадр.
fn copied_text(harness: &Harness<'static, BytetapApp>) -> Option<String> {
    take_copied(&harness.output().platform_output)
}

fn take_copied(output: &egui::PlatformOutput) -> Option<String> {
    output.commands.iter().find_map(|command| match command {
        egui::OutputCommand::CopyText(text) => Some(text.clone()),
        _ => None,
    })
}

fn selected(harness: &Harness<'static, BytetapApp>, needle: &str) -> bool {
    harness
        .get_all_by_label_contains(needle)
        .next()
        .unwrap_or_else(|| panic!("нет строки «{needle}»"))
        .accesskit_node()
        .is_selected()
        .unwrap_or(false)
}

/// Кадры до появления строки: страницы журнала читает фоновый пул, и на медленной
/// машине они приходят не к третьему кадру.
fn wait_for_row(harness: &mut Harness<'static, BytetapApp>, needle: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while harness.query_by_label_contains(needle).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "строка «{needle}» не появилась"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn selection_count(harness: &Harness<'static, BytetapApp>) -> usize {
    harness
        .state()
        .recording()
        .expect("запись")
        .selection_count()
}

/// Клик выделяет одну строку, счётчик в панели фильтров говорит «выбрано 1 · Ctrl+C»,
/// а в AccessKit у строки признак выбора (L1, шаг 2).
#[test]
fn a_click_selects_one_row_and_the_bar_counts_it() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0002", Modifiers::NONE);
    assert_eq!(selection_count(&harness), 1);
    harness.get_by_label_contains("выбрано 1 · Ctrl+C");
    assert!(selected(&harness, "MSG0002"), "строка отмечена выбранной");
    assert!(!selected(&harness, "MSG0001"));
}

/// Shift+клик выделяет диапазон от якоря, Ctrl+клик добавляет и снимает строку.
#[test]
fn shift_click_selects_a_block_and_ctrl_click_adds_rows() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    click_row(&mut harness, "MSG0003", Modifiers::SHIFT);
    assert_eq!(selection_count(&harness), 3, "диапазон из трёх строк");
    harness.get_by_label_contains("выбрано 3 · Ctrl+C");
    for needle in ["MSG0001", "MSG0002", "MSG0003"] {
        assert!(selected(&harness, needle), "строка {needle} в выделении");
    }
    // Ctrl+клик снимает строку из середины и не трогает остальные.
    click_row(&mut harness, "MSG0002", Modifiers::COMMAND);
    assert_eq!(selection_count(&harness), 2);
    assert!(!selected(&harness, "MSG0002"));
    // И возвращает её обратно.
    click_row(&mut harness, "MSG0002", Modifiers::COMMAND);
    assert_eq!(selection_count(&harness), 3);
}

/// Shift+↑/↓ расширяют выделение от якоря, не сбрасывая его.
#[test]
fn shift_arrows_extend_the_selection() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 2);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 3);
    // ↑ без Shift — снова одна строка.
    harness.key_press(Key::ArrowUp);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 1);
}

/// Ctrl+A выделяет видимые строки, Ctrl+C кладёт их в буфер: время, направление,
/// номер соединения и полный текст (решение владельца, Л-В3).
#[test]
fn ctrl_a_and_ctrl_c_copy_the_selected_rows() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 3, "все видимые строки");
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер обмена");
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(lines.len(), 3, "{copied}");
    assert!(
        lines[0].starts_with("10:15:03.500 → #1 ‹VT›MSH|^~\\&"),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains("MSG0002"), "{}", lines[1]);
    assert!(lines[2].contains("MSG0003"), "{}", lines[2]);
}

/// Стрелки после Ctrl+A при закрытой панели ходят от якоря выделения: курсор — он, а
/// панель от стрелок не открывается (m5).
#[test]
fn arrows_after_select_all_start_from_the_anchor() {
    let mut harness = harness();
    // Лента без фокуса в поле: Ctrl+A выделяет видимые строки, панель не открыта.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 3);
    assert!(
        harness.query_by_label_contains("Детали события").is_none(),
        "панель закрыта"
    );
    harness.key_press(Key::ArrowDown);
    harness.run_steps(2);
    assert_eq!(
        selection_count(&harness),
        1,
        "курсор ушёл на соседнюю строку"
    );
    assert!(
        selected(&harness, "MSG0002"),
        "курсор — следующая за якорем"
    );
    assert!(
        harness.query_by_label_contains("Детали события").is_none(),
        "стрелки панель не открывают"
    );
}

/// Правый клик по выделенной строке даёт меню на всё выделение, а не на одну строку.
#[test]
fn the_row_menu_of_a_selected_row_acts_on_the_whole_selection() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    click_row(&mut harness, "MSG0002", Modifiers::SHIFT);
    right_click(&mut harness, "MSG0002");
    harness.run_steps(2);
    harness
        .get_by_label("Скопировать выделенные строки (2)")
        .click();
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер обмена");
    assert_eq!(copied.lines().count(), 2, "{copied}");
    assert!(copied.contains("MSG0001") && copied.contains("MSG0002"));
}

/// Пока есть выделение, лента не догоняет конец; Esc снимает выделение и возвращает
/// следование (решение владельца, Л-В9).
#[test]
fn escape_clears_the_selection_and_resumes_the_follow() {
    let mut harness = harness();
    assert!(harness.state().recording().expect("запись").follow());
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    assert!(!harness.state().recording().expect("запись").follow());
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert_eq!(selection_count(&harness), 0, "выделение снято");
    assert!(
        harness.state().recording().expect("запись").follow(),
        "следование вернулось"
    );
    assert!(
        harness.query_by_label_contains("выбрано").is_none(),
        "счётчик ушёл вместе с выделением"
    );
}

/// Фильтр соединения: Shift+клик через строки другого соединения не выделяет
/// спрятанные строки и не кладёт их в буфер (M1).
#[test]
fn a_filter_keeps_hidden_rows_out_of_the_block() {
    let mut harness = harness_of(two_conns());
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "#1")
        .click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label_contains("F01").is_none(),
        "строки #2 спрятаны фильтром"
    );
    click_row(&mut harness, "F00", Modifiers::NONE);
    click_row(&mut harness, "F04", Modifiers::SHIFT);
    assert_eq!(
        selection_count(&harness),
        3,
        "видимые строки диапазона: F00, F02, F04"
    );
    harness.get_by_label_contains("выбрано 3 · Ctrl+C");
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер обмена");
    assert_eq!(copied.lines().count(), 3, "{copied}");
    assert!(
        !copied.contains("#2"),
        "скрытые строки не копируются: {copied}"
    );
    assert!(
        !copied.contains("F01") && !copied.contains("F03"),
        "{copied}"
    );
}

/// Ctrl+клик снимает строку с выделения, но курсор и панель переезжают на неё — как в
/// Проводнике (m4).
#[test]
fn a_ctrl_click_moves_the_panel_onto_the_row_it_clears() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    click_row(&mut harness, "MSG0003", Modifiers::SHIFT);
    assert_eq!(selection_count(&harness), 3);
    click_row(&mut harness, "MSG0002", Modifiers::COMMAND);
    assert_eq!(selection_count(&harness), 2);
    let text = panel_text(&harness);
    assert!(text.contains("MSG0002"), "панель на снятой строке: {text}");
}

/// Роли AccessKit: лента — список с множественным выбором, строки — его элементы с
/// состоянием выбора (m7, L1-AC17).
#[test]
fn the_feed_is_a_multiselectable_list_of_items() {
    let harness = harness();
    let list = harness.get_by_role_and_label(egui::accesskit::Role::ListBox, "Лента");
    assert!(
        list.accesskit_node().is_multiselectable(),
        "лента говорит, что выбор множественный"
    );
    let row = harness
        .get_all_by_label_contains("MSG0001")
        .next()
        .expect("строка ленты");
    assert_eq!(row.accesskit_node().role(), egui::accesskit::Role::ListItem);
}

/// Вкладка «HEX» забирает Ctrl+A/C себе, но блок строк больше одной строки лента у
/// панели отбирает: в дампе ничего не выделено, значит Ctrl+C — про строки (M2, m7).
#[test]
fn the_hex_tab_keeps_ctrl_a_and_c_until_the_block_is_bigger() {
    let mut harness = harness();
    click_row(&mut harness, "MSG0001", Modifiers::NONE);
    click_row(&mut harness, "MSG0003", Modifiers::SHIFT);
    harness.get_by_label("HEX").click();
    harness.run_steps(2);
    // Ctrl+C — блок строк ленты: в дампе ещё ничего не выделено.
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер обмена");
    assert_eq!(
        copied.lines().count(),
        3,
        "строки ленты, а не дамп: {copied}"
    );
    assert!(copied.starts_with("10:15:03.500 → #1"), "{copied}");
    // Ctrl+A на «HEX» — весь дамп, выделение ленты не трогается.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 3, "лента выделение не потеряла");
    // Теперь в дампе есть выделение — Ctrl+C снова принадлежит панели.
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let dump = copied_text(&harness).expect("буфер обмена");
    assert!(
        dump.starts_with("0000 0B 4D 53 48"),
        "панель скопировала дамп: {dump}"
    );
    // Esc снимает выделение и закрывает панель: Ctrl+A снова принадлежит ленте.
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    assert_eq!(selection_count(&harness), 3, "видимые строки ленты");
    harness.get_by_label_contains("выбрано 3 · Ctrl+C");
}

/// Вытеснение из кольца убирает события из выделения, и счётчик в панели фильтров
/// исчезает (m7, L1-AC16).
#[test]
fn eviction_takes_rows_out_of_the_selection() {
    let fixture = RecordingFixture {
        events: (0..10_000)
            .map(|index| {
                Event::Data(Data::new(
                    at(index),
                    index,
                    1,
                    Dir::AnalyzerToLis,
                    false,
                    &message(&format!("EV{index:05}")),
                ))
            })
            .collect(),
        stats: Stats {
            connections: 1,
            ..Stats::default()
        },
        elapsed: Duration::from_secs(3),
        local_addr: "0.0.0.0:5000".into(),
        journal_path: None,
        dropped: 0,
    };
    let mut harness = harness_of(fixture);
    // Кольцо полное: самые старые события ещё в нём.
    let view = harness.state_mut().recording_mut().expect("запись");
    view.selection_mut().select_all(0..=2);
    harness.run_steps(2);
    harness.get_by_label_contains("выбрано 3 · Ctrl+C");
    // Два новых события вытесняют первые два — они уходят из выделения.
    let shared = harness.state().recording().expect("запись").shared();
    for index in 0..2 {
        let event = Event::Data(Data::new(
            at(20_000 + index),
            20_000 + index,
            1,
            Dir::AnalyzerToLis,
            false,
            &message("EVNEW0"),
        ));
        let event: FeedEvent = FeedEvent::from_session(&event).expect("событие ленты");
        live::lock(&shared).push(Arc::new(event));
    }
    harness.run_steps(3);
    assert_eq!(
        selection_count(&harness),
        1,
        "осталась одна вытесненная строка"
    );
    assert!(
        harness
            .query_by_label_contains("выбрано 1 · Ctrl+C")
            .is_some(),
        "счётчик обновился"
    );
}

/// Журнал из `events` событий в папке `dir`: страницы читает фоновый пул.
///
/// Папка живёт у теста: временная папка внутри функции удалила бы файл раньше, чем
/// пул прочитает из него первую страницу.
fn journal_harness(dir: &std::path::Path, events: usize) -> Harness<'static, BytetapApp> {
    let path = dir.join("journal.jsonl");
    let mut header = bytetap_core::journal::Session::new(
        bytetap_core::journal::Mode::TcpServer,
        bytetap_core::journal::Autoresponder::None,
        serde_json::json!({"listen": "127.0.0.1:5000"}),
        bytetap_core::journal::Host {
            os: "Linux".to_owned(),
        },
    );
    header.analyzer = Some("ANALYZER".to_owned());
    let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
    text.push('\n');
    for index in 0..events {
        let event = Event::Data(Data::new(
            at(index as u64),
            index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            &message(&format!("JEV{index:05}")),
        ));
        text.push_str(&serde_json::to_string(&event).expect("событие"));
        text.push('\n');
    }
    std::fs::write(&path, text).expect("журнал");
    let journal = path.clone();
    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            let cache = JournalCache::default();
            let opened = JournalOpen::open(&cache, &journal).expect("журнал открывается");
            let view = JournalView::new(
                opened.path,
                opened.summary,
                opened.lengths,
                opened.first_event,
            );
            BytetapApp::new(&cc.egui_ctx, settings(), None, None)
                .with_fixtures(fixtures())
                .with_journal_fixture(view)
        });
    harness.run_steps(3);
    harness
}

/// Строки на незагруженных страницах сперва дочитываются, и только потом весь блок
/// уходит в буфер: половина выделения в буфере хуже ожидания (L1, шаг 2).
#[test]
fn a_journal_selection_waits_for_pages_before_copying() {
    let dir = tempfile::tempdir().expect("временная папка");
    let mut harness = journal_harness(dir.path(), 1200);
    wait_for_row(&mut harness, "JEV00000");
    // Первая страница уже в памяти: выделяем 900 событий, из них 400 — на втором листе.
    let view = harness.state_mut().viewing_mut().expect("журнал открыт");
    view.selection.select_all(0..900);
    assert_eq!(view.selection.count(), 900);
    assert!(
        view.source.event(PAGE_SIZE + 10).is_none(),
        "второй страницы нет"
    );
    harness.event(egui::Event::Copy);
    harness.run_steps(2);
    assert!(
        harness
            .query_by_label_contains("читаю строки журнала")
            .is_some(),
        "пока страницы едут, говорим об этом"
    );
    assert!(
        copied_text(&harness).is_none(),
        "половину блока не копируем"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        harness.step();
        if let Some(copied) = copied_text(&harness) {
            assert_eq!(copied.lines().count(), 900, "весь блок");
            assert!(copied.contains("JEV00000"), "первая строка блока");
            assert!(copied.contains("JEV00899"), "последняя строка блока");
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "страницы не дочитались"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Кадры до копирования: страницы журнала читает фоновый пул, и строки копятся по
/// мере их прихода. Возвращает текст, который лёг в буфер.
fn wait_for_copy(harness: &mut Harness<'static, BytetapApp>, rows: usize) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        harness.step();
        if let Some(copied) = copied_text(harness) {
            assert_eq!(copied.lines().count(), rows, "строк в буфере");
            return copied;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "копирование не закончилось"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Большой блок в журнале копируется целиком и до дедлайна: строки собираются
/// курсором по мере чтения страниц, а не пересборкой всего выделения (B1).
#[test]
fn a_big_journal_block_copies_ten_thousand_rows() {
    let dir = tempfile::tempdir().expect("временная папка");
    let mut harness = journal_harness(dir.path(), 10_600);
    wait_for_row(&mut harness, "JEV00000");
    // Ровно предел копирования: 250…10 249.
    harness
        .state_mut()
        .viewing_mut()
        .expect("журнал открыт")
        .selection
        .select_all(250..10_250);
    harness.event(egui::Event::Copy);
    let copied = wait_for_copy(&mut harness, 10_000);
    assert!(copied.starts_with("10:15:03.250 → #1"), "{}", &copied[..40]);
    let tail = copied.lines().last().unwrap_or_default();
    assert!(
        tail.contains("JEV10249") && tail.ends_with("‹FS›‹CR›"),
        "хвост: {tail}"
    );
    let view = harness.state().viewing().expect("журнал открыт");
    assert!(view.copy_job.is_none(), "задание закрыто");
    assert!(
        view.copy_notice.is_none(),
        "предел не срабатывал: {:?}",
        view.copy_notice
    );
}

/// Предел копирования виден в панели фильтров: в буфер вошло меньше, чем выделено
/// (m7, L1-AC14).
#[test]
fn the_copy_limit_is_shown_in_the_bar() {
    let dir = tempfile::tempdir().expect("временная папка");
    let mut harness = journal_harness(dir.path(), 10_600);
    wait_for_row(&mut harness, "JEV00000");
    harness
        .state_mut()
        .viewing_mut()
        .expect("журнал открыт")
        .selection
        .select_all(250..10_600);
    harness.event(egui::Event::Copy);
    wait_for_copy(&mut harness, 10_000);
    harness.run_steps(2);
    harness.get_by_label_contains("предел копирования");
}

/// Две далёкие строки: копирование просит страницы только с выделенными строками, а не
/// весь промежуток между ними (B1).
#[test]
fn two_far_rows_read_only_their_own_pages() {
    let dir = tempfile::tempdir().expect("временная папка");
    let mut harness = journal_harness(dir.path(), 10_600);
    wait_for_row(&mut harness, "JEV00000");
    harness
        .state_mut()
        .viewing_mut()
        .expect("журнал открыт")
        .selection
        .select_all([5, 10_599]);
    harness.event(egui::Event::Copy);
    let copied = wait_for_copy(&mut harness, 2);
    assert!(
        copied.contains("JEV00005") && copied.contains("JEV10599"),
        "{copied}"
    );
    let view = harness.state_mut().viewing_mut().expect("журнал открыт");
    let pages = view.source.pages_in_memory();
    assert!(
        pages <= 3,
        "прочитано {pages} страниц, а не весь промежуток"
    );
}

/// «×» панели отменяет незаконченное копирование: выделения, которое оно собирало,
/// больше нет (m6).
#[test]
fn the_close_button_cancels_the_copy() {
    let dir = tempfile::tempdir().expect("временная папка");
    let mut harness = journal_harness(dir.path(), 10_600);
    wait_for_row(&mut harness, "JEV00000");
    click_row(&mut harness, "JEV00000", Modifiers::NONE);
    harness
        .state_mut()
        .viewing_mut()
        .expect("журнал открыт")
        .selection
        .select_all(300..10_000);
    harness.event(egui::Event::Copy);
    harness.run_steps(2);
    let view = harness.state().viewing().expect("журнал открыт");
    assert!(view.copy_job.is_some(), "копирование ждёт страниц");
    harness.get_by_label("Закрыть панель").click();
    harness.run_steps(2);
    let view = harness.state().viewing().expect("журнал открыт");
    assert!(view.copy_job.is_none(), "задание отменено");
    assert!(view.copy_notice.is_none(), "сообщения не осталось");
    assert_eq!(view.selection.count(), 0, "выделение снято");
}

/// В просмотре журнала выделение тоже работает: клик, Shift+клик и Ctrl+C кладут
/// строки журнала с временем и направлением.
#[test]
fn a_journal_selection_copies_its_rows() {
    let dir = tempfile::tempdir().expect("временная папка");
    let mut harness = journal_harness(dir.path(), 3);
    wait_for_row(&mut harness, "JEV00000");
    click_row(&mut harness, "JEV00000", Modifiers::NONE);
    click_row(&mut harness, "JEV00002", Modifiers::SHIFT);
    let view = harness.state().viewing().expect("журнал открыт");
    assert_eq!(view.selection.count(), 3);
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let copied = copied_text(&harness).expect("буфер обмена");
    assert_eq!(copied.lines().count(), 3, "{copied}");
    assert!(copied.lines().next().unwrap().contains("JEV00000"));
}
