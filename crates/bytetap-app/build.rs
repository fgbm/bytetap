//! Rust-константы темы из `design/tokens.css` (U1-AC4).
//!
//! Токены — единый источник для сайта документации и приложения: `build.rs` читает
//! переменные `--bt-*` из блока `:root` (общие значения и светлая тема) и из блока
//! тёмной темы `:root[data-theme="dark"], html.dark` и пишет `tokens.rs` в `OUT_DIR`.
//! Цвета, которые есть в тёмной теме, становятся полями `Palette` (константы `LIGHT` и
//! `DARK`), остальные переменные — общими константами: `px` — `f32` в точках, `s` —
//! `f32` в секундах, `#rrggbb` — `Color32`, прочее — строка.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

const TOKENS: &str = "../../design/tokens.css";

fn main() {
    println!("cargo:rerun-if-changed={TOKENS}");
    println!("cargo:rerun-if-changed=build.rs");
    let css = std::fs::read_to_string(TOKENS).expect("design/tokens.css");
    let blocks = blocks(&strip_comments(&css));

    let base = block(&blocks, |selector| selector == ":root");
    let light = block(&blocks, |selector| {
        selector == r#":root[data-theme="light"]"#
    });
    let dark = block(&blocks, |selector| {
        selector.contains(r#"data-theme="dark""#) && selector.contains("html.dark")
    });

    // Явная светлая тема повторяет `:root`: расхождение — ошибка в токенах.
    for (name, value) in &light {
        assert_eq!(
            base.get(name),
            Some(value),
            "{name} в :root[data-theme=\"light\"] расходится с :root"
        );
    }
    for name in dark.keys() {
        assert!(base.contains_key(name), "{name} есть только в тёмной теме");
    }

    let mut out = String::from("// Сгенерировано build.rs из design/tokens.css. Не править.\n\n");
    let mut lookup = String::new();
    for (name, value) in &base {
        if dark.contains_key(name) {
            continue;
        }
        let ident = name.to_uppercase().replace('-', "_");
        let (ty, expr) = constant(value);
        writeln!(out, "/// `--bt-{name}: {value}`").unwrap();
        writeln!(out, "pub const {ident}: {ty} = {expr};").unwrap();
        writeln!(
            lookup,
            "        \"--bt-{name}\" => Some(\"{}\"),",
            value.replace('"', "\\\"")
        )
        .unwrap();
    }

    out.push_str("\n/// Цвета одной темы: переменные `--bt-*`, у которых есть тёмное значение.\n");
    out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct Palette {\n");
    for name in dark.keys() {
        writeln!(
            out,
            "    /// `--bt-{name}`\n    pub {}: Color32,",
            field(name)
        )
        .unwrap();
    }
    out.push_str("}\n");
    for (title, ident, values) in [
        ("Светлая тема «Белая»", "LIGHT", &base),
        ("Тёмная тема «Осциллограф»", "DARK", &dark),
    ] {
        writeln!(
            out,
            "\n/// {title}.\npub const {ident}: Palette = Palette {{"
        )
        .unwrap();
        for name in dark.keys() {
            let (ty, expr) = constant(&values[name]);
            assert_eq!(ty, "Color32", "--bt-{name}: цвет темы не #rrggbb");
            writeln!(out, "    {}: {expr},", field(name)).unwrap();
        }
        out.push_str("};\n");
    }

    out.push_str("\nimpl Palette {\n    /// Имена переменных CSS полей палитры.\n");
    out.push_str("    pub const CSS_NAMES: &[&str] = &[\n");
    for name in dark.keys() {
        writeln!(out, "        \"--bt-{name}\",").unwrap();
    }
    out.push_str("    ];\n\n    /// Цвет по имени переменной CSS.\n");
    out.push_str(
        "    pub fn by_css_name(&self, name: &str) -> Option<Color32> {\n        match name {\n",
    );
    for name in dark.keys() {
        writeln!(
            out,
            "            \"--bt-{name}\" => Some(self.{}),",
            field(name)
        )
        .unwrap();
    }
    out.push_str("            _ => None,\n        }\n    }\n}\n");

    out.push_str(
        "\n/// Исходное значение общей переменной CSS: для теста сверки с `tokens.css`.\n",
    );
    out.push_str(
        "pub fn common_css_value(name: &str) -> Option<&'static str> {\n    match name {\n",
    );
    out.push_str(&lookup);
    out.push_str("        _ => None,\n    }\n}\n");

    let path = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("tokens.rs");
    std::fs::write(path, out).expect("tokens.rs");
}

fn field(name: &str) -> String {
    name.replace('-', "_")
}

/// Тип и выражение константы по значению CSS.
fn constant(value: &str) -> (&'static str, String) {
    if let Some(hex) = value.strip_prefix('#') {
        assert!(
            hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()),
            "цвет {value}: ожидается #rrggbb"
        );
        return ("Color32", format!("hex(0x{})", hex.to_lowercase()));
    }
    if let Some(number) = value.strip_suffix("px").and_then(|n| n.parse::<f32>().ok()) {
        return ("f32", format!("{number:?}"));
    }
    if let Some(number) = value.strip_suffix('s').and_then(|n| n.parse::<f32>().ok()) {
        return ("f32", format!("{number:?}"));
    }
    ("&str", format!("{value:?}"))
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find("*/").expect("незакрытый комментарий") + start + 2;
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Блоки верхнего уровня: селектор и тело. `@media` и прочие at-правила пропускаются.
fn blocks(css: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut selector_start = 0;
    let mut body_start = 0;
    let mut selector = String::new();
    for (index, ch) in css.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    selector = css[selector_start..index].trim().to_owned();
                    body_start = index + 1;
                }
                depth += 1;
            }
            '}' => {
                depth = depth.checked_sub(1).expect("лишняя }");
                if depth == 0 {
                    if !selector.starts_with('@') {
                        out.push((selector.clone(), css[body_start..index].to_owned()));
                    }
                    selector_start = index + 1;
                }
            }
            _ => {}
        }
    }
    out
}

/// Переменные `--bt-*` первого блока с подходящим селектором, без префикса `--bt-`.
fn block(blocks: &[(String, String)], pick: impl Fn(&str) -> bool) -> BTreeMap<String, String> {
    let (selector, body) = blocks
        .iter()
        .find(|(selector, _)| pick(&normalize(selector)))
        .expect("блок токенов не найден");
    let mut vars = BTreeMap::new();
    for declaration in body.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let Some(name) = name.trim().strip_prefix("--bt-") else {
            continue;
        };
        let previous = vars.insert(name.to_owned(), value.trim().to_owned());
        assert!(previous.is_none(), "--bt-{name} дважды в {selector}");
    }
    vars
}

/// Селектор в одну строку: переводы строк между частями списка — пробелы.
fn normalize(selector: &str) -> String {
    selector.split_whitespace().collect::<Vec<_>>().join(" ")
}
