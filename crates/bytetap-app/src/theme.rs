//! Палитра и размеры из `design/tokens.css`, шрифты IBM Plex и настройка `Visuals` egui.
//!
//! Значения токенов — константы модуля [`tokens`], их генерирует `build.rs`: ручных копий
//! цветов в коде нет (U1-AC4).

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke,
    Theme, Vec2,
};

/// Константы `--bt-*`, сгенерированные `build.rs` из `design/tokens.css`.
pub mod tokens {
    use eframe::egui::Color32;

    const fn hex(rgb: u32) -> Color32 {
        Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    include!(concat!(env!("OUT_DIR"), "/tokens.rs"));
}

pub use tokens::{DARK, LIGHT, Palette};

pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

/// Смешивание как `color-mix(in srgb, a (1-t), b t)`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Радиус токена в целых точках, как его берёт `CornerRadius`.
pub fn radius(token: f32) -> CornerRadius {
    CornerRadius::same(token.round() as u8)
}

const SANS_MEDIUM: &str = "plex-sans-medium";
const SANS_SEMIBOLD: &str = "plex-sans-semibold";
const MONO_MEDIUM: &str = "plex-mono-medium";

/// Шрифты IBM Plex (OFL) встроены в exe. egui не знает весов шрифта: каждый вес —
/// отдельное семейство.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let faces: [(&str, &'static [u8]); 5] = [
        (
            "plex-sans",
            include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
        ),
        (
            SANS_MEDIUM,
            include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
        ),
        (
            SANS_SEMIBOLD,
            include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
        ),
        (
            "plex-mono",
            include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
        ),
        (
            MONO_MEDIUM,
            include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
        ),
    ];
    for (name, bytes) in faces {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    // Встроенные шрифты egui остаются запасными: в них есть глифы, которых нет в Plex.
    let fallback_sans = fonts.families[&FontFamily::Proportional].clone();
    let fallback_mono = fonts.families[&FontFamily::Monospace].clone();
    let family = |first: &str, rest: &[String]| {
        let mut list = vec![first.to_owned()];
        list.extend(rest.iter().cloned());
        list
    };
    fonts.families.insert(
        FontFamily::Proportional,
        family("plex-sans", &fallback_sans),
    );
    fonts
        .families
        .insert(FontFamily::Monospace, family("plex-mono", &fallback_mono));
    for name in [SANS_MEDIUM, SANS_SEMIBOLD] {
        fonts
            .families
            .insert(FontFamily::Name(name.into()), family(name, &fallback_sans));
    }
    fonts.families.insert(
        FontFamily::Name(MONO_MEDIUM.into()),
        family(MONO_MEDIUM, &fallback_mono),
    );
    ctx.set_fonts(fonts);
}

pub fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn sans_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_MEDIUM.into()))
}

pub fn sans_semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_SEMIBOLD.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_MEDIUM.into()))
}

/// Тихие `Visuals`: скругления из токенов, тонкие рамки, тени только у всплывающих меню.
pub fn apply(ctx: &egui::Context, dark: bool) {
    use tokens::{
        RADIUS_CONTROL, RADIUS_PANEL, SPACE_1, SPACE_2, TEXT_BASE, TEXT_LG, TEXT_SM, TEXT_XS,
    };

    let p = palette(dark);
    let theme = if dark { Theme::Dark } else { Theme::Light };
    ctx.set_theme(theme);
    ctx.style_mut_of(theme, |style| {
        let v = &mut style.visuals;
        v.dark_mode = dark;
        v.override_text_color = Some(p.text);
        v.panel_fill = p.surface_0;
        v.window_fill = p.surface_0;
        v.extreme_bg_color = p.surface_0;
        v.text_edit_bg_color = Some(p.surface_0);
        v.faint_bg_color = p.surface_1;
        v.code_bg_color = p.surface_1;
        v.hyperlink_color = p.ctrl;
        v.warn_fg_color = p.warn;
        v.error_fg_color = p.danger;
        v.selection.bg_fill = mix(p.surface_0, p.ctrl, 0.22);
        v.selection.stroke = Stroke::new(1.0, p.ctrl);
        v.text_cursor.stroke = Stroke::new(1.5, p.text);
        v.window_corner_radius = radius(RADIUS_PANEL);
        v.menu_corner_radius = radius(RADIUS_CONTROL);
        v.window_stroke = Stroke::new(1.0, p.border);
        let shadow = Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 110 } else { 36 }),
        };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = p.surface_0;
        w.noninteractive.weak_bg_fill = p.surface_0;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.border_faint);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        w.inactive.bg_fill = p.surface_1;
        w.inactive.weak_bg_fill = p.surface_0;
        w.inactive.bg_stroke = Stroke::new(1.0, p.border);
        w.inactive.fg_stroke = Stroke::new(1.0, p.text);
        w.hovered.bg_fill = p.hover;
        w.hovered.weak_bg_fill = p.hover;
        w.hovered.bg_stroke = Stroke::new(1.0, p.border);
        w.hovered.fg_stroke = Stroke::new(1.0, p.text);
        w.active.bg_fill = p.hover;
        w.active.weak_bg_fill = p.hover;
        w.active.bg_stroke = Stroke::new(1.0, p.ctrl);
        w.active.fg_stroke = Stroke::new(1.0, p.text);
        w.open.bg_fill = p.surface_0;
        w.open.weak_bg_fill = p.surface_0;
        w.open.bg_stroke = Stroke::new(1.0, p.border);
        w.open.fg_stroke = Stroke::new(1.0, p.text);
        for state in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            state.corner_radius = radius(RADIUS_CONTROL);
            state.expansion = 0.0;
        }

        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(SPACE_2, SPACE_1);
        s.button_padding = Vec2::new(SPACE_2, SPACE_1);
        s.menu_margin = egui::Margin::same(SPACE_1 as i8);
        s.interact_size.y = 24.0;
        // Затухание краёв прокрутки — лишний шум: UI v2 его не рисует.
        s.scroll.fade.strength = 0.0;

        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Small, sans(TEXT_XS)),
            (TextStyle::Body, sans(TEXT_BASE)),
            (TextStyle::Button, sans(TEXT_SM)),
            (TextStyle::Heading, sans_semibold(TEXT_LG)),
            (TextStyle::Monospace, mono(TEXT_SM)),
        ]
        .into();
        style.interaction.selectable_labels = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const TOKENS_CSS: &str = include_str!("../../../design/tokens.css");
    const TOKENS_SYSTEM_CSS: &str = include_str!("../../../design/tokens-system.css");

    /// Пары `--bt-имя: значение` куска CSS — независимо от разбора в `build.rs`.
    fn vars(css: &str) -> BTreeMap<String, String> {
        css.lines()
            .filter_map(|line| {
                let line = line.trim();
                let (name, value) = line.split_once(':')?;
                name.starts_with("--bt-").then(|| {
                    (
                        name.to_owned(),
                        value.trim().trim_end_matches(';').to_owned(),
                    )
                })
            })
            .collect()
    }

    fn section<'a>(css: &'a str, from: &str, to: &str) -> &'a str {
        let start = css.find(from).expect("начало блока");
        let end = css[start..].find(to).map_or(css.len(), |end| start + end);
        &css[start..end]
    }

    fn color(value: &str) -> Color32 {
        let rgb = u32::from_str_radix(value.trim_start_matches('#'), 16).expect("#rrggbb");
        Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    #[test]
    fn every_css_variable_has_a_constant_with_the_same_value() {
        let base = vars(section(
            TOKENS_CSS,
            ":root {",
            r#":root[data-theme="light"]"#,
        ));
        let dark = vars(section(TOKENS_CSS, "html.dark {", "}"));
        assert!(!dark.is_empty(), "тёмная тема не найдена в tokens.css");

        let theme: Vec<&str> = Palette::CSS_NAMES.to_vec();
        let dark_names: Vec<&str> = dark.keys().map(String::as_str).collect();
        assert_eq!(theme, dark_names, "поля Palette и переменные тёмной темы");

        for (name, value) in &base {
            if let Some(light) = LIGHT.by_css_name(name) {
                assert_eq!(light, color(value), "{name} светлой темы");
            } else {
                assert_eq!(
                    tokens::common_css_value(name),
                    Some(value.as_str()),
                    "{name}: нет константы или значение расходится"
                );
            }
        }
        for (name, value) in &dark {
            assert_eq!(
                DARK.by_css_name(name),
                Some(color(value)),
                "{name} тёмной темы"
            );
        }
    }

    #[test]
    fn the_system_dark_theme_matches_the_dark_constants() {
        let system = vars(section(
            TOKENS_SYSTEM_CSS,
            "prefers-color-scheme: dark",
            "}",
        ));
        assert_eq!(system.len(), Palette::CSS_NAMES.len());
        for (name, value) in &system {
            assert_eq!(DARK.by_css_name(name), Some(color(value)), "{name}");
        }
    }

    #[test]
    fn sizes_come_from_the_tokens() {
        assert_eq!(tokens::TITLE_HEIGHT, 44.0);
        assert_eq!(radius(tokens::RADIUS_CONTROL), CornerRadius::same(6));
    }

    #[test]
    fn mix_blends_like_color_mix() {
        let black = Color32::from_rgb(0, 0, 0);
        let white = Color32::from_rgb(255, 255, 255);
        assert_eq!(mix(black, white, 0.5), Color32::from_rgb(128, 128, 128));
        assert_eq!(mix(black, white, 0.0), black);
    }
}
