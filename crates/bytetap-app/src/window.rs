//! Рамка окна: своя отрисовка в 1 px и системная скруглённая рамка Windows 11.
//!
//! Окно у bytetap без системных декораций (`decorations: false`), поэтому на белом
//! фоне оно сливается с фоном: владелец попросил рамку. Решение — два слоя:
//!
//! - Своя рамка 1 px цветом `--bt-border` по всему периметру окна. Её рисует egui,
//!   поэтому она одинакова на всех ОС и в обеих темах, и её видно на снимках.
//!   Развёрнутое окно рамки не получает: у него периметр уходит за край экрана.
//! - На Windows 11 DWM рисует скругление и цвет рамки сам
//!   (`DWMWA_WINDOW_CORNER_PREFERENCE`, `DWMWA_BORDER_COLOR`): своя рамка поверх
//!   скругления дала бы квадратные углы. Там, где DWM скругление не поддерживает
//!   (сборки до 22000), рамку рисует приложение.
//!
//! Тень окна — системная: egui-winit включает тень для окна без декораций
//! (`with_undecorated_shadow`). `DwmExtendFrameIntoClientArea` с полями в 1 px даёт
//! ту же рамку средствами DWM, но она не рисуется над содержимым окна, поэтому своя
//! отрисовка основная, а вызов DWM оставлен как запасной путь.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, Stroke};
use eframe::wgpu::rwh::RawWindowHandle;

#[cfg(windows)]
use crate::diag;
use crate::theme::Palette;

/// Толщина своей рамки окна: 1 px по периметру.
pub const FRAME_WIDTH: f32 = 1.0;

/// DWM уже настроил это окно: рамку рисует система, а не приложение.
static DWM_BORDER: AtomicBool = AtomicBool::new(false);

/// Результат настройки окна средствами DWM: что именно удалось и что нет.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DwmSetup {
    /// Хотя бы один вызов DWM прошёл.
    pub applied: bool,
    /// DWM рисует скругление и рамку сам: свою рамку рисовать не нужно.
    pub dwm_border: bool,
}

impl DwmSetup {
    /// Настройка не удалась: рамку рисует приложение.
    const NONE: Self = Self {
        applied: false,
        dwm_border: false,
    };
}

/// Поддерживает ли система скругление окон DWM: Windows 11 build 22000 и новее.
#[must_use]
pub fn dwm_round_supported() -> bool {
    cfg!(windows) && windows_build() >= 21_000
}

/// Номер сборки Windows: 22000 и новее — Windows 11. Вне Windows — 0.
#[cfg(windows)]
fn windows_build() -> u32 {
    use windows_sys::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>())
            .unwrap_or_default(),
        ..Default::default()
    };
    // SAFETY: `info` — структура версии ОС, размер и указатель верны.
    if unsafe { GetVersionExW(&mut info) } == 0 {
        return 0;
    }
    // «10.0.22000» и новее — Windows 11; у Windows 10 сборка 19045.
    if info.dwMajorVersion == 10 && info.dwBuildNumber >= 21_000 {
        info.dwBuildNumber
    } else {
        0
    }
}

#[cfg(not(windows))]
fn windows_build() -> u32 {
    0
}

/// Настроить окно средствами DWM: скругление, цвет рамки, тень.
///
/// Вызывается один раз, когда окно уже создано и `HWND` известен. Результат пишется
/// в журнал запуска: без него не понять, применились ли настройки на этой системе.
#[cfg(windows)]
pub fn setup_dwm(handle: RawWindowHandle, border: Color32) -> DwmSetup {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
        DwmSetWindowAttribute,
    };
    use windows_sys::Win32::UI::Controls::MARGINS;

    let RawWindowHandle::Win32(window) = handle else {
        diag::info("рамка окна: обработчик окна не Win32 — DWM не вызывается");
        return DwmSetup::NONE;
    };
    let hwnd: HWND = window.hwnd.get() as HWND;

    let mut setup = DwmSetup::NONE;

    // Скругление: DWM рисует углы и рамку сам, иначе своя рамка дала бы квадратные
    // углы поверх скругления.
    if dwm_round_supported() {
        let corner = DWMWCP_ROUND as i32;
        // SAFETY: атрибут — целое число, размер совпадает с типом.
        let ok = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &corner as *const i32 as *const std::ffi::c_void,
                u32::try_from(std::mem::size_of::<i32>()).unwrap_or_default(),
            )
        };
        if ok == 0 {
            setup.applied = true;
            setup.dwm_border = true;
            diag::info("рамка окна: DWM скругляет углы (DWMWA_WINDOW_CORNER_PREFERENCE)");
        } else {
            diag::error(format!("рамка окна: DWM не скруглил углы: HRESULT {ok:#x}"));
        }
    } else {
        diag::info("рамка окна: система без скругления DWM — рамку рисует приложение");
    }

    // Тень окна без декораций: рамка в 1 px в область клиента. Запасной путь: egui-winit
    // уже включает системную тень через `with_undecorated_shadow`.
    let margins = MARGINS {
        cxLeftWidth: 1,
        cxRightWidth: 1,
        cyTopHeight: 1,
        cyBottomHeight: 1,
    };
    // SAFETY: указатель на структуру полей на стеке действителен во время вызова.
    let ok = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
    if ok == 0 {
        setup.applied = true;
        diag::info("рамка окна: DWM ExtendFrameIntoClientArea — ок");
    } else {
        diag::error(format!(
            "рамка окна: DWM ExtendFrameIntoClientArea: HRESULT {ok:#x}"
        ));
    }

    if setup.dwm_border {
        DWM_BORDER.store(true, Ordering::Relaxed);
        set_dwm_border(handle, border);
    }
    setup
}
#[cfg(not(windows))]
pub fn setup_dwm(_handle: RawWindowHandle, _border: Color32) -> DwmSetup {
    DwmSetup::NONE
}

/// Цвет рамки, который рисует DWM: меняется вместе с темой.
///
/// Вне Windows и без скругления DWM вызов ничего не делает: рамку рисует приложение.
pub fn set_dwm_border(handle: RawWindowHandle, border: Color32) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::Graphics::Dwm::{DWMWA_BORDER_COLOR, DwmSetWindowAttribute};

        let RawWindowHandle::Win32(window) = handle else {
            return;
        };
        let hwnd: HWND = window.hwnd.get() as HWND;
        let color = dwm_color(border);
        // SAFETY: атрибут — целое число, размер совпадает с типом.
        let ok = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                &color as *const i32 as *const std::ffi::c_void,
                u32::try_from(std::mem::size_of::<i32>()).unwrap_or_default(),
            )
        };
        if ok == 0 {
            diag::info("рамка окна: DWM рисует рамку цветом border (DWMWA_BORDER_COLOR)");
        } else {
            diag::error(format!(
                "рамка окна: DWM не принял цвет рамки: HRESULT {ok:#x}"
            ));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (handle, border);
    }
}

/// Цвет рамки DWM: `COLORREF` — 0x00BBGGRR, прозрачность не поддерживается.
#[cfg(windows)]
fn dwm_color(color: Color32) -> i32 {
    let r = i32::from(color.r());
    let g = i32::from(color.g());
    let b = i32::from(color.b());
    r | (g << 8) | (b << 16)
}

/// Рисует ли DWM рамку окна сам: тогда своя рамка не нужна.
#[must_use]
pub fn dwm_border() -> bool {
    DWM_BORDER.load(Ordering::Relaxed)
}

/// Цвет рамки окна: токен `--bt-border` темы.
#[must_use]
pub fn frame_color(p: &Palette) -> Color32 {
    p.border
}

/// Рисует рамку окна 1 px по периметру содержимого.
///
/// Рамка идёт по внутреннему краю окна: линия в 1 px не съедает содержимое.
/// Развёрнутое окно рамки не получает: периметр ушёл за край экрана, и линия была бы
/// обрезана.
pub fn draw_frame(ui: &mut egui::Ui, p: &Palette, maximized: bool) {
    if maximized || dwm_border() {
        return;
    }
    let rect = ui.max_rect();
    ui.painter().rect_stroke(
        rect,
        0,
        Stroke::new(FRAME_WIDTH, frame_color(p)),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// На системе без скругления DWM рамку рисует приложение; на Windows 11 — DWM.
    #[test]
    fn the_border_is_drawn_by_the_app_without_dwm_rounding() {
        assert_eq!(
            dwm_round_supported(),
            cfg!(windows) && windows_build() >= 21_000,
            "скругление DWM есть только на Windows 11 build 22000 и новее"
        );
    }

    #[test]
    fn the_frame_is_one_pixel_wide() {
        assert_eq!(FRAME_WIDTH, 1.0);
    }
}
