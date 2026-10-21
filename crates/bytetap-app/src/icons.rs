//! Иконки Lucide (`lucide-static`, ISC — `assets/icons/LICENSE`) встроены в exe, SVG
//! растеризует `egui_extras` (resvg).
//!
//! В SVG `currentColor` заменён на белый, цвет задаёт `tint`.

use eframe::egui::{self, Color32, ImageSource, Rect, Ui, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Copy,
    Minus,
    Moon,
    Square,
    Sun,
    X,
}

impl Icon {
    pub fn source(self) -> ImageSource<'static> {
        match self {
            Self::Copy => egui::include_image!("../assets/icons/copy.svg"),
            Self::Minus => egui::include_image!("../assets/icons/minus.svg"),
            Self::Moon => egui::include_image!("../assets/icons/moon.svg"),
            Self::Square => egui::include_image!("../assets/icons/square.svg"),
            Self::Sun => egui::include_image!("../assets/icons/sun.svg"),
            Self::X => egui::include_image!("../assets/icons/x.svg"),
        }
    }
}

/// Рисует иконку `size`×`size` по центру `center`.
pub fn paint(ui: &Ui, icon: Icon, center: egui::Pos2, size: f32, color: Color32) {
    // Целые точки: при дробном положении растр иконки размывается.
    let rect = Rect::from_min_size(
        (center - Vec2::splat(size / 2.0)).round(),
        Vec2::splat(size),
    );
    egui::Image::new(icon.source())
        .tint(color)
        .fit_to_exact_size(Vec2::splat(size))
        .paint_at(ui, rect);
}
