//! Ядро захвата bytetap: транспорты, автоответчик и журнал.
//!
//! Crate намеренно не зависит от Tauri, чтобы его можно было тестировать отдельно и собрать консольную версию (раздел 10 требований, M0-AC2).

pub mod journal;
pub mod text;

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

        let mut parts = version().split('.');
        assert_eq!(parts.next(), Some(env!("CARGO_PKG_VERSION_MAJOR")));
        assert_eq!(parts.next(), Some(env!("CARGO_PKG_VERSION_MINOR")));
        assert_eq!(parts.next(), Some(env!("CARGO_PKG_VERSION_PATCH")));
        assert_eq!(parts.next(), None);
    }
}
