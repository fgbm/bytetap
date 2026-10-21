//! Строки ленты: вид «Текст» и «HEX» — общие для записи и просмотра журнала.
//!
//! Отрисовка строки одна и та же при записи и при просмотре: та же колонка времени,
//! тот же глиф стрелки, тот же `‹CR›` цветом `ctrl`, тот же контекстный меню строки.
//! Различается только источник событий, поэтому рисование и вынесено отдельно от
//! экранов.

use std::sync::Arc;

use bytetap_core::journal::{ClosedBy, Dir};
use eframe::egui::{
    self, Align, Align2, Color32, Rect, Role, Sense, Stroke, TextFormat, Ui, WidgetInfo, pos2,
    text::LayoutJob, text::TextWrapping, vec2,
};

use crate::feed::{self, DataPayload, FeedEvent, FeedKind, HexLine, Segment};
use crate::settings::Encoding;
use crate::theme::tokens::{SPACE_2, SPACE_3, TEXT_2XS, TEXT_DATA, TEXT_XS};
use crate::theme::{self, Palette};
use crate::{t, widgets};

/// Высота строки ленты в виде «Текст».
pub const TEXT_ROW_HEIGHT: f32 = 28.0;
/// Высота строки HEX-дампа.
pub const HEX_ROW_HEIGHT: f32 = 25.0;
/// Колонка времени.
pub const TIME_WIDTH: f32 = 94.0;
/// Колонка глифа направления.
pub const ARROW_WIDTH: f32 = 12.0;
/// Колонка номера соединения.
pub const CONN_WIDTH: f32 = 24.0;

/// Вид строки: цвет текста и шрифт служебных строк — как классы `svc`, `ev`, `err`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowClass {
    /// Фрагмент обмена: цвет направления.
    Data(Dir),
    /// Служебная строка `open`/`close`.
    Service,
    /// Событие `info` или комментарий.
    Event,
    /// Ошибка.
    Error,
}

/// Текст служебной строки (у фрагмента текст идёт сегментами).
pub fn row_text(event: &FeedEvent) -> (RowClass, String) {
    let conn = event.conn.unwrap_or_default();
    match &event.kind {
        FeedKind::Data(data) => (RowClass::Data(data.dir), String::new()),
        FeedKind::Open { peer } => (
            RowClass::Service,
            t!("feed.opened", conn = conn, peer = peer),
        ),
        FeedKind::Close { by } => (
            RowClass::Service,
            t!("feed.closedSimple", conn = conn, by = closed_by(*by)),
        ),
        FeedKind::Error { code, message } => {
            (RowClass::Error, event_text(code.as_deref(), message))
        }
        FeedKind::Info { code, message } => (RowClass::Event, event_text(code.as_deref(), message)),
        FeedKind::Note { text } => (RowClass::Event, text.clone()),
    }
}

/// Кто закрыл соединение: анализатор, ЛИС или приложение.
pub fn closed_by(by: ClosedBy) -> &'static str {
    match by {
        ClosedBy::Analyzer => t!("feed.byAnalyzer"),
        ClosedBy::Lis => t!("feed.byLis"),
        ClosedBy::Bytetap => t!("feed.byApp"),
    }
}

/// Текст события `error`/`info` по коду; неизвестный код — английский `message` журнала.
pub fn event_text(code: Option<&str>, message: &str) -> String {
    let text = match code {
        Some("accept_failed") => t!("events.accept_failed"),
        Some("connect_failed") => t!("events.connect_failed"),
        Some("serial_open_failed") => t!("events.serial_open_failed"),
        Some("connection_read_failed") => t!("events.connection_read_failed"),
        Some("connection_write_failed") => t!("events.connection_write_failed"),
        Some("answer_timeout") => t!("events.answer_timeout"),
        Some("journal_failure") => t!("events.journal_failure"),
        Some("protocol_undetected") => t!("events.protocol_undetected"),
        _ => message,
    };
    text.to_owned()
}

/// Глиф направления в строке.
pub fn glyph(dir: Dir) -> &'static str {
    match dir {
        Dir::AnalyzerToLis => "→",
        Dir::LisToAnalyzer => "←",
    }
}

/// Цвет направления в строке.
pub fn dir_color(p: &Palette, dir: Dir) -> Color32 {
    match dir {
        Dir::AnalyzerToLis => p.dir_a2l,
        Dir::LisToAnalyzer => p.dir_l2a,
    }
}

/// Формат текста в галлере: шрифт, цвет и выравнивание по середине строки.
pub fn text_format(font: egui::FontId, color: Color32) -> TextFormat {
    TextFormat {
        font_id: font,
        color,
        valign: Align::Center,
        ..Default::default()
    }
}

/// Текст в одну строку, обрезанный «…» по ширине.
pub fn truncated(ui: &Ui, mut job: LayoutJob, width: f32) -> Arc<egui::Galley> {
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    ui.painter().layout_job(job)
}

/// Строка ленты во всю ширину: наведение — фоном, роль `label` с текстом строки.
fn row_frame(ui: &mut Ui, p: &Palette, height: f32, name: &str) -> (Rect, egui::Response) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
    if response.hovered() {
        ui.painter().rect_filled(rect, 0, p.hover);
    }
    (rect, response)
}

/// Текст фрагмента одной строкой с глифами `‹CR›` и «автоответ».
fn data_job(p: &Palette, segments: &[Segment], synthetic: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let ctrl = p.ctrl.gamma_multiply(0.55);
    for segment in segments {
        let fmt = if segment.ctrl {
            text_format(theme::mono(TEXT_2XS), ctrl)
        } else {
            text_format(theme::mono(TEXT_DATA), p.text)
        };
        job.append(&segment.text, 0.0, fmt);
    }
    if synthetic {
        job.append(
            t!("feed.synthetic"),
            6.0,
            text_format(theme::sans(TEXT_2XS), p.dir_l2a),
        );
    }
    job
}

/// Строка «Текст»: время · стрелка · №соединения · данные · «автоответ».
pub fn text_row(ui: &mut Ui, p: &Palette, event: &FeedEvent, multi: bool, encoding: Encoding) {
    let (class, text) = row_text(event);
    let segments = event.data().map(|data| data.segments(encoding));
    let body = segments
        .as_deref()
        .map_or_else(|| text.clone(), feed::plain_text);
    let name = match (class, event.conn) {
        (RowClass::Data(dir), Some(conn)) if multi => {
            format!("{} {} #{conn} {body}", event.time, glyph(dir))
        }
        (RowClass::Data(dir), _) => format!("{} {} {body}", event.time, glyph(dir)),
        _ => format!("{} {body}", event.time),
    };
    let (rect, response) = row_frame(ui, p, TEXT_ROW_HEIGHT, &name);
    let painter = ui.painter().clone();
    let cy = rect.center().y;
    let x0 = rect.left() + SPACE_3;
    let right = rect.right() - SPACE_3;
    let service = class == RowClass::Service;
    let size = if service { TEXT_XS } else { TEXT_DATA };
    painter.text(
        pos2(x0, cy),
        Align2::LEFT_CENTER,
        &event.time,
        theme::mono(size),
        p.text_faint,
    );
    let mut x = x0 + TIME_WIDTH + SPACE_2;
    match (class, event.data()) {
        (RowClass::Data(dir), Some(data)) => {
            painter.text(
                pos2(x, cy),
                Align2::LEFT_CENTER,
                glyph(dir),
                theme::mono_medium(TEXT_DATA),
                dir_color(p, dir),
            );
            x += ARROW_WIDTH + SPACE_2;
            if multi {
                painter.text(
                    pos2(x, cy),
                    Align2::LEFT_CENTER,
                    format!("#{}", event.conn.unwrap_or_default()),
                    theme::mono(TEXT_2XS),
                    p.text_faint,
                );
                x += CONN_WIDTH + SPACE_2;
            }
            let job = data_job(p, segments.as_deref().unwrap_or_default(), data.synthetic);
            let galley = truncated(ui, job, right - x);
            painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, p.text);
        }
        _ => {
            let color = match class {
                RowClass::Error => p.danger,
                _ => p.text_muted,
            };
            let job =
                LayoutJob::single_section(text.clone(), text_format(theme::mono(size), color));
            let galley = truncated(ui, job, right - x);
            let end = x + galley.size().x;
            painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, color);
            // Служебная строка: линия начинается за текстом и уходит до края.
            if service && end + SPACE_2 + 12.0 <= right {
                painter.hline(end + SPACE_2..=right, cy, Stroke::new(1.0, p.border_faint));
            }
        }
    }
    row_menu(&response, p, event, &body);
}

/// Строка HEX-дампа: время и стрелка только у первой строки события.
pub fn hex_row(
    ui: &mut Ui,
    p: &Palette,
    event: &FeedEvent,
    line: usize,
    multi: bool,
    encoding: Encoding,
) {
    let (class, text) = row_text(event);
    let bytes = event.data().map_or(&[][..], |data| data.bytes.as_slice());
    let dump: HexLine = feed::hex_line(bytes, line);
    let hex: Vec<&str> = dump.bytes.iter().map(|(value, _)| value.as_str()).collect();
    let name = format!(
        "{} {} {} {}",
        if line == 0 { event.time.as_str() } else { "" },
        dump.offset,
        hex.join(" "),
        dump.ascii
    );
    let (rect, response) = row_frame(ui, p, HEX_ROW_HEIGHT, name.trim());
    let painter = ui.painter().clone();
    let font = theme::mono(TEXT_XS);
    let y = rect.top() + 3.0;
    let x0 = rect.left() + SPACE_3;
    let first = line == 0;
    if first {
        painter.text(
            pos2(x0, y),
            Align2::LEFT_TOP,
            &event.time,
            font.clone(),
            p.text_faint,
        );
    }
    let mut x = x0 + TIME_WIDTH + SPACE_2;
    if first && let RowClass::Data(dir) = class {
        painter.text(
            pos2(x, y),
            Align2::LEFT_TOP,
            glyph(dir),
            theme::mono_medium(TEXT_XS),
            dir_color(p, dir),
        );
    }
    x += ARROW_WIDTH + SPACE_2;
    if multi {
        if first && let Some(conn) = event.conn {
            painter.text(
                pos2(x, y),
                Align2::LEFT_TOP,
                format!("#{conn}"),
                font.clone(),
                p.text_faint,
            );
        }
        x += CONN_WIDTH + SPACE_2;
    }
    let (byte_color, ascii_color) = match class {
        RowClass::Data(_) => (p.text, p.text_muted),
        RowClass::Error => (p.danger, p.danger),
        RowClass::Event | RowClass::Service => (p.text_muted, p.text_muted),
    };
    let mut job = LayoutJob::default();
    job.append(&dump.offset, 0.0, text_format(font.clone(), p.text_faint));
    for (index, (value, control)) in dump.bytes.iter().enumerate() {
        let fmt = if *control && matches!(class, RowClass::Data(_)) {
            text_format(theme::mono_medium(TEXT_XS), p.ctrl)
        } else {
            text_format(font.clone(), byte_color)
        };
        job.append(value, if index == 0 { SPACE_2 } else { 2.0 }, fmt.clone());
        job.append(" ", 0.0, fmt);
    }
    job.append(&dump.ascii, SPACE_2, text_format(font, ascii_color));
    let galley = truncated(ui, job, rect.right() - SPACE_3 - x);
    painter.galley(pos2(x, y), galley, p.text);
    let copy_text = event
        .data()
        .map_or(text, |data| feed::plain_text(&data.segments(encoding)));
    row_menu(&response, p, event, &copy_text);
}

/// Контекстное меню строки: текст, HEX и Base64 фрагмента (U1-AC16).
fn row_menu(response: &egui::Response, p: &Palette, event: &FeedEvent, text: &str) {
    response.context_menu(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        if widgets::menu_item(ui, p, t!("feed.copyText"), true).clicked() {
            ui.ctx().copy_text(text.to_owned());
        }
        if let Some(data) = event.data() {
            if widgets::menu_item(ui, p, t!("feed.copyHex"), true).clicked() {
                ui.ctx().copy_text(feed::hex_string(&data.bytes));
            }
            if widgets::menu_item(ui, p, t!("feed.copyBase64"), true).clicked() {
                ui.ctx().copy_text(feed::base64_string(&data.bytes));
            }
        }
    });
}

/// Событие ленты из строки журнала: `None` — заголовок `session` или итоги.
pub fn event_of(event: &bytetap_core::journal::Event) -> Option<FeedEvent> {
    FeedEvent::from_session(event)
}

/// Байты фрагмента для меню копирования.
pub fn payload_bytes(data: &DataPayload) -> &[u8] {
    &data.bytes
}
