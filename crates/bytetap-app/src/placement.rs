//! Положение окна между запусками (FR-30, U1-AC3): в физических пикселях, только если
//! шапка окна попадает на один из мониторов, и без положения «прижатого» окна (Aero Snap).

/// Полоса шапки от левого верхнего угла окна, которая должна быть видна: за неё окно
/// можно перетащить, даже если остальное ушло за край экрана.
pub const VISIBLE_STRIP: [i32; 2] = [100, 44];

/// Прямоугольник `[left, top, right, bottom]` в физических пикселях экрана.
pub type PixelRect = [i32; 4];

/// Полоса шапки окна с левым верхним углом в `position`.
#[must_use]
pub fn title_strip(position: [i32; 2]) -> PixelRect {
    let [x, y] = position;
    [
        x,
        y,
        x.saturating_add(VISIBLE_STRIP[0]),
        y.saturating_add(VISIBLE_STRIP[1]),
    ]
}

/// Запомненное положение, если полоса шапки попадает на монитор (`on_monitor`), иначе
/// `None` — окно по центру: монитор отключили или поменяли расположение мониторов.
#[must_use]
pub fn visible_position(
    saved: Option<[i32; 2]>,
    on_monitor: impl Fn(PixelRect) -> bool,
) -> Option<[i32; 2]> {
    saved.filter(|position| on_monitor(title_strip(*position)))
}

/// Пересекает ли прямоугольник хотя бы один монитор из списка.
#[must_use]
pub fn intersects_any(rect: PixelRect, monitors: &[PixelRect]) -> bool {
    let [left, top, right, bottom] = rect;
    monitors
        .iter()
        .any(|m| left < m[2] && m[0] < right && top < m[3] && m[1] < bottom)
}

/// Прямоугольник на одном из мониторов: `MonitorFromRect` без подстановки ближайшего.
#[cfg(windows)]
#[must_use]
pub fn on_monitor(rect: PixelRect) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};

    let [left, top, right, bottom] = rect;
    let rect = RECT {
        left,
        top,
        right,
        bottom,
    };
    // SAFETY: указатель на RECT на стеке действителен на время вызова.
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_null()
}

/// Вне Windows мониторы не проверяются, и окно открывается по центру.
#[cfg(not(windows))]
#[must_use]
pub fn on_monitor(_rect: PixelRect) -> bool {
    false
}

/// Окно прижато к краю или четверти экрана (Aero Snap) или развёрнуто: его размер
/// отличается от «обычного» размера, который Windows вернёт при восстановлении. Такое
/// положение не запоминается — при запуске окно получило бы размер половины экрана.
#[cfg(windows)]
#[must_use]
pub fn arranged(frame: &eframe::Frame) -> bool {
    use eframe::wgpu::rwh::{HasWindowHandle as _, RawWindowHandle};
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, GetWindowRect, IsZoomed, WINDOWPLACEMENT,
    };

    let Ok(handle) = frame.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let mut rect = RECT::default();
    // SAFETY: hwnd — живое окно этого процесса, структуры на стеке, длина заполнена.
    unsafe {
        if IsZoomed(hwnd) != 0 {
            return true;
        }
        if GetWindowPlacement(hwnd, &mut placement) == 0 || GetWindowRect(hwnd, &mut rect) == 0 {
            return false;
        }
    }
    let normal = placement.rcNormalPosition;
    !same_size(
        [normal.left, normal.top, normal.right, normal.bottom],
        [rect.left, rect.top, rect.right, rect.bottom],
    )
}

#[cfg(not(windows))]
#[must_use]
pub fn arranged(_frame: &eframe::Frame) -> bool {
    false
}

/// Размеры совпадают. `rcNormalPosition` — в координатах рабочей области, поэтому
/// сравнивается только размер, а не положение.
#[must_use]
pub fn same_size(a: PixelRect, b: PixelRect) -> bool {
    a[2] - a[0] == b[2] - b[0] && a[3] - a[1] == b[3] - b[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Два монитора: основной 1920×1080 и слева от него 1280×1024.
    const MONITORS: [PixelRect; 2] = [[0, 0, 1920, 1080], [-1280, 0, 0, 1024]];

    fn on(rect: PixelRect) -> bool {
        intersects_any(rect, &MONITORS)
    }

    #[test]
    fn a_position_on_a_monitor_is_kept() {
        assert_eq!(visible_position(Some([200, 100]), on), Some([200, 100]));
        assert_eq!(visible_position(Some([-1000, 50]), on), Some([-1000, 50]));
        // Окно ушло за левый край основного монитора, но шапка ещё на нём.
        assert_eq!(visible_position(Some([1850, 1050]), on), Some([1850, 1050]));
    }

    #[test]
    fn a_position_off_every_monitor_is_dropped() {
        // Монитор слева отключён.
        assert_eq!(
            visible_position(Some([-1000, 50]), |r| intersects_any(r, &MONITORS[..1])),
            None
        );
        assert_eq!(visible_position(Some([5000, 100]), on), None);
        assert_eq!(visible_position(Some([-1280, 1030]), on), None);
        assert_eq!(visible_position(Some([100, -44]), on), None);
        assert_eq!(visible_position(None, on), None);
        assert_eq!(visible_position(Some([i32::MAX, i32::MAX]), on), None);
    }

    #[test]
    fn the_strip_is_the_top_left_of_the_title_bar() {
        assert_eq!(title_strip([10, 20]), [10, 20, 110, 64]);
    }

    #[test]
    fn a_snapped_window_differs_in_size_only() {
        assert!(same_size([0, 0, 100, 50], [300, 200, 400, 250]));
        assert!(!same_size([0, 0, 1200, 800], [0, 0, 960, 1040]));
    }
}
