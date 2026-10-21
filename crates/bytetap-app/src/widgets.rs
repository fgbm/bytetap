//! Мелкие виджеты в стиле UI v2: поля, списки, кнопки-пилюли, флажки.
//!
//! Размеры и цвета — из токенов (`app/src/styles/app.css` Vue-версии): поле 34px с
//! тонкой рамкой, подпись 12px над ним, ошибка 12px `danger` под ним. У каждого своего
//! виджета — роль и подпись для AccessKit.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Margin, Popup, Rect, Response,
    RichText, Role, ScrollArea, Sense, Stroke, TextEdit, TextWrapMode, Ui, Vec2, WidgetInfo, pos2,
    vec2,
};

use crate::icons::{self, Icon};
use crate::theme::tokens::{
    RADIUS_CHIP, RADIUS_CONTROL, RADIUS_RECORD, SPACE_2, TEXT_BASE, TEXT_SM, TEXT_XS,
};
use crate::theme::{self, Palette, mix, radius};

/// Высота поля ввода и списка.
pub const FIELD_HEIGHT: f32 = 34.0;
/// Сторона кнопки-иконки.
pub const ICON_BUTTON_WIDTH: f32 = 28.0;
/// Внутренний отступ поля слева и справа.
const FIELD_PADDING: f32 = 9.0;
/// Высота кнопки-пилюли второго уровня.
const CHIP_HEIGHT: f32 = 26.0;
/// Высота кнопки «Старт».
const START_HEIGHT: f32 = 42.0;

/// Кнопка-иконка 28×28: muted, при наведении — фон `hover` и цвет текста. Подпись —
/// подсказка и имя для AccessKit.
pub fn icon_button(ui: &mut Ui, p: &Palette, icon: Icon, icon_size: f32, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(ICON_BUTTON_WIDTH), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), label));
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.painter()
            .rect_filled(rect, radius(RADIUS_CONTROL), p.hover);
    }
    focus_ring(ui, p, &response, rect, radius(RADIUS_CONTROL));
    icons::paint(
        ui,
        icon,
        rect.center(),
        icon_size,
        if hovered { p.text } else { p.text_muted },
    );
    response.on_hover_text(label)
}

/// Рамка фокуса с клавиатуры: 1px `ctrl` вокруг элемента.
pub fn focus_ring(ui: &Ui, p: &Palette, response: &Response, rect: Rect, corner: CornerRadius) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            corner,
            Stroke::new(1.0, p.ctrl),
            egui::StrokeKind::Outside,
        );
    }
}

/// Подпись поля 12px `text-muted`; её `id` связывает подпись с полем для AccessKit.
pub fn field_label(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.label(RichText::new(text).size(TEXT_XS).color(p.text_muted))
}

/// Ошибка под полем: 12px, цвет `danger` (или `warn` для мягкого предупреждения).
pub fn field_error(ui: &mut Ui, color: Color32, text: &str) -> Response {
    ui.label(RichText::new(text).size(TEXT_XS).color(color))
}

/// Строка-подсказка 12px `text-muted`.
pub fn hint(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.label(RichText::new(text).size(TEXT_XS).color(p.text_muted))
}

/// Как выглядит поле.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldLook {
    /// Моноширинный текст: порты, адреса, пути.
    pub mono: bool,
    /// Рамка `danger`: под полем ошибка.
    pub error: bool,
    /// Только чтение: текст `text-muted`, фокуса нет.
    pub readonly: bool,
}

fn field_font(look: FieldLook) -> egui::FontId {
    if look.mono {
        theme::mono(TEXT_SM)
    } else {
        theme::sans(TEXT_SM)
    }
}

fn field_stroke(p: &Palette, look: FieldLook, focused: bool) -> Stroke {
    let color = if look.error {
        p.danger
    } else if focused {
        p.ctrl
    } else {
        p.border
    };
    Stroke::new(1.0, color)
}

/// Поле ввода 34px во всю ширину `ui`. `label` — подпись над полем (имя для AccessKit).
pub fn text_input(
    ui: &mut Ui,
    p: &Palette,
    id: Id,
    text: &mut String,
    label: &Response,
    placeholder: &str,
    look: FieldLook,
) -> Response {
    let focused = ui.memory(|m| m.has_focus(id));
    let font = field_font(look);
    let width = ui.available_width();
    let frame = Frame::new()
        .fill(p.surface_0)
        .stroke(field_stroke(p, look, focused && !look.readonly))
        .corner_radius(radius(RADIUS_CONTROL))
        .inner_margin(Margin::symmetric(FIELD_PADDING as i8, 0));
    let edit = TextEdit::singleline(text)
        .id(id)
        .font(font.clone())
        .text_color(if look.readonly { p.text_muted } else { p.text })
        .hint_text(RichText::new(placeholder).font(font).color(p.text_faint))
        .frame(frame)
        .interactive(!look.readonly)
        .desired_width(width - 2.0 * FIELD_PADDING)
        .min_size(vec2(width, FIELD_HEIGHT))
        .align(Align2::LEFT_CENTER);
    ui.add(edit).labelled_by(label.id)
}

/// Свой выпадающий список на токенах: поле 34px с шевроном, всплывающий список
/// пунктов. Возвращает номер выбранного пункта, если его выбрали в этом кадре.
pub fn select(
    ui: &mut Ui,
    p: &Palette,
    id: Id,
    label: &str,
    options: &[String],
    selected: Option<usize>,
    look: FieldLook,
) -> (Response, Option<usize>) {
    let current = selected
        .and_then(|index| options.get(index))
        .map_or("", String::as_str);
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), FIELD_HEIGHT), Sense::hover());
    let response = ui
        .interact(rect, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ComboBox, ui.is_enabled(), label);
        info.current_text_value = Some(current.to_owned());
        info
    });
    let popup_id = id.with("popup");
    let open = Popup::is_id_open(ui.ctx(), popup_id);
    let corner = radius(RADIUS_CONTROL);
    ui.painter().rect(
        rect,
        corner,
        if response.hovered() {
            mix(p.surface_0, p.hover, 0.5)
        } else {
            p.surface_0
        },
        field_stroke(p, look, open || response.has_focus()),
        egui::StrokeKind::Inside,
    );
    let chevron = 14.0;
    let text_width = rect.width() - 2.0 * FIELD_PADDING - chevron - 8.0;
    let galley = ui
        .painter()
        .layout(current.to_owned(), field_font(look), p.text, f32::INFINITY);
    let galley = if galley.size().x > text_width {
        let mut job =
            egui::text::LayoutJob::simple_singleline(current.to_owned(), field_font(look), p.text);
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_width);
        ui.painter().layout_job(job)
    } else {
        galley
    };
    ui.painter().galley(
        pos2(
            rect.left() + FIELD_PADDING,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        p.text,
    );
    icons::paint(
        ui,
        Icon::ChevronDown,
        pos2(rect.right() - 8.0 - chevron / 2.0, rect.center().y),
        chevron,
        p.text_muted,
    );

    let mut chosen = None;
    Popup::menu(&response)
        .id(popup_id)
        .width(rect.width())
        .gap(2.0)
        .show(|ui| {
            ui.set_min_width(rect.width() - 8.0);
            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
            ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for (index, option) in options.iter().enumerate() {
                    let item = list_item(ui, p, option, selected == Some(index), look.mono);
                    if item.clicked() {
                        chosen = Some(index);
                    }
                }
            });
        });
    if chosen.is_some() {
        Popup::close_id(ui.ctx(), popup_id);
    }
    (response, chosen)
}

/// Пункт списка 28px: при наведении фон `hover`, выбранный — фон `surface-1`.
fn list_item(ui: &mut Ui, p: &Palette, text: &str, selected: bool, mono: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::ListBoxOption, true, selected, text));
    let fill = if response.hovered() || response.has_focus() {
        p.hover
    } else if selected {
        p.surface_1
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, radius(RADIUS_CHIP), fill);
    let font = if mono {
        theme::mono(TEXT_SM)
    } else {
        theme::sans(TEXT_SM)
    };
    ui.painter().text(
        pos2(rect.left() + SPACE_2, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        p.text,
    );
    response
}

/// Пункт меню шапки: 12px, отступы 6×8, при наведении фон `hover`.
pub fn menu_item(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let font = theme::sans(TEXT_XS);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, p.text);
    let width = (galley.size().x + 16.0).max(216.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), sense);
    response.widget_info(|| WidgetInfo::labeled(Role::MenuItem, enabled, text));
    if enabled && (response.hovered() || response.has_focus()) {
        ui.painter().rect_filled(rect, radius(RADIUS_CHIP), p.hover);
    }
    focus_ring(ui, p, &response, rect, radius(RADIUS_CHIP));
    ui.painter().galley(
        pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        if enabled { p.text } else { p.text_faint },
    );
    response
}

/// Разделитель пунктов меню: линия `border-faint` с отступами 4px.
pub fn menu_separator(ui: &mut Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, p.border_faint),
    );
}

/// Размер текста кнопки с иконкой: ширина содержимого и галерея текста.
fn label_galley(ui: &Ui, text: &str, size: f32, color: Color32) -> std::sync::Arc<egui::Galley> {
    ui.painter()
        .layout_no_wrap(text.to_owned(), theme::sans(size), color)
}

/// Ширина [`chip_button`]: чтобы ставить кнопки справа в порядке слева направо.
pub fn chip_button_width(ui: &Ui, icon: bool, text: &str) -> f32 {
    let icon_width = if icon { 13.0 + 5.0 } else { 0.0 };
    20.0 + icon_width
        + label_galley(ui, text, TEXT_XS, Color32::PLACEHOLDER)
            .size()
            .x
}

/// Ширина [`confirm_button`].
pub fn confirm_button_width(ui: &Ui, text: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        theme::sans_semibold(TEXT_XS),
        Color32::PLACEHOLDER,
    );
    galley.size().x + 24.0
}

/// Кнопка-пилюля второго уровня: «Скопировать», «Выбрать», «Сохранить» — 26px, рамка,
/// текст 12px `text-muted`.
pub fn chip_button(
    ui: &mut Ui,
    p: &Palette,
    icon: Option<Icon>,
    text: &str,
    enabled: bool,
) -> Response {
    let galley = label_galley(ui, text, TEXT_XS, p.text_muted);
    let icon_width = if icon.is_some() { 13.0 + 5.0 } else { 0.0 };
    let size = vec2(chip_button_width(ui, icon.is_some(), text), CHIP_HEIGHT);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    let hovered = enabled && response.hovered();
    let corner = radius(RADIUS_RECORD.min(127.0));
    let mut color = if hovered { p.text } else { p.text_muted };
    if !enabled {
        color = mix(p.surface_0, color, 0.45);
    }
    ui.painter().rect(
        rect,
        corner,
        if hovered {
            p.hover
        } else {
            Color32::TRANSPARENT
        },
        Stroke::new(
            1.0,
            if enabled {
                p.border
            } else {
                mix(p.surface_0, p.border, 0.45)
            },
        ),
        egui::StrokeKind::Inside,
    );
    focus_ring(ui, p, &response, rect, corner);
    let mut x = rect.left() + 10.0;
    if let Some(icon) = icon {
        icons::paint(ui, icon, pos2(x + 6.5, rect.center().y), 13.0, color);
        x += icon_width;
    }
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Кнопка-ссылка: иконка и текст 12px `text-muted`, при наведении фон `hover`.
/// `flush` — без отступов и фона (переключатель «Дополнительно»).
pub fn link_button(
    ui: &mut Ui,
    p: &Palette,
    icon: Icon,
    icon_size: f32,
    text: &str,
    flush: bool,
) -> Response {
    let galley = label_galley(ui, text, TEXT_XS, p.text_muted);
    let pad = if flush {
        vec2(0.0, 4.0)
    } else {
        vec2(6.0, 4.0)
    };
    let size = vec2(
        2.0 * pad.x + icon_size + 6.0 + galley.size().x,
        2.0 * pad.y + galley.size().y.max(icon_size),
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), text));
    let hovered = response.hovered();
    if hovered && !flush {
        ui.painter()
            .rect_filled(rect, radius(RADIUS_CONTROL), p.hover);
    }
    focus_ring(ui, p, &response, rect, radius(RADIUS_CONTROL));
    let color = if hovered { p.text } else { p.text_muted };
    let left = rect.left() + pad.x;
    icons::paint(
        ui,
        icon,
        pos2(left + icon_size / 2.0, rect.center().y),
        icon_size,
        color,
    );
    ui.painter().galley(
        pos2(
            left + icon_size + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Ширина [`link_button`]: чтобы правую группу шапки ставить от её края.
pub fn link_button_width(ui: &Ui, icon_size: f32, text: &str) -> f32 {
    2.0 * 6.0
        + icon_size
        + 6.0
        + label_galley(ui, text, TEXT_XS, Color32::PLACEHOLDER)
            .size()
            .x
}

/// Флажок: квадрат 14px (отмеченный — заливка `ctrl` и галочка) и подпись 12px.
pub fn checkbox(ui: &mut Ui, p: &Palette, value: &mut bool, text: &str) -> Response {
    let galley = label_galley(ui, text, TEXT_XS, p.text);
    let box_size = 14.0;
    let size = vec2(
        box_size + 6.0 + galley.size().x,
        galley.size().y.max(box_size) + 4.0,
    );
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let checked = *value;
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), checked, text));
    let square = Rect::from_center_size(
        pos2(rect.left() + box_size / 2.0, rect.center().y),
        Vec2::splat(box_size),
    );
    let corner = CornerRadius::same(3);
    if checked {
        ui.painter().rect_filled(square, corner, p.ctrl);
        icons::paint(ui, Icon::Check, square.center(), 11.0, Color32::WHITE);
    } else {
        ui.painter().rect(
            square,
            corner,
            p.surface_0,
            Stroke::new(
                1.0,
                if response.hovered() {
                    p.text_muted
                } else {
                    p.border
                },
            ),
            egui::StrokeKind::Inside,
        );
    }
    focus_ring(ui, p, &response, square, corner);
    ui.painter().galley(
        pos2(
            square.right() + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        p.text,
    );
    response
}

/// «Старт»: тёмная пилюля 42px во всю ширину с красной точкой.
pub fn start_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), START_HEIGHT), sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    let corner = radius(RADIUS_RECORD.min(127.0));
    let mut fill = if enabled && response.hovered() {
        mix(p.rec_bg, p.rec_fg, 0.12)
    } else {
        p.rec_bg
    };
    if !enabled {
        fill = mix(p.surface_0, fill, 0.55);
    }
    ui.painter().rect_filled(rect, corner, fill);
    focus_ring(ui, p, &response, rect, corner);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), theme::sans_semibold(TEXT_BASE), p.rec_fg);
    let width = 9.0 + 8.0 + galley.size().x;
    let left = rect.center().x - width / 2.0;
    ui.painter()
        .circle_filled(pos2(left + 4.5, rect.center().y), 4.5, p.rec);
    ui.painter().galley(
        pos2(left + 17.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.rec_fg,
    );
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Тёмная кнопка-пилюля второго уровня: подтверждение в диалоге.
pub fn confirm_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), theme::sans_semibold(TEXT_XS), p.rec_fg);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(vec2(confirm_button_width(ui, text), 28.0), sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    let corner = radius(RADIUS_RECORD.min(127.0));
    let mut fill = if enabled && response.hovered() {
        mix(p.rec_bg, p.rec_fg, 0.12)
    } else {
        p.rec_bg
    };
    if !enabled {
        fill = mix(p.surface_0, fill, 0.55);
    }
    ui.painter().rect_filled(rect, corner, fill);
    focus_ring(ui, p, &response, rect, corner);
    ui.painter().galley(
        pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.rec_fg,
    );
    response
}
