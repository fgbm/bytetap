//! Иконки Lucide (`lucide-static`, ISC — `assets/icons/LICENSE`) встроены в exe, SVG
//! растеризует `egui_extras` (resvg).
//!
//! В SVG `currentColor` заменён на белый, цвет задаёт `tint`.

use eframe::egui::{self, Color32, ImageSource, Rect, Ui, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    ArrowDown,
    ArrowLeft,
    Check,
    ChevronDown,
    ChevronRight,
    CircleAlert,
    Copy,
    Ellipsis,
    FileText,
    FolderOpen,
    Info,
    Minus,
    MonitorDown,
    Moon,
    Network,
    Pen,
    RefreshCw,
    Square,
    Sun,
    TriangleAlert,
    Usb,
    X,
}

impl Icon {
    pub fn source(self) -> ImageSource<'static> {
        match self {
            Self::ArrowDown => egui::include_image!("../assets/icons/arrow-down.svg"),
            Self::ArrowLeft => egui::include_image!("../assets/icons/arrow-left.svg"),
            Self::Check => egui::include_image!("../assets/icons/check.svg"),
            Self::ChevronDown => egui::include_image!("../assets/icons/chevron-down.svg"),
            Self::ChevronRight => egui::include_image!("../assets/icons/chevron-right.svg"),
            Self::CircleAlert => egui::include_image!("../assets/icons/circle-alert.svg"),
            Self::Copy => egui::include_image!("../assets/icons/copy.svg"),
            Self::Ellipsis => egui::include_image!("../assets/icons/ellipsis.svg"),
            Self::FileText => egui::include_image!("../assets/icons/file-text.svg"),
            Self::FolderOpen => egui::include_image!("../assets/icons/folder-open.svg"),
            Self::Info => egui::include_image!("../assets/icons/info.svg"),
            Self::Minus => egui::include_image!("../assets/icons/minus.svg"),
            Self::MonitorDown => egui::include_image!("../assets/icons/monitor-down.svg"),
            Self::Moon => egui::include_image!("../assets/icons/moon.svg"),
            Self::Network => egui::include_image!("../assets/icons/network.svg"),
            Self::Pen => egui::include_image!("../assets/icons/pen.svg"),
            Self::RefreshCw => egui::include_image!("../assets/icons/refresh-cw.svg"),
            Self::Square => egui::include_image!("../assets/icons/square.svg"),
            Self::Sun => egui::include_image!("../assets/icons/sun.svg"),
            Self::TriangleAlert => egui::include_image!("../assets/icons/triangle-alert.svg"),
            Self::Usb => egui::include_image!("../assets/icons/usb.svg"),
            Self::X => egui::include_image!("../assets/icons/x.svg"),
        }
    }
}

/// Рисует иконку `size`×`size` по центру `center`.
pub fn paint(ui: &Ui, icon: Icon, center: egui::Pos2, size: f32, color: Color32) {
    // Проба цены кадра (`probe.rs`): кадр без иконок — столько стоит их растр в кадре.
    #[cfg(feature = "test-hooks")]
    if crate::probe::enabled() && crate::probe::mode() == crate::probe::NO_ICONS {
        return;
    }
    // Целые физические пиксели: при дробном положении растр иконки размывается, а на
    // масштабе 125 и 150 % целая точка — это 1.25 и 1.5 физического пикселя (U1-AC26).
    let ppp = ui.pixels_per_point();
    let align = |value: f32| (value * ppp).round() / ppp;
    let rect = Rect::from_min_size(
        egui::pos2(align(center.x - size / 2.0), align(center.y - size / 2.0)),
        Vec2::splat(size),
    );
    egui::Image::new(icon.source())
        .tint(color)
        .fit_to_exact_size(Vec2::splat(size))
        .paint_at(ui, rect);
}
