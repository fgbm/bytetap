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
use crate::details;
use crate::feedrows::{
    HEX_ROW_HEIGHT, Pair, TEXT_ROW_HEIGHT, error_row, hex_row, pending_row, text_row, truncated,
};
use crate::icons::{self, Icon};
use crate::selection::{self, MenuAction};
use crate::settings::{Encoding, FeedView};
use crate::theme::tokens::{RADIUS_CHIP, SPACE_2, SPACE_3, TEXT_2XS, TEXT_XS};
use crate::theme::{self, Palette, radius};
use crate::viewing::{
    NOTES_SHOWN, density_bars, file_name, journal_meta, notes_line, pair_of_journal,
};
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
    // У журнала прокси в подписи — сводка двух сторон, у остальных — анализатор.
    let (name, meta) = match &app.viewing {
        Some(view) => (
            file_name(&view.path),
            journal_meta(&view.summary.header, view.summary.duration_us),
        ),
        None => (String::new(), String::new()),
    };
    let (icon, theme_label) = if app.dark() {
        (Icon::Sun, t!("theme.toLight"))
    } else {
        (Icon::Moon, t!("theme.toDark"))
    };
    let right_width = widgets::ICON_BUTTON_WIDTH
        + SPACE_2
        + widgets::link_button_width(ui, 15.0, t!("journal.comment"));
    app.title_bar(
        ui,
        right_width,
        |app, ui| {
            app.brand(ui);
            let p = app.pal();
            if widgets::icon_button(ui, &p, Icon::X, 16.0, t!("journal.close")).clicked() {
                app.close_journal();
            }
            // Имя файла моноширинным, метаданные — приглушённым. Оба обрезаются «…» по
            // остатку шапки: длинный анализатор в узком окне не выталкивает кнопки окна
            // за край (находка ревью шага 5). Полный текст — в подсказке и AccessKit.
            let name_font = theme::mono(TEXT_XS);
            let meta_font = theme::sans(TEXT_XS);
            let meta_text = format!("· {meta}");
            let full_name = ui
                .painter()
                .layout_no_wrap(name.clone(), name_font.clone(), p.text);
            let full_meta =
                ui.painter()
                    .layout_no_wrap(meta_text.clone(), meta_font.clone(), p.text_muted);
            let available = ui.available_width().max(0.0);
            // Имя файла и метаданные делят остаток: имя берёт свою фактическую ширину, но
            // не больше половины, чтобы длинный анализатор не оставлял от имени одно «…».
            // Метаданным достаётся всё остальное. Имя журнала — одно длинное слово без
            // пробелов, поэтому и «…» по остатку ширины. Пока имя и метаданные помещаются
            // вместе, имя не режется вовсе: половинный предел — только для тесной шапки
            // (находка контрольного ревью шага 5: короткое имя с коротким анализатором
            // обрезалось «…» на ровном месте).
            let name_width = name_fits_width(available, full_name.size().x, full_meta.size().x);
            let job = egui::text::LayoutJob::simple_singleline(name.clone(), name_font, p.text);
            let name_galley = truncated(ui, job, name_width);
            let (rect, response) = ui.allocate_exact_size(name_galley.size(), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
            if full_name.size().x > name_width {
                response.on_hover_text(&name);
            }
            ui.painter().galley(rect.min, name_galley, p.text);
            let meta_width = (available - name_width - SPACE_2).max(0.0);
            let job = egui::text::LayoutJob::simple_singleline(
                meta_text.clone(),
                meta_font,
                p.text_muted,
            );
            let meta_galley = truncated(ui, job, meta_width);
            let (rect, response) = ui.allocate_exact_size(meta_galley.size(), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &meta));
            if full_meta.size().x > meta_width {
                // Подсказка — без ведущего «· »: он отделяет метаданные от имени файла, а
                // в подсказке отделять нечего.
                response.on_hover_text(&meta);
            }
            ui.painter().galley(rect.min, meta_galley, p.text_muted);
        },
        |app, ui| {
            let p = app.pal();
            // Тема — левее «Комментария»: так эти кнопки стояли и раньше, и так их
            // обходит Tab (U1-AC25).
            if widgets::icon_button(ui, &p, icon, 16.0, theme_label).clicked() {
                app.toggle_theme(ui.ctx());
            }
            if widgets::link_button(ui, &p, Icon::Pen, 15.0, t!("journal.comment"), false).clicked()
            {
                app.open_journal_note();
            }
        },
    );
}

/// Ширина, которую получает имя файла в шапке просмотра.
///
/// Пока имя и метаданные вместе с отступом помещаются в остаток шапки, имя берёт свою
/// фактическую ширину целиком. В тесной шапке оно не больше 45 % остатка: длинный
/// анализатор не должен оставлять от имени одно «…», а метаданным нужен остаток.
fn name_fits_width(available: f32, name: f32, meta: f32) -> f32 {
    if name + meta + SPACE_2 <= available {
        name
    } else {
        name.min(available * 0.45)
    }
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

    // Счётчик выделения — справа, как при записи: «выбрано 3 · Ctrl+C». Сообщение о
    // копировании встаёт левее: предел или ожидание страниц журнала.
    let (count, notice) = app.viewing.as_ref().map_or((0, None), |view| {
        (view.selection.count(), view.copy_notice.clone())
    });
    let counter = (count > 0).then(|| t!("feed.selectionCount", count = count));
    let measure = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), theme::sans(TEXT_2XS), p.text_faint)
            .size()
            .x
    };
    let counter_width = counter.as_ref().map_or(0.0, |text| measure(text));
    let notice_width = notice.as_ref().map_or(0.0, |text| measure(text));
    let width = counter_width
        + if notice.is_some() {
            SPACE_2 + notice_width
        } else {
            0.0
        };
    if counter.is_some() || notice.is_some() {
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(inner.right() - width, inner.top()),
                    inner.max,
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        right.spacing_mut().item_spacing.x = SPACE_2;
        if let Some(text) = notice {
            right.label(egui::RichText::new(text).size(TEXT_2XS).color(p.warn));
        }
        if let Some(text) = counter {
            right.label(egui::RichText::new(text).size(TEXT_2XS).color(p.text_muted));
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
    widgets::focus_ring(ui, p, &response, rect, radius(RADIUS_CHIP));
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
///
/// Клик по строке выбирает событие и открывает панель деталей, ↑/↓ ходят по ленте, Esc
/// снимает выбор (L1-AC4, L1-AC9). Событие в панели — копия: страницу журнала окно
/// может выгрузить из памяти, а прочитанный текст остаётся.
pub fn feed(app: &mut BytetapApp, ui: &mut Ui, popup_open: bool) {
    let p = app.pal();
    let hex = app.settings.feed.view == FeedView::Hex;
    let encoding = app.settings.feed.encoding;
    let panel_height = app.settings.feed.panel_height;
    // «Что-то открыто» снято в начале кадра, до отрисовки шапки: меню и списки Esc
    // закрывает сам, и панель не должна уйти вместе с ними (L1-AC9).
    let escape =
        ui.input(|input| input.key_pressed(egui::Key::Escape)) && app.note.is_none() && !popup_open;
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
    // Пара двух сторон — у журнала прокси: «#1 открыто: анализатор → ЛИС».
    let pair = pair_of_journal(&view.summary.header);
    view.details.set_default_height(panel_height);
    // Панель была открыта до этого кадра: клик только меняет выбор, ленту не двигает.
    let was_open = view.details.is_open();
    // Панель деталей: место снизу лента уступает до того, как её нарисовать, а тело
    // панели идёт после ленты — тогда Tab обходит сначала строки, потом панель.
    let place = details::reserve(ui, &p, &view.details);
    // Клавиши ленты: ↑/↓ и Shift+↑/↓ ходят по событиям. Поле панели и её разделитель
    // забирают стрелки себе: там они принадлежат тексту фрагмента и высоте панели.
    let feed_keys = !ui.ctx().text_edit_focused() && !details::resizer_focused(ui);
    // Ctrl+A и Ctrl+C — те же клавиши у панели: поле забирает их фокусом, вкладка HEX —
    // целиком, потому что там они выделяют и копируют строки дампа (L1-AC5, L1-AC6).
    let hex_panel = view.details.is_open() && view.details.tab() == details::Tab::Hex;
    let select_keys = feed_keys && !hex_panel;
    // Исключение — блок строк на открытой «HEX»: Ctrl+C отдаётся ленте, если выделено
    // больше одной строки и в дампе ничего не выделено (находка M2 ревью шага 2).
    let copy_keys = feed_keys
        && (!hex_panel || (view.selection.count() > 1 && !view.details.has_hex_selection()));
    if feed_keys && (view.details.is_open() || !view.selection.is_empty()) {
        let (step, extend) = ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowUp) {
                (-1, true)
            } else if input.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowDown) {
                (1, true)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                (-1, false)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                (1, false)
            } else {
                (0, false)
            }
        });
        if step != 0 {
            step_selection(view, step, extend);
        }
    }
    if let Some(event) = view.seek.take() {
        // Переход к событию: строка — по номеру строки события в текущем виде.
        let target = if hex { view.hex_row_of(event) } else { event };
        view.scroll.set_top(target);
    }
    // Строка, по которой кликнули, могла уйти под только что открывшуюся панель: лента
    // поднимается ровно настолько, чтобы низ строки встал на верхнюю границу панели.
    if let Some(rect) = view.reveal.take()
        && let Some(place) = place
    {
        view.scroll.scroll_rows(place.reveal_rows(rect, row_height));
    }
    // Лента — список для диктора: строки под ним помечены выбранными, а сам список
    // говорит, что выделение множественное (L1, шаг 2).
    let area = ui.available_rect_before_wrap();
    let list = ui.interact(area, Id::unique("bt-journal-list"), egui::Sense::hover());
    list.widget_info(|| WidgetInfo::labeled(Role::ListBox, true, t!("feed.list")));
    ui.ctx().accesskit_node_builder(list.id, |builder| {
        builder.set_multiselectable();
    });
    // Прокрутка копируется на кадр: замыкание строк держит изменяемую ссылку на журнал.
    let mut scroll = view.scroll;
    let count = view.selection.count();
    let mut visible: Vec<u64> = Vec::new();
    let mut clicked = None;
    let mut action = MenuAction::None;
    let out = scroll.show(
        ui,
        &p,
        Id::unique("bt-journal-feed"),
        list.id,
        total,
        row_height,
        |ui, range| {
            for row in range {
                let Some((index, line)) = view.event_of_row(row, hex) else {
                    break;
                };
                // Строка HEX — кусок события: в видимые номера попадает событие целиком.
                if visible.last() != Some(&(index as u64)) {
                    visible.push(index as u64);
                }
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
                let selected = view.selection.contains(index as u64);
                let mut row_action = MenuAction::None;
                let response = if hex {
                    hex_row(
                        ui,
                        &p,
                        &event,
                        line,
                        multi,
                        pair,
                        encoding,
                        selected,
                        count,
                        &mut row_action,
                    )
                } else {
                    text_row(
                        ui,
                        &p,
                        &event,
                        multi,
                        pair,
                        encoding,
                        selected,
                        count,
                        &mut row_action,
                    )
                };
                if row_action != MenuAction::None {
                    action = row_action;
                }
                if response.clicked() {
                    clicked = Some((index, std::sync::Arc::new(event), response.rect));
                }
            }
        },
    );
    view.scroll = scroll;
    view.feed_rect = Some(out.inner);
    // Ctrl+A — видимые строки: выделять миллион строк журнала, читая его по страницам,
    // нечего (решение исполнителя, см. L1). Дальше по журналу выделяют Shift+кликом —
    // выделение живёт в номерах событий.
    if select_keys
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
    {
        view.selection.select_all(visible);
        view.copy_notice = None;
    }
    // Ctrl+C по выделенным строкам: строки на незагруженных страницах сперва
    // дочитываются, а в буфер уходит весь выделенный блок.
    if copy_keys
        && !view.selection.is_empty()
        && ui.input(|input| input.events.contains(&egui::Event::Copy))
    {
        ui.input_mut(|input| {
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Copy));
        });
        let indices: Vec<usize> = view
            .selection
            .seqs()
            .map(|seq| seq as usize)
            .take(selection::COPY_MAX_ROWS)
            .collect();
        view.copy_job = Some(crate::viewing::CopyJob::new(
            selection::CopyKind::Text,
            indices,
            view.selection.count(),
        ));
        view.copy_notice = None;
        ui.ctx().request_repaint();
    }
    if let Some((index, event, rect)) = clicked {
        let modifiers = ui.input(|input| input.modifiers);
        if modifiers.command && modifiers.shift {
            // Ctrl+Shift+клик: диапазон от якоря добавляется к уже выделенному.
            view.selection.extend_add(index as u64);
            view.details.open(index as u64, event);
            if !was_open {
                view.reveal = Some(rect);
            }
        } else if modifiers.command {
            // Ctrl+клик: курсор и панель переезжают на строку, даже когда её сняли.
            view.selection.toggle(index as u64);
            view.details.open(index as u64, event);
            if !was_open {
                view.reveal = Some(rect);
            }
        } else if modifiers.shift {
            // Shift+клик: диапазон от якоря заменяет прежний, база Ctrl-отметок остаётся.
            view.selection.extend(index as u64);
            view.details.open(index as u64, event);
            if !was_open {
                view.reveal = Some(rect);
            }
        } else {
            view.selection.click(index as u64);
            view.details.open(index as u64, event);
            // Подтягиваем строку только когда панель открылась этим кликом: при уже
            // открытой панели лента стоит на месте, иначе она дёргалась бы под каждым
            // кликом.
            if !was_open {
                view.reveal = Some(rect);
            }
        }
        view.copy_notice = None;
    }
    match action {
        MenuAction::CopyText | MenuAction::CopyHex | MenuAction::CopyBase64 => {
            let indices: Vec<usize> = view
                .selection
                .seqs()
                .map(|seq| seq as usize)
                .take(selection::COPY_MAX_ROWS)
                .collect();
            let kind = match action {
                MenuAction::CopyHex => selection::CopyKind::Hex,
                MenuAction::CopyBase64 => selection::CopyKind::Base64,
                _ => selection::CopyKind::Text,
            };
            view.copy_job = Some(crate::viewing::CopyJob::new(
                kind,
                indices,
                view.selection.count(),
            ));
            ui.ctx().request_repaint();
        }
        MenuAction::Clear => {
            view.selection.clear();
            view.copy_notice = None;
        }
        MenuAction::None => {}
    }
    if let Some(place) = place
        && details::show(ui, &p, &mut view.details, encoding, pair, place)
    {
        view.details.close();
        view.selection.clear();
        // «×» отменяет и незаконченное копирование: выделения, которое оно собирало,
        // больше нет (находка m6).
        view.copy_job = None;
        view.copy_notice = None;
    }
    if escape && (view.details.is_open() || !view.selection.is_empty()) {
        view.details.close();
        view.selection.clear();
        view.copy_job = None;
        view.copy_notice = None;
        ui.ctx().request_repaint();
    }
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
    // Высоту, выбранную разделителем, запоминаем один раз — на отпускании мыши.
    let height = view.details.take_height_saved();
    app.set_visible(Some((start, end)));
    let ctx = ui.ctx().clone();
    app.request_pages(&ctx, first_event, last_event.max(first_event + 1));
    // Выделение могло лечь на страницы, которых нет в памяти: их дочитываем по мере
    // копирования и только потом кладём собранные строки в буфер.
    if let Some((from, to)) = copy_step(app, &ctx, encoding, pair) {
        app.request_pages(&ctx, from, to.max(from + 1));
    }
    if let Some(height) = height {
        app.settings.feed.panel_height = height;
        app.save_settings();
    }
    if let Some(pause) = app.retry_pages() {
        ctx.request_repaint_after(pause);
    }
}

/// Шаг копирования выделения: собрать готовые строки и попросить недостающие страницы.
///
/// Строки идут курсором по возрастанию номеров: страница, которая уже в памяти, отдаёт
/// свои строки сразу и в текст, а недостающая просится одна (и одна вперёд). Поэтому
/// большой блок не перечитывается каждый кадр и не требует держать в памяти весь
/// промежуток выделения — LRU страниц его вытеснил бы (находка B1 ревью шага 2).
///
/// Возвращает диапазон событий, страницы которых надо попросить: чтение живёт в
/// приложении, а экран только говорит, чего не хватает.
fn copy_step(
    app: &mut BytetapApp,
    ctx: &egui::Context,
    encoding: Encoding,
    pair: Pair,
) -> Option<(usize, usize)> {
    let view = app.viewing.as_mut()?;
    let job = view.copy_job.as_mut()?;
    let total = view.source.total();
    while job.cursor < job.indices.len() {
        let index = job.indices[job.cursor];
        if view.source.page_has_error(index) {
            // Строку не прочитать: в буфер её не кладём, но и не ждём вечно.
            job.skipped += 1;
            job.cursor += 1;
            continue;
        }
        let Some(event) = view.source.event(index) else {
            // Страница ещё едет: ждём её и одну вперёд — следующая понадобится сразу.
            view.copy_notice = Some(t!(
                "journal.copyWaiting",
                count = selection::rows_count(job.total)
            ));
            let ahead = (index + crate::journal::PAGE_SIZE).min(total);
            return Some((index, ahead));
        };
        job.cursor += 1;
        let Some(event) = crate::viewing::feed_event(event) else {
            job.skipped += 1;
            continue;
        };
        let Some(line) = selection::line_for(&event, job.kind, pair, encoding) else {
            continue;
        };
        let mut out = selection::CopyOut {
            text: std::mem::take(&mut job.text),
            rows: job.rows,
            bytes: job.bytes,
            truncated: job.truncated,
        };
        let fits = selection::push_line(&mut out, &line);
        job.text = out.text;
        job.rows = out.rows;
        job.bytes = out.bytes;
        job.truncated = out.truncated;
        if !fits {
            break;
        }
    }
    // Готово: в буфер уходит собранное, а сообщение — только если что-то потерялось.
    let text = std::mem::take(&mut job.text);
    if !text.is_empty() {
        ctx.copy_text(text);
    }
    let notice = if job.truncated {
        Some(t!(
            "feed.copyLimited",
            copied = selection::rows_count(job.rows),
            total = selection::rows_count(job.total)
        ))
    } else if job.skipped > 0 {
        Some(t!(
            "journal.copySkipped",
            copied = selection::rows_count(job.rows),
            total = selection::rows_count(job.total),
            skipped = selection::rows_count(job.skipped)
        ))
    } else {
        None
    };
    view.copy_notice = notice;
    view.copy_job = None;
    None
}

/// Соседнее событие журнала для ↑/↓ и Shift+↑/↓: номер события ±1, но не за пределами
/// журнала.
///
/// `extend` — Shift: к выделению добавляется диапазон от якоря до соседа, а не
/// выбирается одна строка (L1, шаг 2).
fn step_selection(view: &mut crate::viewing::JournalView, step: i32, extend: bool) {
    // Курсор — выбранное событие, а при закрытой панели якорь выделения: после Ctrl+A
    // стрелки ходят от него (находка m5).
    let Some(key) = view.details.key().or_else(|| view.selection.anchor()) else {
        return;
    };
    let total = view.events();
    let index = key as i64 + i64::from(step);
    if index < 0 || index as usize >= total {
        return;
    }
    let index = index as usize;
    let Some(event) = view.source.event(index) else {
        // Страница ещё едет: выбор не переезжает на заполнитель.
        return;
    };
    let Some(event) = crate::viewing::feed_event(event) else {
        return;
    };
    if extend {
        view.selection.extend(index as u64);
    } else {
        view.selection.click(index as u64);
    }
    view.copy_notice = None;
    // Панель переезжает на соседа только когда она открыта: при закрытой панели
    // стрелки двигают выделение, а панель не открывается под руками.
    if view.details.is_open() {
        view.details.open(index as u64, std::sync::Arc::new(event));
    }
    view.seek = Some(index);
}

/// Количество корзин плотности — как в UI v2.
fn view_label(view: FeedView) -> &'static str {
    match view {
        FeedView::Text => t!("feed.viewText"),
        FeedView::Hex => t!("feed.viewHex"),
    }
}

fn encoding_label(encoding: Encoding) -> &'static str {
    crate::feed::encoding_label(encoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Короткое имя в просторной шапке не режется: половинный предел — только когда имя
    /// и метаданные вместе не помещаются (находка контрольного ревью шага 5).
    #[test]
    fn a_short_journal_name_is_not_cut() {
        // Имя 90 px, метаданные 120 px, отступ 8 px: в 910 px шапки всё помещается.
        assert_eq!(name_fits_width(910.0, 90.0, 120.0), 90.0);
        // Ровно в притык — ещё помещается, дальше — уже нет.
        assert_eq!(name_fits_width(218.0, 90.0, 120.0), 90.0);
        assert_eq!(
            name_fits_width(217.0, 90.0, 120.0),
            90.0,
            "90 ≤ 45 % от 217"
        );
        // Длинное имя с длинным анализатором: имя берёт не больше 45 % остатка, остальное
        // достаётся метаданным.
        assert_eq!(name_fits_width(400.0, 900.0, 300.0), 180.0);
        // Тесная шапка и короткое имя: своя ширина меньше предела.
        assert_eq!(name_fits_width(120.0, 40.0, 300.0), 40.0);
    }
}
