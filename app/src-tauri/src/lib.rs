//! Оболочка bytetap на Tauri v2.
//!
//! На M0 интерфейс — окно-заглушка; вся логика захвата позже будет жить в `bytetap-core`.

/// Запускает приложение.
///
/// # Panics
///
/// Паникует, если Tauri не смог запустить цикл событий: без окна приложение бесполезно.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("не удалось запустить bytetap");
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_and_core_versions_match() {
        assert_eq!(bytetap_core::version(), env!("CARGO_PKG_VERSION"));
    }
}
