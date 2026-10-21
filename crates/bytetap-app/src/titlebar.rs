//! Шапка = заголовок окна без системной рамки (FR-25, U1-AC3): перетаскивание,
//! двойной клик, кнопки окна в стиле Windows 46×44 и края для изменения размера.

use eframe::egui::{
    self, Align, Color32, CursorIcon, Layout, PointerButton, Rect, ResizeDirection, Role, Sense,
    Stroke, Ui, UiBuilder, ViewportCommand, WidgetInfo, pos2, text::LayoutJob, vec2,
};

use crate::BytetapApp;
use crate::icons::{self, Icon};
use crate::t;
use crate::theme::mono_medium;
use crate::theme::tokens::{SPACE_2, SPACE_3, TEXT_SM, TITLE_HEIGHT, WINDOW_CLOSE};
use crate::widgets;

/// Ширина кнопки окна, как у системных кнопок Windows 11.
pub const WINDOW_BUTTON_WIDTH: f32 = 46.0;

/// Полоса по периметру окна, за которую меняется размер.
const RESIZE_EDGE: f32 = 5.0;

impl BytetapApp {
    /// Шапка: `left` — слева направо после отступа 12px, `right` — справа налево
    /// перед кнопками окна; пустое место между ними — область перетаскивания.
    pub(crate) fn title_bar(
        &mut self,
        ui: &mut Ui,
        left: impl FnOnce(&mut Self, &mut Ui),
        right: impl FnOnce(&mut Self, &mut Ui),
    ) {
        let p = self.pal();
        let (bar, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), TITLE_HEIGHT), Sense::hover());
        ui.painter().rect_filled(bar, 0, p.surface_0);
        ui.painter().hline(
            bar.x_range(),
            bar.bottom() - 0.5,
            Stroke::new(1.0, p.border),
        );

        let mut left_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(bar.with_min_x(bar.left() + SPACE_3))
                .layout(Layout::left_to_right(Align::Center)),
        );
        left_ui.spacing_mut().item_spacing.x = SPACE_2;
        left(self, &mut left_ui);
        let left_end = left_ui.min_rect().right().max(bar.left() + SPACE_3);

        let mut right_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(bar)
                .layout(Layout::right_to_left(Align::Center)),
        );
        right_ui.spacing_mut().item_spacing.x = 0.0;
        self.window_buttons(&mut right_ui);
        right_ui.add_space(SPACE_2);
        right_ui.spacing_mut().item_spacing.x = SPACE_2;
        right(self, &mut right_ui);
        let right_start = right_ui.min_rect().left();

        if right_start > left_end + 2.0 * SPACE_2 {
            let drag =
                Rect::from_x_y_ranges(left_end + SPACE_2..=right_start - SPACE_2, bar.y_range());
            let response = ui.interact(
                drag,
                ui.scope_id().with("title-drag"),
                Sense::click_and_drag(),
            );
            if response.double_clicked() {
                toggle_maximized(ui.ctx());
            } else if response.drag_started_by(PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
            }
        }
    }

    /// Свернуть / развернуть-восстановить / закрыть (справа налево).
    fn window_buttons(&mut self, ui: &mut Ui) {
        let p = self.pal();
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        let buttons = [
            (Icon::X, 16.0, t!("window.close"), true),
            if maximized {
                (Icon::Copy, 14.0, t!("window.restore"), false)
            } else {
                (Icon::Square, 13.0, t!("window.maximize"), false)
            },
            (Icon::Minus, 16.0, t!("window.minimize"), false),
        ];
        for (index, (icon, size, label, close)) in buttons.into_iter().enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(vec2(WINDOW_BUTTON_WIDTH, TITLE_HEIGHT), Sense::click());
            response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
            let hovered = response.hovered();
            let (fill, color) = match (hovered, close) {
                (true, true) => (WINDOW_CLOSE, Color32::WHITE),
                (true, false) => (p.hover, p.text),
                _ => (Color32::TRANSPARENT, p.text_muted),
            };
            // Без нижнего пикселя: линия шапки остаётся видна под наведённой кнопкой.
            ui.painter().rect_filled(
                rect.shrink2(vec2(0.0, 0.5)).translate(vec2(0.0, -0.5)),
                0,
                fill,
            );
            icons::paint(ui, icon, rect.center(), size, color);
            if response.on_hover_text(label).clicked() {
                match index {
                    0 => ui.ctx().send_viewport_cmd(ViewportCommand::Close),
                    1 => toggle_maximized(ui.ctx()),
                    _ => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
                }
            }
        }
    }

    /// Бренд: две точки направлений и моноширинное имя.
    pub(crate) fn brand(&self, ui: &mut Ui) {
        let p = self.pal();
        let mut job = LayoutJob::default();
        job.append(
            "bytetap",
            0.0,
            egui::TextFormat {
                font_id: mono_medium(TEXT_SM),
                color: p.text,
                extra_letter_spacing: -0.26,
                ..Default::default()
            },
        );
        let galley = ui.painter().layout_job(job);
        let (rect, _) =
            ui.allocate_exact_size(vec2(15.0 + 6.0 + galley.size().x, 20.0), Sense::hover());
        let cy = rect.center().y;
        ui.painter()
            .circle_filled(pos2(rect.left() + 3.0, cy), 3.0, p.dir_a2l);
        ui.painter()
            .circle_filled(pos2(rect.left() + 12.0, cy), 3.0, p.dir_l2a);
        ui.painter().galley(
            pos2(rect.left() + 21.0, cy - galley.size().y / 2.0),
            galley,
            p.text,
        );
    }

    /// «Журналы»: открыть журнал на просмотр или папку журналов в проводнике.
    ///
    /// Во время записи журнал не открыть: лента занята сессией. Пункт остаётся
    /// видимым, но неактивным, и подсказывает почему.
    fn journals_menu(&mut self, ui: &mut Ui) {
        let p = self.pal();
        let button =
            widgets::link_button(ui, &p, Icon::FolderOpen, 15.0, t!("header.journals"), false);
        let busy = self.session_busy();
        let mut open_journal = false;
        let mut open_folder = false;
        egui::Popup::menu(&button)
            .id(egui::Id::unique("bt-journals-menu"))
            .width(224.0)
            .gap(4.0)
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let item = widgets::menu_item(ui, &p, t!("header.openJournal"), !busy);
                if busy {
                    item.clone().on_hover_text(t!("header.openJournalBusy"));
                }
                open_journal = item.clicked();
                open_folder = widgets::menu_item(ui, &p, t!("header.openFolder"), true).clicked();
            });
        let ctx = ui.ctx().clone();
        if open_journal {
            self.pick_journal(&ctx);
        }
        if open_folder {
            self.open_log_dir(&ctx);
        }
    }

    /// Шапка экранов: бренд, «Журналы», переключатель темы, кнопки окна.
    pub(crate) fn home_title_bar(&mut self, ui: &mut Ui) {
        self.title_bar(
            ui,
            |app, ui| {
                app.brand(ui);
                app.journals_menu(ui);
            },
            |app, ui| {
                let p = app.pal();
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
}

fn toggle_maximized(ctx: &egui::Context) {
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
}

/// Без системной рамки у окна нет краёв: изменение размера — полосами 5px по периметру.
///
/// Полосы — отдельные области слоя `Foreground` с `Sense::drag()`: они выше шапки и
/// кнопок окна, поэтому нажатие на краю не достаётся ни «Закрыть» под углом, ни
/// перетаскиванию и двойному клику шапки под верхней кромкой. Вызывается до отрисовки
/// экрана.
pub fn resize_edges(ctx: &egui::Context) {
    let (maximized, rect, pos, pressed) = ctx.input(|i| {
        (
            i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false),
            i.content_rect(),
            i.pointer.hover_pos(),
            i.pointer.primary_pressed(),
        )
    });
    if maximized {
        return;
    }
    let strips = [
        ("top", rect.with_max_y(rect.top() + RESIZE_EDGE)),
        ("bottom", rect.with_min_y(rect.bottom() - RESIZE_EDGE)),
        ("left", rect.with_max_x(rect.left() + RESIZE_EDGE)),
        ("right", rect.with_min_x(rect.right() - RESIZE_EDGE)),
    ];
    let mut on_strip = false;
    for (name, strip) in strips {
        let response = egui::Area::new(egui::Id::unique(("bt-resize-edge", name)))
            .order(egui::Order::Foreground)
            .fixed_pos(strip.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.allocate_exact_size(strip.size(), Sense::drag()).1
            })
            .inner;
        on_strip |= response.hovered();
    }
    let Some(pos) = pos.filter(|_| on_strip) else {
        return;
    };
    let l = pos.x < rect.left() + RESIZE_EDGE;
    let r = pos.x > rect.right() - RESIZE_EDGE;
    let t = pos.y < rect.top() + RESIZE_EDGE;
    let b = pos.y > rect.bottom() - RESIZE_EDGE;
    let (dir, cursor) = match (l, r, t, b) {
        (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNwSe),
        (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeNwSe),
        (_, true, true, _) => (ResizeDirection::NorthEast, CursorIcon::ResizeNeSw),
        (true, _, _, true) => (ResizeDirection::SouthWest, CursorIcon::ResizeNeSw),
        (true, ..) => (ResizeDirection::West, CursorIcon::ResizeHorizontal),
        (_, true, ..) => (ResizeDirection::East, CursorIcon::ResizeHorizontal),
        (_, _, true, _) => (ResizeDirection::North, CursorIcon::ResizeVertical),
        (.., true) => (ResizeDirection::South, CursorIcon::ResizeVertical),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}
