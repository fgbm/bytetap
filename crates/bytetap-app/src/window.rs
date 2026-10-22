//! Рамка окна: своя отрисовка в 1 px и системное скругление углов Windows 11.
//!
//! Окно у bytetap без системных декораций (`decorations: false`), поэтому на белом
//! фоне оно сливается с фоном: владелец попросил рамку. Решение — два слоя:
//!
//! - Своя рамка 1 px цветом `--bt-border` по всему периметру окна. Её рисует egui
//!   последней в кадре, поэтому она одинакова на всех ОС и в обеих темах, видна на
//!   всех экранах и не зависит от того, активно ли окно, и её видно на снимках.
//!   Развёрнутое окно рамки не получает: у него периметр уходит за край экрана.
//! - На Windows 11 DWM скругляет углы (`DWMWA_WINDOW_CORNER_PREFERENCE`), и своя
//!   рамка повторяет его радиус (`DWM_CORNER_RADIUS`): квадратная линия поверх
//!   скругления оставила бы квадратные углы. Свою рамку поверх скругления DWM
//!   не срезает — линия идёт по внутреннему краю окна и совпадает с его дугой.
//!
//! Рамку DWM (`DWMWA_BORDER_COLOR`) приложение не заказывает: DWM рисует её только у
//! активного окна, а при смене активности сбрасывает цвет на системный — в документации
//! так и написано: «приложение само отвечает за цвет рамки при смене состояния, например
//! активности окна». У окна без системной рамки системный цвет на белом фоне почти не
//! виден, и рамка «пропадала», как только фокус забирало чужое окно: диалог брандмауэра
//! Windows при первом прослушивании порта в режиме TCP-сервера (жалоба владельца,
//! Windows 11 22000). Поэтому цвет DWM получает `DWMWA_COLOR_NONE` — DWM рамку не
//! рисует вовсе, а периметр всегда рисует приложение своим цветом. Атрибуты DWM
//! подтверждаются повторно при смене активности окна, и это пишется в журнал запуска.
//!
//! Тень окна — системная: egui-winit включает тень для окна без декораций
//! (`with_undecorated_shadow`). `DwmExtendFrameIntoClientArea` с полями в 1 px даёт
//! ту же рамку средствами DWM, но она не рисуется над содержимым окна, поэтому своя
//! отрисовка основная, а вызов DWM оставлен как запасной путь.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, CornerRadius, Stroke};
use eframe::wgpu::rwh::RawWindowHandle;

#[cfg(windows)]
use crate::diag;
use crate::theme::Palette;

/// Толщина своей рамки окна в точках: ровно один физический пиксель на любом
/// масштабе экрана, в том числе меньше 100 % (U1-AC26). Токен `--bt-border` — цвет,
/// не толщина.
pub fn frame_width(pixels_per_point: f32) -> f32 {
    // Масштаб приходит от winit и в норме положителен; нулевой или странный считаем
    // единицей, чтобы рамка не стала бесконечной.
    1.0 / scale(pixels_per_point)
}

/// Масштаб экрана, пригодный для расчётов: нулевой и нечисловой считаются единицей.
fn scale(pixels_per_point: f32) -> f32 {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    }
}

/// Радиус скругления углов, которое рисует DWM на Windows 11, в физических пикселях.
///
/// Своя рамка повторяет этот радиус: иначе линия легла бы по квадратному периметру, и
/// углы окна выглядели бы квадратными. Радиус переводится в точки по масштабу экрана.
pub const DWM_CORNER_RADIUS: f32 = 8.0;

/// DWM скругляет углы этого окна: своя рамка рисуется со скруглением того же радиуса.
static DWM_ROUNDED: AtomicBool = AtomicBool::new(false);

/// Результат настройки окна средствами DWM: что именно удалось и что нет.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DwmSetup {
    /// Хотя бы один вызов DWM прошёл.
    pub applied: bool,
    /// DWM скругляет углы окна: своя рамка повторяет его радиус.
    pub rounded: bool,
}

impl DwmSetup {
    /// Настройка не удалась: рамку рисует приложение с квадратными углами.
    const NONE: Self = Self {
        applied: false,
        rounded: false,
    };
}

/// Поддерживает ли система скругление окон DWM: Windows 11 build 22000 и новее.
///
/// `GetVersionExW` без манифеста совместимости отдаёт 6.2 на любой Windows 10 и новее,
/// поэтому номер сборки спрашивается у `RtlGetVersion` — она возвращает настоящий.
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

/// Настроить окно средствами DWM: скругление углов и выключенную рамку DWM.
///
/// Вызывается после первого кадра, когда окно уже видимо: атрибуты, заданные до того,
/// как окно показано, DWM теряет при первой отрисовке — рамка появлялась только после
/// смены темы (U1-AC18). Результат пишется в журнал запуска: без него не понять,
/// применились ли настройки на этой системе.
#[cfg(windows)]
pub fn setup_dwm(handle: RawWindowHandle, border: Color32) -> DwmSetup {
    use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows_sys::Win32::UI::Controls::MARGINS;

    let Some(hwnd) = hwnd(handle) else {
        diag::info("рамка окна: обработчик окна не Win32 — DWM не вызывается");
        return DwmSetup::NONE;
    };

    let mut setup = DwmSetup::NONE;

    // Скругление: DWM рисует углы сам, своя рамка повторяет их радиус. На Windows 10
    // атрибута нет — вызов вернёт ошибку.
    let corner = set_corner_preference(hwnd);
    if corner == 0 {
        setup.applied = true;
        setup.rounded = true;
        diag::info("рамка окна: DWM скругляет углы (DWMWA_WINDOW_CORNER_PREFERENCE)");
    } else {
        diag::info(format!(
            "рамка окна: DWM не скругляет углы (HRESULT {corner:#x}) — рамка приложения с квадратными углами"
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

    if setup.rounded {
        DWM_ROUNDED.store(true, Ordering::Relaxed);
        hide_dwm_border(hwnd, border, true);
    }
    setup
}

#[cfg(not(windows))]
pub fn setup_dwm(_handle: RawWindowHandle, _border: Color32) -> DwmSetup {
    DwmSetup::NONE
}

/// Повторная настройка окна после смены активности: скругление и скрытая рамка DWM.
///
/// DWM за свои атрибуты при смене состояния окна не отвечает: цвет рамки на такое
/// состояние выставляет приложение (документация `DWMWA_BORDER_COLOR`). Своя рамка от
/// активности не зависит, но состояние DWM подтверждается повторно, и это видно в
/// журнале запуска.
pub fn reapply_dwm(handle: RawWindowHandle, border: Color32) {
    #[cfg(windows)]
    {
        let Some(hwnd) = hwnd(handle) else {
            return;
        };
        let _ = set_corner_preference(hwnd);
        hide_dwm_border(hwnd, border, true);
    }
    #[cfg(not(windows))]
    {
        let _ = (handle, border);
    }
}

/// Обработчик окна Win32; вне Windows и у чужого обработчика — `None`.
#[cfg(windows)]
fn hwnd(handle: RawWindowHandle) -> Option<windows_sys::Win32::Foundation::HWND> {
    let RawWindowHandle::Win32(window) = handle else {
        return None;
    };
    Some(window.hwnd.get() as windows_sys::Win32::Foundation::HWND)
}

/// Скругление углов средствами DWM: 0 — принято, иначе HRESULT отказа.
#[cfg(windows)]
fn set_corner_preference(hwnd: windows_sys::Win32::Foundation::HWND) -> i32 {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };

    let corner = DWMWCP_ROUND;
    // SAFETY: атрибут — целое число, размер совпадает с типом.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &corner as *const i32 as *const std::ffi::c_void,
            u32::try_from(std::mem::size_of::<i32>()).unwrap_or_default(),
        )
    }
}

/// Рамку рисует приложение: DWM получает `DWMWA_COLOR_NONE` — своей рамки он не рисует.
///
/// Так периметр не зависит от активности окна: рамку DWM видно только у активного окна,
/// а после смены активности она получает системный цвет, почти невидимый на белом фоне
/// (см. заголовок модуля). В журнал запуска идёт цвет, которым рамку рисует приложение.
#[cfg(windows)]
fn hide_dwm_border(hwnd: windows_sys::Win32::Foundation::HWND, border: Color32, log: bool) {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DwmSetWindowAttribute,
    };

    let color = DWMWA_COLOR_NONE;
    // SAFETY: атрибут — целое число, размер совпадает с типом.
    let ok = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            &color as *const u32 as *const std::ffi::c_void,
            u32::try_from(std::mem::size_of::<u32>()).unwrap_or_default(),
        )
    };
    if !log {
        return;
    }
    if ok == 0 {
        diag::info(format!(
            "рамка окна: DWM-рамка скрыта (DWMWA_COLOR_NONE), свою рисует приложение цветом {}",
            hex_color(border)
        ));
    } else {
        diag::error(format!(
            "рамка окна: DWM не принял DWMWA_COLOR_NONE: HRESULT {ok:#x}"
        ));
    }
}

/// Скругляет ли углы этого окна DWM: своя рамка повторяет радиус скругления.
#[must_use]
pub fn dwm_rounded() -> bool {
    DWM_ROUNDED.load(Ordering::Relaxed)
}

/// Углы окна скругляет DWM: Windows 11 сборки 22000 и новее, либо DWM уже настроен.
#[must_use]
pub fn rounded_corners() -> bool {
    dwm_rounded() || dwm_expected()
}

/// Скругление своей рамки в точках: радиус DWM на масштабе экрана.
#[must_use]
pub fn corner_radius(pixels_per_point: f32) -> CornerRadius {
    let points = DWM_CORNER_RADIUS / scale(pixels_per_point);
    // CornerRadius — целые точки; радиус меньше пикселя не скругляет ничего.
    CornerRadius::same(points.round().clamp(0.0, u8::MAX as f32) as u8)
}

/// Цвет рамки в виде `#RRGGBB`: по этой строке смоук CI проверяет журнал запуска.
#[must_use]
pub fn hex_color(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// Цвет рамки окна: токен `--bt-border` темы.
#[must_use]
pub fn frame_color(p: &Palette) -> Color32 {
    p.border
}

/// Рисует рамку окна 1 px по периметру содержимого.
///
/// Рамка идёт по внутреннему краю окна: линия в 1 px не съедает содержимое. `rounded` —
/// углы окна скругляет DWM (Windows 11): своя рамка повторяет его радиус, иначе углы
/// выглядели бы квадратными. Вызывается последней в кадре, поверх экрана: панели,
/// трасса и лента рамку не перекрывают.
pub fn draw_frame(ui: &mut egui::Ui, p: &Palette, rounded: bool) {
    let rect = ui.max_rect();
    let radius = if rounded {
        corner_radius(ui.pixels_per_point())
    } else {
        CornerRadius::ZERO
    };
    ui.painter().rect_stroke(
        rect,
        radius,
        Stroke::new(frame_width(ui.pixels_per_point()), frame_color(p)),
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

    /// Своя рамка — 1 физический пиксель: на 125 и 150 % это 0.8 и 0.67 точки, а на
    /// 75 % (масштаб меньше 100 %) — 1.33 точки, а не целая точка.
    #[test]
    fn the_frame_is_one_physical_pixel_wide() {
        for ppp in [0.75, 1.0, 1.25, 1.5, 2.0] {
            let width = frame_width(ppp);
            assert!(
                (width * ppp - 1.0).abs() < 1e-6,
                "на {ppp} рамка {width} точки — {} физических пикселей",
                width * ppp
            );
        }
        // Масштаб от системы всегда положителен; нулевой не делает рамку бесконечной.
        assert_eq!(frame_width(0.0), 1.0);
        assert_eq!(frame_width(f32::NAN), 1.0);
    }

    /// Радиус своей рамки — радиус скругления DWM: 8 физических пикселей на любом
    /// масштабе, и на 150 % это 5 точек, а не 8.
    #[test]
    fn the_frame_follows_the_dwm_corner_radius() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            let points = f32::from(corner_radius(ppp).nw);
            assert!(
                (points * ppp - DWM_CORNER_RADIUS).abs() <= ppp,
                "на {ppp} радиус {points} точки — {} физических пикселей",
                points * ppp
            );
        }
        assert_eq!(corner_radius(0.0).nw, DWM_CORNER_RADIUS as u8);
    }

    /// Цвет рамки идёт в журнал запуска как `#RRGGBB`: по этой строке смоук CI видит,
    /// каким цветом рамку рисует приложение.
    #[test]
    fn the_frame_colour_is_written_as_a_hex_token() {
        assert_eq!(hex_color(Color32::from_rgb(1, 2, 3)), "#010203");
        assert_eq!(hex_color(Color32::from_rgb(0xab, 0xcd, 0xef)), "#abcdef");
    }
}
