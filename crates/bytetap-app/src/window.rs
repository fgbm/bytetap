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
//! Скругление бывает и не нужно: DWM не скругляет углы у развёрнутого и прижатого к
//! краю окна, а на программном растре (`gpu::software()`, ВМ без видеодрайвера) его
//! скругление не видно из-за цены кадра. Тогда приложение рисует квадратную рамку, а
//! DWM на программном растре получает `DWMWCP_DONOTROUND`, чтобы линия и угол окна
//! совпадали ([`rounded_frame`]).
//!
//! Рамку DWM (`DWMWA_BORDER_COLOR`) приложение не заказывает: по документации DWM не
//! перекрашивает её сам при смене состояния окна — «это забота приложения», — и без
//! своей настройки у окна без системной рамки остаётся системный цвет, почти невидимый
//! на белом фоне. Поэтому DWM получает `DWMWA_COLOR_NONE`: этот атрибут подавляет
//! отрисовку рамки DWM всегда, в любом состоянии окна, а периметр всегда рисует
//! приложение своим цветом. Атрибуты DWM подтверждаются повторно при смене активности
//! окна; в журнал запуска идут первая потеря и первое возвращение фокуса за запуск и
//! отказы HRESULT ([`activation_to_log`]).
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

/// Радиус скругления углов, которое рисует DWM на Windows 11, в физических пикселях
/// при 96 DPI: DWM масштабирует его линейно, на 125 и 150 % это 10 и 12 физических
/// пикселей.
///
/// Своя рамка повторяет этот радиус: иначе линия легла бы по квадратному периметру, и
/// углы окна выглядели бы квадратными. Радиус переводится в точки по масштабу экрана.
pub const DWM_CORNER_RADIUS: f32 = 8.0;

/// DWM скругляет углы этого окна: своя рамка рисуется со скруглением того же радиуса.
static DWM_ROUNDED: AtomicBool = AtomicBool::new(false);

/// DWM принял атрибут угла: `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE)`
/// вернул 0 — и для `DWMWCP_ROUND`, и для `DWMWCP_DONOTROUND`.
///
/// Скругления флаг не обещает (на Windows 10 атрибута нет), но говорит, что настройка
/// DWM дошла до окна: по нему кадровый цикл решает, подтверждать ли состояние DWM при
/// смене активности окна. `DwmSetup::applied` для этого не годится: на Windows 10 он
/// истинен из-за `DwmExtendFrameIntoClientArea`.
static DWM_CORNER_ACCEPTED: AtomicBool = AtomicBool::new(false);

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
/// смены темы (U1-AC18). Каждый вызов пишет в журнал запуска одну строку о своём
/// результате: без неё не понять, применились ли настройки на этой системе.
///
/// `software` — кадр рисует программный растр ([`crate::gpu::software`]): скругления
/// углов у такого окна не видно, поэтому DWM получает `DWMWCP_DONOTROUND`, а свою
/// рамку приложение рисует квадратной — линия и угол окна совпадают.
#[cfg(windows)]
pub fn setup_dwm(handle: RawWindowHandle, border: Color32, software: bool) -> DwmSetup {
    use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows_sys::Win32::UI::Controls::MARGINS;

    let Some(hwnd) = hwnd(handle) else {
        diag::error("рамка окна: обработчик окна не Win32 — DWM не вызывается");
        return DwmSetup::NONE;
    };

    let mut setup = DwmSetup::NONE;

    // Скругление: DWM рисует углы сам, своя рамка повторяет их радиус. На Windows 10
    // атрибута нет — вызов вернёт ошибку, и это штатный отказ, а не сбой: уровень
    // строки журнала зависит от того, ждали ли здесь скругления (`dwm_expected`).
    let corner = set_corner_preference(hwnd, software);
    if corner != 0 {
        if dwm_expected() {
            diag::error(format!(
                "рамка окна: DWM не принял {}: HRESULT {corner:#x} — рамку рисует приложение с квадратными углами",
                if software {
                    "DWMWCP_DONOTROUND"
                } else {
                    "DWMWCP_ROUND"
                }
            ));
        } else {
            diag::info(format!(
                "рамка окна: DWM не принял {}: HRESULT {corner:#x} — на этой Windows атрибута угла нет, рамку рисует приложение с квадратными углами",
                if software {
                    "DWMWCP_DONOTROUND"
                } else {
                    "DWMWCP_ROUND"
                }
            ));
        }
    } else if software {
        setup.applied = true;
        diag::info(
            "рамка окна: программный растр — углы квадратные (DWMWCP_DONOTROUND), рамку рисует приложение",
        );
    } else {
        setup.applied = true;
        setup.rounded = true;
        diag::info("рамка окна: DWM скругляет углы (DWMWA_WINDOW_CORNER_PREFERENCE)");
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
    }
    // Рамку рисует приложение, DWM — только скругление: `DWMWA_COLOR_NONE` подавляет
    // рамку DWM в любом состоянии окна.
    let hidden = hide_dwm_border(hwnd);
    if hidden == 0 {
        diag::info(format!(
            "рамка окна: DWM-рамка скрыта (DWMWA_COLOR_NONE), свою рисует приложение цветом {}",
            hex_color(border)
        ));
    } else {
        diag::error(format!(
            "рамка окна: DWM не принял DWMWA_COLOR_NONE: HRESULT {hidden:#x}"
        ));
    }
    setup
}

#[cfg(not(windows))]
pub fn setup_dwm(_handle: RawWindowHandle, _border: Color32, _software: bool) -> DwmSetup {
    DwmSetup::NONE
}

/// Повторная настройка окна после смены активности: скругление и скрытая рамка DWM.
///
/// DWM за свои атрибуты при смене состояния окна не отвечает: по документации
/// `DWMWA_BORDER_COLOR` цвет рамки на такое состояние выставляет приложение. Своя рамка
/// от активности не зависит, но состояние DWM подтверждается повторно. В журнал запуска
/// эта функция не пишет: что записать, решает кадровый цикл ([`activation_to_log`]) —
/// смена активности случается часто, и журнал не должен расти с ней.
///
/// Возвращается первый HRESULT отказа, `None` — все вызовы прошли.
pub fn reapply_dwm(handle: RawWindowHandle, software: bool) -> Option<i32> {
    #[cfg(windows)]
    {
        let hwnd = hwnd(handle)?;
        let corner = set_corner_preference(hwnd, software);
        let hidden = hide_dwm_border(hwnd);
        [corner, hidden].into_iter().find(|code| *code != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = (handle, software);
        None
    }
}

/// Пора ли записать в журнал запуска смену активности окна: первая потеря фокуса и
/// первое его возвращение за запуск. `logged` — что уже записано: `[потеря, возвращение]`.
///
/// Окно переключается часто (диалог брандмауэра, чужое окно, меню системы), и строка на
/// каждое переключение забивала бы журнал запуска: по двум строкам видно, что окно
/// теряло фокус и получило его обратно, и этого достаточно.
#[must_use]
pub fn activation_to_log(focused: bool, logged: &mut [bool; 2]) -> bool {
    let index = usize::from(focused);
    let first = !logged[index];
    logged[index] = true;
    first
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
///
/// `software` — кадр рисует программный растр: углы заказываются квадратными, чтобы
/// линия своей рамки совпадала с углом окна.
///
/// Итог вызова записывается в [`DWM_CORNER_ACCEPTED`]: по нему кадровый цикл решает,
/// подтверждать ли состояние DWM при смене активности окна.
#[cfg(windows)]
fn set_corner_preference(hwnd: windows_sys::Win32::Foundation::HWND, software: bool) -> i32 {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute,
    };

    let corner = if software {
        DWMWCP_DONOTROUND
    } else {
        DWMWCP_ROUND
    };
    // SAFETY: атрибут — целое число, размер совпадает с типом.
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &corner as *const i32 as *const std::ffi::c_void,
            u32::try_from(std::mem::size_of::<i32>()).unwrap_or_default(),
        )
    };
    // Флаг общий для обоих значений атрибута: важно, что DWM его принял, а не как.
    DWM_CORNER_ACCEPTED.store(result == 0, Ordering::Relaxed);
    result
}

/// Рамку рисует приложение: DWM получает `DWMWA_COLOR_NONE` — своей рамки он не рисует.
///
/// Так периметр не зависит от активности окна: `DWMWA_COLOR_NONE` подавляет отрисовку
/// рамки DWM в любом состоянии окна (см. заголовок модуля). Возвращается HRESULT: 0 —
/// принято, иначе отказ.
#[cfg(windows)]
fn hide_dwm_border(hwnd: windows_sys::Win32::Foundation::HWND) -> i32 {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DwmSetWindowAttribute,
    };

    let color = DWMWA_COLOR_NONE;
    // SAFETY: атрибут — целое число, размер совпадает с типом.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            &color as *const u32 as *const std::ffi::c_void,
            u32::try_from(std::mem::size_of::<u32>()).unwrap_or_default(),
        )
    }
}

/// Скругляет ли углы этого окна DWM: своя рамка повторяет радиус скругления.
#[must_use]
pub fn dwm_rounded() -> bool {
    DWM_ROUNDED.load(Ordering::Relaxed)
}

/// Принял ли DWM атрибут угла этого окна, любое из двух его значений.
///
/// Флаг взводится при настройке окна и при каждом повторе ([`reapply_dwm`]) по HRESULT
/// `DwmSetWindowAttribute`. Кадровый цикл по нему решает, повторять ли настройку DWM при
/// смене активности окна: там, где атрибута нет (Windows 10), повторять нечего, и строки
/// о подтверждении состояния DWM в журнале запуска не будет.
#[must_use]
pub fn dwm_corner_accepted() -> bool {
    DWM_CORNER_ACCEPTED.load(Ordering::Relaxed)
}

/// Скругляет ли своя рамка углы: только когда DWM скруглил углы именно этого окна, оно
/// не развёрнуто и не прижато к краю и кадр рисует не программный растр.
///
/// DWM не скругляет развёрнутое и прижатое к краю окно (`placement::arranged`): его углы
/// квадратные, и своя скруглённая линия торчала бы из угла окна. На программном растре
/// скругления не видно, поэтому там DWM получает `DWMWCP_DONOTROUND`, а линия идёт по
/// квадратному углу.
#[must_use]
pub fn rounded_frame(dwm_rounded: bool, arranged: bool, software: bool) -> bool {
    dwm_rounded && !arranged && !software
}

/// Скругление своей рамки в точках: радиус DWM, переведённый в точки по масштабу окна.
///
/// DWM скругляет углы в физических пикселях от масштаба системы: 8 px при 96 DPI, 10 на
/// 125 % и 12 на 150 %. `native_pixels_per_point` — масштаб окна без зума, поэтому без
/// зума радиус — 8 точек, а при зуме 200 % — 4 точки: линия по-прежнему идёт по дуге
/// окна, а не загибается внутрь.
#[must_use]
pub fn corner_radius(pixels_per_point: f32, native_pixels_per_point: f32) -> CornerRadius {
    let points = DWM_CORNER_RADIUS * scale(native_pixels_per_point) / scale(pixels_per_point);
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
/// углы окна скругляет DWM ([`rounded_frame`]): своя рамка повторяет его радиус, иначе
/// углы выглядели бы квадратными. Вызывается последней в кадре, поверх экрана: панели,
/// трасса и лента рамку не перекрывают.
pub fn draw_frame(ui: &mut egui::Ui, p: &Palette, rounded: bool) {
    let rect = ui.max_rect();
    let pixels_per_point = ui.pixels_per_point();
    let native = ui
        .ctx()
        .native_pixels_per_point()
        .unwrap_or(pixels_per_point);
    let radius = if rounded {
        corner_radius(pixels_per_point, native)
    } else {
        CornerRadius::ZERO
    };
    ui.painter().rect_stroke(
        rect,
        radius,
        Stroke::new(frame_width(pixels_per_point), frame_color(p)),
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

    /// Радиус своей рамки — радиус скругления DWM, а DWM масштабирует его линейно:
    /// 8 физических пикселей при 96 DPI, 10 на 125 % и 12 на 150 %, то есть 8 точек на
    /// любом масштабе без зума, а не 6.4 и 5.3 точки.
    #[test]
    fn the_frame_follows_the_dwm_corner_radius() {
        for (native, physical) in [(1.0, 8.0), (1.25, 10.0), (1.5, 12.0), (2.0, 16.0)] {
            let points = f32::from(corner_radius(native, native).nw);
            assert_eq!(points, DWM_CORNER_RADIUS, "на {native} радиус не 8 точек");
            assert_eq!(
                points * native,
                physical,
                "на {native} радиус {points} точки — не {physical} физических пикселей"
            );
        }
        // Зум меняет масштаб кадра, но не масштаб окна: радиус в точках падает так, что
        // физический остаётся радиусом DWM.
        for zoom in [0.5, 1.0, 1.5, 2.0] {
            let native = 1.25;
            let points = f32::from(corner_radius(native * zoom, native).nw);
            let physical = points * native * zoom;
            assert!(
                (physical - 10.0).abs() <= native * zoom,
                "зум {zoom}: радиус {points} точки — {physical} физических пикселей"
            );
        }
        // Масштаб от системы всегда положителен; нулевой даёт радиус в точках.
        assert_eq!(corner_radius(0.0, 1.0).nw, DWM_CORNER_RADIUS as u8);
        assert_eq!(
            corner_radius(f32::NAN, f32::NAN).nw,
            DWM_CORNER_RADIUS as u8
        );
    }

    /// Скругляет своя рамка углы только там, где DWM скруглил их у этого окна: у
    /// развёрнутого и прижатого к краю окна углы квадратные, как и на программном растре.
    #[test]
    fn the_frame_is_square_where_dwm_does_not_round() {
        assert!(rounded_frame(true, false, false), "обычное окно на GPU");
        assert!(!rounded_frame(false, false, false), "Windows 10 и старее");
        assert!(
            !rounded_frame(true, true, false),
            "развернуть или прижать к краю"
        );
        assert!(!rounded_frame(true, false, true), "программный растр");
        assert!(!rounded_frame(true, true, true));
        assert!(!rounded_frame(false, true, true));
    }

    /// В журнал запуска идут только первая потеря фокуса и первое его возвращение:
    /// окно переключается часто, и строка на каждое переключение забивала бы журнал.
    #[test]
    fn the_first_focus_change_each_way_is_logged_once() {
        let mut logged = [false; 2];
        assert!(
            activation_to_log(false, &mut logged),
            "первая потеря фокуса"
        );
        assert!(
            activation_to_log(true, &mut logged),
            "первое возвращение фокуса"
        );
        assert!(!activation_to_log(false, &mut logged), "повтор потери");
        assert!(!activation_to_log(true, &mut logged), "повтор возвращения");
        assert!(!activation_to_log(false, &mut logged));
    }

    /// Цвет рамки идёт в журнал запуска как `#RRGGBB`: по этой строке смоук CI видит,
    /// каким цветом рамку рисует приложение.
    #[test]
    fn the_frame_colour_is_written_as_a_hex_token() {
        assert_eq!(hex_color(Color32::from_rgb(1, 2, 3)), "#010203");
        assert_eq!(hex_color(Color32::from_rgb(0xab, 0xcd, 0xef)), "#abcdef");
    }
}
