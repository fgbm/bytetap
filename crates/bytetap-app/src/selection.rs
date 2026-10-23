//! Выделение строк ленты блоком (L1, шаг 2; вариант А исследования без горизонтальной
//! прокрутки).
//!
//! Выделение живёт **номерами событий**, а не строками экрана и не выделением egui:
//! только так оно переживает прокрутку, приход новых строк, смену вида «Текст»/«HEX» и
//! вытеснение кольца. Из-за этого его нельзя делать средствами `Label::selectable`:
//! выделение egui сбрасывается, как только один из его концов уходит за пределы
//! видимой области (`label_text_selection.rs`, конец кадра).
//!
//! Выделенное хранится непересекающимися диапазонами: 10 000 строк, выделенных
//! Shift+кликом и Ctrl+кликом, — это единицы диапазонов, а проверка «строка выделена»
//! в кадре стоит двоичный поиск, а не проход по 10 000 номеров.

use crate::feed::FeedEvent;
use crate::feedrows::{self, Pair, RowClass};
use crate::settings::Encoding;

/// Сколько строк максимум уходит в буфер обмена за одно копирование.
pub const COPY_MAX_ROWS: usize = 10_000;
/// Сколько байт текста максимум уходит в буфер обмена за одно копирование.
pub const COPY_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Предел, после которого сужение выделения под фильтр соединения не идёт по номерам:
/// в записи этого не бывает (кольцо 10 000), а проход по миллиону номеров в кадре
/// фильтра не нужен.
const NARROW_MAX: usize = 100_000;

/// Выделенные строки ленты: диапазоны номеров событий, счётчик и якорь.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Непересекающиеся диапазоны `(первый, последний)` включительно, по возрастанию.
    ranges: Vec<(u64, u64)>,
    /// Сколько событий выделено: пересчитывается только при правке, не в кадре.
    count: usize,
    /// Якорь Shift+клика и Shift+стрелок: от него строится диапазон.
    anchor: Option<u64>,
}

impl Selection {
    /// Ничего не выделено.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Сколько событий выделено.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Якорь диапазона: строка, с которой началось выделение.
    pub fn anchor(&self) -> Option<u64> {
        self.anchor
    }

    /// Строка `seq` выделена: двоичный поиск по диапазонам, не проход по номерам.
    pub fn contains(&self, seq: u64) -> bool {
        let index = self.ranges.partition_point(|(start, _)| *start <= seq);
        index > 0 && self.ranges[index - 1].1 >= seq
    }

    /// Снять выделение целиком (Esc).
    pub fn clear(&mut self) {
        self.ranges.clear();
        self.count = 0;
        self.anchor = None;
    }

    /// Клик: одна строка, и она же якорь следующего Shift+клика.
    pub fn click(&mut self, seq: u64) {
        self.ranges.clear();
        self.ranges.push((seq, seq));
        self.count = 1;
        self.anchor = Some(seq);
    }

    /// Shift+клик и Shift+стрелки: диапазон от якоря до `seq` добавляется к выделению.
    ///
    /// Диапазоны складываются, а не заменяют друг друга: Shift+клик после Ctrl+клика
    /// не теряет отмеченные строки, а повторный Shift+клик ближе к якорю выделение не
    /// сужает. Снять лишнее можно Ctrl+кликом по строке или Esc.
    pub fn extend(&mut self, seq: u64) {
        let anchor = self.anchor.unwrap_or(seq);
        let (from, to) = if anchor <= seq {
            (anchor, seq)
        } else {
            (seq, anchor)
        };
        self.add_range(from, to);
    }

    /// Ctrl+клик: строка добавляется к выделению или убирается из него.
    pub fn toggle(&mut self, seq: u64) {
        if self.contains(seq) {
            self.remove_seq(seq);
        } else {
            self.add_range(seq, seq);
        }
        self.anchor = Some(seq);
    }

    /// Ctrl+A: выделить перечисленные события (видимые строки ленты).
    ///
    /// Номера сортируются и склеиваются одним проходом: «выделить всё» в журнале не
    /// должно стоить квадрата от числа видимых строк.
    pub fn select_all(&mut self, seqs: impl IntoIterator<Item = u64>) {
        let mut numbers: Vec<u64> = seqs.into_iter().collect();
        numbers.sort_unstable();
        numbers.dedup();
        self.ranges.clear();
        for seq in numbers {
            match self.ranges.last_mut() {
                Some((_, end)) if *end + 1 == seq => *end = seq,
                _ => self.ranges.push((seq, seq)),
            }
        }
        self.recount();
        self.anchor = self.ranges.first().map(|(start, _)| *start);
    }

    /// События, номера которых меньше `first`, ушли из кольца: их больше не выделить.
    pub fn retain_after(&mut self, first: u64) {
        if self.count == 0 {
            return;
        }
        let mut ranges = Vec::with_capacity(self.ranges.len());
        for (start, end) in self.ranges.drain(..) {
            let start = start.max(first);
            if start <= end {
                ranges.push((start, end));
            }
        }
        self.ranges = ranges;
        self.recount();
        if self.anchor.is_some_and(|anchor| anchor < first) {
            self.anchor = None;
        }
    }

    /// Оставить только события, для которых `keep` истинно: фильтр соединения спрятал
    /// строку — она и не выделена.
    pub fn retain(&mut self, keep: impl Fn(u64) -> bool) {
        if self.count == 0 {
            return;
        }
        if self.count > NARROW_MAX {
            // Проход по миллиону номеров в кадре фильтра не стоит своей пользы.
            self.clear();
            return;
        }
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for (start, end) in self.ranges.drain(..) {
            let mut run: Option<(u64, u64)> = None;
            for seq in start..=end {
                if keep(seq) {
                    run = Some(match run {
                        Some((from, _)) => (from, seq),
                        None => (seq, seq),
                    });
                } else if let Some(run) = run.take() {
                    ranges.push(run);
                }
            }
            if let Some(run) = run {
                ranges.push(run);
            }
        }
        self.ranges = ranges;
        self.recount();
        if self.anchor.is_some_and(|anchor| !keep(anchor)) {
            self.anchor = None;
        }
    }

    /// Выделенные номера по возрастанию: порядок копирования — порядок ленты.
    pub fn seqs(&self) -> impl Iterator<Item = u64> + '_ {
        self.ranges.iter().flat_map(|(start, end)| *start..=*end)
    }

    /// Добавить диапазон `from..=to` с объединением пересекающихся.
    fn add_range(&mut self, from: u64, to: u64) {
        let mut ranges = Vec::with_capacity(self.ranges.len() + 1);
        let mut merged = (from, to);
        let mut inserted = false;
        for &(start, end) in &self.ranges {
            if end.saturating_add(1) < merged.0 {
                ranges.push((start, end));
            } else if start > merged.1.saturating_add(1) {
                if !inserted {
                    ranges.push(merged);
                    inserted = true;
                }
                ranges.push((start, end));
            } else {
                // Диапазоны соприкасаются или пересекаются: склеиваем в один.
                merged = (merged.0.min(start), merged.1.max(end));
            }
        }
        if !inserted {
            ranges.push(merged);
        }
        self.ranges = ranges;
        self.recount();
    }

    /// Убрать один номер, разорвав диапазон на два, если он был внутри.
    fn remove_seq(&mut self, seq: u64) {
        let Some(index) = self
            .ranges
            .iter()
            .position(|(start, end)| *start <= seq && seq <= *end)
        else {
            return;
        };
        let (start, end) = self.ranges.remove(index);
        if start < seq {
            self.ranges.insert(index, (start, seq - 1));
        }
        if seq < end {
            self.ranges
                .insert(index + usize::from(start < seq), (seq + 1, end));
        }
        self.recount();
    }

    /// Счётчик выделенного — сумма длин диапазонов.
    fn recount(&mut self) {
        self.count = self
            .ranges
            .iter()
            .map(|(start, end)| (end - start + 1) as usize)
            .sum();
    }
}

/// Что ушло в буфер обмена и упёрлось ли копирование в предел.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyOut {
    /// Текст для буфера обмена.
    pub text: String,
    /// Сколько строк вошло.
    pub rows: usize,
    /// Сколько байт занял текст.
    pub bytes: usize,
    /// Предел сработал: строк вошло меньше, чем выделено.
    pub truncated: bool,
}

/// Строка копирования события: «время → #1 текст» (решение владельца, Л-В3).
///
/// Управляющие байты — глифами `‹CR›`, как в ленте (Л-В4), текст фрагмента — целиком,
/// без предела показа в строке. У служебной строки (открытие, закрытие, ошибка,
/// замечание, комментарий) копируется её строка ленты, и номер соединения в ней уже есть.
pub fn line_text(event: &FeedEvent, pair: Pair, encoding: Encoding) -> String {
    let (class, text) = feedrows::row_text(event, pair);
    match (class, event.data(), event.conn) {
        (RowClass::Data(dir), Some(data), Some(conn)) => format!(
            "{} {} #{} {}",
            event.time,
            feedrows::glyph(dir),
            conn,
            data.shown_text(encoding)
        ),
        (RowClass::Data(dir), Some(data), None) => format!(
            "{} {} {}",
            event.time,
            feedrows::glyph(dir),
            data.shown_text(encoding)
        ),
        _ => format!("{} {}", event.time, text),
    }
}

/// Текст выделенных строк по одной строке на событие, с пределом на копирование.
pub fn copy_rows<'a>(
    events: impl Iterator<Item = &'a FeedEvent>,
    pair: Pair,
    encoding: Encoding,
) -> CopyOut {
    let mut out = CopyOut::default();
    for event in events {
        let line = line_text(event, pair, encoding);
        if out.rows >= COPY_MAX_ROWS || out.bytes + line.len() > COPY_MAX_BYTES {
            out.truncated = true;
            break;
        }
        if out.rows > 0 {
            out.text.push('\n');
            out.bytes += 1;
        }
        out.bytes += line.len();
        out.text.push_str(&line);
        out.rows += 1;
    }
    out
}

/// HEX выделенных строк: по строке на событие, байты через пробел.
///
/// Строки разделены переводом строки, а не склеены в один поток байт: у служебных
/// строк байтов нет, а границу события иначе не восстановить.
pub fn copy_rows_hex<'a>(events: impl Iterator<Item = &'a FeedEvent>) -> CopyOut {
    let mut out = CopyOut::default();
    for event in events {
        let Some(data) = event.data() else {
            continue;
        };
        let line = crate::feed::hex_string(&data.bytes);
        if out.rows >= COPY_MAX_ROWS || out.bytes + line.len() > COPY_MAX_BYTES {
            out.truncated = true;
            break;
        }
        if out.rows > 0 {
            out.text.push('\n');
            out.bytes += 1;
        }
        out.bytes += line.len();
        out.text.push_str(&line);
        out.rows += 1;
    }
    out
}

/// Base64 выделенных строк: по строке на событие.
pub fn copy_rows_base64<'a>(events: impl Iterator<Item = &'a FeedEvent>) -> CopyOut {
    let mut out = CopyOut::default();
    for event in events {
        let Some(data) = event.data() else {
            continue;
        };
        let line = crate::feed::base64_string(&data.bytes);
        if out.rows >= COPY_MAX_ROWS || out.bytes + line.len() > COPY_MAX_BYTES {
            out.truncated = true;
            break;
        }
        if out.rows > 0 {
            out.text.push('\n');
            out.bytes += 1;
        }
        out.bytes += line.len();
        out.text.push_str(&line);
        out.rows += 1;
    }
    out
}

/// Что копировать из выделения.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyKind {
    /// Текст: время, направление, номер соединения и фрагмент.
    #[default]
    Text,
    /// Байты фрагментов через пробел, по строке на событие.
    Hex,
    /// Base64 фрагментов, по строке на событие.
    Base64,
}

/// Что экран делает после контекстного меню выделенной строки.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuAction {
    /// Меню не выбрано.
    #[default]
    None,
    /// «Скопировать N строк»: время, направление, номер и текст.
    CopyText,
    /// Те же строки в HEX.
    CopyHex,
    /// Те же строки в Base64.
    CopyBase64,
    /// «Снять выделение».
    Clear,
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::Dir;

    use super::*;
    use crate::feed::{DataPayload, FeedKind};

    fn event(conn: u32, text: &str) -> FeedEvent {
        FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: Some(0),
            conn: Some(conn),
            kind: FeedKind::Data(DataPayload::new(
                Dir::AnalyzerToLis,
                false,
                text.as_bytes().to_vec(),
            )),
        }
    }

    fn selected(selection: &Selection) -> Vec<u64> {
        selection.seqs().collect()
    }

    /// Клик выделяет одну строку и ставит якорь, Shift+клик строит диапазон.
    #[test]
    fn a_click_and_a_shift_click_make_a_range() {
        let mut selection = Selection::default();
        assert!(selection.is_empty());
        selection.click(5);
        assert_eq!(selected(&selection), [5]);
        assert_eq!(selection.anchor(), Some(5));
        selection.extend(9);
        assert_eq!(selected(&selection), [5, 6, 7, 8, 9]);
        assert_eq!(selection.count(), 5);
        // Shift+клик выше якоря — тот же диапазон, порядок не важен.
        selection.click(5);
        selection.extend(2);
        assert_eq!(selected(&selection), [2, 3, 4, 5]);
        assert!(selection.contains(3));
        assert!(!selection.contains(1));
        assert!(!selection.contains(6));
    }

    /// Ctrl+клик добавляет и убирает строку, не сбрасывая остальное выделение.
    #[test]
    fn ctrl_click_adds_and_removes_rows() {
        let mut selection = Selection::default();
        selection.click(5);
        selection.extend(6);
        selection.toggle(9);
        assert_eq!(selected(&selection), [5, 6, 9]);
        selection.toggle(5);
        assert_eq!(selected(&selection), [6, 9]);
        selection.toggle(5);
        assert_eq!(selected(&selection), [5, 6, 9]);
        // Якорь переехал на последнюю отмеченную строку.
        assert_eq!(selection.anchor(), Some(5));
        selection.extend(7);
        assert_eq!(selected(&selection), [5, 6, 7, 9]);
    }

    /// Ctrl+A выделяет перечисленные строки, Esc снимает выделение целиком.
    #[test]
    fn select_all_and_clear() {
        let mut selection = Selection::default();
        selection.click(50);
        selection.select_all([1, 2, 3, 7, 8]);
        assert_eq!(selected(&selection), [1, 2, 3, 7, 8]);
        assert_eq!(selection.count(), 5);
        selection.clear();
        assert!(selection.is_empty());
        assert_eq!(selection.anchor(), None);
    }

    /// Вытеснение из кольца убирает выделенные события из выделения, счётчик падает.
    #[test]
    fn evicted_rows_leave_the_selection() {
        let mut selection = Selection::default();
        selection.select_all(1..=10);
        selection.retain_after(4);
        assert_eq!(selected(&selection), [4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(selection.count(), 7);
        // Якорь ушёл вместе с выделением — следующему Shift+клику нужен новый.
        assert_eq!(selection.anchor(), None);
        selection.retain_after(9);
        assert_eq!(selected(&selection), [9, 10]);
        selection.retain_after(11);
        assert!(selection.is_empty());
    }

    /// Фильтр соединения прячет строку — она не выделена и не копируется.
    #[test]
    fn a_filtered_row_is_not_selected() {
        let mut selection = Selection::default();
        selection.select_all(1..=6);
        selection.retain(|seq| seq % 2 == 0);
        assert_eq!(selected(&selection), [2, 4, 6]);
        assert_eq!(selection.count(), 3);
    }

    /// Строка копирования: время, направление, номер соединения и полный текст.
    #[test]
    fn a_copied_line_keeps_time_direction_and_conn() {
        let data = event(2, "\x0bMSH|^~\\&\rMSA|AA\x1c\r");
        let line = line_text(&data, Pair::None, Encoding::Cp1251);
        assert_eq!(line, "10:15:03.000 → #2 ‹VT›MSH|^~\\&‹CR›MSA|AA‹FS›‹CR›");
        // Полный текст, а не первые 2048 байт: предел показа в строке ленты остался.
        let long = event(1, &"A".repeat(3_000));
        let line = line_text(&long, Pair::None, Encoding::Cp1251);
        assert_eq!(line.matches('A').count(), 3_000);
        assert!(!line.ends_with('…'));
    }

    /// Служебная строка копируется своей строкой ленты: времени, направления и
    /// номера соединения у неё нет.
    #[test]
    fn a_service_row_copies_its_own_line() {
        let event = FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: Some(0),
            conn: Some(1),
            kind: FeedKind::Close {
                by: bytetap_core::journal::ClosedBy::Lis,
                reason: "eof".into(),
            },
        };
        assert_eq!(
            line_text(&event, Pair::None, Encoding::Cp1251),
            "10:15:03.000 #1 отключился · ЛИС"
        );
    }

    /// Строки идут по номеру события, по одной на событие.
    #[test]
    fn copied_rows_keep_the_order_of_the_feed() {
        let events: Vec<FeedEvent> = (1..=3).map(|conn| event(conn, "A")).collect();
        let out = copy_rows(events.iter(), Pair::None, Encoding::Cp1251);
        assert_eq!(out.rows, 3);
        assert!(!out.truncated);
        let lines: Vec<&str> = out.text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("#1"), "{lines:?}");
        assert!(lines[2].contains("#3"), "{lines:?}");
    }

    /// Предел копирования: строк больше [`COPY_MAX_ROWS`] — в буфер идёт предел и
    /// признак, по которому экран говорит об этом инженеру.
    #[test]
    fn the_copy_stops_at_the_limit() {
        let events: Vec<FeedEvent> = (0..COPY_MAX_ROWS as u32 + 5)
            .map(|conn| event(conn, "A"))
            .collect();
        let out = copy_rows(events.iter(), Pair::None, Encoding::Cp1251);
        assert!(out.truncated);
        assert_eq!(out.rows, COPY_MAX_ROWS);
        assert_eq!(out.text.lines().count(), COPY_MAX_ROWS);
        // Предел по байтам: 16 МБ текста в буфер не кладём.
        let long = [event(1, &"A".repeat(16 * 1024))];
        let out = copy_rows(
            long.iter().cycle().take(2_000),
            Pair::None,
            Encoding::Cp1251,
        );
        assert!(out.truncated);
        assert!(out.bytes <= COPY_MAX_BYTES);
    }

    /// HEX и Base64 идут по строке на событие; у служебной строки байтов нет.
    #[test]
    fn hex_and_base64_go_line_by_line() {
        let data = event(1, "AB");
        let service = FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: Some(0),
            conn: Some(1),
            kind: FeedKind::Note {
                text: "заметка".into(),
            },
        };
        let events = [data, service];
        let hex = copy_rows_hex(events.iter());
        assert_eq!(hex.rows, 1);
        assert_eq!(hex.text, "41 42");
        let base64 = copy_rows_base64(events.iter());
        assert_eq!(base64.rows, 1);
        assert_eq!(base64.text, "QUI=");
    }

    /// Выделение 10 000 строк не перебирает номера в кадре: проверка строки — двоичный
    /// поиск по диапазонам (NFR-5).
    #[test]
    fn a_huge_selection_answers_in_a_logarithm() {
        let mut selection = Selection::default();
        selection.select_all(0..10_000);
        assert_eq!(selection.count(), 10_000);
        assert!(selection.contains(9_999));
        assert!(!selection.contains(10_000));
        // Ровно один диапазон: Shift+клик по всей ленте — не 10 000 записей.
        assert_eq!(selection.ranges.len(), 1);
        selection.toggle(5_000);
        assert_eq!(selection.ranges.len(), 2);
        assert!(!selection.contains(5_000));
    }
}
