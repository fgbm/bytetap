//! Проверка WebView2 до создания окна (NFR-3, M2-AC14).
//!
//! Без рантайма WebView2 окно не создаётся, и приложение падает необъяснимо. Поэтому на
//! Windows до `tauri::Builder` проверяется запись установщика Edge Update, а если её нет —
//! показывается системное окно с текстом из макета и выход с кодом 1.
//!
//! Проверка идёт по реестру, а не через `GetAvailableCoreWebView2BrowserVersionString`:
//! та функция тянет за собой `WebView2Loader.dll` (ещё одна нативная зависимость в сборке
//! и рядом с exe), а реестр читается тем же `windows-sys`, который уже есть у ядра на
//! Windows. Ключи и значение `pv` — те же, по которым ищет рантайм сам установщик.

/// Заголовок системного окна.
#[cfg(windows)]
pub const TITLE: &str = "bytetap";

/// Текст системного окна из макета (M2-AC14).
#[cfg(any(windows, test))]
pub const MISSING_MESSAGE: &str = "Для работы bytetap нужен компонент Microsoft Edge WebView2, а на этом компьютере его нет.

Установите WebView2 Runtime (автономный установщик можно принести на флешке) или используйте bytetap-cli.";

/// `true`, если в списке версий есть установленный рантайм.
///
/// Установщик пишет в `pv` версию, а отсутствие рантайма — `0.0.0.0` или пустую строку.
/// Поэтому проверяется именно версия, а не наличие ключа: ключ с `0.0.0.0` встречается
/// после удаления рантайма.
#[cfg(any(windows, test))]
#[must_use]
pub fn runtime_present<'a>(versions: impl IntoIterator<Item = &'a str>) -> bool {
    versions.into_iter().any(|version| {
        let version = version.trim();
        !version.is_empty() && version != "0.0.0.0"
    })
}

/// Проверяет рантайм и завершает программу, если его нет. На других ОС ничего не делает.
#[cfg(not(windows))]
pub fn ensure_or_exit() {}

#[cfg(windows)]
pub fn ensure_or_exit() {
    let versions = read_versions();
    if runtime_present(versions.iter().map(String::as_str)) {
        return;
    }
    show_missing_and_exit();
}

/// Ключи, куда установщик Edge Update пишет версию WebView2 Runtime.
#[cfg(windows)]
const RUNTIME_KEYS: [(&str, &str); 3] = [
    (
        "HKLM",
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
    ),
    (
        "HKLM",
        r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
    ),
    (
        "HKCU",
        r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
    ),
];

/// Версии `pv` из всех ключей, где они есть.
#[cfg(windows)]
fn read_versions() -> Vec<String> {
    RUNTIME_KEYS
        .iter()
        .filter_map(|(root, key)| read_pv(root, key))
        .collect()
}

/// Значение `pv` из ключа реестра; `None` — ключа нет или значения нет.
#[cfg(windows)]
fn read_pv(root: &str, key: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };

    let root = match root {
        "HKLM" => HKEY_LOCAL_MACHINE,
        _ => HKEY_CURRENT_USER,
    };
    let key: Vec<u16> = key.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = "pv".encode_utf16().chain([0]).collect();
    let mut buffer = [0u16; 64];
    let mut size = u32::try_from(buffer.len() * std::mem::size_of::<u16>()).ok()?;
    // SAFETY: строки с завершающим NUL живут до конца вызова, буфер и его размер согласованы,
    // `RegGetValueW` пишет не больше `size` байт и обновляет `size`.
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let chars = (size as usize / std::mem::size_of::<u16>()).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..chars.min(buffer.len())]))
}

/// Системное окно с текстом из макета и выход с кодом 1.
#[cfg(windows)]
fn show_missing_and_exit() -> ! {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let text: Vec<u16> = MISSING_MESSAGE.encode_utf16().chain([0]).collect();
    let title: Vec<u16> = TITLE.encode_utf16().chain([0]).collect();
    // SAFETY: строки с завершающим NUL живут до конца вызова, других требований у функции нет.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_version_means_the_runtime_is_there() {
        assert!(runtime_present(["120.0.2210.144"]));
        assert!(runtime_present(["", "120.0.2210.144"]));
    }

    #[test]
    fn a_missing_or_removed_runtime_is_detected() {
        assert!(!runtime_present(std::iter::empty::<&str>()));
        assert!(!runtime_present([""]));
        assert!(!runtime_present(["  "]));
        assert!(!runtime_present(["0.0.0.0"]));
        assert!(!runtime_present(["0.0.0.0", ""]));
    }

    #[test]
    fn the_message_is_the_one_from_the_mockup() {
        assert!(MISSING_MESSAGE.contains("Microsoft Edge WebView2"));
        assert!(MISSING_MESSAGE.contains("bytetap-cli"));
        assert_eq!(MISSING_MESSAGE.lines().count(), 3, "два абзаца из макета");
    }
}
