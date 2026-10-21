//! Экран просмотра журнала: шапка, строка о персональных данных, карта сессии и
//! лента по страницам (FR-29, FR-34, U1-AC18, U1-AC19).
//!
//! Карта сессии — плотность всей сессии на 600 корзин с рамкой видимой области
//! ленты; клик и перетаскивание переводят ленту к месту. Лента — та же, что при
//! записи: те же строки, тот же HEX-вид, те же кодировки, но источник — страницы
//! журнала по 500 событий.

use eframe::egui::{
    self, Align, Align2, CursorIcon, Frame, Id, Layout, Popup, Rect, RectAlign, Role, Sense,
    Stroke, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};

use bytetap_core::journal::view::DENSITY_BUCKETS;

use crate::app::BytetapApp;
use crate::feedrows::{HEX_ROW_HEIGHT, TEXT_ROW_HEIGHT, error_row, hex_row, pending_row, text_row};
use crate::icons::{self, Icon};
use crate::settings::{Encoding, FeedView};
use crate::theme::tokens::{RADIUS_CHIP, SPACE_2, SPACE_3, TEXT_2XS, TEXT_XS};
use crate::theme::{self, Palette, radius};
use crate::viewing::{NOTES_SHOWN, density_bars, duration, file_name, notes_line};
use crate::{t, widgets};

/// Высота панели над лентой — как при записи.
const BAR_HEIGHT: f32 = 32.0;

/// Высота дорожки карты сессии.
const LANE_HEIGHT: f32 = 22.0;
/// Отступы карты сверху и снизу.
const PAD_TOP: f32 = 6.0;
const PAD_BOTTOM: f32 = 8.0;
/// Ширина подписи дорожки.
const LABEL_WIDTH: f32 = 16.0;

/// Шапка просмотра журнала: бренд, «×», имя файла, метаданные, комментарий, тема.
pub fn title_bar(app: &mut BytetapApp, ui: &mut Ui) {
    let (name, meta) = match &app.viewing {
        Some(view) => {
            let name = file_name(&view.path);
            let analyzer = view
                .summary
                .header
                .analyzer
                .clone()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| t!("journal.notSpecified").to_owned());
            let meta = format!("{analyzer} · {}", duration(view.summary.duration_us));
            (name, meta)
        }
        None => (String::new(), String::new()),
    };
    app.title_bar(
        ui,
        |app, ui| {
            app.brand(ui);
            let p = app.pal();
            if widgets::icon_button(ui, &p, Icon::X, 16.0, t!("journal.close")).clicked() {
                app.close_journal();
            }
            // Имя файла моноширинным, метаданные — приглушённым.
            let mut job = egui::text::LayoutJob::simple_singleline(
                name.clone(),
                theme::mono(TEXT_XS),
                p.text,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(320.0);
            let galley = ui.painter().layout_job(job);
            let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
            ui.painter().galley(rect.min, galley, p.text);
            ui.label(
                egui::RichText::new(format!("· {meta}"))
                    .size(TEXT_XS)
                    .color(p.text_muted),
            );
        },
        |app, ui| {
            let p = app.pal();
            if widgets::link_button(ui, &p, Icon::Pen, 15.0, t!("journal.comment"), false).clicked()
            {
                app.open_journal_note();
            }
            let (icon, label) = if app.dark() {
                (Icon::Sun, t!("theme.toLight"))
            } else {
                (Icon::Moon, t!("theme.toDark"))
            };
            if widgets::icon_button(ui, &p, icon, 16.0, label).clicked() {
                app.toggle_theme(ui.ctx());
            }
        },
    );
}

/// Строка под шапкой: предупреждение о персональных данных и комментарии журнала.
pub fn notice(app: &mut BytetapApp, ui: &mut Ui) {
    let Some(view) = &app.viewing else {
        return;
    };
    let p = app.pal();
    let header_note = view.summary.header.note.clone().unwrap_or_default();
    let appended = view.source.notes(NOTES_SHOWN);
    let notes = notes_line(&header_note, &appended);
    let text = t!("journal.personalData");
    Frame::new()
        .inner_margin(egui::Margin::symmetric(SPACE_3 as i8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACE_2;
                let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
                icons::paint(ui, Icon::TriangleAlert, rect.center(), 15.0, p.dir_l2a);
                let label = ui.label(egui::RichText::new(text).size(TEXT_XS).color(p.text_muted));
                label.widget_info(|| WidgetInfo::labeled(Role::Note, true, text));
                if !notes.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let note = format!("«{notes}»");
                        ui.label(egui::RichText::new(note).size(TEXT_XS).color(p.text_faint));
                    });
                }
            });
        });
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        line.x_range(),
        line.center().y,
        Stroke::new(1.0, p.border_faint),
    );
}

/// Ошибка открытия журнала: строка цветом `danger` под шапкой.
pub fn error(app: &BytetapApp, ui: &mut Ui) {
    let Some(error) = &app.view_error else {
        return;
    };
    let p = app.pal();
    let text = error.clone();
    Frame::new()
        .inner_margin(egui::Margin::symmetric(SPACE_3 as i8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACE_2;
                let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
                icons::paint(ui, Icon::CircleAlert, rect.center(), 15.0, p.danger);
                let label = ui.label(egui::RichText::new(&text).size(TEXT_XS).color(p.danger));
                label.widget_info(|| WidgetInfo::labeled(Role::Alert, true, &text));
            });
        });
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        line.x_range(),
        line.center().y,
        Stroke::new(1.0, p.border_faint),
    );
}

/// Карта сессии: плотность всей сессии, рамка видимой области и переход к месту.
pub fn map(app: &mut BytetapApp, ui: &mut Ui) {
    let p = app.pal();
    let height = PAD_TOP + 2.0 * LANE_HEIGHT + PAD_BOTTOM;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0, p.trace_bg);
    let left = rect.left() + SPACE_3 + LABEL_WIDTH + SPACE_2;
    let width = (rect.right() - SPACE_3 - left).max(0.0);

    let (a2l, l2a, visible) = match &app.viewing {
        Some(view) => (
            density_bars(&view.summary.density.a2l),
            density_bars(&view.summary.density.l2a),
            view.visible,
        ),
        None => (Vec::new(), Vec::new(), None),
    };
    for (lane, (bars, glyph, color, title)) in [
        (a2l, "→", p.dir_a2l, t!("trace.a2l")),
        (l2a, "←", p.dir_l2a, t!("trace.l2a")),
    ]
    .into_iter()
    .enumerate()
    {
        let top = rect.top() + PAD_TOP + LANE_HEIGHT * lane as f32;
        let label = Rect::from_min_max(
            pos2(rect.left() + SPACE_3, top),
            pos2(rect.left() + SPACE_3 + LABEL_WIDTH, top + LANE_HEIGHT),
        );
        let response = ui.interact(label, Id::unique(("bt-journal-lane", lane)), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, title));
        painter.text(
            pos2(
                rect.left() + SPACE_3 + LABEL_WIDTH / 2.0,
                top + LANE_HEIGHT / 2.0,
            ),
            Align2::CENTER_CENTER,
            glyph,
            theme::mono(TEXT_2XS),
            p.text_muted,
        );
        // Плотность — столбик на корзину: 600 корзин на всю сессию.
        let slot = width / DENSITY_BUCKETS as f32;
        for (index, value) in bars.iter().enumerate() {
            if *value <= 0.0 {
                continue;
            }
            let height = (value * (LANE_HEIGHT - 1.0)).max(1.0);
            let x = left + index as f32 * slot;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x, top + LANE_HEIGHT - height),
                    pos2(x + (slot - 0.5).max(1.0), top + LANE_HEIGHT),
                ),
                0,
                color.gamma_multiply(0.75),
            );
        }
    }

    // Рамка видимой области ленты: она двигается при прокрутке.
    if let Some((start, end)) = visible {
        let brush = Rect::from_min_max(
            pos2(left + start * width, rect.top() + PAD_TOP),
            pos2(left + end * width, rect.top() + PAD_TOP + 2.0 * LANE_HEIGHT),
        );
        painter.rect_stroke(
            brush,
            radius(RADIUS_CHIP),
            Stroke::new(1.5, p.ctrl),
            egui::StrokeKind::Inside,
        );
    }

    // Клик и перетаскивание переходят к месту: доля времени → номер события.
    let area = Rect::from_min_max(
        pos2(left, rect.top() + PAD_TOP),
        pos2(left + width, rect.top() + PAD_TOP + 2.0 * LANE_HEIGHT),
    );
    let response = ui.interact(area, Id::unique("bt-journal-map"), Sense::click_and_drag());
    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let fraction = ((pos.x - area.left()) / area.width().max(1.0)).clamp(0.0, 1.0);
        app.seek_journal(fraction);
    }
    let hint = t!("trace.seekHint");
    response.widget_info(|| WidgetInfo::labeled(Role::Slider, true, hint));
    if response.hovered() {
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
}

/// Панель над лентой: вид, кодировка и фильтр соединений.
pub fn bar(app: &mut BytetapApp, ui: &mut Ui) {
    let p = app.pal();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAR_HEIGHT), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, p.border_faint),
    );
    let inner = rect.shrink2(vec2(SPACE_3, 0.0));
    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.spacing_mut().item_spacing.x = SPACE_2;
    let view = app.settings.feed.view;
    let button = view_button(&mut left, &p, view_label(view), t!("feed.viewGroup"));
    let mut chosen_view = view;
    Popup::menu(&button)
        .id(Id::unique("bt-journal-view"))
        .align(RectAlign::BOTTOM_START)
        .gap(4.0)
        .width(160.0)
        .show(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for option in [FeedView::Text, FeedView::Hex] {
                if widgets::menu_item(ui, &p, view_label(option), true).clicked() {
                    chosen_view = option;
                }
            }
        });
    if chosen_view != app.settings.feed.view {
        app.set_feed_view(chosen_view);
    }
    if app.settings.feed.view == FeedView::Text {
        let encoding = app.settings.feed.encoding;
        let button = view_button(&mut left, &p, encoding_label(encoding), t!("feed.encoding"));
        let mut chosen_encoding = encoding;
        Popup::menu(&button)
            .id(Id::unique("bt-journal-encoding"))
            .align(RectAlign::BOTTOM_START)
            .gap(4.0)
            .width(160.0)
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for option in crate::feed::ENCODINGS {
                    if widgets::menu_item(ui, &p, encoding_label(option), true).clicked() {
                        chosen_encoding = option;
                    }
                }
            });
        if chosen_encoding != app.settings.feed.encoding {
            app.set_feed_encoding(chosen_encoding);
        }
    }
}

/// Кнопка панели: текст 12px и шеврон, как при записи.
fn view_button(ui: &mut Ui, p: &Palette, text: &str, name: &str) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::sans(TEXT_XS), p.text_muted);
    let (rect, response) = ui.allocate_exact_size(
        vec2(6.0 + galley.size().x + 4.0 + 12.0 + 6.0, 24.0),
        Sense::click(),
    );
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ComboBox, true, name);
        info.current_text_value = Some(text.to_owned());
        info
    });
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.painter().rect_filled(rect, radius(RADIUS_CHIP), p.hover);
    }
    let color = if hovered { p.text } else { p.text_muted };
    let x = rect.left() + 6.0;
    let width = galley.size().x;
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    icons::paint(
        ui,
        Icon::ChevronDown,
        pos2(x + width + 4.0 + 6.0, rect.center().y),
        12.0,
        color,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Лента журнала: видимые строки своей прокруткой в строках, страницы по требованию.
pub fn feed(app: &mut BytetapApp, ui: &mut Ui) {
    let p = app.pal();
    let hex = app.settings.feed.view == FeedView::Hex;
    let encoding = app.settings.feed.encoding;
    let Some(view) = &mut app.viewing else {
        return;
    };
    let total = view.rows(hex);
    if total == 0 {
        ui.add_space(28.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(
                egui::RichText::new(t!("journal.emptyTitle"))
                    .font(theme::sans_semibold(TEXT_XS))
                    .color(p.text_muted),
            );
            ui.label(
                egui::RichText::new(t!("journal.emptyText"))
                    .size(TEXT_XS)
                    .color(p.text_muted),
            );
        });
        return;
    }
    let row_height = if hex { HEX_ROW_HEIGHT } else { TEXT_ROW_HEIGHT };
    // Колонка номера соединения — когда соединений было больше одного: как в UI v2,
    // где в просмотре берутся итоги сессии из сводки журнала.
    let multi = view.summary.stats.connections > 1;
    if let Some(event) = view.seek.take() {
        // Переход к событию: строка — по номеру строки события в текущем виде.
        let target = if hex { view.hex_row_of(event) } else { event };
        view.scroll.set_top(target);
    }
    // Прокрутка копируется на кадр: замыкание строк держит изменяемую ссылку на журнал.
    let mut scroll = view.scroll;
    let out = scroll.show(
        ui,
        Id::unique("bt-journal-feed"),
        total,
        row_height,
        |ui, range| {
            for row in range {
                let Some((index, line)) = view.event_of_row(row, hex) else {
                    break;
                };
                // Страница в пути: заполнитель той же высоты — строки не сдвигаются.
                if view.source.page_has_error(index) {
                    error_row(ui, &p, row_height);
                    continue;
                }
                let Some(event) = view.source.event(index) else {
                    pending_row(ui, &p, row_height);
                    continue;
                };
                let Some(event) = crate::viewing::feed_event(event) else {
                    pending_row(ui, &p, row_height);
                    continue;
                };
                if hex {
                    hex_row(ui, &p, &event, line, multi, encoding);
                } else {
                    text_row(ui, &p, &event, multi, encoding);
                }
            }
        },
    );
    view.scroll = scroll;
    // Видимая область — рамка на карте сессии; страницы — по событиям, а не по строкам:
    // одна строка HEX — кусок события, и номер строки не номер события.
    let last = (out.first + out.visible).min(total.saturating_sub(1));
    let first_event = view
        .event_of_row(out.first, hex)
        .map_or(0, |(index, _)| index);
    let last_event = view
        .event_of_row(last, hex)
        .map_or(total, |(index, _)| index + 1);
    let start = view.fraction_at_event(first_event);
    let end = (view.fraction_at_event(last_event.saturating_sub(1)) + 1.0 / DENSITY_BUCKETS as f32)
        .min(1.0);
    app.set_visible(Some((start, end)));
    let ctx = ui.ctx().clone();
    app.request_pages(&ctx, first_event, last_event.max(first_event + 1));
    if let Some(pause) = app.retry_pages() {
        ctx.request_repaint_after(pause);
    }
}

/// Количество корзин плотности — как в UI v2.
fn view_label(view: FeedView) -> &'static str {
    match view {
        FeedView::Text => t!("feed.viewText"),
        FeedView::Hex => t!("feed.viewHex"),
    }
}

fn encoding_label(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Ascii => t!("feed.encAscii"),
        Encoding::Cp1251 => t!("feed.encCp1251"),
        Encoding::Utf8 => t!("feed.encUtf8"),
        Encoding::Gbk => t!("feed.encGbk"),
        Encoding::Latin1 => t!("feed.encLatin1"),
    }
}
