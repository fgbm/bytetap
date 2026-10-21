//! Лента записи: события сессии в виде для экрана, кольцевой буфер на 10 000 событий,
//! текст фрагмента по кодировке и HEX-дамп (FR-22, FR-26…FR-28).
//!
//! Модель — та же, что в Vue-версии (`app/src/feed/**`): события ленты строятся из тех же
//! событий, что попадают в журнал; байты фрагмента декодируются из base64 один раз, при
//! приёме, вне потока окна. Текст по кодировке считается лениво и кешируется в самом
//! событии: строка декодируется один раз на кодировку. Кодировка влияет только на показ —
//! в журнал всегда пишутся исходные байты.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use bytetap_core::journal::{ClosedBy, Dir, Event};

use crate::settings::Encoding;

/// Сколько событий держит лента: старые уходят, память не растёт (FR-28).
pub const FEED_CAPACITY: usize = 10_000;

/// Сколько байт фрагмента показывается в строке ленты: остальное заменяется на «…».
pub const TEXT_LIMIT_BYTES: usize = 2048;

/// Байт в строке HEX-дампа.
pub const HEX_BYTES_PER_LINE: usize = 16;

/// Порядок кодировок в панели ленты.
pub const ENCODINGS: [Encoding; 5] = [
    Encoding::Ascii,
    Encoding::Cp1251,
    Encoding::Utf8,
    Encoding::Gbk,
    Encoding::Latin1,
];

/// Кусок текста фрагмента: печатный текст или глиф управляющего байта `‹CR›`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub ctrl: bool,
}

/// Фрагмент обмена: направление, автоответ и исходные байты.
#[derive(Debug)]
pub struct DataPayload {
    pub dir: Dir,
    pub synthetic: bool,
    pub bytes: Vec<u8>,
    /// Текст по кодировке в порядке [`ENCODINGS`]: считается при первом показе.
    text: [OnceLock<Arc<[Segment]>>; 5],
}

impl DataPayload {
    pub fn new(dir: Dir, synthetic: bool, bytes: Vec<u8>) -> Self {
        Self {
            dir,
            synthetic,
            bytes,
            text: Default::default(),
        }
    }

    /// Текст фрагмента по кодировке с кешем на событие.
    pub fn segments(&self, encoding: Encoding) -> Arc<[Segment]> {
        let slot = &self.text[encoding_index(encoding)];
        Arc::clone(
            slot.get_or_init(|| text_segments(&self.bytes, encoding, TEXT_LIMIT_BYTES).into()),
        )
    }
}

/// Что за событие ленты.
#[derive(Debug)]
pub enum FeedKind {
    Data(DataPayload),
    Open {
        peer: String,
    },
    Close {
        by: ClosedBy,
    },
    Error {
        code: Option<String>,
        message: String,
    },
    Info {
        code: Option<String>,
        message: String,
    },
    Note {
        text: String,
    },
}

/// Событие ленты: время уже в виде `HH:MM:SS.mmm`, номер соединения, если он есть.
#[derive(Debug)]
pub struct FeedEvent {
    pub time: String,
    pub mono_us: Option<u64>,
    pub conn: Option<u32>,
    pub kind: FeedKind,
}

impl FeedEvent {
    /// Событие ленты из строки журнала; заголовок `session` и итоги `session_end` в
    /// ленту не идут — как у моста Tauri-версии.
    pub fn from_session(event: &Event) -> Option<Self> {
        let (t, mono_us, conn, kind) = match event {
            Event::Data(data) => {
                // Битый base64 даёт пустой фрагмент, а не панику: длина видна в журнале.
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .unwrap_or_default();
                (
                    &data.t,
                    Some(data.mono_us),
                    Some(data.conn),
                    FeedKind::Data(DataPayload::new(data.dir, data.synthetic, bytes)),
                )
            }
            Event::Open(open) => (
                &open.t,
                Some(open.mono_us),
                Some(open.conn),
                FeedKind::Open {
                    peer: open.peer.clone(),
                },
            ),
            Event::Close(close) => (
                &close.t,
                Some(close.mono_us),
                Some(close.conn),
                FeedKind::Close { by: close.by },
            ),
            Event::Error(error) => (
                &error.t,
                Some(error.mono_us),
                error.conn,
                FeedKind::Error {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
            ),
            Event::Info(info) => (
                &info.t,
                Some(info.mono_us),
                info.conn,
                FeedKind::Info {
                    code: info.code.clone(),
                    message: info.message.clone(),
                },
            ),
            Event::Note(note) => (
                &note.t,
                note.mono_us,
                None,
                FeedKind::Note {
                    text: note.text.clone(),
                },
            ),
            Event::Session(_) | Event::SessionEnd(_) => return None,
        };
        Some(Self {
            time: format_time(t),
            mono_us,
            conn,
            kind,
        })
    }

    pub fn data(&self) -> Option<&DataPayload> {
        match &self.kind {
            FeedKind::Data(data) => Some(data),
            _ => None,
        }
    }

    /// Сколько строк событие занимает в HEX-виде; не фрагмент — одна строка.
    pub fn hex_lines(&self) -> usize {
        self.data()
            .map_or(1, |data| hex_line_count(data.bytes.len()))
    }
}

/// Кольцевой буфер ленты: последние [`FEED_CAPACITY`] событий с порядковыми номерами.
///
/// Номер события (`seq`) растёт с начала сессии и не переиспользуется: экран держит
/// по номерам свой индекс строк и дописывает только новые события. Вытесненные из
/// буфера события не считаются пропущенными — это обычная прокрутка истории;
/// пропущенные — те, что не дошли до ленты (отставший подписчик, `Lagged`).
#[derive(Debug)]
pub struct FeedRing {
    events: VecDeque<Arc<FeedEvent>>,
    capacity: usize,
    first_seq: u64,
    dropped: u64,
}

impl Default for FeedRing {
    fn default() -> Self {
        Self::new(FEED_CAPACITY)
    }
}

impl FeedRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(capacity.min(FEED_CAPACITY)),
            capacity: capacity.max(1),
            first_seq: 0,
            dropped: 0,
        }
    }

    /// Пустой буфер, нумерация которого начинается с `seq`: окно отстало от общего
    /// буфера больше чем на его ёмкость.
    pub fn starting_at(seq: u64) -> Self {
        Self {
            first_seq: seq,
            ..Self::default()
        }
    }

    /// Добавляет событие; возвращает вытесненное, если буфер был полон.
    pub fn push(&mut self, event: Arc<FeedEvent>) -> Option<Arc<FeedEvent>> {
        let evicted = if self.events.len() == self.capacity {
            self.first_seq += 1;
            self.events.pop_front()
        } else {
            None
        };
        self.events.push_back(event);
        evicted
    }

    /// Подписчик отстал: `count` событий до ленты не дошли.
    pub fn lagged(&mut self, count: u64) {
        self.dropped += count;
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Номер самого старого события в буфере.
    pub fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// Номер следующего события: сколько событий лента получила за сессию.
    pub fn next_seq(&self) -> u64 {
        self.first_seq + self.events.len() as u64
    }

    /// Сколько событий не дошло до ленты.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn get(&self, seq: u64) -> Option<&Arc<FeedEvent>> {
        let index = seq.checked_sub(self.first_seq)?;
        self.events.get(usize::try_from(index).ok()?)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Arc<FeedEvent>> {
        self.events.iter()
    }

    /// События с номера `from` (или с самого старого, если `from` уже вытеснен).
    pub fn since(&self, from: u64) -> impl Iterator<Item = (u64, &Arc<FeedEvent>)> {
        let start = from.max(self.first_seq);
        let skip = usize::try_from(start - self.first_seq).unwrap_or(usize::MAX);
        self.events
            .iter()
            .enumerate()
            .skip(skip)
            .map(|(index, event)| (self.first_seq + index as u64, event))
    }
}

fn encoding_index(encoding: Encoding) -> usize {
    ENCODINGS
        .iter()
        .position(|e| *e == encoding)
        .unwrap_or_default()
}

/// Управляющий байт: по FR-22 показывается глифом, а не текстом.
pub fn is_control_byte(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}

/// Имя управляющего байта по FR-22; остальные — `xHH`.
pub fn control_name(byte: u8) -> String {
    let name = match byte {
        0x02 => "STX",
        0x03 => "ETX",
        0x04 => "EOT",
        0x05 => "ENQ",
        0x06 => "ACK",
        0x0a => "LF",
        0x0b => "VT",
        0x0d => "CR",
        0x15 => "NAK",
        0x17 => "ETB",
        0x1c => "FS",
        _ => return format!("x{byte:02X}"),
    };
    name.to_owned()
}

fn glyph(name: &str) -> Segment {
    Segment {
        text: format!("‹{name}›"),
        ctrl: true,
    }
}

/// Печатный текст с `<` глифом `‹x3C›`: иначе текст прибора притворился бы управляющим
/// символом (FR-22).
fn push_text(out: &mut Vec<Segment>, text: &str) {
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        if at > 0 {
            push_plain(out, &rest[..at]);
        }
        out.push(glyph("x3C"));
        rest = &rest[at + 1..];
    }
    if !rest.is_empty() {
        push_plain(out, rest);
    }
}

fn push_plain(out: &mut Vec<Segment>, text: &str) {
    match out.last_mut() {
        Some(last) if !last.ctrl => last.text.push_str(text),
        _ => out.push(Segment {
            text: text.to_owned(),
            ctrl: false,
        }),
    }
}

fn decode_run(run: &[u8], encoding: Encoding) -> String {
    let table = match encoding {
        // ASCII печатный: управляющие и старшие байты сюда не попадают.
        Encoding::Ascii => return run.iter().map(|&b| char::from(b)).collect(),
        Encoding::Cp1251 => encoding_rs::WINDOWS_1251,
        Encoding::Utf8 => encoding_rs::UTF_8,
        Encoding::Gbk => encoding_rs::GBK,
        // `iso-8859-1` по WHATWG — Windows-1252, как у `TextDecoder` Vue-версии.
        Encoding::Latin1 => encoding_rs::WINDOWS_1252,
    };
    table.decode_without_bom_handling(run).0.into_owned()
}

/// Текст фрагмента: управляющие байты `<0x20` и `0x7F` (в ASCII ещё и `≥0x80`) — глифы
/// `‹CR›`, печатные куски — декодированные выбранной кодировкой. Текст режется до
/// декодирования, поэтому управляющий байт внутри многобайтовой последовательности
/// никогда не попадёт в декодер. Длиннее `limit` байт — «…» в конце.
pub fn text_segments(bytes: &[u8], encoding: Encoding, limit: usize) -> Vec<Segment> {
    let end = bytes.len().min(limit);
    let mut out = Vec::new();
    let mut start = 0;
    for (index, &byte) in bytes[..end].iter().enumerate() {
        if is_control_byte(byte) || (encoding == Encoding::Ascii && byte >= 0x80) {
            if start < index {
                push_text(&mut out, &decode_run(&bytes[start..index], encoding));
            }
            out.push(glyph(&control_name(byte)));
            start = index + 1;
        }
    }
    if start < end {
        push_text(&mut out, &decode_run(&bytes[start..end], encoding));
    }
    if bytes.len() > end {
        push_plain(&mut out, "…");
    }
    out
}

/// Строка текста целиком: для копирования из контекстного меню.
pub fn plain_text(segments: &[Segment]) -> String {
    segments.iter().map(|s| s.text.as_str()).collect()
}

/// Сколько строк дампа занимает фрагмент; пустой фрагмент — тоже строка.
pub fn hex_line_count(len: usize) -> usize {
    len.div_ceil(HEX_BYTES_PER_LINE).max(1)
}

/// Строка HEX-дампа: смещение, байты с признаком управляющего и ASCII-колонка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexLine {
    pub offset: String,
    pub bytes: Vec<(String, bool)>,
    pub ascii: String,
}

/// Одна строка дампа по номеру строки.
pub fn hex_line(bytes: &[u8], line: usize) -> HexLine {
    let start = (line * HEX_BYTES_PER_LINE).min(bytes.len());
    let slice = &bytes[start..(start + HEX_BYTES_PER_LINE).min(bytes.len())];
    HexLine {
        offset: format!("{start:04X}"),
        bytes: slice
            .iter()
            .map(|&b| (format!("{b:02X}"), is_control_byte(b)))
            .collect(),
        ascii: slice
            .iter()
            .map(|&b| {
                if (0x20..=0x7e).contains(&b) {
                    char::from(b)
                } else {
                    '.'
                }
            })
            .collect(),
    }
}

/// Байты фрагмента в HEX через пробел: для копирования.
pub fn hex_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{byte:02X}");
    }
    out
}

/// Байты фрагмента в base64: для копирования.
pub fn base64_string(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Время события: `HH:MM:SS.mmm` в поясе, записанном в журнале.
pub fn format_time(t: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(t).map_or_else(
        |_| "--:--:--.---".to_owned(),
        |time| time.format("%H:%M:%S%.3f").to_string(),
    )
}

/// Таймер записи: «14:07» до часа, «01:14:07» после.
pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = seconds % 3600 / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Номера соединений для фильтра: только последние `CONN_CHIPS`, ряд не растёт.
pub const CONN_CHIPS: usize = 8;

/// Событие проходит фильтр соединения: события без соединения (комментарий, ошибка
/// уровня сессии) — контекст всей сессии и видны при любом фильтре.
pub fn passes(event: &FeedEvent, filter: Option<u32>) -> bool {
    match (filter, event.conn) {
        (None, _) | (_, None) => true,
        (Some(wanted), Some(conn)) => wanted == conn,
    }
}

/// Протокол, который «Авто» выбрал по первому байту первого фрагмента анализатора.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detected {
    Hl7,
    Astm,
    Unknown,
}

pub fn detect(event: &FeedEvent) -> Option<Detected> {
    let data = event.data().filter(|d| d.dir == Dir::AnalyzerToLis)?;
    Some(match data.bytes.first() {
        Some(0x0b) => Detected::Hl7,
        Some(0x05) => Detected::Astm,
        _ => Detected::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::{Close, Data, Note, Open};

    use super::*;

    fn texts(segments: &[Segment]) -> Vec<(&str, bool)> {
        segments.iter().map(|s| (s.text.as_str(), s.ctrl)).collect()
    }

    #[test]
    fn control_bytes_become_glyphs_in_every_encoding() {
        let bytes = b"\x0bMSH|A\rB\x1c\r";
        for encoding in ENCODINGS {
            let segments = text_segments(bytes, encoding, TEXT_LIMIT_BYTES);
            assert_eq!(
                texts(&segments),
                [
                    ("‹VT›", true),
                    ("MSH|A", false),
                    ("‹CR›", true),
                    ("B", false),
                    ("‹FS›", true),
                    ("‹CR›", true),
                ],
                "{encoding:?}"
            );
        }
    }

    #[test]
    fn each_encoding_decodes_its_text() {
        // «Глюкоза» в Windows-1251 и UTF-8, «血糖» в GBK, «é» в Latin-1.
        let cp1251 = [0xc3, 0xeb, 0xfe, 0xea, 0xee, 0xe7, 0xe0];
        assert_eq!(
            plain_text(&text_segments(&cp1251, Encoding::Cp1251, 100)),
            "Глюкоза"
        );
        assert_eq!(
            plain_text(&text_segments("Глюкоза".as_bytes(), Encoding::Utf8, 100)),
            "Глюкоза"
        );
        assert_eq!(
            plain_text(&text_segments(
                &[0xd1, 0xaa, 0xcc, 0xc7],
                Encoding::Gbk,
                100
            )),
            "血糖"
        );
        assert_eq!(
            plain_text(&text_segments(
                &[b'c', b'a', b'f', 0xe9],
                Encoding::Latin1,
                100
            )),
            "café"
        );
        // ASCII: старшие байты — глифы, а не догадка о кодировке.
        assert_eq!(
            texts(&text_segments(&[b'a', 0xc3, b'b'], Encoding::Ascii, 100)),
            [("a", false), ("‹xC3›", true), ("b", false)]
        );
        // Битая последовательность UTF-8 — символ замены, а не паника.
        assert_eq!(
            plain_text(&text_segments(&[0xff, b'x'], Encoding::Utf8, 100)),
            "\u{fffd}x"
        );
    }

    #[test]
    fn a_less_than_sign_is_not_mistaken_for_a_control_glyph() {
        assert_eq!(
            texts(&text_segments(b"a<CR>", Encoding::Cp1251, 100)),
            [("a", false), ("‹x3C›", true), ("CR>", false)]
        );
    }

    #[test]
    fn a_long_fragment_is_cut_with_an_ellipsis() {
        let bytes = vec![b'x'; 10];
        assert_eq!(
            plain_text(&text_segments(&bytes, Encoding::Ascii, 4)),
            "xxxx…"
        );
    }

    #[test]
    fn the_text_is_decoded_once_per_encoding() {
        let data = DataPayload::new(Dir::AnalyzerToLis, false, b"A\r".to_vec());
        let first = data.segments(Encoding::Cp1251);
        let again = data.segments(Encoding::Cp1251);
        assert!(Arc::ptr_eq(&first, &again), "кеш на событие и кодировку");
        assert!(!Arc::ptr_eq(&first, &data.segments(Encoding::Utf8)));
    }

    #[test]
    fn the_hex_dump_goes_by_16_bytes() {
        let bytes: Vec<u8> = (0..20).collect();
        assert_eq!(hex_line_count(0), 1);
        assert_eq!(hex_line_count(16), 1);
        assert_eq!(hex_line_count(17), 2);
        let first = hex_line(&bytes, 0);
        assert_eq!(first.offset, "0000");
        assert_eq!(first.bytes.len(), 16);
        assert_eq!(first.bytes[13], ("0D".to_owned(), true));
        assert_eq!(first.ascii, "................");
        let second = hex_line(&bytes, 1);
        assert_eq!(second.offset, "0010");
        assert_eq!(second.bytes.len(), 4);
        let text = hex_line(b"AB\x7f", 0);
        assert_eq!(text.ascii, "AB.");
        assert_eq!(text.bytes[2], ("7F".to_owned(), true));
        assert_eq!(hex_string(b"\x0bA"), "0B 41");
        assert_eq!(base64_string(b"\x0bA"), "C0E=");
    }

    fn note(index: usize) -> Arc<FeedEvent> {
        Arc::new(FeedEvent {
            time: String::new(),
            mono_us: Some(index as u64),
            conn: None,
            kind: FeedKind::Note {
                text: index.to_string(),
            },
        })
    }

    #[test]
    fn the_ring_keeps_the_last_events_and_numbers_them() {
        let mut ring = FeedRing::new(3);
        for index in 0..5 {
            let evicted = ring.push(note(index));
            assert_eq!(evicted.is_some(), index >= 3);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.first_seq(), 2);
        assert_eq!(ring.next_seq(), 5);
        assert_eq!(ring.dropped(), 0, "вытеснение — не пропуск");
        assert!(ring.get(1).is_none());
        assert!(matches!(&ring.get(4).unwrap().kind, FeedKind::Note { text } if text == "4"));
        let since: Vec<u64> = ring.since(3).map(|(seq, _)| seq).collect();
        assert_eq!(since, [3, 4]);
        let since: Vec<u64> = ring.since(0).map(|(seq, _)| seq).collect();
        assert_eq!(since, [2, 3, 4], "вытесненное не возвращается");
        assert_eq!(ring.since(5).count(), 0);
        ring.lagged(7);
        assert_eq!(ring.dropped(), 7);
    }

    #[test]
    fn journal_events_become_feed_events() {
        let data = Event::Data(Data::new(
            "2026-10-19T10:15:03.000512+03:00",
            3_000_389,
            2,
            Dir::LisToAnalyzer,
            true,
            b"\x06",
        ));
        let event = FeedEvent::from_session(&data).unwrap();
        assert_eq!(event.time, "10:15:03.000");
        assert_eq!(event.conn, Some(2));
        let payload = event.data().unwrap();
        assert_eq!(payload.bytes, b"\x06");
        assert!(payload.synthetic);
        let open = Event::Open(Open {
            t: "bad".into(),
            mono_us: 1,
            conn: 1,
            peer: "192.168.1.51:49152".into(),
            local: "0.0.0.0:5000".into(),
        });
        let event = FeedEvent::from_session(&open).unwrap();
        assert_eq!(event.time, "--:--:--.---");
        assert!(matches!(event.kind, FeedKind::Open { .. }));
        let close = Event::Close(Close {
            t: "t".into(),
            mono_us: 2,
            conn: 1,
            by: ClosedBy::Analyzer,
            reason: "eof".into(),
        });
        assert!(matches!(
            FeedEvent::from_session(&close).unwrap().kind,
            FeedKind::Close {
                by: ClosedBy::Analyzer
            }
        ));
        let note = Event::Note(Note {
            t: "t".into(),
            mono_us: None,
            text: "проба".into(),
        });
        assert_eq!(FeedEvent::from_session(&note).unwrap().conn, None);
    }

    #[test]
    fn the_filter_keeps_session_wide_events() {
        let event = |conn| FeedEvent {
            time: String::new(),
            mono_us: None,
            conn,
            kind: FeedKind::Note {
                text: String::new(),
            },
        };
        assert!(passes(&event(Some(1)), None));
        assert!(passes(&event(Some(1)), Some(1)));
        assert!(!passes(&event(Some(2)), Some(1)));
        assert!(passes(&event(None), Some(1)));
    }

    #[test]
    fn durations_and_detection() {
        assert_eq!(format_duration(5), "00:05");
        assert_eq!(format_duration(847), "14:07");
        assert_eq!(format_duration(4447), "01:14:07");
        let data = |dir, bytes: &[u8]| FeedEvent {
            time: String::new(),
            mono_us: None,
            conn: Some(1),
            kind: FeedKind::Data(DataPayload::new(dir, false, bytes.to_vec())),
        };
        assert_eq!(
            detect(&data(Dir::AnalyzerToLis, b"\x0bMSH")),
            Some(Detected::Hl7)
        );
        assert_eq!(
            detect(&data(Dir::AnalyzerToLis, b"\x05")),
            Some(Detected::Astm)
        );
        assert_eq!(
            detect(&data(Dir::AnalyzerToLis, b"x")),
            Some(Detected::Unknown)
        );
        assert_eq!(detect(&data(Dir::LisToAnalyzer, b"\x06")), None);
    }
}
