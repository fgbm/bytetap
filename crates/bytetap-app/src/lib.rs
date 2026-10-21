//! Настольное приложение bytetap на egui, eframe и wgpu (ADR 0003): один exe без WebView2.

mod app;
pub mod diag;
pub mod gpu;
pub mod i18n;
pub mod icons;
pub mod settings;
pub mod theme;
mod titlebar;
pub mod widgets;

pub use app::{BytetapApp, Screen, resolve_dark};

/// Размер окна при первом запуске (U1-AC3).
pub const WINDOW_SIZE: [f32; 2] = [1200.0, 800.0];
/// Наименьший размер окна.
pub const WINDOW_MIN_SIZE: [f32; 2] = [720.0, 560.0];
