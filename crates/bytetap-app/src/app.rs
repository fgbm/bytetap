//! Состояние приложения и каркас экранов.
//!
//! Экраны — сценарии, шаг режима, запись, просмотр журнала; содержимое им дают шаги 2–4
//! этапа U1, сейчас у каждого только заголовок.

use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke, Theme, Ui, vec2};

use crate::icons::Icon;
use crate::settings::{LoadWarning, SessionMode, Settings, Store, ThemeMode, WindowState};
use crate::theme::tokens::{SPACE_2, SPACE_3, SPACE_5, SPACE_6, TEXT_XS};
use crate::theme::{self, Palette, mix};
use crate::{diag, t, titlebar, widgets};

/// Экран приложения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Стартовый экран: выбор сценария.
    Scenarios,
    /// Шаг сценария: настройки режима перед стартом.
    Step(SessionMode),
    /// Идёт запись.
    Recording,
    /// Просмотр сохранённого журнала.
    Viewing,
}

pub struct BytetapApp {
    pub screen: Screen,
    settings: Settings,
    /// `None` — настройки не сохраняются (снимки экранов, нет каталога конфигурации).
    store: Option<Store>,
    warning: Option<LoadWarning>,
    dark: bool,
    /// Окно уже нарисовало первый кадр: «интерфейс готов» записан.
    ready: bool,
}

impl BytetapApp {
    pub fn new(
        ctx: &egui::Context,
        settings: Settings,
        warning: Option<LoadWarning>,
        store: Option<Store>,
    ) -> Self {
        theme::install_fonts(ctx);
        egui_extras::install_image_loaders(ctx);
        let dark = resolve_dark(settings.theme, ctx.system_theme());
        theme::apply(ctx, dark);
        Self {
            screen: Screen::Scenarios,
            settings,
            store,
            warning,
            dark,
            ready: false,
        }
    }

    #[must_use]
    pub fn with_screen(mut self, screen: Screen) -> Self {
        self.screen = screen;
        self
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn dark(&self) -> bool {
        self.dark
    }

    pub fn pal(&self) -> Palette {
        theme::palette(self.dark)
    }

    fn set_dark(&mut self, ctx: &egui::Context, dark: bool) {
        if self.dark != dark {
            self.dark = dark;
            theme::apply(ctx, dark);
        }
    }

    /// Sun/Moon: явная тема, противоположная текущей; выбор сохраняется.
    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        let dark = !self.dark;
        self.settings.theme = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        self.set_dark(ctx, dark);
        self.save_settings();
    }

    fn save_settings(&self) {
        if let Some(store) = &self.store
            && let Err(err) = store.save(&self.settings)
        {
            diag::error(format!(
                "настройки не сохранены в {}: {err}",
                store.path().display()
            ));
        }
    }

    /// Системная тема меняется на лету, пока пользователь не выбрал свою.
    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        let dark = resolve_dark(self.settings.theme, ctx.system_theme());
        self.set_dark(ctx, dark);
    }

    /// Размер и положение окна вне развёрнутого состояния: их окно получит при запуске.
    fn track_window(&mut self, ctx: &egui::Context) {
        let (outer, inner, maximized, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.maximized.unwrap_or(false),
                v.minimized.unwrap_or(false),
            )
        });
        if minimized {
            return;
        }
        let previous = self.settings.window;
        let window = if maximized {
            previous.map(|w| WindowState {
                maximized: true,
                ..w
            })
        } else {
            inner.map(|inner| WindowState {
                position: outer.map(|outer| [outer.min.x, outer.min.y]),
                size: [inner.width(), inner.height()],
                maximized: false,
            })
        };
        if window.is_some() {
            self.settings.window = window;
        }
    }

    /// Весь интерфейс: шапка-заголовок окна, предупреждение о настройках и экран.
    pub fn draw(&mut self, ui: &mut Ui) {
        let p = self.pal();
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, p.surface_0);
        self.home_title_bar(ui);
        self.settings_warning(ui);
        egui::ScrollArea::vertical()
            .id_salt("screen")
            .auto_shrink(false)
            .show(ui, |ui| self.screen_body(ui));
        titlebar::resize_edges(ui.ctx());
    }

    /// Строка под шапкой о сброшенных настройках, как в Tauri-версии.
    fn settings_warning(&mut self, ui: &mut Ui) {
        let Some(warning) = &self.warning else {
            return;
        };
        let text = match warning {
            LoadWarning::Reset => t!("settings.warningReset").to_owned(),
            LoadWarning::Corrupt { copy } => t!(
                "settings.warningCorrupt",
                path = copy
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
        };
        let p = self.pal();
        let fill = mix(p.surface_0, p.warn, 0.12);
        let mut dismiss = false;
        egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(SPACE_3 as i8, SPACE_2 as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(text).size(TEXT_XS).color(p.warn));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        dismiss =
                            widgets::icon_button(ui, &p, Icon::X, 14.0, t!("settings.dismiss"))
                                .clicked();
                    });
                });
            });
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter()
            .hline(line.x_range(), line.center().y, Stroke::new(1.0, p.border));
        if dismiss {
            self.warning = None;
        }
    }

    fn screen_body(&mut self, ui: &mut Ui) {
        let title = match self.screen {
            Screen::Scenarios => t!("scenarios.question"),
            Screen::Step(SessionMode::TcpServer) => t!("scenarios.serverTitle"),
            Screen::Step(SessionMode::TcpClient) => t!("scenarios.clientTitle"),
            Screen::Step(SessionMode::Serial) => t!("scenarios.serialTitle"),
            Screen::Recording => t!("screen.recording"),
            Screen::Viewing => t!("screen.viewing"),
        };
        ui.add_space(SPACE_6);
        ui.horizontal(|ui| {
            ui.add_space(SPACE_5);
            ui.heading(title);
        });
    }
}

/// Тёмная ли тема: выбор пользователя, а при «системной» — тема ОС (без неё — светлая).
pub fn resolve_dark(mode: ThemeMode, system: Option<Theme>) -> bool {
    match mode {
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
        ThemeMode::System => system == Some(Theme::Dark),
    }
}

impl eframe::App for BytetapApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.ready
            && let Some(state) = frame.wgpu_render_state()
        {
            let info = state.adapter.get_info();
            diag::info(format!(
                "рендерер: wgpu {:?}, адаптер {} ({:?}), драйвер {} {}",
                info.backend, info.name, info.device_type, info.driver, info.driver_info
            ));
        }
        self.follow_system_theme(&ctx);
        self.track_window(&ctx);
        self.draw(ui);
        if !self.ready {
            self.ready = true;
            diag::info("интерфейс готов");
        }
    }

    fn on_exit(&mut self) {
        diag::info("выход");
        self.save_settings();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.pal().surface_0.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_theme_is_followed_only_in_system_mode() {
        assert!(resolve_dark(ThemeMode::System, Some(Theme::Dark)));
        assert!(!resolve_dark(ThemeMode::System, Some(Theme::Light)));
        assert!(!resolve_dark(ThemeMode::System, None));
        assert!(resolve_dark(ThemeMode::Dark, Some(Theme::Light)));
        assert!(!resolve_dark(ThemeMode::Light, Some(Theme::Dark)));
    }

    #[test]
    fn the_theme_toggle_is_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path().join("settings.json"));
        let ctx = egui::Context::default();
        let mut app = BytetapApp::new(&ctx, Settings::default(), None, Some(store.clone()));
        let was_dark = app.dark();
        app.toggle_theme(&ctx);
        assert_eq!(app.dark(), !was_dark);
        let (saved, _) = store.load("");
        let expected = if was_dark {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        assert_eq!(saved.theme, expected);
    }
}
