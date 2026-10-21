//! Строки интерфейса (FR-31, U1-AC6): каталоги по языкам и макрос [`t!`](crate::t).
//!
//! Каталог — вложенный JSON (`locales/<язык>.json`), встроенный в exe; ключ — путь через
//! точку, как в Vue-версии: `t!("window.close")`. Подстановки — `{имя}`:
//! `t!("settings.warningCorrupt", path = copy.display())`. Новый язык — новый каталог в
//! [`Locale`] и `CATALOGS`, экраны не меняются. Ключа нет в каталоге языка — берётся `ru`,
//! нет и там — возвращается сам ключ (тест `every_key_in_code_is_in_the_catalog` этого не
//! допускает).

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// Язык интерфейса.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Locale {
    Ru = 0,
}

impl Locale {
    pub const ALL: [Self; 1] = [Self::Ru];

    fn from_u8(value: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|locale| *locale as u8 == value)
            .unwrap_or(Self::Ru)
    }
}

/// Исходники каталогов в порядке [`Locale`].
const SOURCES: [&str; 1] = [include_str!("../locales/ru.json")];

type Catalog = BTreeMap<String, String>;

static CATALOGS: LazyLock<Vec<Catalog>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .map(|source| {
            let value: serde_json::Value =
                serde_json::from_str(source).expect("каталог строк — корректный JSON");
            let mut catalog = Catalog::new();
            flatten("", &value, &mut catalog);
            catalog
        })
        .collect()
});

static CURRENT: AtomicU8 = AtomicU8::new(Locale::Ru as u8);

fn flatten(prefix: &str, value: &serde_json::Value, out: &mut Catalog) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, value, out);
            }
        }
        serde_json::Value::String(text) => {
            out.insert(prefix.to_owned(), text.clone());
        }
        other => panic!("{prefix}: в каталоге строк только строки, а не {other}"),
    }
}

/// Язык интерфейса сейчас.
pub fn locale() -> Locale {
    Locale::from_u8(CURRENT.load(Ordering::Relaxed))
}

pub fn set_locale(locale: Locale) {
    CURRENT.store(locale as u8, Ordering::Relaxed);
}

/// Ключи каталога языка.
pub fn keys(locale: Locale) -> impl Iterator<Item = &'static str> {
    CATALOGS[locale as usize].keys().map(String::as_str)
}

/// Строка по ключу без подстановок.
pub fn text(key: &'static str) -> &'static str {
    CATALOGS[locale() as usize]
        .get(key)
        .or_else(|| CATALOGS[Locale::Ru as usize].get(key))
        .map_or(key, String::as_str)
}

/// Строка по ключу с подстановкой `{имя}`.
pub fn format(key: &'static str, args: &[(&str, String)]) -> String {
    fill(text(key), args)
}

/// Подстановка `{имя}` в готовую строку каталога: когда ключ выбирается по коду
/// (`t!` в ветках `match`), а подстановки собираются отдельно.
pub fn fill(template: &str, args: &[(&str, String)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Строка интерфейса по ключу каталога: `t!("window.close")`,
/// `t!("diag.crash", summary = text)`.
#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::i18n::text($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::format($key, &[$((stringify!($name), $value.to_string())),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// Модули журнала запуска: их строки — не интерфейс, а то, что они показывают
    /// пользователю (системное окно), идёт через `t!`.
    const LOG_ONLY: [&str; 2] = ["diag.rs", "gpu.rs"];

    fn sources() -> Vec<(PathBuf, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        let mut stack = vec![dir];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src") {
                let path = entry.expect("src").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).expect("исходник");
                    out.push((path, text));
                }
            }
        }
        out
    }

    /// Код без тестового модуля: строки тестов — не интерфейс.
    fn code(text: &str) -> &str {
        text.split("#[cfg(test)]").next().unwrap_or(text)
    }

    /// Ключи из вызовов `t!("…")`.
    fn used_keys() -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for (path, text) in sources() {
            if path.ends_with("i18n.rs") {
                continue;
            }
            let mut rest = code(&text);
            while let Some(start) = rest.find("t!(") {
                // `format!(` и `print!(` — не `t!(`.
                let macro_name = rest[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                rest = rest[start + 3..].trim_start();
                if !macro_name {
                    continue;
                }
                let key = rest.strip_prefix('"').expect("ключ t! — строковый литерал");
                let end = key.find('"').expect("ключ t! без кавычки");
                keys.insert(key[..end].to_owned());
                rest = &key[end..];
            }
        }
        keys
    }

    /// Строковые литералы строки кода без комментария.
    fn literals(line: &str) -> Vec<&str> {
        let code = line.split("//").next().unwrap_or(line);
        let mut out = Vec::new();
        let mut parts = code.split('"');
        parts.next();
        while let Some(literal) = parts.next() {
            out.push(literal);
            parts.next();
        }
        out
    }

    #[test]
    fn every_key_in_code_is_in_the_catalog() {
        let catalog: BTreeSet<String> = keys(Locale::Ru).map(str::to_owned).collect();
        let used = used_keys();
        let missing: Vec<_> = used.difference(&catalog).collect();
        assert!(missing.is_empty(), "ключей нет в ru.json: {missing:?}");
    }

    #[test]
    fn the_catalog_has_no_unused_keys() {
        let catalog: BTreeSet<String> = keys(Locale::Ru).map(str::to_owned).collect();
        let used = used_keys();
        let unused: Vec<_> = catalog.difference(&used).collect();
        assert!(
            unused.is_empty(),
            "ключи ru.json не используются: {unused:?}"
        );
    }

    #[test]
    fn screens_have_no_russian_literals() {
        let mut found = Vec::new();
        for (path, text) in sources() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name == "i18n.rs" || LOG_ONLY.contains(&name) {
                continue;
            }
            // Глубина скобок вызова `diag::…(…)`: строки журнала запуска — не интерфейс.
            let mut in_log = 0i32;
            for (index, line) in code(&text).lines().enumerate() {
                let line = line.trim_start();
                if line.starts_with("//") {
                    continue;
                }
                let starts_log = in_log == 0 && line.contains("diag::");
                if starts_log || in_log > 0 {
                    let from = if starts_log {
                        &line[line.find("diag::").unwrap_or(0)..]
                    } else {
                        line
                    };
                    in_log += from.matches('(').count() as i32 - from.matches(')').count() as i32;
                    in_log = in_log.max(0);
                    continue;
                }
                for literal in literals(line) {
                    if literal
                        .chars()
                        .any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
                    {
                        found.push(format!("{name}:{}: {literal}", index + 1));
                    }
                }
            }
        }
        assert!(found.is_empty(), "русский текст мимо t!: {found:#?}");
    }

    #[test]
    fn substitutions_replace_named_placeholders() {
        assert_eq!(
            crate::t!("diag.startFailed", reason = "нет адаптера"),
            "Не удалось запустить bytetap: нет адаптера"
        );
        assert_eq!(crate::t!("window.close"), "Закрыть");
    }

    #[test]
    fn an_unknown_key_falls_back_to_itself() {
        assert_eq!(text("no.such.key"), "no.such.key");
    }
}
