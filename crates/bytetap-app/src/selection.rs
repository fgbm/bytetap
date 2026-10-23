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
use crate::t;

/// Сколько строк максимум уходит в буфер обмена за одно копирование.
pub const COPY_MAX_ROWS: usize = 10_000;
/// Сколько байт текста максимум уходит в буфер обмена за одно копирование.
pub const COPY_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Предел, после которого сужение выделения под фильтр соединения не идёт по номерам:
/// в записи этого не бывает (кольцо 10 000), а проход по миллиону номеров в кадре
/// фильтра не нужен.
const NARROW_MAX: usize = 100_000;

/// Выделенные строки ленты: отметки, диапазон от якоря, счётчик и якорь.
///
/// Как в Проводнике Windows (решение владельца 2026-10-23): Shift+клик заменяет
/// выделение диапазоном от якоря, но не сбрасывает «базу» — отметки, сделанные
/// Ctrl+кликом. Выделение — это `marks ∪ span`, а `marks` помнит и то, что было
/// отмечено до установки якоря.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Отметки Ctrl+клика: база, которую не сбрасывает Shift+клик.
    marks: Vec<(u64, u64)>,
    /// Диапазон Shift+клика и Shift+стрелок: от якоря до цели.
    span: Option<(u64, u64)>,
    /// Якорь: строка, от которой строится диапазон.
    anchor: Option<u64>,
    /// Склеенные `marks ∪ span` по возрастанию: их и обходят `seqs` и копирование.
    merged: Vec<(u64, u64)>,
    /// Сколько событий выделено: пересчитывается только при правке, не в кадре.
    count: usize,
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
        let index = self.merged.partition_point(|(start, _)| *start <= seq);
        index > 0 && self.merged[index - 1].1 >= seq
    }

    /// Снять выделение целиком (Esc).
    pub fn clear(&mut self) {
        self.marks.clear();
        self.span = None;
        self.anchor = None;
        self.recount();
    }

    /// Клик: одна строка, и она же якорь следующего Shift+клика.
    ///
    /// Отметки Ctrl+клика клик снимает: новая выделенная строка — это новое выделение.
    pub fn click(&mut self, seq: u64) {
        self.marks.clear();
        self.span = Some((seq, seq));
        self.anchor = Some(seq);
        self.recount();
    }

    /// Shift+клик и Shift+стрелки: диапазон от якоря **заменяет** прежний диапазон.
    ///
    /// База (`marks`) остаётся: отметки Ctrl+клика до установки якоря Shift+клик не
    /// сбрасывает. Shift+↑ после Shift+↓ поэтому сужает диапазон, а не расширяет его.
    pub fn extend(&mut self, seq: u64) {
        let anchor = *self.anchor.get_or_insert(seq);
        self.span = Some((anchor.min(seq), anchor.max(seq)));
        self.recount();
    }

    /// Ctrl+Shift+клик: диапазон от якоря добавляется к базе, прежнее выделение остаётся.
    pub fn extend_add(&mut self, seq: u64) {
        let anchor = *self.anchor.get_or_insert(seq);
        let (from, to) = (anchor.min(seq), anchor.max(seq));
        self.marks = merge(
            self.marks
                .iter()
                .copied()
                .chain(std::iter::once((from, to))),
        );
        self.recount();
    }

    /// Ctrl+клик: строка добавляется к выделению или убирается из него.
    ///
    /// Диапазон Shift+клика при этом становится базой: иначе снятие одной строки в нём
    /// сбросило бы весь диапазон.
    pub fn toggle(&mut self, seq: u64) {
        if let Some(span) = self.span.take() {
            self.marks = merge(self.marks.iter().copied().chain(std::iter::once(span)));
        }
        let was = self.contains(seq);
        if was {
            self.marks = remove(self.marks.iter().copied(), seq);
        } else {
            self.marks = merge(
                self.marks
                    .iter()
                    .copied()
                    .chain(std::iter::once((seq, seq))),
            );
        }
        self.anchor = Some(seq);
        self.recount();
    }

    /// Ctrl+A: выделить перечисленные события (видимые строки ленты).
    ///
    /// Номера сортируются и склеиваются одним проходом: «выделить всё» в журнале не
    /// должно стоить квадрата от числа видимых строк.
    pub fn select_all(&mut self, seqs: impl IntoIterator<Item = u64>) {
        let mut numbers: Vec<u64> = seqs.into_iter().collect();
        numbers.sort_unstable();
        numbers.dedup();
        self.marks = merge(numbers.into_iter().map(|seq| (seq, seq)));
        self.span = None;
        self.anchor = self.marks.first().map(|(start, _)| *start);
        self.recount();
    }

    /// События, номера которых меньше `first`, ушли из кольца: их больше не выделить.
    pub fn retain_after(&mut self, first: u64) {
        if self.count == 0 {
            return;
        }
        self.marks = clamp_from(self.marks.iter().copied(), first);
        self.span = self
            .span
            .map(|(start, end)| (start.max(first), end))
            .filter(|(start, end)| start <= end);
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
        self.marks = keep_each(self.marks.iter().copied(), &keep);
        if let Some(span) = self.span.take() {
            // Диапазон сужается и остаётся диапазоном, пока спрятанные строки не
            // разорвут его на части: тогда его хвосты уходят в базу.
            let ranges = keep_each(std::iter::once(span), &keep);
            self.span = ranges.first().copied();
            if ranges.len() > 1 {
                self.marks = merge(self.marks.iter().copied().chain(ranges.into_iter().skip(1)));
            }
        }
        self.recount();
        if self.anchor.is_some_and(|anchor| !keep(anchor)) {
            self.anchor = None;
        }
    }

    /// Выделенные номера по возрастанию: порядок копирования — порядок ленты.
    pub fn seqs(&self) -> impl Iterator<Item = u64> + '_ {
        self.merged.iter().flat_map(|(start, end)| *start..=*end)
    }

    /// Пересобрать склейку и счётчик после правки.
    fn recount(&mut self) {
        let all = self.marks.iter().copied().chain(self.span.iter().copied());
        self.merged = merge(all);
        self.count = self
            .merged
            .iter()
            .map(|(start, end)| (end - start + 1) as usize)
            .sum();
    }
}

/// Склеить диапазоны по возрастанию, без пересечений и соприкосновений.
fn merge(ranges: impl Iterator<Item = (u64, u64)>) -> Vec<(u64, u64)> {
    let mut sorted: Vec<(u64, u64)> = ranges.filter(|(start, end)| start <= end).collect();
    sorted.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(sorted.len());
    for (start, end) in sorted {
        match out.last_mut() {
            Some((_, last)) if start <= last.saturating_add(1) => *last = (*last).max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// Убрать один номер, разорвав диапазон на два.
fn remove(ranges: impl Iterator<Item = (u64, u64)>, seq: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for (start, end) in ranges {
        if seq < start || seq > end {
            out.push((start, end));
            continue;
        }
        if start < seq {
            out.push((start, seq - 1));
        }
        if seq < end {
            out.push((seq + 1, end));
        }
    }
    out
}

/// Сдвинуть начало диапазонов к `first`, выбросив пустые.
fn clamp_from(ranges: impl Iterator<Item = (u64, u64)>, first: u64) -> Vec<(u64, u64)> {
    ranges
        .map(|(start, end)| (start.max(first), end))
        .filter(|(start, end)| start <= end)
        .collect()
}

/// Оставить в диапазонах только номера, прошедшие `keep`.
fn keep_each(
    ranges: impl Iterator<Item = (u64, u64)>,
    keep: &impl Fn(u64) -> bool,
) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        let mut run: Option<(u64, u64)> = None;
        for seq in start..=end {
            if keep(seq) {
                run = Some(match run {
                    Some((from, _)) => (from, seq),
                    None => (seq, seq),
                });
            } else if let Some(run) = run.take() {
                out.push(run);
            }
        }
        if let Some(run) = run {
            out.push(run);
        }
    }
    out
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
    let line = match (class, event.data(), event.conn) {
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
    };
    // «Одна строка на событие» держится и для комментария журнала: переводы строк в
    // его тексте становятся глифами, как управляющие байты фрагмента в ленте.
    if line.contains(['\n', '\r']) {
        return line
            .chars()
            .map(|ch| match ch {
                '\n' => "‹LF›".to_owned(),
                '\r' => "‹CR›".to_owned(),
                other => other.to_string(),
            })
            .collect();
    }
    line
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
        if !push_line(&mut out, &line) {
            break;
        }
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
        match line_for(event, CopyKind::Hex, Pair::None, Encoding::Ascii) {
            Some(line) => {
                if !push_line(&mut out, &line) {
                    break;
                }
            }
            None => continue,
        }
    }
    out
}

/// Base64 выделенных строк: по строке на событие.
pub fn copy_rows_base64<'a>(events: impl Iterator<Item = &'a FeedEvent>) -> CopyOut {
    let mut out = CopyOut::default();
    for event in events {
        match line_for(event, CopyKind::Base64, Pair::None, Encoding::Ascii) {
            Some(line) => {
                if !push_line(&mut out, &line) {
                    break;
                }
            }
            None => continue,
        }
    }
    out
}

/// Строка события для буфера обмена; `None` — у события нет байтов (HEX и Base64
/// служебной строки пропускаются).
pub fn line_for(
    event: &FeedEvent,
    kind: CopyKind,
    pair: Pair,
    encoding: Encoding,
) -> Option<String> {
    match kind {
        CopyKind::Text => Some(line_text(event, pair, encoding)),
        CopyKind::Hex => event
            .data()
            .map(|data| crate::feed::hex_string(&data.bytes)),
        CopyKind::Base64 => event
            .data()
            .map(|data| crate::feed::base64_string(&data.bytes)),
    }
}

/// Дописать строку в копирование: перевод строки между событиями, предел строк и байт.
///
/// `false` — предел сработал, строку не добавляли; `truncated` в ответе это отмечает.
/// Перевод строки между событиями считается в пределе байт: он тоже уходит в буфер.
#[must_use]
pub fn push_line(out: &mut CopyOut, line: &str) -> bool {
    let separator = usize::from(out.rows > 0);
    if out.rows >= COPY_MAX_ROWS || out.bytes + separator + line.len() > COPY_MAX_BYTES {
        out.truncated = true;
        return false;
    }
    if separator > 0 {
        out.text.push('\n');
    }
    out.bytes += separator + line.len();
    out.text.push_str(line);
    out.rows += 1;
    true
}

/// Число строк с разделителями разрядов и падежом: «10 000 строк», «201 строка».
///
/// Русское склонение считает само сообщение: «в буфер вошло 201 строка из 250 строк»
/// читается, а «201 строк» — нет.
pub fn rows_count(count: usize) -> String {
    let digits = count.to_string();
    let mut separated = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        // Разделитель разрядов — перед каждой третьей цифрой с конца.
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            separated.push(' ');
        }
        separated.push(ch);
    }
    format!("{separated} {}", rows_word(count))
}

/// Падеж слова «строка» для числа: одна строка, две строки, пять строк.
fn rows_word(count: usize) -> &'static str {
    let last_two = count % 100;
    let last = count % 10;
    if (11..=14).contains(&last_two) {
        t!("feed.rowsMany")
    } else if last == 1 {
        t!("feed.rowsOne")
    } else if (2..=4).contains(&last) {
        t!("feed.rowsFew")
    } else {
        t!("feed.rowsMany")
    }
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

    /// Как в Проводнике: Shift+клик заменяет диапазон, а не складывает его с прежним.
    #[test]
    fn a_shift_click_replaces_the_range() {
        let mut selection = Selection::default();
        selection.click(10);
        selection.extend(30);
        assert_eq!(selection.count(), 21);
        selection.extend(20);
        assert_eq!(selected(&selection), (10..=20).collect::<Vec<u64>>());
        assert_eq!(selection.count(), 11, "10 → Shift 30 → Shift 20 = 11 строк");
        // Shift+↑ после Shift+↓ сужает диапазон обратно.
        selection.click(10);
        selection.extend(11);
        selection.extend(12);
        assert_eq!(selection.count(), 3);
        selection.extend(11);
        assert_eq!(selection.count(), 2, "Shift+↓↓↑ = 2 строки");
    }

    /// База — отметки Ctrl+клика — Shift+клик не сбрасывает, а Ctrl+Shift+клик
    /// добавляет к ней второй диапазон.
    #[test]
    fn the_ctrl_marks_survive_a_shift_click() {
        let mut selection = Selection::default();
        selection.toggle(5);
        selection.toggle(10);
        // Якорь — последняя отмеченная строка, база {5, 10} остаётся.
        selection.extend(20);
        assert_eq!(selected(&selection), {
            let mut want: Vec<u64> = vec![5];
            want.extend(10..=20);
            want
        });
        assert_eq!(selection.count(), 12);
        // Ctrl+Shift+клик добавляет ещё один диапазон, не сбрасывая прежние.
        selection.toggle(50);
        selection.extend_add(52);
        assert_eq!(selection.count(), 15, "5 + 10..20 + 50..52");
        assert!(selection.contains(5) && selection.contains(15) && selection.contains(51));
        // Простой клик начинает выделение заново: база сбрасывается.
        selection.click(40);
        assert_eq!(selection.count(), 1);
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

    /// Фильтр рвёт диапазон Shift+клика: остаются видимые строки, а не только начало.
    #[test]
    fn a_filter_narrows_the_span_without_losing_its_tail() {
        let mut selection = Selection::default();
        selection.click(1);
        selection.extend(9);
        selection.retain(|seq| seq != 2 && (3..=7).contains(&seq));
        assert_eq!(selected(&selection), (3..=7).collect::<Vec<u64>>());
        // Дырка в середине: хвост диапазона остаётся в выделении.
        let mut selection = Selection::default();
        selection.click(1);
        selection.extend(9);
        selection.retain(|seq| seq != 5);
        assert_eq!(selected(&selection), [1, 2, 3, 4, 6, 7, 8, 9]);
    }

    /// Комментарий с переводами строк остаётся одной строкой копирования: переводы
    /// строк становятся глифами, как управляющие байты в ленте.
    #[test]
    fn a_comment_with_newlines_stays_one_line() {
        let event = FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: Some(0),
            conn: None,
            kind: FeedKind::Note {
                text: "первая\nвторая\r\nтретья".into(),
            },
        };
        let line = line_text(&event, Pair::None, Encoding::Cp1251);
        assert_eq!(line, "10:15:03.000 первая‹LF›вторая‹CR›‹LF›третья");
        assert_eq!(line.lines().count(), 1);
    }

    /// Числа в сообщениях — с разделителями разрядов и в нужном падеже.
    #[test]
    fn counts_are_written_with_separators_and_plurals() {
        assert_eq!(rows_count(1), "1 строка");
        assert_eq!(rows_count(3), "3 строки");
        assert_eq!(rows_count(5), "5 строк");
        assert_eq!(rows_count(11), "11 строк");
        assert_eq!(rows_count(21), "21 строка");
        assert_eq!(rows_count(201), "201 строка");
        assert_eq!(rows_count(1_000), "1 000 строк");
        assert_eq!(rows_count(10_000), "10 000 строк");
        assert_eq!(rows_count(12_500), "12 500 строк");
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
        // Перевод строки между событиями тоже занимает место в пределе.
        let half = COPY_MAX_BYTES / 2;
        assert_eq!(half * 2, COPY_MAX_BYTES);
        let mut out = CopyOut::default();
        assert!(push_line(&mut out, &"x".repeat(half)));
        // Две строки по половине предела без перевода строки влезли бы, а с ним — нет.
        assert!(!push_line(&mut out, &"x".repeat(half)));
        assert!(out.truncated);
        assert_eq!(out.bytes, half);
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
        assert_eq!(selection.merged.len(), 1);
        selection.toggle(5_000);
        assert_eq!(selection.merged.len(), 2, "снятая строка рвёт диапазон");
        assert!(!selection.contains(5_000));
        assert_eq!(selection.count(), 9_999);
    }
}
