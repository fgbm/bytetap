//! Настольное приложение bytetap на egui, eframe и wgpu (ADR 0003): один exe без WebView2.

mod app;
pub mod background;
pub mod diag;
pub mod errors;
pub mod form;
pub mod gpu;
pub mod i18n;
pub mod icons;
pub mod scenarios;
pub mod settings;
mod step;
pub mod theme;
mod titlebar;
pub mod widgets;

pub use app::{BytetapApp, Fixtures, Screen, resolve_dark};

/// Размер окна при первом запуске (U1-AC3).
pub const WINDOW_SIZE: [f32; 2] = [1200.0, 800.0];
/// Наименьший размер окна.
pub const WINDOW_MIN_SIZE: [f32; 2] = [720.0, 560.0];
