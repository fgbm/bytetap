//! Состояние приложения и каркас экранов.
//!
//! Экраны — сценарии, шаг режима, запись, просмотр журнала. Сценарии и шаг — шаг 2
//! этапа U1 (`scenarios.rs`, `step.rs`); запись и просмотр пока заглушки с заголовком.

use std::path::PathBuf;

use bytetap_core::journal::{LogDirWarningKind, RecentJournal};
use bytetap_core::session::SessionConfig;
use bytetap_core::transport::serial::PortInfo;
use chrono::{DateTime, FixedOffset};
use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke, Theme, Ui, vec2};

use crate::background::{self, Interface, Loaded, Task};
use crate::errors::StartError;
use crate::form::FormState;
use crate::icons::Icon;
use crate::settings::{LoadWarning, SessionMode, Settings, Store, ThemeMode, WindowState};
use crate::theme::tokens::{SPACE_2, SPACE_3, SPACE_4, SPACE_5, TEXT_XS};
use crate::theme::{self, Palette, mix};
use crate::{diag, t, titlebar, widgets};

/// Сколько последних записей показывает стартовый экран.
pub const RECENT_SHOWN: usize = 3;

/// Данные окружения для снимков и тестов вместо фонового опроса системы.
#[derive(Clone, Debug)]
pub struct Fixtures {
    pub interfaces: Vec<Interface>,
    pub ports: Vec<PortInfo>,
    pub recents: Vec<RecentJournal>,
    /// «Сейчас» для «сегодня 10:15».
    pub now: DateTime<FixedOffset>,
}

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
    pub(crate) settings: Settings,
    /// `None` — настройки не сохраняются (снимки экранов, нет каталога конфигурации).
    store: Option<Store>,
    warning: Option<LoadWarning>,
    dark: bool,
    /// Окно уже нарисовало первый кадр: «интерфейс готов» записан.
    ready: bool,
    /// Карточка сценария под клавиатурой.
    pub(crate) selected_card: usize,
    pub(crate) form: FormState,
    /// Ошибка последнего «Старт» по коду: текст под своим полем.
    pub(crate) start_error: Option<StartError>,
    pub(crate) interfaces: Loaded<Vec<Interface>>,
    pub(crate) ports: Loaded<Vec<PortInfo>>,
    pub(crate) recents: Loaded<Vec<RecentJournal>>,
    /// Предупреждение о папке журналов и папка, для которой оно посчитано.
    pub(crate) log_dir_warning: Option<(String, Option<LogDirWarningKind>)>,
    log_dir_warning_task: Option<(String, Task<Option<LogDirWarningKind>>)>,
    folder_pick: Option<Task<Option<PathBuf>>>,
    journal_pick: Option<Task<Option<PathBuf>>>,
    folder_open: Option<Task<Result<(), String>>>,
    /// До какого времени (`input.time`) кнопка показывает «Скопировано».
    pub(crate) copied_until: Option<f64>,
    /// Открыт диалог имени профиля: набранный текст.
    pub(crate) profile_prompt: Option<String>,
    /// Журнал, открытый на просмотр.
    pub(crate) viewing: Option<PathBuf>,
    /// Конфигурация, собранная «Старт»; запускает её шаг 3.
    pending: Option<SessionConfig>,
    /// Снимки и тесты: данные окружения даны заранее, фоновых опросов нет.
    fixtures: bool,
    pub(crate) now: Option<DateTime<FixedOffset>>,
    /// Настройки изменены и ещё не записаны.
    dirty: bool,
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
        let form = FormState::new(&settings.config);
        let mut app = Self {
            screen: Screen::Scenarios,
            settings,
            store,
            warning,
            dark,
            ready: false,
            selected_card: 0,
            form,
            start_error: None,
            interfaces: Loaded::Idle,
            ports: Loaded::Idle,
            recents: Loaded::Idle,
            log_dir_warning: None,
            log_dir_warning_task: None,
            folder_pick: None,
            journal_pick: None,
            folder_open: None,
            copied_until: None,
            profile_prompt: None,
            viewing: None,
            pending: None,
            fixtures: false,
            now: None,
            dirty: false,
        };
        app.preselect_last_card();
        app.interfaces = Loaded::Loading(Task::spawn(ctx, background::list_interfaces));
        app.refresh_ports(ctx);
        app.refresh_recents(ctx);
        app
    }

    #[must_use]
    pub fn with_screen(mut self, screen: Screen) -> Self {
        if let Screen::Step(mode) = screen {
            self.settings.config.mode = mode;
            self.form = FormState::new(&self.settings.config);
        }
        self.screen = screen;
        self
    }

    /// Данные окружения вместо опроса системы: снимки и тесты не зависят от машины.
    #[must_use]
    pub fn with_fixtures(mut self, fixtures: Fixtures) -> Self {
        self.fixtures = true;
        self.interfaces = Loaded::Ready(fixtures.interfaces);
        self.ports = Loaded::Ready(fixtures.ports);
        self.recents = Loaded::Ready(fixtures.recents);
        self.now = Some(fixtures.now);
        self
    }

    /// Раскрыть «Дополнительно» шага сценария.
    #[must_use]
    pub fn with_more(mut self) -> Self {
        self.form.more = true;
        self
    }

    /// Конфигурация сессии, которую собрал последний «Старт».
    pub fn pending_session(&self) -> Option<&SessionConfig> {
        self.pending.as_ref()
    }

    pub fn start_error(&self) -> Option<&StartError> {
        self.start_error.as_ref()
    }

    pub fn viewing(&self) -> Option<&PathBuf> {
        self.viewing.as_ref()
    }

    pub fn selected_card(&self) -> usize {
        self.selected_card
    }

    pub(crate) fn settings_mut(&mut self) -> &mut Settings {
        self.dirty = true;
        &mut self.settings
    }

    /// Порты COM в фоне (FR-12): при запуске и по «Обновить».
    pub(crate) fn refresh_ports(&mut self, ctx: &egui::Context) {
        if self.fixtures {
            return;
        }
        self.ports = Loaded::Loading(Task::spawn(ctx, || {
            bytetap_core::transport::serial::list_ports().unwrap_or_default()
        }));
    }

    /// Последние записи папки журналов в фоне: заголовок и хвост каждого файла.
    pub(crate) fn refresh_recents(&mut self, ctx: &egui::Context) {
        if self.fixtures {
            return;
        }
        let dir = self.settings.config.log_dir.trim().to_owned();
        if dir.is_empty() {
            self.recents = Loaded::Ready(Vec::new());
            return;
        }
        self.recents = Loaded::Loading(Task::spawn(ctx, move || {
            bytetap_core::journal::recent(std::path::Path::new(&dir), RECENT_SHOWN)
                .unwrap_or_default()
        }));
    }

    /// Предупреждение о папке журналов (OneDrive, сеть) для текущей папки.
    fn refresh_log_dir_warning(&mut self, ctx: &egui::Context) {
        let dir = self.settings.config.log_dir.trim().to_owned();
        let known = self
            .log_dir_warning
            .as_ref()
            .is_some_and(|(for_dir, _)| *for_dir == dir)
            || self
                .log_dir_warning_task
                .as_ref()
                .is_some_and(|(for_dir, _)| *for_dir == dir);
        if known {
            return;
        }
        if self.fixtures || dir.is_empty() {
            let kind = (!dir.is_empty())
                .then(|| bytetap_core::journal::log_dir_warning_kind(&dir))
                .flatten();
            self.log_dir_warning = Some((dir, kind));
            return;
        }
        let path = dir.clone();
        self.log_dir_warning_task = Some((
            dir,
            Task::spawn(ctx, move || {
                bytetap_core::journal::log_dir_warning_kind(&path)
            }),
        ));
    }

    /// Результаты фоновых задач, пришедшие к этому кадру.
    fn poll_background(&mut self, ctx: &egui::Context) {
        self.interfaces.poll();
        self.ports.poll();
        self.recents.poll();
        if let Some((dir, task)) = &mut self.log_dir_warning_task
            && let Some(kind) = task.poll()
        {
            self.log_dir_warning = Some((dir.clone(), kind));
            self.log_dir_warning_task = None;
        }
        self.refresh_log_dir_warning(ctx);
        if let Some(task) = &mut self.folder_pick
            && let Some(picked) = task.poll()
        {
            self.folder_pick = None;
            if let Some(dir) = picked {
                self.settings_mut().config.log_dir = dir.to_string_lossy().into_owned();
                if self.start_error.as_ref().is_some_and(|e| {
                    e.field(self.settings.config.mode) == crate::errors::ErrorField::LogDir
                }) {
                    self.start_error = None;
                }
                self.save_settings();
            }
        }
        if let Some(task) = &mut self.journal_pick
            && let Some(picked) = task.poll()
        {
            self.journal_pick = None;
            if let Some(path) = picked {
                self.open_journal(path);
            }
        }
        if let Some(task) = &mut self.folder_open
            && let Some(result) = task.poll()
        {
            self.folder_open = None;
            if let Err(err) = result {
                diag::error(format!("папка журналов не открылась: {err}"));
            }
        }
    }

    /// Диалог выбора папки журналов (FR-19) — в фоне: окно не ждёт диалог.
    pub(crate) fn pick_log_dir(&mut self, ctx: &egui::Context) {
        if self.folder_pick.is_some() {
            return;
        }
        let start = self.settings.config.log_dir.clone();
        self.folder_pick = Some(Task::spawn(ctx, move || background::pick_folder(&start)));
    }

    /// «Открыть журнал…»: диалог `*.jsonl` в фоне.
    pub(crate) fn pick_journal(&mut self, ctx: &egui::Context) {
        if self.journal_pick.is_some() {
            return;
        }
        let start = self.settings.config.log_dir.clone();
        self.journal_pick = Some(Task::spawn(ctx, move || background::pick_journal(&start)));
    }

    /// «Открыть папку журналов» в проводнике: только существующий локальный каталог.
    pub(crate) fn open_log_dir(&mut self, ctx: &egui::Context) {
        let dir = self.settings.config.log_dir.clone();
        self.folder_open = Some(Task::spawn(ctx, move || {
            let path = background::folder_to_open(&dir)
                .ok_or_else(|| format!("`{dir}` is not an existing absolute folder"))?;
            background::open_folder(&path)
        }));
    }

    /// Просмотр журнала (шаг 4 этапа): пока экран с путём.
    pub(crate) fn open_journal(&mut self, path: PathBuf) {
        self.viewing = Some(path);
        self.screen = Screen::Viewing;
    }

    /// Карточка последнего сценария выбрана заранее.
    fn preselect_last_card(&mut self) {
        if let Some(run) = &self.settings.last_run {
            self.selected_card = crate::scenarios::card_index(run.mode);
        }
    }

    /// Шаг сценария: режим запоминается, поля берутся из последних настроек.
    pub(crate) fn open_step(&mut self, mode: SessionMode) {
        self.settings_mut().config.mode = mode;
        self.form = FormState::new(&self.settings.config);
        self.start_error = None;
        self.copied_until = None;
        self.screen = Screen::Step(mode);
        self.save_settings();
    }

    /// «← Сценарии»: набранное сохраняется, список последних записей перечитывается.
    pub(crate) fn back_to_scenarios(&mut self, ctx: &egui::Context) {
        if matches!(self.screen, Screen::Step(_)) {
            let form = self.form.clone();
            form.commit(&mut self.settings_mut().config);
        }
        self.start_error = None;
        self.screen = Screen::Scenarios;
        self.preselect_last_card();
        self.save_settings();
        self.refresh_recents(ctx);
    }

    /// «Старт»: поля проверены — набранное уходит в настройки, из них собирается
    /// конфигурация ядра. Ошибка — текст под своим полем; файл журнала не создаётся.
    pub(crate) fn start(&mut self) {
        self.form.attempted = true;
        self.start_error = None;
        let cfg = &self.settings.config;
        if !self.form.valid(cfg) {
            return;
        }
        let mut cfg = cfg.clone();
        self.form.commit(&mut cfg);
        self.settings_mut().config = cfg;
        match crate::form::session_config(
            &self.settings.config,
            bytetap_core::journal::default_log_dir(),
        ) {
            Ok(config) => {
                self.settings.last_run = Some(crate::form::last_run_of(&self.settings.config));
                self.pending = Some(config);
                self.screen = Screen::Recording;
            }
            Err(error) => {
                diag::info(format!(
                    "старт отклонён: {:?}: {}",
                    error.code, error.message
                ));
                if error.field(self.settings.config.mode) == crate::errors::ErrorField::LogDir
                    || (error.field(self.settings.config.mode)
                        == crate::errors::ErrorField::Address
                        && self.settings.config.mode == SessionMode::TcpServer)
                {
                    self.form.more = true;
                }
                self.start_error = Some(error);
            }
        }
        self.save_settings();
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

    pub(crate) fn save_settings(&mut self) {
        self.dirty = false;
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
        self.poll_background(&ui.ctx().clone());
        let p = self.pal();
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, p.surface_0);
        self.home_title_bar(ui);
        self.settings_warning(ui);
        egui::ScrollArea::vertical()
            .id_salt("screen")
            .auto_shrink(false)
            .show(ui, |ui| self.screen_body(ui));
        self.profile_prompt(ui.ctx());
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
        match self.screen {
            Screen::Scenarios => self.scenarios(ui),
            Screen::Step(mode) => self.step(ui, mode),
            Screen::Recording | Screen::Viewing => self.placeholder(ui),
        }
    }

    /// Запись и просмотр — шаги 3 и 4 этапа: пока заголовок, путь журнала и «← Сценарии».
    fn placeholder(&mut self, ui: &mut Ui) {
        let p = self.pal();
        let title = if self.screen == Screen::Recording {
            t!("screen.recording")
        } else {
            t!("screen.viewing")
        };
        let path = self
            .viewing
            .as_ref()
            .filter(|_| self.screen == Screen::Viewing)
            .map(|path| path.display().to_string());
        let ctx = ui.ctx().clone();
        centered(ui, 512.0, |ui| {
            ui.add_space(SPACE_4);
            if widgets::link_button(ui, &p, Icon::ArrowLeft, 15.0, t!("form.back"), false).clicked()
            {
                self.back_to_scenarios(&ctx);
            }
            ui.add_space(SPACE_3);
            ui.heading(title);
            if let Some(path) = path {
                ui.add_space(SPACE_2);
                ui.label(
                    RichText::new(path)
                        .font(theme::mono(TEXT_XS))
                        .color(p.text_muted),
                );
            }
        });
    }
}

/// Колонка экрана по центру: не шире `max_width`, с полями `SPACE_5` по краям.
pub(crate) fn centered<R>(ui: &mut Ui, max_width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let available = ui.available_width();
    let width = max_width.min(available - 2.0 * SPACE_5).max(0.0);
    let left = ((available - width) / 2.0).floor();
    let rect = egui::Rect::from_min_size(
        ui.cursor().min + vec2(left, 0.0),
        vec2(width, ui.available_height().max(1.0)),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
        add,
    )
    .inner
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
        if self.dirty && self.screen != Screen::Step(self.settings.config.mode) {
            self.save_settings();
        }
        if !self.ready {
            self.ready = true;
            diag::info("интерфейс готов");
        }
    }

    fn on_exit(&mut self) {
        diag::info("выход");
        if matches!(self.screen, Screen::Step(_)) {
            let form = self.form.clone();
            form.commit(&mut self.settings.config);
        }
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
