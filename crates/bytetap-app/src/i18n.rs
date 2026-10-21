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

    /// Вызовы журнала запуска: их строки — не интерфейс. Всё, что видит пользователь, в
    /// том числе текст системного окна о сбое, идёт через `t!`.
    const LOG_CALLS: [&str; 2] = ["diag::info(", "diag::error("];

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

    /// Строковый литерал исходника: содержимое без кавычек (экранирование не
    /// раскрывается), строка файла и место в [`Lexed::code`].
    #[derive(Debug, PartialEq)]
    struct Literal {
        text: String,
        line: usize,
        at: usize,
    }

    /// Исходник, разобранный настолько, насколько нужно проверкам: код без комментариев,
    /// где каждый строковый литерал заменён одним символом `\u{1}`, и сами литералы.
    struct Lexed {
        code: String,
        literals: Vec<Literal>,
    }

    /// Разбор исходника Rust: комментарии `//` и `/* */` (вложенные), строки с `\"`,
    /// сырые строки `r"…"` и `r#"…"#`, байтовые строки, символы `'"'` и времена жизни.
    fn lex(source: &str) -> Lexed {
        let chars: Vec<char> = source.chars().collect();
        let mut code = String::new();
        let mut literals = Vec::new();
        let mut line = 1;
        let mut i = 0;
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            let prev_ident = i > 0 && ident(chars[i - 1]);
            if c == '/' && next == Some('/') {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            } else if c == '/' && next == Some('*') {
                let mut depth = 0;
                while i < chars.len() {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        if chars[i] == '\n' {
                            line += 1;
                            code.push('\n');
                        }
                        i += 1;
                    }
                }
            } else if !prev_ident && (c == 'r' || (c == 'b' && next == Some('r'))) && {
                let start = if c == 'b' { i + 2 } else { i + 1 };
                let hashes = chars[start..].iter().take_while(|&&h| h == '#').count();
                chars.get(start + hashes) == Some(&'"')
            } {
                let start = if c == 'b' { i + 2 } else { i + 1 };
                let hashes = chars[start..].iter().take_while(|&&h| h == '#').count();
                let body = start + hashes + 1;
                let mut end = body;
                while end < chars.len()
                    && !(chars[end] == '"'
                        && chars[end + 1..].iter().take_while(|&&h| h == '#').count() >= hashes)
                {
                    end += 1;
                }
                let text: String = chars[body..end.min(chars.len())].iter().collect();
                literals.push(Literal {
                    line,
                    at: code.len(),
                    text: text.clone(),
                });
                line += text.matches('\n').count();
                code.push('\u{1}');
                i = end + 1 + hashes;
            } else if c == '"' {
                let mut end = i + 1;
                while end < chars.len() && chars[end] != '"' {
                    end += if chars[end] == '\\' { 2 } else { 1 };
                }
                let text: String = chars[i + 1..end.min(chars.len())].iter().collect();
                literals.push(Literal {
                    line,
                    at: code.len(),
                    text: text.clone(),
                });
                line += text.matches('\n').count();
                code.push('\u{1}');
                i = end + 1;
            } else if c == '\'' {
                // Символ `'x'`, `'\n'`, `'\u{..}'` или время жизни `'a`.
                if next == Some('\\') {
                    let mut end = i + 2;
                    while end < chars.len() && chars[end] != '\'' {
                        end += 1;
                    }
                    code.push_str("' '");
                    i = end + 1;
                } else if chars.get(i + 2) == Some(&'\'') {
                    code.push_str("' '");
                    i += 3;
                } else {
                    code.push(c);
                    i += 1;
                }
            } else {
                if c == '\n' {
                    line += 1;
                }
                code.push(c);
                i += 1;
            }
        }
        let mut lexed = Lexed { code, literals };
        drop_test_modules(&mut lexed);
        lexed
    }

    /// Тестовые модули `#[cfg(test)] mod … { … }` выпадают целиком: строки тестов — не
    /// интерфейс. Прочие элементы с `#[cfg(test)]` остаются под проверкой.
    fn drop_test_modules(lexed: &mut Lexed) {
        let mut from = 0;
        while let Some(found) = lexed.code[from..].find("#[cfg(test)]") {
            let start = from + found;
            let after = lexed.code[start + "#[cfg(test)]".len()..].trim_start();
            if !after.starts_with("mod ") {
                from = start + 1;
                continue;
            }
            let Some(open) = lexed.code[start..].find('{').map(|o| start + o) else {
                break;
            };
            let mut depth = 0;
            let mut end = lexed.code.len();
            for (offset, ch) in lexed.code[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + offset + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            lexed
                .literals
                .retain(|literal| literal.at < start || literal.at >= end);
            let removed = end - start;
            for literal in &mut lexed.literals {
                if literal.at >= end {
                    literal.at -= removed;
                }
            }
            lexed.code.replace_range(start..end, "");
            from = start;
        }
    }

    /// Места литералов внутри аргументов вызовов журнала запуска.
    fn in_log_calls(lexed: &Lexed) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        for call in LOG_CALLS {
            let mut from = 0;
            while let Some(found) = lexed.code[from..].find(call) {
                let open = from + found + call.len() - 1;
                let mut depth = 0;
                let mut end = lexed.code.len();
                for (offset, ch) in lexed.code[open..].char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = open + offset;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                ranges.push(open..end);
                from = open + 1;
            }
        }
        ranges
    }

    /// Ключи из вызовов `t!("…")`.
    fn used_keys() -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for (path, text) in sources() {
            if path.ends_with("i18n.rs") {
                continue;
            }
            let lexed = lex(&text);
            let mut from = 0;
            while let Some(found) = lexed.code[from..].find("t!(") {
                let start = from + found;
                from = start + 3;
                // `format!(` и `print!(` — не `t!(`.
                if lexed.code[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    continue;
                }
                let rest = &lexed.code[from..];
                let key_at = from + (rest.len() - rest.trim_start().len());
                let key = lexed
                    .literals
                    .iter()
                    .find(|literal| literal.at == key_at)
                    .unwrap_or_else(|| panic!("{}: ключ t! — строковый литерал", path.display()));
                keys.insert(key.text.clone());
            }
        }
        keys
    }

    #[test]
    fn the_lexer_understands_escapes_raw_strings_and_comments() {
        let source = concat!(
            "let a = \"он сказал \\\"привет\\\"\"; // «комментарий»\n",
            "let b = r#\"сырая \"строка\"\"#; /* «блок /* вложенный */» */\n",
            "let c = '\"'; let d: &'static str = r\"x\";\n",
            "#[cfg(test)]\nfn helper() { \"тест-функция\" }\n",
            "#[cfg(test)]\nmod tests { fn f() { \"тест\"; } }\n",
            "let e = \"после\";\n",
        );
        let lexed = lex(source);
        let texts: Vec<(&str, usize)> = lexed
            .literals
            .iter()
            .map(|literal| (literal.text.as_str(), literal.line))
            .collect();
        assert_eq!(
            texts,
            [
                ("он сказал \\\"привет\\\"", 1),
                ("сырая \"строка\"", 2),
                ("x", 3),
                ("тест-функция", 5),
                ("после", 8),
            ]
        );
        assert!(!lexed.code.contains("комментарий"));
        assert!(!lexed.code.contains("mod tests"));
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
            if name == "i18n.rs" {
                continue;
            }
            let lexed = lex(&text);
            let log = in_log_calls(&lexed);
            for literal in &lexed.literals {
                if log.iter().any(|range| range.contains(&literal.at)) {
                    continue;
                }
                if literal
                    .text
                    .chars()
                    .any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
                {
                    found.push(format!("{name}:{}: {}", literal.line, literal.text));
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
