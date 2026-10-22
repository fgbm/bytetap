//! Тестовые хуки: ключи окружения, которые меняют поведение exe (CI, смоуки).
//!
//! Все они читаются только вместе с [`ENV`] (`BYTETAP_TEST_HOOKS=1`). Поставляемый exe
//! не должен ни начинать запись, ни падать, ни переключаться на синтетический экран
//! от одной переменной окружения в чужой оболочке: ключ хуков — единственный вход.
//! Более того, в релизном exe хуков нет вовсе: модуль собран из feature `test-hooks`
//! (`Cargo.toml`), и без неё [`enabled`] — константа `false`, а [`var`] и [`flag`]
//! переменные окружения не читают.
//!
//! Ключи: `BYTETAP_TEST_AUTOSTART` (`app.rs`), `BYTETAP_TEST_PANIC` (`app.rs`),
//! `BYTETAP_TEST_SCREEN` и `BYTETAP_TEST_JOURNAL` (`app.rs`, снимки для согласования),
//! `BYTETAP_TEST_PROBE*` и его переключатели отрисовки (`probe.rs`, `gpu.rs`).

/// Ключ, без которого остальные тестовые ключи не читаются.
#[cfg(feature = "test-hooks")]
pub const ENV: &str = "BYTETAP_TEST_HOOKS";

/// Хуки включены: `BYTETAP_TEST_HOOKS=1`.
///
/// Без feature `test-hooks` хуков нет: ответ — константа, ключ не читается.
#[must_use]
#[cfg(feature = "test-hooks")]
pub fn enabled() -> bool {
    is_enabled(std::env::var(ENV).ok().as_deref())
}

/// Хуки включены: в релизном exe — всегда `false`, переменная окружения не читается.
#[must_use]
#[cfg(not(feature = "test-hooks"))]
pub fn enabled() -> bool {
    false
}

/// Значение тестового ключа или `None`, если хуки выключены: без [`ENV`] ключ не
/// читается вовсе.
#[must_use]
#[cfg(feature = "test-hooks")]
pub fn var(key: &str) -> Option<String> {
    gated(enabled(), std::env::var(key).ok())
}

/// Значение тестового ключа: без feature `test-hooks` окружение не читается.
#[must_use]
#[cfg(not(feature = "test-hooks"))]
pub fn var(_key: &str) -> Option<String> {
    None
}

/// Ключ-флаг: `BYTETAP_TEST_<...>=1` и хуки включены.
#[must_use]
#[cfg(feature = "test-hooks")]
pub fn flag(key: &str) -> bool {
    var(key).is_some_and(|value| value.trim() == "1")
}

/// Ключ-флаг: без feature `test-hooks` — всегда `false`.
#[must_use]
#[cfg(not(feature = "test-hooks"))]
pub fn flag(_key: &str) -> bool {
    false
}

/// Значение ключа хуков: включено только `1` (пробелы по краям не считаются).
#[must_use]
#[cfg(feature = "test-hooks")]
fn is_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.trim() == "1")
}

/// Значение тестового ключа: `None`, когда хуки выключены.
#[must_use]
#[cfg(feature = "test-hooks")]
fn gated(hooks: bool, value: Option<String>) -> Option<String> {
    hooks.then_some(value).flatten()
}

#[cfg(all(test, feature = "test-hooks"))]
mod tests {
    use super::*;

    /// Без `BYTETAP_TEST_HOOKS=1` тестовый ключ не читается, даже когда он задан:
    /// поставляемый exe не должен реагировать на одиночную переменную окружения.
    #[test]
    fn test_keys_are_ignored_without_the_hooks_key() {
        let set = Some("recording-text".to_owned());
        assert_eq!(gated(is_enabled(None), set.clone()), None);
        assert_eq!(gated(is_enabled(Some("")), set.clone()), None);
        assert_eq!(gated(is_enabled(Some("0")), set.clone()), None);
        assert_eq!(gated(is_enabled(Some("true")), set), None);
        // С ключом — читается; пробелы по краям не мешают.
        assert_eq!(
            gated(is_enabled(Some(" 1 ")), Some("recording-text".to_owned())),
            Some("recording-text".to_owned())
        );
    }
}
