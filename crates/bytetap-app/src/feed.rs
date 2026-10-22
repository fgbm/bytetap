//! Лента записи: события сессии в виде для экрана, кольцевой буфер на 10 000 событий,
//! текст фрагмента по кодировке и HEX-дамп (FR-22, FR-26…FR-28).
//!
//! Модель — та же, что в Vue-версии v0.1.0 (`app/src/feed/**`): события ленты строятся из
//! тех же
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

/// Кусок текста фрагмента: печатный текст, глиф управляющего байта `‹CR›` или
/// экранированный символ данных.
///
/// Кусок текста — не строка, а место в общей строке фрагмента: смещение, длина и вид.
/// Строка на кусок означала бы столько же аллокаций, сколько управляющих байт во
/// фрагменте: чередование `01 61 02 61…` давало бы две тысячи строк и две тысячи
/// аллокаций на кадр отрисовки. Имя глифа (`CR`, `x0B`) получается по байту при
/// отрисовке и копировании.
///
/// Подряд идущие одинаковые управляющие байты — один спан с повтором: фрагмент из
/// двух тысяч одинаковых байт не должен давать две тысячи спанов (FR-28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Смещение в [`Segments::text`], байты.
    start: u32,
    /// Длина куска в [`Segments::text`], байты; у глифа — ноль.
    len: u32,
    /// Вид куска.
    kind: SpanKind,
}

/// Вид куска текста фрагмента.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    /// Печатный текст: `start..start + len` общей строки.
    Text,
    /// Символ данных, который выглядел бы глифом: `‹` или `›`.
    ///
    /// Показывается как `‹x2039›` / `‹x203A›` — код символа в шестнадцатерицине.
    /// Такой вид не даёт глифу управления (у него `xHH` — ровно две цифры), поэтому
    /// символ данных нельзя принять за управляющий (FR-22).
    Escaped,
    /// Глиф управляющего байта: сам байт и сколько раз он подряд повторился.
    Ctrl(u8, u32),
}

/// Кусок текста фрагмента так, как он показывается в ленте.
///
/// Заимствованный вид [`Span`]: отрисовка берёт кусок из общей строки, ничего не
/// копируя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    /// Печатный текст.
    Text(&'a str),
    /// Глиф управляющего байта: сам байт и сколько раз он подряд повторился.
    Ctrl(u8, u32),
    /// Символ данных, который выглядел бы глифом: `‹` или `›`.
    Escaped(&'a str),
}

/// Текст фрагмента по кодировке: одна строка на фрагмент и компактные спаны в неё.
///
/// Декодированный текст фрагмента лежит одной строкой, а спаны только указывают в неё:
/// фрагмент на 2048 байт стоит одну строку и один `Vec` спанов, сколько бы кусков в
/// нём ни было (FR-28).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segments {
    text: String,
    spans: Vec<Span>,
}

impl Segments {
    /// Пустой фрагмент: у события ленты нет текста (служебная строка, комментарий).
    pub const EMPTY: Self = Self {
        text: String::new(),
        spans: Vec::new(),
    };

    fn new() -> Self {
        Self::default()
    }

    /// Декодированный текст фрагмента: общая строка всех текстовых спанов.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Спаны фрагмента в порядке показа.
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Куски фрагмента в порядке показа.
    pub fn iter(&self) -> impl Iterator<Item = Segment<'_>> {
        self.spans.iter().map(|span| self.view(span))
    }

    fn view(&self, span: &Span) -> Segment<'_> {
        let start = usize::try_from(span.start).unwrap_or(usize::MAX);
        let end = start + usize::try_from(span.len).unwrap_or(usize::MAX);
        let piece = self.text.get(start..end).unwrap_or_default();
        match span.kind {
            SpanKind::Text => Segment::Text(piece),
            SpanKind::Escaped => Segment::Escaped(piece),
            SpanKind::Ctrl(byte, count) => Segment::Ctrl(byte, count),
        }
    }

    /// Печатный кусок: дописывается в конец общей строки и удлиняет соседний текстовый
    /// спан, если тот кончается там же.
    fn push_plain(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let start = self.offset();
        self.text.push_str(text);
        let len = self.length(text.len());
        if let Some(last) = self.spans.last_mut()
            && last.kind == SpanKind::Text
            && last.start + last.len == start
        {
            last.len += len;
            return;
        }
        self.spans.push(Span {
            start,
            len,
            kind: SpanKind::Text,
        });
    }

    /// Глиф управляющего байта: подряд идущие одинаковые байты — один спан с повтором.
    fn push_ctrl(&mut self, byte: u8) {
        match self.spans.last_mut() {
            Some(Span {
                kind: SpanKind::Ctrl(last, count),
                ..
            }) if *last == byte => *count += 1,
            _ => {
                let start = self.offset();
                self.spans.push(Span {
                    start,
                    len: 0,
                    kind: SpanKind::Ctrl(byte, 1),
                });
            }
        }
    }

    /// Символ данных, похожий на глиф: сам символ лежит в общей строке, спан только
    /// указывает на него.
    fn push_escaped(&mut self, ch: char) {
        let start = self.offset();
        self.text.push(ch);
        let len = self.length(ch.len_utf8());
        self.spans.push(Span {
            start,
            len,
            kind: SpanKind::Escaped,
        });
    }

    /// Смещение в общей строке; фрагмент длиннее 4 ГиБ в ленту не попадает (у приёма
    /// свой предел буфера), но тип спана не должен от этого врать.
    fn offset(&self) -> u32 {
        u32::try_from(self.text.len()).unwrap_or(u32::MAX)
    }

    fn length(&self, len: usize) -> u32 {
        u32::try_from(len).unwrap_or(u32::MAX)
    }
}

/// Фрагмент обмена: направление, автоответ и исходные байты.
#[derive(Debug)]
pub struct DataPayload {
    pub dir: Dir,
    pub synthetic: bool,
    pub bytes: Vec<u8>,
    /// Текст по кодировке в порядке [`ENCODINGS`]: считается при первом показе.
    text: [OnceLock<Arc<Segments>>; 5],
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
    pub fn segments(&self, encoding: Encoding) -> Arc<Segments> {
        let slot = &self.text[encoding_index(encoding)];
        Arc::clone(
            slot.get_or_init(|| Arc::new(text_segments(&self.bytes, encoding, TEXT_LIMIT_BYTES))),
        )
    }

    /// Полный текст фрагмента по кодировке — без предела [`TEXT_LIMIT_BYTES`].
    ///
    /// Считается только по клику «Скопировать текст»: строка ленты показывает первые
    /// 2048 байт, а копировать инженеру нужно весь фрагмент (U1-AC16).
    pub fn full_text(&self, encoding: Encoding) -> String {
        plain_text(&text_segments(&self.bytes, encoding, usize::MAX))
    }
}

/// Что за событие ленты.
#[derive(Debug)]
pub enum FeedKind {
    Data(DataPayload),
    Open {
        /// Адрес принятой стороны.
        peer: String,
        /// Исходящая сторона пары прокси (FR-5); у остальных источников её нет.
        link: Option<bytetap_core::journal::Link>,
    },
    Close {
        by: ClosedBy,
        /// Причина из журнала: `eof`, `reset`, `timeout`, `stop`, `error`.
        reason: String,
    },
    Error {
        code: Option<String>,
        message: String,
    },
    Info {
        code: Option<String>,
        message: String,
        /// Сколько одинаковых ошибок источника схлопнулось в это замечание (формат `/2`).
        repeats: Option<u32>,
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
                    link: open.link.clone(),
                },
            ),
            Event::Close(close) => (
                &close.t,
                Some(close.mono_us),
                Some(close.conn),
                FeedKind::Close {
                    by: close.by,
                    reason: close.reason.clone(),
                },
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
                    repeats: info.repeats,
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

/// Глиф управляющего байта: `‹CR›`.
pub fn ctrl_glyph(byte: u8) -> String {
    format!("‹{}›", control_name(byte))
}

/// Символ данных, похожий на глиф управления, — как `‹x2039›`.
///
/// Глиф управления — `‹CR›` или `‹x0B›`: между стрелками имя, а не код символа.
/// Поэтому заменяем оба символа на вид с шестнадцатеричным кодом: `‹x2039›`.
pub fn escaped_glyph(ch: char) -> String {
    format!("‹x{:04X}›", ch as u32)
}

/// Печатный текст с экранированием глифов: `<` — как `‹x3C›`, `‹` и `›` — как
/// `‹x2039›` и `‹x203A›`.
///
/// Иначе текст прибора притворился бы управляющим символом (FR-22): `‹` в данных
/// иначе выглядел бы как начало глифа `‹CR›`.
fn push_text(out: &mut Segments, text: &str) {
    let mut rest = text;
    while let Some(at) = rest.find(['<', '\u{2039}', '\u{203a}']) {
        if at > 0 {
            out.push_plain(&rest[..at]);
        }
        let ch = rest[at..].chars().next().expect("a char is there");
        match ch {
            '<' => out.push_ctrl(b'<'),
            _ => out.push_escaped(ch),
        }
        rest = &rest[at + ch.len_utf8()..];
    }
    if !rest.is_empty() {
        out.push_plain(rest);
    }
}

/// Декодирует печатный кусок в общий буфер: буфер один на фрагмент, и строка на
/// каждый кусок не создаётся.
fn decode_into(run: &[u8], encoding: Encoding, out: &mut String) {
    let table = match encoding {
        // ASCII печатный: управляющие и старшие байты сюда не попадают.
        Encoding::Ascii => {
            out.extend(run.iter().map(|&b| char::from(b)));
            return;
        }
        Encoding::Cp1251 => encoding_rs::WINDOWS_1251,
        Encoding::Utf8 => encoding_rs::UTF_8,
        Encoding::Gbk => encoding_rs::GBK,
        // `iso-8859-1` по WHATWG — Windows-1252, как у `TextDecoder` Vue-версии.
        Encoding::Latin1 => encoding_rs::WINDOWS_1252,
    };
    // `decode_to_string` пишет только в запас ёмкости: место резервируется заранее,
    // и остаток куска дочитывается, если его не хватило.
    let mut decoder = table.new_decoder_without_bom_handling();
    let mut rest = run;
    loop {
        let needed = decoder
            .max_utf8_buffer_length(rest.len())
            .unwrap_or(rest.len() * 4 + 4);
        out.reserve(needed);
        let (result, read, _) = decoder.decode_to_string(rest, out, true);
        rest = &rest[read..];
        if result == encoding_rs::CoderResult::InputEmpty {
            break;
        }
    }
}

/// Текст фрагмента: управляющие байты `<0x20` и `0x7F` (в ASCII ещё и `≥0x80`) — глифы
/// `‹CR›`, печатные куски — декодированные выбранной кодировкой. Текст режется до
/// декодирования, поэтому управляющий байт внутри многобайтовой последовательности
/// никогда не попадёт в декодер. Длиннее `limit` байт — «…» в конце.
///
/// Подряд идущие обычные байты дают один спан: фрагмент из двух тысяч одинаковых
/// байт — это один кусок текста, а не две тысячи спанов (FR-22, FR-28).
pub fn text_segments(bytes: &[u8], encoding: Encoding, limit: usize) -> Segments {
    let end = bytes.len().min(limit);
    let mut out = Segments::new();
    // Буфер декодера один на фрагмент: он только принимает очередной печатный кусок.
    let mut decoded = String::new();
    let mut start = 0;
    for (index, &byte) in bytes[..end].iter().enumerate() {
        if is_control_byte(byte) || (encoding == Encoding::Ascii && byte >= 0x80) {
            if start < index {
                decoded.clear();
                decode_into(&bytes[start..index], encoding, &mut decoded);
                push_text(&mut out, &decoded);
            }
            out.push_ctrl(byte);
            start = index + 1;
        }
    }
    if start < end {
        decoded.clear();
        decode_into(&bytes[start..end], encoding, &mut decoded);
        push_text(&mut out, &decoded);
    }
    if bytes.len() > end {
        out.push_plain("…");
    }
    out
}

/// Текст сегмента так, как он показывается в ленте: глиф — `‹CR›`.
pub fn segment_text(segment: &Segment) -> String {
    match segment {
        Segment::Text(text) => (*text).to_owned(),
        Segment::Ctrl(byte, count) => ctrl_glyph(*byte).repeat(*count as usize),
        Segment::Escaped(text) => text.chars().next().map(escaped_glyph).unwrap_or_default(),
    }
}

/// Сколько символов текста фрагмента уходит в подпись строки для AccessKit.
pub const LABEL_CHARS: usize = 256;

/// Начало текста фрагмента для подписи строки: до `chars` символов, с «…» в конце.
///
/// Подпись строится каждый кадр для каждой видимой строки, поэтому повторы глифов не
/// разворачиваются целиком: у фрагмента из двух тысяч одинаковых управляющих байт в
/// подпись попадает столько глифов, сколько влезает в предел, а не все две тысячи
/// (U1-AC16).
///
/// Считаются символы показанного текста: у глифа их несколько (`‹x01›`), и глиф
/// укладывается целиком или не попадает вовсе.
pub fn preview(segments: &Segments, chars: usize) -> String {
    let mut out = String::new();
    let mut left = chars;
    'spans: for segment in segments.iter() {
        match segment {
            Segment::Text(text) => {
                for ch in text.chars() {
                    if left == 0 {
                        out.push('…');
                        break 'spans;
                    }
                    out.push(ch);
                    left -= 1;
                }
            }
            Segment::Escaped(text) => {
                let glyph = text.chars().next().map(escaped_glyph).unwrap_or_default();
                if !push_limited(&mut out, &glyph, &mut left) {
                    out.push('…');
                    break 'spans;
                }
            }
            Segment::Ctrl(byte, count) => {
                let glyph = ctrl_glyph(byte);
                let width = glyph.chars().count().max(1);
                let count = count as usize;
                let fits = (left / width).min(count);
                for _ in 0..fits {
                    out.push_str(&glyph);
                }
                left -= fits * width;
                if fits < count {
                    out.push('…');
                    break 'spans;
                }
            }
        }
    }
    out
}

/// Дописывает кусок, если он влезает в остаток предела; `false` — не влез.
fn push_limited(out: &mut String, piece: &str, left: &mut usize) -> bool {
    let width = piece.chars().count();
    if width > *left {
        return false;
    }
    *left -= width;
    out.push_str(piece);
    true
}

/// Строка текста целиком: для копирования из контекстного меню.
pub fn plain_text(segments: &Segments) -> String {
    let mut out = String::new();
    for segment in segments.iter() {
        match segment {
            Segment::Text(text) => out.push_str(text),
            Segment::Ctrl(byte, count) => {
                for _ in 0..count {
                    out.push_str(&ctrl_glyph(byte));
                }
            }
            Segment::Escaped(text) => {
                if let Some(ch) = text.chars().next() {
                    out.push_str(&escaped_glyph(ch));
                }
            }
        }
    }
    out
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

    /// Счётчик аллокаций потока: сколько раз код просил у системы память.
    ///
    /// Нужен, чтобы проверить главное в представлении текста фрагмента: куски — это
    /// спаны в одной строке, а не строки на каждый кусок. Счёт ведётся по потоку, и
    /// соседние тесты ему не мешают; `System` остаётся настоящим аллокатором.
    mod allocs {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            /// Сколько раз этот поток просил память.
            static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
            /// Считать ли: только внутри [`count`].
            static COUNTING: Cell<bool> = const { Cell::new(false) };
        }

        struct Counting;

        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                COUNTING.with(|on| {
                    if on.get() {
                        ALLOCATIONS.with(|count| count.set(count.get() + 1));
                    }
                });
                unsafe { System.alloc(layout) }
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                unsafe { System.dealloc(ptr, layout) }
            }

            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
                COUNTING.with(|on| {
                    if on.get() {
                        ALLOCATIONS.with(|count| count.set(count.get() + 1));
                    }
                });
                unsafe { System.realloc(ptr, layout, size) }
            }
        }

        #[global_allocator]
        static ALLOCATOR: Counting = Counting;

        /// Сколько раз поток просил память, пока работал `f`.
        pub fn count(f: impl FnOnce()) -> usize {
            ALLOCATIONS.with(|count| count.set(0));
            COUNTING.with(|on| on.set(true));
            f();
            COUNTING.with(|on| on.set(false));
            ALLOCATIONS.with(Cell::get)
        }
    }

    /// Тексты сегментов так, как они показываются в ленте.
    fn shown(segments: &Segments) -> Vec<(String, bool)> {
        segments
            .iter()
            .map(|segment| {
                let ctrl = !matches!(segment, Segment::Text(_));
                (segment_text(&segment), ctrl)
            })
            .collect()
    }

    #[test]
    fn control_bytes_become_glyphs_in_every_encoding() {
        let bytes = b"\x0bMSH|A\rB\x1c\r";
        for encoding in ENCODINGS {
            let segments = text_segments(bytes, encoding, TEXT_LIMIT_BYTES);
            assert_eq!(
                shown(&segments),
                [
                    ("‹VT›".to_owned(), true),
                    ("MSH|A".to_owned(), false),
                    ("‹CR›".to_owned(), true),
                    ("B".to_owned(), false),
                    ("‹FS›".to_owned(), true),
                    ("‹CR›".to_owned(), true),
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
            shown(&text_segments(&[b'a', 0xc3, b'b'], Encoding::Ascii, 100)),
            [
                ("a".to_owned(), false),
                ("‹xC3›".to_owned(), true),
                ("b".to_owned(), false)
            ]
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
            shown(&text_segments(b"a<CR>", Encoding::Cp1251, 100)),
            [
                ("a".to_owned(), false),
                ("‹x3C›".to_owned(), true),
                ("CR>".to_owned(), false)
            ]
        );
    }

    /// Символы данных, похожие на глиф, не выглядят как глиф (FR-22).
    #[test]
    fn a_data_bracket_is_not_mistaken_for_a_glyph() {
        let text = "a\u{2039}b\u{203a}c";
        let segments = text_segments(text.as_bytes(), Encoding::Utf8, 100);
        assert_eq!(
            shown(&segments),
            [
                ("a".to_owned(), false),
                ("‹x2039›".to_owned(), true),
                ("b".to_owned(), false),
                ("‹x203A›".to_owned(), true),
                ("c".to_owned(), false),
            ]
        );
        // В HEX-виде текст не считается: там байты показываются числами.
        assert_eq!(hex_line(b"\x0b", 0).ascii, ".");
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

    /// Фрагмент из одних управляющих байтов: один спан-повтор, без строк — имя
    /// глифа получается по байту при отрисовке (FR-28).
    #[test]
    fn a_control_only_fragment_has_no_text_allocations() {
        let bytes = vec![0x01u8; 2048];
        let segments = text_segments(&bytes, Encoding::Ascii, TEXT_LIMIT_BYTES);
        assert_eq!(segments.len(), 1, "одинаковые байты — один спан");
        assert_eq!(
            segments.spans()[0].kind,
            SpanKind::Ctrl(0x01, 2048),
            "с повтором"
        );
        assert!(segments.text().is_empty(), "у глифов нет текста");
        // Текст при этом полный: 2048 глифа.
        assert_eq!(
            plain_text(&segments).matches("‹x01›").count(),
            2048,
            "глиф на каждый байт"
        );
    }

    /// Подряд идущие обычные байты — один спан: 2048 байт дают один кусок.
    #[test]
    fn a_plain_fragment_is_one_segment() {
        let bytes = vec![b'x'; 2048];
        let segments = text_segments(&bytes, Encoding::Ascii, TEXT_LIMIT_BYTES);
        assert_eq!(segments.len(), 1, "подряд идущие байты — один спан");
        assert_eq!(segments.text(), "x".repeat(2048));
        assert_eq!(plain_text(&segments), "x".repeat(2048));
    }

    /// Смешанные данные: управляющий и печатный байт по очереди дают 2048 спанов, но
    /// текст фрагмента — одна строка, а не строка на спан (FR-28).
    ///
    /// Считаются аллокации потока: у строки на кусок их было бы больше тысячи, здесь —
    /// единицы на рост буфера строки и `Vec` спанов.
    #[test]
    fn a_mixed_fragment_keeps_one_string() {
        let bytes: Vec<u8> = (0..2048)
            .map(|index| if index % 2 == 0 { 0x01 } else { b'a' })
            .collect();
        let counting = allocs::count(|| {
            let segments = text_segments(&bytes, Encoding::Ascii, TEXT_LIMIT_BYTES);
            assert_eq!(segments.len(), 2048, "глиф и буква по очереди");
            assert_eq!(segments.text(), "a".repeat(1024), "одна строка фрагмента");
        });
        assert!(
            counting < 64,
            "одна строка и Vec спанов, а не строка на спан: {counting} аллокаций"
        );
        let segments = text_segments(&bytes, Encoding::Ascii, TEXT_LIMIT_BYTES);
        // Показ и копирование те же, что были у строк на каждый кусок.
        assert_eq!(
            plain_text(&segments),
            "‹x01›a".repeat(1024),
            "текст фрагмента"
        );
        assert_eq!(preview(&segments, 8), "‹x01›a…", "глиф не влез — «…»");
    }

    /// Подпись строки — начало фрагмента: повторы глифов не разворачиваются целиком.
    #[test]
    fn the_label_is_a_limited_prefix() {
        let bytes = vec![0x01u8; 2048];
        let segments = text_segments(&bytes, Encoding::Ascii, TEXT_LIMIT_BYTES);
        let label = preview(&segments, LABEL_CHARS);
        assert!(
            label.chars().count() <= LABEL_CHARS + 1,
            "не длиннее предела: {label}"
        );
        assert!(label.ends_with('…'), "сокращено: {label}");
        assert_eq!(label.matches("‹x01›").count(), LABEL_CHARS / 5);
        // Короткий фрагмент попадает в подпись целиком.
        let short = text_segments(b"AB\r", Encoding::Ascii, TEXT_LIMIT_BYTES);
        assert_eq!(preview(&short, LABEL_CHARS), "AB‹CR›");
    }

    /// Копирование отдаёт весь фрагмент, а не первые 2048 байт (U1-AC16).
    #[test]
    fn the_copied_text_is_the_whole_fragment() {
        let data = DataPayload::new(Dir::AnalyzerToLis, false, vec![b'x'; 3000]);
        assert_eq!(
            data.segments(Encoding::Ascii).len(),
            1,
            "в строке ленты — один кусок"
        );
        let full = data.full_text(Encoding::Ascii);
        assert_eq!(full.chars().count(), 3000, "весь фрагмент, без предела");
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
            link: None,
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
                by: ClosedBy::Analyzer,
                ..
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
