//! Ядро захвата bytetap: транспорты, автоответчик и журнал.
//!
//! Crate намеренно не зависит от Tauri, чтобы его можно было тестировать отдельно и собрать консольную версию (раздел 10 требований, M0-AC2).

/// Версия ядра из `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Версия ядра — общая для оболочки и CLI.
#[must_use]
pub fn version() -> &'static str {
    VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comes_from_manifest() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(version().split('.').count(), 3);
    }
}
