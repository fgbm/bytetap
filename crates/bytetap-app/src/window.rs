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
//! - Пока DWM не настроен (первый кадр), на Windows 11 своя рамка тоже не рисуется:
//!   скругление появится следующим кадром, и квадратные углы мелькнули бы на один
//!   кадр. Ожидание скругления берётся из сборки ОС (`RtlGetVersion`), а не из ответа
//!   DWM: ответа на первом кадре ещё нет.
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
///
/// `GetVersionExW` без манифеста совместимости отдаёт 6.2 на любой Windows 10 и новее,
/// поэтому номер сборки спрашивается у `RtlGetVersion` — она возвращает настоящий.
/// Ответ нужен до первого вызова DWM: в самом первом кадре свою рамку рисовать нельзя,
/// если система вот-вот нарисует скруглённую, иначе на кадр мелькают квадратные углы.
#[must_use]
pub fn dwm_expected() -> bool {
    windows_build().is_some_and(round_from_build)
}

/// Номер сборки Windows без манифеста совместимости; вне Windows — `None`.
#[cfg(windows)]
fn windows_build() -> Option<u32> {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>())
            .unwrap_or_default(),
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: структура на стеке нужного размера; `RtlGetVersion` заполняет её поля.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    // NTSTATUS: 0 — успех, остальное — отказ вызова.
    (status == 0 && info.dwBuildNumber > 0).then_some(info.dwBuildNumber)
}

#[cfg(not(windows))]
fn windows_build() -> Option<u32> {
    None
}

/// Windows 11 ли сборка: скругление окон DWM появилось в сборке 22000.
#[must_use]
fn round_from_build(build: u32) -> bool {
    build >= 22_000
}

/// Настроить окно средствами DWM: скругление, расширение рамки и цвет рамки.
///
/// Вызывается после первого кадра, когда окно уже видимо: атрибуты, заданные до того,
/// как окно показано, DWM теряет при первой отрисовке — рамка появлялась только после
/// смены темы (U1-AC18). Результат пишется в журнал запуска: без него не понять,
/// применились ли настройки на этой системе.
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
    // углы поверх скругления. На Windows 10 атрибута нет — вызов вернёт ошибку.
    let corner = DWMWCP_ROUND;
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
        diag::info(format!(
            "рамка окна: DWM не скругляет углы (HRESULT {ok:#x}) — рамку рисует приложение"
        ));
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

/// Цвет рамки в виде `#RRGGBB`: по этой строке смоук CI проверяет журнал запуска.
#[must_use]
pub fn hex_color(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Цвет рамки, который рисует DWM: меняется вместе с темой.
///
/// Вне Windows и без скругления DWM вызов ничего не делает: рамку рисует приложение.
/// Результат пишется в журнал запуска — первая установка после первого кадра и есть
/// та, которую проверяет смоук.
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
            diag::info(format!("цвет рамки DWM принят: {}", hex_color(border)));
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
/// `system` — рамку рисует система: DWM уже настроен, DWM ожидается на Windows 11
/// (первый кадр, до `setup_dwm`), или окно развёрнуто и его периметр ушёл за край экрана.
pub fn draw_frame(ui: &mut egui::Ui, p: &Palette, system: bool) {
    if system {
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

    /// Скругление DWM ожидается на Windows 11: сборка 22000 и новее.
    #[test]
    fn the_dwm_rounding_is_expected_from_windows_11() {
        assert!(!round_from_build(19_045), "Windows 10 22H2");
        assert!(!round_from_build(20_348), "Windows Server 2022");
        assert!(round_from_build(22_000), "первая сборка Windows 11");
        assert!(round_from_build(26_100), "Windows 11 24H2");
        // Вне Windows скругления не ждём: там рамку рисует приложение.
        #[cfg(not(windows))]
        assert!(!dwm_expected());
    }

    #[test]
    fn the_frame_is_one_pixel_wide() {
        assert_eq!(FRAME_WIDTH, 1.0);
    }

    /// Цвет рамки идёт в журнал запуска как `#RRGGBB`: по этой строке смоук CI видит,
    /// что DWM принял цвет без переключения темы.
    #[test]
    fn the_dwm_colour_is_written_as_a_hex_token() {
        assert_eq!(hex_color(Color32::from_rgb(1, 2, 3)), "#010203");
        assert_eq!(hex_color(Color32::from_rgb(0xab, 0xcd, 0xef)), "#abcdef");
    }
}
