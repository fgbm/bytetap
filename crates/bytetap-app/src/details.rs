//! Панель деталей выбранного события ленты (L1, FR-26).
//!
//! Строка ленты показывает первые 2048 байт фрагмента, а инженеру нужен весь: панель снизу
//! открывает полный текст по кодировке ленты и полный HEX-дамп, в них работает выделение
//! произвольного фрагмента мышью, Ctrl+A и Ctrl+C. Панель — одна на запись и просмотр
//! журнала: состояние держит [`Details`], а экран говорит, какой номер события выбран.
//!
//! Событие хранится копией `Arc<FeedEvent>`: панель переживает вытеснение из кольца записи
//! и выгрузку страницы журнала (Л-В6), а полный текст считается один раз на событие и
//! кодировку — в кадре только раскладка уже готовых спанов.

use std::sync::Arc;

use eframe::egui::{
    self, Align, CursorIcon, Frame, Id, Key, Layout, Modifiers, Rangef, Rect, Response, Role,
    ScrollArea, Sense, Stroke, TextEdit, Ui, UiBuilder, WidgetInfo,
    text::{CCursorRange, LayoutJob, TextWrapping},
    vec2,
};

use crate::feed::{self, DataPayload, FeedEvent, FeedKind, Segments};
use crate::feedrows::{self, Pair, truncated};
use crate::icons::Icon;
use crate::settings::Encoding;
use crate::theme::tokens::{RADIUS_CHIP, SPACE_2, SPACE_3, TEXT_2XS, TEXT_DATA, TEXT_XS};
use crate::theme::{self, Palette, radius};
use crate::{t, widgets};

/// Высота шапки панели: вкладки, метаданные и действия.
const HEAD_HEIGHT: f32 = 36.0;
/// Высота строки подсказки или замечания внизу панели.
const FOOT_HEIGHT: f32 = 24.0;
/// Наименьшая высота панели: шапка, подсказка и три строки текста.
pub const PANEL_MIN: f32 = 140.0;
/// Высота панели по умолчанию — как на согласованном макете `feed-c-details`.
pub const PANEL_DEFAULT: f32 = 252.0;
/// Доля высоты ленты, больше которой панель не тянется.
const PANEL_MAX_SHARE: f32 = 0.7;

/// Вкладка панели.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    /// Полный текст фрагмента по кодировке ленты.
    #[default]
    Text,
    /// Полный HEX-дамп с шагом 16 байт.
    Hex,
}

/// Выделенные строки дампа: якорь и второй конец, оба включительно.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HexRange {
    anchor: usize,
    end: usize,
}

impl HexRange {
    fn between(anchor: usize, end: usize) -> Self {
        Self { anchor, end }
    }

    fn first(self) -> usize {
        self.anchor.min(self.end)
    }

    fn last(self) -> usize {
        self.anchor.max(self.end)
    }

    fn contains(self, line: usize) -> bool {
        (self.first()..=self.last()).contains(&line)
    }

    /// Расширить до `line`: второй конец двигается, якорь стоит.
    fn to(self, line: usize) -> Self {
        Self {
            anchor: self.anchor,
            end: line,
        }
    }
}

/// Раскладка полного текста: строится один раз на событие, кодировку, тему и ширину.
struct TextLayout {
    /// Событие и кодировка, для которых построена раскладка.
    key: (u64, Encoding),
    dark: bool,
    wrap: f32,
    text: Arc<str>,
    galley: Arc<egui::Galley>,
}

/// Выбранное событие и открытая панель: одно состояние на экран.
pub struct Details {
    /// Номер события в нумерации экрана: `seq` записи или индекс журнала.
    key: Option<u64>,
    /// Копия события: панель переживает вытеснение из кольца и выгрузку страницы.
    event: Option<Arc<FeedEvent>>,
    tab: Tab,
    /// Высота панели в точках: тянется разделителем и запоминается в настройках.
    height: f32,
    /// Высоту изменили разделителем — экран запишет её в настройки.
    height_changed: bool,
    /// Событие вытеснено из ленты: текст остался, а строки в ленте уже нет (Л-В6).
    evicted: bool,
    /// Выделенные строки дампа в HEX-вкладке.
    hex: Option<HexRange>,
    text: Option<TextLayout>,
}

impl Default for Details {
    fn default() -> Self {
        Self {
            key: None,
            event: None,
            tab: Tab::default(),
            height: PANEL_DEFAULT,
            height_changed: false,
            evicted: false,
            hex: None,
            text: None,
        }
    }
}

impl Details {
    /// Пустое состояние с высотой панели из настроек.
    pub fn new(height: f32) -> Self {
        Self {
            height: height.clamp(PANEL_MIN, 4_000.0),
            ..Self::default()
        }
    }

    pub fn is_open(&self) -> bool {
        self.event.is_some()
    }

    /// Номер выбранного события в нумерации экрана.
    pub fn key(&self) -> Option<u64> {
        self.key
    }

    pub fn event(&self) -> Option<&Arc<FeedEvent>> {
        self.event.as_ref()
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    /// Высота изменилась разделителем с прошлого кадра.
    pub fn take_height_changed(&mut self) -> bool {
        std::mem::take(&mut self.height_changed)
    }

    pub fn evicted(&self) -> bool {
        self.evicted
    }

    /// Высота панели из настроек, когда она закрыта: открытая панель её не перебивает.
    pub fn set_default_height(&mut self, height: f32) {
        if !self.is_open() {
            self.height = height.clamp(PANEL_MIN, 4_000.0);
        }
    }

    /// Открыть событие с номером `key`.
    pub fn open(&mut self, key: u64, event: Arc<FeedEvent>) {
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.event = Some(event);
        self.evicted = false;
        self.hex = None;
        self.text = None;
        // У простого события (открытие, закрытие, замечание) HEX-дампа нет — вкладка одна.
        if self
            .event
            .as_ref()
            .is_some_and(|event| event.data().is_none())
        {
            self.tab = Tab::Text;
        }
    }

    /// Закрыть панель и снять выбор.
    pub fn close(&mut self) {
        self.key = None;
        self.event = None;
        self.evicted = false;
        self.hex = None;
        self.text = None;
    }

    /// Событие вытеснено из кольцевой ленты: панель остаётся с прочитанным текстом.
    pub fn check_evicted(&mut self, first_seq: u64) {
        if self.key.is_some_and(|key| key < first_seq) {
            self.evicted = true;
        }
    }

    /// Раскладка полного текста события: пересобирается только при смене события,
    /// кодировки, темы или ширины — не каждый кадр.
    fn text_layout(
        &mut self,
        ui: &Ui,
        p: &Palette,
        key: u64,
        data: &DataPayload,
        encoding: Encoding,
        wrap: f32,
    ) -> &TextLayout {
        let dark = ui.visuals().dark_mode;
        let stale = self.text.as_ref().is_none_or(|cache| {
            cache.key != (key, encoding) || cache.dark != dark || (cache.wrap - wrap).abs() > 0.5
        });
        if stale {
            let text = data.shown_text(encoding);
            let segments = data.full_segments(encoding);
            let galley = ui.painter().layout_job(text_job(p, &segments, wrap));
            self.text = Some(TextLayout {
                key: (key, encoding),
                dark,
                wrap,
                text,
                galley,
            });
        }
        self.text.as_ref().expect("text layout is built")
    }
}

/// Текст фрагмента для панели: перенос по ширине, глифы управления цветом `ctrl`.
///
/// Перенос обязателен: горизонтальной прокрутки нет ни в панели, ни в ленте (решение
/// владельца), а строка HL7 без пробелов иначе ушла бы за правый край.
fn text_job(p: &Palette, segments: &Segments, wrap: f32) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap.max(1.0),
            max_rows: usize::MAX,
            break_anywhere: true,
            overflow_character: None,
        },
        ..Default::default()
    };
    let plain = egui::TextFormat {
        font_id: theme::mono(TEXT_DATA),
        color: p.text,
        ..Default::default()
    };
    let glyph = egui::TextFormat {
        font_id: theme::mono(TEXT_2XS),
        color: p.ctrl.gamma_multiply(0.55),
        ..Default::default()
    };
    let push = |job: &mut LayoutJob, piece: &str, format: egui::TextFormat| {
        let start = job.text.len();
        job.text.push_str(piece);
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(job.text.len()),
            format,
        });
    };
    for segment in segments.iter() {
        match segment {
            feed::Segment::Text(piece) => {
                if !piece.is_empty() {
                    push(&mut job, piece, plain.clone());
                }
            }
            feed::Segment::Ctrl(byte, count) => {
                push(
                    &mut job,
                    &feed::ctrl_glyph(byte).repeat(count as usize),
                    glyph.clone(),
                );
            }
            feed::Segment::Escaped(piece) => {
                let piece = piece
                    .chars()
                    .next()
                    .map(feed::escaped_glyph)
                    .unwrap_or_default();
                push(&mut job, &piece, glyph.clone());
            }
        }
    }
    job
}

/// Подпись события в шапке панели: «10:15:05.004 → #1 · ORU^R01 · 371 Б · CP1251».
///
/// Тип сообщения — только когда его видно просто (поле MSH-9 HL7); у ASTM и служебных
/// строк его нет, и подпись короче: время, номер и текст строки.
pub fn meta_line(event: &FeedEvent, pair: Pair, encoding: Encoding) -> String {
    let mut parts: Vec<String> = Vec::new();
    match &event.kind {
        FeedKind::Data(data) => {
            let mut lead = event.time.clone();
            if !lead.is_empty() {
                lead.push(' ');
            }
            lead.push_str(feedrows::glyph(data.dir));
            if let Some(conn) = event.conn {
                lead.push_str(&format!(" #{conn}"));
            }
            parts.push(lead);
            if let Some(kind) = feed::message_type(data) {
                parts.push(kind);
            }
            parts.push(crate::recording::format_bytes(data.bytes.len() as u64));
            parts.push(feed::encoding_label(encoding).to_owned());
        }
        _ => {
            let (_, text) = feedrows::row_text(event, pair);
            let mut lead = event.time.clone();
            if let Some(conn) = event.conn {
                if !lead.is_empty() {
                    lead.push(' ');
                }
                lead.push_str(&format!("#{conn}"));
            }
            if !lead.is_empty() {
                parts.push(lead);
            }
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    parts.join(" · ")
}

/// Что панель знает о выбранном событии: само событие, его номер, подпись для AccessKit,
/// метаданные, кодировка ленты и пара сторон.
struct Panel<'a> {
    event: &'a Arc<FeedEvent>,
    key: u64,
    meta: &'a str,
    title: &'a str,
    encoding: Encoding,
    pair: Pair,
}

impl Panel<'_> {
    fn data(&self) -> Option<&DataPayload> {
        self.event.data()
    }
}

/// Рисует панель, если событие выбрано; `true` — панель закрыли.
///
/// Экран зовёт панель до ленты: панель забирает высоту снизу, и лента получает остаток.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    details: &mut Details,
    encoding: Encoding,
    pair: Pair,
) -> bool {
    let Some(event) = details.event.clone() else {
        return false;
    };
    let Some(key) = details.key() else {
        return false;
    };
    let meta = meta_line(&event, pair, encoding);
    let title = t!("details.panel", meta = &meta);
    let available = ui.available_height();
    let max = (available * PANEL_MAX_SHARE).max(PANEL_MIN + 1.0);
    let default = details.height.clamp(PANEL_MIN, max);
    let mut height = default;
    let mut closed = false;
    egui::Panel::bottom("bt-details")
        .resizable(true)
        .show_separator_line(false)
        .frame(
            Frame::NONE
                .fill(p.surface_0)
                .inner_margin(egui::Margin::ZERO),
        )
        .default_size(default)
        .size_range(Rangef::new(PANEL_MIN, max))
        .show(ui, |ui| {
            // Панель занимает всю высоту, которую ей дали: без этого кадр панели
            // сжимается до содержимого, и разделитель не тянет ничего.
            ui.expand_to_include_rect(ui.max_rect());
            height = ui.max_rect().height();
            let panel = Panel {
                event: &event,
                key,
                meta: &meta,
                title: &title,
                encoding,
                pair,
            };
            closed = body(ui, p, details, &panel);
        });
    if (height - details.height).abs() > 0.5 {
        // Запоминается высота, которую выбрал инженер, а не та, что получилась от
        // уменьшения окна: разделитель тянут мышью.
        details.height_changed = ui.input(|input| input.pointer.any_down());
        details.height = height;
    }
    closed
}

/// Тело панели: шапка, содержимое вкладки и строка подсказки.
fn body(ui: &mut Ui, p: &Palette, details: &mut Details, panel: &Panel<'_>) -> bool {
    let Panel {
        key, meta, title, ..
    } = *panel;
    let rect = ui.max_rect();
    // Панель снизу: её верхняя граница — разделитель, за который тянут высоту.
    ui.painter()
        .hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, p.border));
    let data = panel.data();
    let mut closed = false;
    let mut chosen = None;
    {
        let head = Rect::from_min_size(rect.min, vec2(rect.width(), HEAD_HEIGHT));
        // Шапка — регион для AccessKit: диктор называет выбранное событие целиком. Регион
        // регистрируется до кнопок: иначе он лежит поверх них и забирает клики.
        let head_response = ui.interact(head, Id::unique(("bt-details-head", key)), Sense::hover());
        head_response.widget_info(|| WidgetInfo::labeled(Role::Region, ui.is_enabled(), title));
        let inner = Rect::from_min_max(
            egui::pos2(head.left() + SPACE_3, head.top()),
            egui::pos2(head.right() - SPACE_3, head.bottom()),
        );
        let mut left = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::left_to_right(Align::Center)),
        );
        left.spacing_mut().item_spacing.x = SPACE_3;
        if tab_button(
            &mut left,
            p,
            t!("details.tabText"),
            details.tab == Tab::Text,
        )
        .clicked()
        {
            chosen = Some(Tab::Text);
        }
        if data.is_some()
            && tab_button(&mut left, p, t!("details.tabHex"), details.tab == Tab::Hex).clicked()
        {
            chosen = Some(Tab::Hex);
        }
        // Действия справа: «Копировать» и «×».
        let right_width =
            widgets::chip_button_width(&left, true, t!("details.copy")) + SPACE_2 + 26.0;
        let meta_width = (left.available_width() - right_width - SPACE_3).max(0.0);
        let job = LayoutJob::single_section(
            meta.to_owned(),
            feedrows::text_format(theme::mono(TEXT_2XS), p.text_faint),
        );
        let galley = truncated(&left, job, meta_width);
        let (meta_rect, response) = left.allocate_exact_size(galley.size(), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, meta));
        left.painter().galley(meta_rect.min, galley, p.text_faint);
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    egui::pos2(inner.right() - right_width, inner.top()),
                    inner.max,
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        right.spacing_mut().item_spacing.x = SPACE_2;
        if widgets::chip_button(&mut right, p, Some(Icon::Copy), t!("details.copy"), true).clicked()
        {
            copy_active_tab(&right, details, panel);
        }
        if widgets::icon_button(&mut right, p, Icon::X, 14.0, t!("details.close")).clicked() {
            closed = true;
        }
        // Линия под шапкой: содержимое отделено от вкладок и метаданных.
        ui.painter().hline(
            head.x_range(),
            head.bottom() - 0.5,
            Stroke::new(1.0, p.border_faint),
        );
    }
    if let Some(tab) = chosen {
        details.tab = tab;
        details.hex = None;
        // С вкладки «Текст» уходим с фокусом в поле: иначе он остаётся у невидимого
        // поля и Ctrl+A/C на вкладке HEX не доходят до панели.
        ui.ctx()
            .memory_mut(|memory| memory.surrender_focus(text_id(key)));
    }
    shortcuts(ui, details, panel);

    let foot = Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - FOOT_HEIGHT),
        vec2(rect.width(), FOOT_HEIGHT),
    );
    let content = Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + HEAD_HEIGHT),
        egui::pos2(rect.right(), foot.top()),
    );
    let mut content_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(content.shrink2(vec2(SPACE_3, 0.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    content_ui.spacing_mut().item_spacing.y = 0.0;
    content_ui.visuals_mut().selection.bg_fill = p.selection_strong;
    match (details.tab, data) {
        (Tab::Hex, Some(data)) => hex_body(&mut content_ui, p, details, data),
        _ => text_body(&mut content_ui, p, details, panel),
    }
    // Подсказка внизу — как в макете; вытеснение из ленты её заменяет (Л-В6).
    let (text, color) = if details.evicted() {
        (t!("details.evicted"), p.warn)
    } else if details.tab == Tab::Hex {
        (t!("details.hintHex"), p.text_faint)
    } else {
        (t!("details.hintText"), p.text_faint)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::sans(TEXT_2XS), color);
    let response = ui.interact(foot, Id::unique(("bt-details-foot", key)), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    ui.painter().galley(
        egui::pos2(
            foot.left() + SPACE_3,
            foot.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    closed
}

/// Вкладка панели: подпись 12.5px, выбранная — цветом текста и линией снизу.
fn tab_button(ui: &mut Ui, p: &Palette, text: &str, on: bool) -> Response {
    let color = if on { p.text } else { p.text_muted };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::sans(TEXT_DATA), color);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 4.0, 28.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, text));
    let x = rect.center().x - galley.size().x / 2.0;
    ui.painter().galley(
        egui::pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if on {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 3.0,
            Stroke::new(2.0, p.text),
        );
    }
    widgets::focus_ring(ui, p, &response, rect, radius(RADIUS_CHIP));
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Вкладка «Текст»: полный фрагмент в поле только для чтения, с выделением мышью.
fn text_body(ui: &mut Ui, p: &Palette, details: &mut Details, panel: &Panel<'_>) {
    let key = panel.key;
    let encoding = panel.encoding;
    let Some(data) = panel.data() else {
        // Служебная строка: текста фрагмента нет, показываем строку ленты целиком.
        let (_, text) = feedrows::row_text(panel.event, panel.pair);
        let mut buffer: &str = &text;
        ui.add(
            TextEdit::multiline(&mut buffer)
                .id(text_id(key))
                .frame(Frame::NONE)
                .margin(egui::Margin::ZERO)
                .desired_width(f32::INFINITY)
                .text_color(p.text),
        );
        return;
    };
    ScrollArea::vertical()
        .id_salt("bt-details-text")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let width = ui.available_width().max(1.0);
            let text = details
                .text_layout(ui, p, key, data, encoding, width)
                .text
                .clone();
            let buffer: &str = &text;
            let mut layouter = |ui: &Ui, _text: &dyn egui::TextBuffer, wrap_width: f32| {
                details
                    .text_layout(ui, p, key, data, encoding, wrap_width)
                    .galley
                    .clone()
            };
            read_only(ui, p, key, buffer, &mut layouter);
        });
}

/// Поле только для чтения: выделение, Ctrl+A и Ctrl+C даёт сам `TextEdit`.
fn read_only(
    ui: &mut Ui,
    p: &Palette,
    key: u64,
    buffer: &str,
    layouter: &mut dyn FnMut(&Ui, &dyn egui::TextBuffer, f32) -> Arc<egui::Galley>,
) {
    let mut buffer = buffer;
    ui.add(
        TextEdit::multiline(&mut buffer)
            .id(text_id(key))
            .frame(Frame::NONE)
            .margin(egui::Margin::ZERO)
            .desired_width(f32::INFINITY)
            .text_color(p.text)
            .layouter(layouter),
    );
}

/// Вкладка «HEX»: полный дамп по 16 байт, строки выделяются кликом и Shift+кликом.
fn hex_body(ui: &mut Ui, p: &Palette, details: &mut Details, data: &DataPayload) {
    let lines = feed::hex_line_count(data.bytes.len());
    let bytes = data.bytes.as_slice();
    let mut clicked = None;
    let shift = ui.input(|i| i.modifiers.shift);
    ScrollArea::vertical()
        .id_salt("bt-details-hex")
        .auto_shrink([false, false])
        .show_rows(ui, feedrows::HEX_ROW_HEIGHT, lines, |ui, range| {
            for line in range {
                let selected = details.hex.is_some_and(|hex| hex.contains(line));
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), feedrows::HEX_ROW_HEIGHT),
                    Sense::click(),
                );
                let dump = feed::hex_line(bytes, line);
                if selected {
                    ui.painter().rect_filled(rect, 0, p.selection);
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 0, p.hover);
                }
                let name = format!(
                    "{} {} {}",
                    dump.offset,
                    dump.bytes
                        .iter()
                        .map(|(value, _)| value.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    dump.ascii
                );
                response.widget_info(|| WidgetInfo::selected(Role::Label, true, selected, &name));
                let font = theme::mono(TEXT_XS);
                let mut job = LayoutJob::default();
                job.append(
                    &dump.offset,
                    0.0,
                    feedrows::text_format(font.clone(), p.text_faint),
                );
                for (index, (value, control)) in dump.bytes.iter().enumerate() {
                    let format = if *control {
                        feedrows::text_format(theme::mono_medium(TEXT_XS), p.ctrl)
                    } else {
                        feedrows::text_format(font.clone(), p.text)
                    };
                    job.append(
                        value,
                        if index == 0 { SPACE_2 } else { 2.0 },
                        format.clone(),
                    );
                    job.append(" ", 0.0, format);
                }
                job.append(
                    &dump.ascii,
                    SPACE_2,
                    feedrows::text_format(font, p.text_muted),
                );
                let galley = truncated(ui, job, rect.width() - SPACE_2);
                let y = rect.center().y - galley.size().y / 2.0;
                ui.painter()
                    .galley(egui::pos2(rect.left(), y), galley, p.text);
                if response.clicked() {
                    clicked = Some((line, shift));
                }
            }
        });
    if let Some((line, extend)) = clicked {
        details.hex = Some(match (details.hex, extend) {
            (Some(hex), true) => hex.to(line),
            _ => HexRange::between(line, line),
        });
    }
}

/// Ctrl+A и Ctrl+C панели: в поле их ловит `TextEdit`, в HEX-дампе и без фокуса — панель.
fn shortcuts(ui: &Ui, details: &mut Details, panel: &Panel<'_>) {
    let key = panel.key;
    let encoding = panel.encoding;
    let data = panel.data();
    let in_field = ui.ctx().text_edit_focused() && details.tab == Tab::Text;
    if in_field {
        return;
    }
    let select_all = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A));
    let copy = ui.input(|i| i.events.contains(&egui::Event::Copy));
    match details.tab {
        Tab::Text => {
            let Some(data) = data else {
                return;
            };
            let text = data.shown_text(encoding);
            if select_all {
                select_all_text(ui, text_id(key), &text);
            }
            if copy {
                let text =
                    selected_text(ui, text_id(key), &text).unwrap_or_else(|| text.to_string());
                ui.ctx().copy_text(text);
            }
        }
        Tab::Hex => {
            let Some(data) = data else {
                return;
            };
            let lines = feed::hex_line_count(data.bytes.len());
            if select_all {
                details.hex = Some(HexRange::between(0, lines.saturating_sub(1)));
            }
            if copy {
                let (from, to) = details.hex.map_or((0, lines.saturating_sub(1)), |hex| {
                    (hex.first(), hex.last())
                });
                ui.ctx().copy_text(hex_lines_text(&data.bytes, from, to));
            }
        }
    }
}

/// «Копировать» активной вкладки: текст целиком, выделение в нём или строки дампа.
fn copy_active_tab(ui: &Ui, details: &Details, panel: &Panel<'_>) {
    let Some(data) = panel.data() else {
        return;
    };
    let key = panel.key;
    let encoding = panel.encoding;
    let text = match details.tab {
        Tab::Text => {
            let shown = data.shown_text(encoding);
            selected_text(ui, text_id(key), &shown).unwrap_or_else(|| shown.to_string())
        }
        Tab::Hex => {
            let lines = feed::hex_line_count(data.bytes.len());
            let (from, to) = details.hex.map_or((0, lines.saturating_sub(1)), |hex| {
                (hex.first(), hex.last())
            });
            hex_lines_text(&data.bytes, from, to)
        }
    };
    if !text.is_empty() {
        ui.ctx().copy_text(text);
    }
}

/// Где стоит выделение в поле панели: выделенный текст, если оно есть.
fn selected_text(ui: &Ui, id: Id, text: &str) -> Option<String> {
    let state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), id)?;
    let range = state.cursor.char_range()?;
    let range = range.as_sorted_char_range();
    let from = range.start.0;
    let to = range.end.0.min(text.chars().count());
    (to > from).then(|| text.chars().skip(from).take(to - from).collect())
}

/// Выделить весь текст в поле и отдать ему фокус: дальше Ctrl+C ловит `TextEdit`.
fn select_all_text(ui: &Ui, id: Id, text: &str) {
    ui.ctx().memory_mut(|memory| memory.request_focus(id));
    let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::select_all_str(text)));
    state.store(ui.ctx(), id);
}

/// Идентификатор поля панели: свой у каждого события, чтобы выделение не переезжало
/// со старого фрагмента на новый.
fn text_id(key: u64) -> Id {
    Id::unique(("bt-details-field", key))
}

/// Строки дампа так, как они видны: смещение, 16 байт и ASCII-колонка.
pub fn hex_lines_text(bytes: &[u8], from: usize, to: usize) -> String {
    let last = feed::hex_line_count(bytes.len()).saturating_sub(1);
    let from = from.min(last);
    let to = to.min(last).max(from);
    let mut out = String::new();
    for line in from..=to {
        if !out.is_empty() {
            out.push('\n');
        }
        let dump = feed::hex_line(bytes, line);
        out.push_str(&dump.offset);
        out.push(' ');
        for (index, (value, _)) in dump.bytes.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push_str(value);
        }
        out.push_str("  ");
        out.push_str(&dump.ascii);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytetap_core::journal::Dir;

    fn data_event(bytes: &[u8], conn: u32) -> FeedEvent {
        FeedEvent {
            time: "10:15:05.004".into(),
            mono_us: Some(0),
            conn: Some(conn),
            kind: FeedKind::Data(DataPayload::new(Dir::AnalyzerToLis, false, bytes.to_vec())),
        }
    }

    /// Подпись в шапке панели: время, направление, номер, тип HL7, размер и кодировка.
    #[test]
    fn the_meta_line_names_the_event() {
        let event = data_event(
            b"\x0bMSH|^~\\&|ANALYZER|LAB|LIS||20261019101500||ORU^R01|MSG1|P|2.3.1\r\x1c\r",
            1,
        );
        let meta = meta_line(&event, Pair::None, Encoding::Cp1251);
        assert!(meta.starts_with("10:15:05.004 → #1 · ORU^R01 · "), "{meta}");
        // Размер — из общего формата шапки: число, неразрывный пробел и «Б».
        assert!(meta.ends_with("\u{a0}Б · CP1251"), "{meta}");
        // У ASTM типа сообщения нет — подпись короче, но размер и кодировка на месте.
        let astm = data_event(b"\x05H|\\^&|ANALYZER", 2);
        let meta = meta_line(&astm, Pair::None, Encoding::Utf8);
        assert!(meta.starts_with("10:15:05.004 → #2 · "), "{meta}");
        assert!(meta.ends_with("\u{a0}Б · UTF-8"), "{meta}");
        assert!(!meta.contains("ORU"), "{meta}");
        // Служебная строка: время и текст строки, без размера и кодировки.
        let open = FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: None,
            conn: Some(1),
            kind: FeedKind::Open {
                peer: "192.168.1.51:49152".into(),
                link: None,
            },
        };
        assert_eq!(
            meta_line(&open, Pair::None, Encoding::Cp1251),
            "10:15:03.000 #1 · #1 подключился 192.168.1.51:49152"
        );
    }

    /// Дамп копируется строками, как виден; концы диапазона не выходят за фрагмент.
    #[test]
    fn the_dump_lines_copy_as_shown() {
        let bytes: Vec<u8> = (0..20).collect();
        assert_eq!(
            hex_lines_text(&bytes, 0, 0),
            "0000 00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F  ................"
        );
        assert_eq!(hex_lines_text(&bytes, 1, 1), "0010 10 11 12 13  ....");
        assert_eq!(
            hex_lines_text(&bytes, 1, 9),
            hex_lines_text(&bytes, 1, 1),
            "за последней строкой — ничего лишнего"
        );
        assert_eq!(hex_lines_text(b"", 0, 0), "0000   ", "пустая строка дампа");
    }

    /// Кольцо вытеснило выбранное событие: панель остаётся с прочитанным текстом.
    #[test]
    fn an_evicted_event_keeps_the_panel() {
        let mut details = Details::new(PANEL_DEFAULT);
        let event = Arc::new(data_event(b"A\r", 1));
        details.open(7, Arc::clone(&event));
        assert!(details.is_open());
        assert!(!details.evicted());
        details.check_evicted(3);
        assert!(!details.evicted(), "7 ещё в ленте");
        details.check_evicted(8);
        assert!(details.evicted(), "7 вытеснено");
        assert_eq!(
            details.event().map(|event| event.time.as_str()),
            Some("10:15:05.004"),
            "копия события осталась"
        );
        details.close();
        assert!(!details.is_open());
        assert!(!details.evicted());
    }

    /// Открытие нового события сбрасывает вкладку и выделение дампа; повторный клик по
    /// той же строке панель не закрывает.
    #[test]
    fn opening_another_event_resets_the_tab_and_keeps_the_same_one() {
        let mut details = Details::new(PANEL_DEFAULT);
        details.open(1, Arc::new(data_event(b"AB", 1)));
        details.tab = Tab::Hex;
        details.hex = Some(HexRange::between(0, 0));
        details.open(1, Arc::new(data_event(b"AB", 1)));
        assert_eq!(details.tab(), Tab::Hex, "то же событие — состояние стоит");
        details.open(2, Arc::new(data_event(b"AB", 1)));
        assert_eq!(details.tab(), Tab::Hex, "вкладка переживает смену события");
        assert_eq!(details.key(), Some(2));
        assert_eq!(details.hex, None, "выделение дампа сброшено");
        // У служебного события HEX-вкладки нет: вкладка остаётся «Текст».
        details.tab = Tab::Hex;
        let open = FeedEvent {
            time: String::new(),
            mono_us: None,
            conn: Some(1),
            kind: FeedKind::Note {
                text: "проба".into(),
            },
        };
        details.open(3, Arc::new(open));
        assert_eq!(details.tab(), Tab::Text);
    }

    /// Выделение строк дампа: Shift+клик расширяет от якоря, новый клик ставит якорь.
    #[test]
    fn the_dump_selection_extends_from_the_anchor() {
        let mut hex = HexRange::between(2, 2);
        assert!(hex.contains(2));
        hex = hex.to(5);
        assert_eq!((hex.first(), hex.last()), (2, 5));
        assert!(hex.contains(4));
        assert!(!hex.contains(6));
        hex = HexRange::between(9, 7);
        assert_eq!((hex.first(), hex.last()), (7, 9));
        assert!(hex.contains(8));
    }

    /// Раскладка панели — тот же текст, что копируется: глифы `‹CR›` на месте, кусков
    /// столько же, сколько спанов события.
    #[test]
    fn the_panel_text_keeps_every_glyph() {
        let palette = crate::theme::LIGHT;
        let data = DataPayload::new(
            Dir::AnalyzerToLis,
            false,
            "\x0bMSH|A\rB\x1c\r".as_bytes().to_vec(),
        );
        let text = data.shown_text(Encoding::Cp1251);
        let segments = data.full_segments(Encoding::Cp1251);
        let job = text_job(&palette, &segments, 400.0);
        assert_eq!(job.text, *text);
        assert_eq!(job.sections.len(), segments.len());
        // Куски покрывают весь текст без дыр и наложений.
        let mut end = 0;
        for section in &job.sections {
            assert_eq!(section.byte_range.start.0, end);
            end = section.byte_range.end.0;
        }
        assert_eq!(end, job.text.len());
    }
}
