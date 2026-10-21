//! Проверка WebView2 до создания окна (NFR-3, M2-AC14).
//!
//! Без рантайма WebView2 окно не создаётся, и приложение падает необъяснимо. Поэтому на
//! Windows до `tauri::Builder` проверяется запись установщика Edge Update: если рантайма
//! нет или он старее [`MIN_VERSION`], показывается системное окно и выход с кодом 1.
//!
//! Проверка идёт по реестру, а не через `GetAvailableCoreWebView2BrowserVersionString`:
//! та функция тянет за собой `WebView2Loader.dll` (ещё одна нативная зависимость в сборке
//! и рядом с exe), а реестр читается тем же `windows-sys`, который уже есть у ядра на
//! Windows. Ключи и значение `pv` — те же, по которым ищет рантайм сам установщик.

#[cfg(any(windows, test))]
/// Версия WebView2: четыре числа `major.minor.build.patch`.
pub type Version = [u32; 4];

#[cfg(any(windows, test))]
/// Самый старый WebView2, на котором работает bytetap.
///
/// wry 0.57 сам по себе создаёт окно на рантайме от 86 (`ICoreWebView2Controller2`), но
/// интерфейс собирается Vite под `chrome111`/`edge111` (`build.target` в
/// `app/vite.config.ts`): на более старом Chromium скрипты и стили интерфейса не работают.
/// Поэтому граница — 111, и менять её нужно вместе с целью сборки.
pub const MIN_VERSION: Version = [111, 0, 0, 0];

/// Текст системного окна из макета (M2-AC14).
#[cfg(any(windows, test))]
pub const MISSING_MESSAGE: &str = "Для работы bytetap нужен компонент Microsoft Edge WebView2, а на этом компьютере его нет.

Установите WebView2 Runtime (автономный установщик можно принести на флешке) или используйте bytetap-cli.";

#[cfg(any(windows, test))]
/// Что нашлось в реестре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    /// Рантайма нет: ключей нет, версия пустая или `0.0.0.0`.
    Missing,
    /// Рантайм есть, но старее [`MIN_VERSION`].
    Outdated(Version),
    /// Рантайм подходит.
    Ok(Version),
}

#[cfg(any(windows, test))]
/// Разбирает `pv`: от одного до четырёх чисел через точку, недостающие — нули.
#[must_use]
pub fn parse_version(text: &str) -> Option<Version> {
    let mut version = [0; 4];
    let mut parts = text.trim().split('.');
    for slot in &mut version {
        match parts.next() {
            Some(part) => *slot = part.parse().ok()?,
            None => break,
        }
    }
    if parts.next().is_some() {
        return None;
    }
    Some(version)
}

#[cfg(any(windows, test))]
/// `major.minor.build.patch` для текста сообщений и журнала.
#[must_use]
pub fn format_version(version: Version) -> String {
    version.map(|part| part.to_string()).join(".")
}

#[cfg(any(windows, test))]
/// Самая новая установленная версия из найденных `pv` против [`MIN_VERSION`].
///
/// Установщик пишет в `pv` версию, а отсутствие рантайма — `0.0.0.0` или пустую строку.
/// Поэтому проверяется именно версия, а не наличие ключа: ключ с `0.0.0.0` встречается
/// после удаления рантайма.
#[must_use]
pub fn check<'a>(versions: impl IntoIterator<Item = &'a str>) -> Runtime {
    let newest = versions
        .into_iter()
        .filter_map(parse_version)
        .filter(|version| *version != [0; 4])
        .max();
    match newest {
        None => Runtime::Missing,
        Some(version) if version < MIN_VERSION => Runtime::Outdated(version),
        Some(version) => Runtime::Ok(version),
    }
}

#[cfg(any(windows, test))]
/// Текст системного окна об устаревшем рантайме.
#[must_use]
pub fn outdated_message(found: Version) -> String {
    format!(
        "Компонент WebView2 устарел (версия {}, нужна {} или новее).

Обновите Microsoft Edge WebView2 Runtime: автономный установщик Evergreen Standalone (x64) можно принести на флешке. Без WebView2 работает bytetap-cli.",
        format_version(found),
        format_version(MIN_VERSION)
    )
}

/// Проверяет рантайм и завершает программу, если его нет. На других ОС ничего не делает.
#[cfg(not(windows))]
pub fn ensure_or_exit() {
    crate::diag::info("WebView2: проверка нужна только на Windows");
}

#[cfg(windows)]
pub fn ensure_or_exit() {
    use crate::diag;

    let versions = read_versions();
    diag::info(format!("WebView2: pv в реестре {versions:?}"));
    match check(versions.iter().map(String::as_str)) {
        Runtime::Ok(version) => diag::info(format!(
            "WebView2 {} (нужна {} или новее)",
            format_version(version),
            format_version(MIN_VERSION)
        )),
        Runtime::Outdated(version) => diag::fatal(&outdated_message(version)),
        Runtime::Missing => {
            // Текст из макета без приписки о журнале: причина и так ясна (M2-AC14).
            diag::error("WebView2 не найден");
            diag::show_error(MISSING_MESSAGE);
            std::process::exit(1);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_version_means_the_runtime_is_there() {
        assert_eq!(check(["120.0.2210.144"]), Runtime::Ok([120, 0, 2210, 144]));
        assert_eq!(
            check(["", "120.0.2210.144"]),
            Runtime::Ok([120, 0, 2210, 144])
        );
    }

    #[test]
    fn a_missing_or_removed_runtime_is_detected() {
        assert_eq!(check(std::iter::empty::<&str>()), Runtime::Missing);
        assert_eq!(check([""]), Runtime::Missing);
        assert_eq!(check(["  "]), Runtime::Missing);
        assert_eq!(check(["0.0.0.0"]), Runtime::Missing);
        assert_eq!(check(["0.0.0.0", ""]), Runtime::Missing);
        assert_eq!(check(["мусор"]), Runtime::Missing);
    }

    #[test]
    fn an_old_runtime_is_outdated() {
        // Windows 11 21H2 из коробки: WebView2 94, если Edge Update не обновлял его.
        assert_eq!(check(["94.0.992.31"]), Runtime::Outdated([94, 0, 992, 31]));
        assert_eq!(
            check(["110.0.1587.69"]),
            Runtime::Outdated([110, 0, 1587, 69])
        );
        assert_eq!(check(["111.0.1661.41"]), Runtime::Ok([111, 0, 1661, 41]));
    }

    #[test]
    fn the_newest_of_several_keys_wins() {
        // Устаревший пользовательский рантайм не мешает свежему системному.
        assert_eq!(
            check(["94.0.992.31", "141.0.3537.71"]),
            Runtime::Ok([141, 0, 3537, 71])
        );
        assert_eq!(
            check(["0.0.0.0", "94.0.992.31"]),
            Runtime::Outdated([94, 0, 992, 31])
        );
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(parse_version("111.0.0.0") > parse_version("94.0.992.31"));
        assert!(parse_version("120.0.10.0") > parse_version("120.0.9.99"));
    }

    #[test]
    fn a_version_is_parsed_strictly() {
        assert_eq!(parse_version(" 94.0.992.31 "), Some([94, 0, 992, 31]));
        assert_eq!(parse_version("111"), Some([111, 0, 0, 0]));
        assert_eq!(parse_version("1.2.3.4.5"), None);
        assert_eq!(parse_version("94.0.x.31"), None);
        assert_eq!(parse_version("94..992.31"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn the_outdated_message_names_both_versions() {
        let text = outdated_message([94, 0, 992, 31]);
        assert!(text.starts_with(
            "Компонент WebView2 устарел (версия 94.0.992.31, нужна 111.0.0.0 или новее)."
        ));
        assert!(text.contains("Evergreen Standalone"));
        assert!(text.contains("bytetap-cli"));
    }

    #[test]
    fn the_minimum_follows_the_frontend_build_target() {
        let vite = include_str!("../../vite.config.ts");
        let major = MIN_VERSION[0];
        assert!(
            vite.contains(&format!("\"chrome{major}\"")),
            "build.target в vite.config.ts"
        );
        assert!(
            vite.contains(&format!("\"edge{major}\"")),
            "build.target в vite.config.ts"
        );
    }

    #[test]
    fn the_message_is_the_one_from_the_mockup() {
        assert!(MISSING_MESSAGE.contains("Microsoft Edge WebView2"));
        assert!(MISSING_MESSAGE.contains("bytetap-cli"));
        assert_eq!(MISSING_MESSAGE.lines().count(), 3, "два абзаца из макета");
    }
}
