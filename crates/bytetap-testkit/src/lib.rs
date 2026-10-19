//! Тестовые помощники для crates bytetap: пути к синтетическим журналам и общие фикстуры.
//!
//! Crate не публикуется в crates.io (`publish = false`) и используется только тестами.

use std::path::{Path, PathBuf};

/// Каталог с синтетическими журналами сессий в репозитории.
///
/// Журналы реальных сессий в репозиторий не попадают (см. AGENTS.md): фикстуры должны быть синтетическими.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_dir_is_absolute_and_inside_repo() {
        let dir = fixtures_dir();
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(dir.ends_with("tests/fixtures"), "{}", dir.display());
    }
}
