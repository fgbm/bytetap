//! Мелкие виджеты в стиле UI v2.

use eframe::egui::{Response, Role, Sense, Ui, Vec2, WidgetInfo};

use crate::icons::{self, Icon};
use crate::theme::{Palette, radius, tokens::RADIUS_CONTROL};

/// Кнопка-иконка 28×28: muted, при наведении — фон `hover` и цвет текста. Подпись —
/// подсказка и имя для AccessKit.
pub fn icon_button(ui: &mut Ui, p: &Palette, icon: Icon, icon_size: f32, label: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), label));
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.painter()
            .rect_filled(rect, radius(RADIUS_CONTROL), p.hover);
    }
    icons::paint(
        ui,
        icon,
        rect.center(),
        icon_size,
        if hovered { p.text } else { p.text_muted },
    );
    response.on_hover_text(label)
}
