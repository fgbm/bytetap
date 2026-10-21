//! Состояние приложения и каркас экранов.
//!
//! Экраны — сценарии, шаг режима, запись, просмотр журнала. Сценарии и шаг — шаг 2
//! этапа U1 (`scenarios.rs`, `step.rs`), запись — шаг 3 (`recording.rs`, `live.rs`);
//! просмотр пока заглушка с заголовком.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytetap_core::journal::{LogDirWarningKind, RecentJournal};
use bytetap_core::session::SessionConfig;
use bytetap_core::transport::Source;
use bytetap_core::transport::serial::PortInfo;
use chrono::{DateTime, FixedOffset};
use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke, Theme, Ui, vec2};

use crate::background::{self, Interface, Loaded, Task};
use crate::errors::StartError;
use crate::form::FormState;
use crate::icons::Icon;
use crate::journal::{JournalOpen, PageError, SharedJournalCache};
use crate::live::{self, Action, JournalFailure, LiveSession, Phase, StopOutcome};
use crate::recording::{RecordingFixture, RecordingView};
use crate::settings::{LoadWarning, SessionMode, Settings, Store, ThemeMode, WindowState};
use crate::theme::tokens::{SPACE_2, SPACE_3, SPACE_5, TEXT_XS};
use crate::theme::{self, Palette, mix};
use crate::viewing::JournalView;
use crate::{diag, t, titlebar, widgets};

/// Страница журнала, которая читается в фоне: номер страницы и задача.
type PageTask = (
    usize,
    Task<Result<Vec<bytetap_core::journal::Event>, PageError>>,
);

/// Сколько последних записей показывает стартовый экран.
pub const RECENT_SHOWN: usize = 3;

/// Окно закрыто принудительно, не дождавшись остановки записи: `main` после выхода
/// показывает системное сообщение — журнал мог быть дописан не полностью.
pub static FORCED_EXIT: AtomicBool = AtomicBool::new(false);

/// Тестовый хук смоука записи (CI): `tcp_server:127.0.0.1:0` — запись стартует сразу.
pub const AUTOSTART_ENV: &str = "BYTETAP_TEST_AUTOSTART";

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
    pub(crate) viewing: Option<JournalView>,
    /// Ошибка открытия журнала: строка `danger` под шапкой.
    pub(crate) view_error: Option<String>,
    /// Открытие журнала в фоне: индекс большого файла строится не в потоке окна.
    view_open: Option<Task<Result<JournalOpen, PageError>>>,
    /// Страницы журнала, которые читаются в фоне.
    view_pages: Vec<PageTask>,
    /// Кеш индексов журналов: общий на приложение, индекс переживает просмотр.
    pub(crate) journal_cache: std::sync::Arc<SharedJournalCache>,
    /// Конфигурация, собранная последним «Старт».
    pending: Option<SessionConfig>,
    /// Runtime записи; `None` — тесты без сессий: «Старт» только собирает конфигурацию.
    runtime: Option<tokio::runtime::Handle>,
    pub(crate) phase: Phase,
    start_task: Option<Task<Result<LiveSession, StartError>>>,
    pub(crate) live: Option<LiveSession>,
    stop_task: Option<Task<StopOutcome>>,
    /// Экран записи: зеркало ленты, прокрутка, сводка.
    pub(crate) rec: Option<RecordingView>,
    /// Отказ журнала: строка под шапкой до «Выбрать другую папку» или «Открыть записанное».
    pub(crate) journal_failure: Option<JournalFailure>,
    /// Журнал последней записи.
    pub(crate) recorded_path: Option<PathBuf>,
    /// Открыт диалог комментария: набранный текст.
    pub(crate) note_prompt: Option<String>,
    /// Комментарий относится к журналу на просмотре, а не к идущей сессии.
    pub(crate) journal_note: bool,
    /// Закрытие окна ждёт остановки записи с этого момента.
    closing: Option<Instant>,
    /// Остановка не уложилась в предел: следующее закрытие не отменяется.
    force_close: bool,
    /// Снимки и тесты: данные окружения даны заранее, фоновых опросов нет.
    fixtures: bool,
    pub(crate) now: Option<DateTime<FixedOffset>>,
    /// Настройки изменены и ещё не записаны.
    dirty: bool,
    /// Запомненное положение окна (физические пиксели) и разворот: ставятся в первом
    /// кадре, когда известен масштаб монитора окна.
    restore: Option<(Option<[i32; 2]>, bool)>,
    /// Окно приложения: нужно, чтобы поменять цвет рамки DWM при смене темы.
    window_handle: Option<eframe::wgpu::rwh::RawWindowHandle>,
    /// Контекст egui: нужен фоновым задачам просмотра журнала.
    context: Option<egui::Context>,
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
        // Файл настроек не прочитан или испорчен без копии: до конца сессии он не
        // перезаписывается, иначе пропали бы настройки пользователя.
        let store = store.map(|store| {
            if warning.as_ref().is_some_and(LoadWarning::read_only) {
                diag::error(format!(
                    "настройки в {} не будут записаны до перезапуска",
                    store.path().display()
                ));
                store.into_read_only()
            } else {
                store
            }
        });
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
            view_error: None,
            view_open: None,
            view_pages: Vec::new(),
            journal_cache: std::sync::Arc::new(SharedJournalCache::default()),
            pending: None,
            runtime: None,
            phase: Phase::Idle,
            start_task: None,
            live: None,
            stop_task: None,
            rec: None,
            journal_failure: None,
            recorded_path: None,
            note_prompt: None,
            journal_note: false,
            closing: None,
            force_close: false,
            fixtures: false,
            now: None,
            dirty: false,
            restore: None,
            window_handle: None,
            context: Some(ctx.clone()),
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
        if screen == Screen::Recording && self.rec.is_none() {
            self.rec = Some(RecordingView::detached(self.settings.config.clone()));
        }
        self.screen = screen;
        self
    }

    /// Runtime записи: «Старт» запускает сессию ядра в нём.
    #[must_use]
    pub fn with_runtime(mut self, runtime: tokio::runtime::Handle) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Экран записи на синтетических данных: снимки и тесты без сессии.
    #[must_use]
    pub fn with_recording_fixture(mut self, fixture: RecordingFixture) -> Self {
        self.recorded_path.clone_from(&fixture.journal_path);
        self.rec = Some(RecordingView::fixture(
            fixture,
            self.settings.config.clone(),
        ));
        self.phase = Phase::Recording;
        self.screen = Screen::Recording;
        self
    }

    /// Строка отказа журнала под шапкой: снимки и тесты.
    #[must_use]
    pub fn with_journal_failure(
        mut self,
        failure: JournalFailure,
        recorded: Option<PathBuf>,
    ) -> Self {
        self.journal_failure = Some(failure);
        self.recorded_path = recorded;
        self.phase = Phase::Failed;
        self
    }

    /// Тестовый хук смоука (CI, [`AUTOSTART_ENV`]): `tcp_server:<адрес>` — запись
    /// TCP-сервера с автоответом «Авто» в папку журналов из настроек сразу после запуска.
    #[must_use]
    pub fn with_test_autostart(mut self, ctx: &egui::Context, spec: &str) -> Self {
        let listen = spec
            .strip_prefix("tcp_server:")
            .and_then(|addr| addr.parse::<std::net::SocketAddr>().ok());
        let Some(listen) = listen else {
            diag::error(format!(
                "{AUTOSTART_ENV}: не понят `{spec}`, нужен tcp_server:<адрес:порт>"
            ));
            return self;
        };
        diag::info(format!("{AUTOSTART_ENV}: запись TCP-сервера на {listen}"));
        let mut cfg = self.settings.config.clone();
        cfg.mode = SessionMode::TcpServer;
        cfg.listen_host = listen.ip().to_string();
        cfg.listen_port = listen.port();
        let config = SessionConfig::new(listen)
            .with_responder(bytetap_core::journal::Autoresponder::Auto)
            .with_log_dir(cfg.log_dir.clone());
        self.launch(ctx, config, cfg);
        self
    }

    /// Сейчас идёт запись (или её старт, или остановка).
    pub fn session_busy(&self) -> bool {
        self.start_task.is_some() || self.live.is_some() || self.stop_task.is_some()
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn recording(&self) -> Option<&RecordingView> {
        self.rec.as_ref()
    }

    pub fn recording_mut(&mut self) -> Option<&mut RecordingView> {
        self.rec.as_mut()
    }

    pub fn journal_failure(&self) -> Option<&JournalFailure> {
        self.journal_failure.as_ref()
    }

    pub fn recorded_path(&self) -> Option<&PathBuf> {
        self.recorded_path.as_ref()
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

    /// Просмотр журнала на синтетических данных: снимки и тесты без файла.
    #[must_use]
    pub fn with_journal_fixture(mut self, view: JournalView) -> Self {
        self.viewing = Some(view);
        self.screen = Screen::Viewing;
        self
    }

    /// Положение окна в физических пикселях (уже проверенное на видимость) и разворот,
    /// которые окно получит в первом кадре.
    #[must_use]
    pub fn with_window_restore(mut self, position: Option<[i32; 2]>, maximized: bool) -> Self {
        self.restore = Some((position, maximized));
        self
    }

    /// Хранилище настроек ничего не пишет до конца сессии.
    pub fn settings_read_only(&self) -> bool {
        self.store.as_ref().is_some_and(Store::is_read_only)
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
                if self.journal_failure.is_some() {
                    self.dismiss_failure();
                }
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
                self.open_journal(ctx, path);
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

    /// Просмотр журнала (шаг 4 этапа): индекс журнала строится в фоне, окно не ждёт
    /// ввод-вывод (U1-AC18).
    pub(crate) fn open_journal(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.viewing = None;
        self.view_error = None;
        self.view_pages.clear();
        self.screen = Screen::Viewing;
        let cache = std::sync::Arc::clone(&self.journal_cache);
        self.view_open = Some(Task::spawn(ctx, move || {
            let cache = cache.handle();
            JournalOpen::open(&cache, &path)
        }));
    }

    /// Закрыть просмотр журнала: возвращает стартовый экран.
    pub(crate) fn close_journal(&mut self) {
        self.viewing = None;
        self.view_error = None;
        self.view_open = None;
        self.view_pages.clear();
        self.screen = Screen::Scenarios;
    }

    /// Открытый на просмотр журнал.
    pub fn viewing(&self) -> Option<&JournalView> {
        self.viewing.as_ref()
    }

    /// Путь открытого на просмотр журнала.
    pub fn viewing_path(&self) -> Option<&PathBuf> {
        self.viewing.as_ref().map(|view| &view.path)
    }

    /// Ошибка открытия журнала.
    pub fn view_error(&self) -> Option<&str> {
        self.view_error.as_deref()
    }

    /// Открыть диалог комментария к журналу.
    pub(crate) fn open_journal_note(&mut self) {
        self.note_prompt = Some(String::new());
        self.journal_note = true;
    }

    /// Дописать комментарий в журнал (FR-23): файл растёт, сводка и лента обновляются.
    pub(crate) fn append_journal_note(&mut self, text: &str) {
        let Some(view) = &self.viewing else {
            return;
        };
        let path = view.path.clone();
        let result = {
            let cache = self.journal_cache.handle();
            cache.append_note(&path, text)
        };
        match result {
            Ok(()) => {
                diag::info("комментарий добавлен в журнал");
                self.reload_journal();
            }
            Err(err) => diag::error(format!("комментарий не записан: {err}")),
        }
    }

    /// Перечитать журнал после комментария: сводка и лента видят новое событие.
    ///
    /// Экран не пересоздаётся: прокрутка и загруженные страницы остаются на месте.
    fn reload_journal(&mut self) {
        let Some(view) = &self.viewing else {
            return;
        };
        let path = view.path.clone();
        let cache = std::sync::Arc::clone(&self.journal_cache);
        let ctx = self.pending_context();
        self.view_open = Some(Task::spawn(&ctx, move || {
            let cache = cache.handle();
            JournalOpen::open(&cache, &path)
        }));
    }

    /// Контекст egui для фоновой задачи: окно уже создано, контекст доступен.
    fn pending_context(&self) -> egui::Context {
        self.context.clone().unwrap_or_default()
    }

    /// Переход к событию по доле времени на карте сессии.
    pub(crate) fn seek_journal(&mut self, fraction: f32) {
        let Some(view) = &mut self.viewing else {
            return;
        };
        view.seek = Some(view.event_at_fraction(fraction));
    }

    /// Рамка видимой области ленты на карте сессии.
    pub(crate) fn set_visible(&mut self, visible: Option<(f32, f32)>) {
        if let Some(view) = &mut self.viewing {
            view.visible = visible;
        }
    }

    /// Попросить страницы журнала, покрывающие видимые строки.
    pub(crate) fn request_pages(&mut self, ctx: &egui::Context, from: usize, to: usize) {
        let Some(view) = &mut self.viewing else {
            return;
        };
        let missing = view.source.missing_pages(from, to);
        if missing.is_empty() {
            return;
        }
        let cache = std::sync::Arc::clone(&self.journal_cache);
        let path = view.path.clone();
        for (page, start) in missing {
            view.source.mark_pending(page);
            let cache = std::sync::Arc::clone(&cache);
            let path = path.clone();
            let task = Task::spawn(ctx, move || {
                let cache = cache.handle();
                cache.read(&path, start, crate::journal::PAGE_SIZE)
            });
            self.view_pages.push((page, task));
        }
    }

    /// Итоги фоновых задач просмотра: открытый журнал и прочитанные страницы.
    fn poll_view(&mut self, ctx: &egui::Context) {
        if let Some(task) = &mut self.view_open
            && let Some(result) = task.poll()
        {
            self.view_open = None;
            match result {
                Ok(opened) => {
                    let total = opened.summary.events;
                    match &mut self.viewing {
                        // Журнал уже открыт: обновляем сводку и длины, экран остаётся.
                        Some(view) => {
                            view.summary = opened.summary.clone();
                            view.first_event = opened.first_event.clone();
                            view.source.set_lengths(opened.lengths.clone());
                            view.source.extend(total);
                            view.invalidate();
                        }
                        None => {
                            self.viewing = Some(JournalView::new(
                                opened.path.clone(),
                                opened.summary.clone(),
                                opened.lengths.clone(),
                                opened.first_event.clone(),
                            ));
                        }
                    }
                    diag::info(format!(
                        "журнал открыт: {} событий, {}",
                        total,
                        opened.path.display()
                    ));
                    // Первая страница нужна сразу: лента иначе пустая.
                    self.request_pages(ctx, 0, 1);
                }
                Err(err) => {
                    diag::error(format!("журнал не открылся: {err}"));
                    self.view_error = Some(t!("journal.openFailed", reason = err.message));
                }
            }
        }
        let mut done: Vec<(usize, Result<Vec<bytetap_core::journal::Event>, PageError>)> =
            Vec::new();
        self.view_pages.retain_mut(|(page, task)| {
            if let Some(result) = task.poll() {
                done.push((*page, result));
                return false;
            }
            true
        });
        for (page, result) in done {
            let Some(view) = &mut self.viewing else {
                continue;
            };
            match result {
                Ok(events) => {
                    view.source.store_page(page, events);
                    view.collect_conns();
                }
                Err(err) => {
                    diag::error(format!("страница журнала не прочитана: {err}"));
                    view.source.mark_failed(page);
                }
            }
        }
    }

    /// Вид ленты: «Текст» или «HEX»; выбор сохраняется в настройках.
    pub(crate) fn set_feed_view(&mut self, view: crate::settings::FeedView) {
        if self.settings.feed.view != view {
            self.settings.feed.view = view;
            self.save_settings();
        }
    }

    /// Кодировка ленты; выбор сохраняется в настройках.
    pub(crate) fn set_feed_encoding(&mut self, encoding: crate::settings::Encoding) {
        if self.settings.feed.encoding != encoding {
            self.settings.feed.encoding = encoding;
            self.save_settings();
        }
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
    pub(crate) fn start(&mut self, ctx: &egui::Context) {
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
                let cfg = self.settings.config.clone();
                self.launch(ctx, config, cfg);
            }
            Err(error) => self.start_failed(error),
        }
        self.save_settings();
    }

    /// Ошибка старта — под своим полем; поле в «Дополнительно» раскрывает его.
    fn start_failed(&mut self, error: StartError) {
        diag::info(format!(
            "старт отклонён: {:?}: {}",
            error.code, error.message
        ));
        let mode = self.settings.config.mode;
        let field = error.field(mode);
        if field == crate::errors::ErrorField::LogDir
            || (field == crate::errors::ErrorField::Address && mode == SessionMode::TcpServer)
        {
            self.form.more = true;
        }
        self.start_error = Some(error);
    }

    /// Запускает сессию ядра в runtime; без runtime (тесты) — сразу экран записи без
    /// сессии.
    fn launch(
        &mut self,
        ctx: &egui::Context,
        config: SessionConfig,
        cfg: crate::settings::AppConfig,
    ) {
        self.pending = Some(config.clone());
        let Some(runtime) = self.runtime.clone() else {
            self.rec = Some(RecordingView::detached(cfg));
            self.screen = Screen::Recording;
            return;
        };
        if self.session_busy() {
            return;
        }
        self.phase = live::reduce(self.phase, Action::StartRequested);
        self.journal_failure = None;
        diag::info(format!("старт записи: {}", describe(&config.source)));
        let repaint = ctx.clone();
        self.start_task = Some(Task::spawn_async(&runtime, ctx, async move {
            live::start(config, repaint).await
        }));
        // Сводка шапки берёт режим из настроек старта.
        self.rec = Some(RecordingView::detached(cfg));
    }

    /// «Стоп» и закрытие окна: штатная остановка в runtime, итоги — в следующем кадре.
    pub(crate) fn stop_recording(&mut self, ctx: &egui::Context) {
        self.note_prompt = None;
        match (self.live.take(), self.runtime.clone()) {
            (Some(session), Some(runtime)) => {
                self.phase = live::reduce(self.phase, Action::StopRequested);
                diag::info("остановка записи");
                self.stop_task = Some(Task::spawn_async(&runtime, ctx, session.stop()));
            }
            (Some(session), None) => self.live = Some(session),
            (None, _) => {
                if self.stop_task.is_none() && self.start_task.is_none() {
                    // Экран без сессии (тесты, снимки): назад к сценариям.
                    self.rec = None;
                    self.phase = Phase::Idle;
                    self.screen = Screen::Scenarios;
                }
            }
        }
    }

    /// Комментарий в идущую сессию (FR-23).
    pub(crate) fn add_note(&mut self, text: &str) {
        let Some(session) = &self.live else {
            return;
        };
        match session.note(text) {
            Ok(()) => diag::info("комментарий добавлен в журнал"),
            Err(err) => diag::error(format!("комментарий не записан: {err}")),
        }
    }

    pub(crate) fn dismiss_failure(&mut self) {
        self.journal_failure = None;
        self.phase = live::reduce(self.phase, Action::FailureDismissed);
    }

    /// Старт, отказ журнала и остановка: результаты задач runtime к этому кадру.
    fn poll_session(&mut self, ctx: &egui::Context) {
        if let Some(task) = &mut self.start_task
            && let Some(result) = task.poll()
        {
            self.start_task = None;
            match result {
                Ok(session) => {
                    self.phase = live::reduce(self.phase, Action::Started);
                    diag::info(format!(
                        "запись идёт: адрес {}, журнал {}",
                        session.local_addr(),
                        session.journal_path().display()
                    ));
                    let cfg = self
                        .rec
                        .take()
                        .map_or_else(|| self.settings.config.clone(), |rec| rec.config);
                    self.rec = Some(RecordingView::live(&session, cfg));
                    self.recorded_path = Some(session.journal_path().to_path_buf());
                    self.live = Some(session);
                    self.start_error = None;
                    self.screen = Screen::Recording;
                    if self.closing.is_some() {
                        self.stop_recording(ctx);
                    }
                }
                Err(error) => {
                    self.phase = live::reduce(self.phase, Action::StartFailed);
                    self.rec = None;
                    self.start_failed(error);
                }
            }
        }
        // Отказ журнала: ядро уже остановило запись, слот освобождается без «Стоп».
        if self.live.is_some()
            && let Some(failure) = self.rec.as_ref().and_then(RecordingView::failure)
        {
            diag::error(format!(
                "отказ журнала: {:?}: {}",
                failure.code, failure.message
            ));
            self.stop_recording(ctx);
            self.phase = live::reduce(self.phase, Action::JournalFailed);
            self.journal_failure = Some(failure);
        }
        if let Some(task) = &mut self.stop_task
            && let Some(outcome) = task.poll()
        {
            self.stop_task = None;
            crate::recording::log_outcome(&outcome);
            self.recorded_path = Some(outcome.journal_path.clone());
            if let Some(failure) = outcome.failure {
                self.phase = live::reduce(self.phase, Action::JournalFailed);
                self.journal_failure = Some(failure);
            } else {
                self.phase = live::reduce(self.phase, Action::Stopped);
            }
            self.rec = None;
            if self.screen == Screen::Recording {
                self.screen = Screen::Scenarios;
                self.preselect_last_card();
            }
            self.refresh_recents(ctx);
        }
        if self.journal_failure.is_some() && self.screen == Screen::Recording && self.live.is_none()
        {
            // Как в UI v2: после отказа — стартовый экран со строкой причины.
            self.screen = Screen::Scenarios;
        }
    }

    /// Закрытие окна во время записи: сначала штатная остановка (журнал заканчивается
    /// `session_end`), потом окно. Дольше [`live::STOP_WAIT`] окно не ждёт.
    fn handle_close(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested());
        if requested && self.session_busy() && !self.force_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.closing.is_none() {
                diag::info("закрытие окна во время записи: сначала остановка сессии");
                self.closing = Some(Instant::now());
                self.stop_recording(ctx);
            }
        }
        let Some(since) = self.closing else {
            return;
        };
        if !self.session_busy() {
            diag::info("запись остановлена, окно закрывается");
            self.closing = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if since.elapsed() >= live::STOP_WAIT {
            diag::error(format!(
                "запись не остановилась за {} с: окно закрывается принудительно",
                live::STOP_WAIT.as_secs()
            ));
            self.closing = None;
            self.force_close = true;
            FORCED_EXIT.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
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
            // Рамку окна рисует DWM: её цвет — тот же токен `--bt-border`, что и своя.
            if let Some(handle) = self.window_handle {
                crate::window::set_dwm_border(handle, self.pal().border);
            }
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
            && !store.is_read_only()
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

    /// Первый кадр: запомненное положение и разворот. Физические пиксели переводятся в
    /// точки по масштабу монитора, на котором окно создано, — egui-winit переведёт их
    /// обратно по тому же масштабу, и окно встанет ровно в запомненный пиксель.
    fn restore_window(&mut self, ctx: &egui::Context) {
        let Some((position, maximized)) = self.restore.take() else {
            return;
        };
        if let Some([x, y]) = position {
            let scale = ctx.pixels_per_point();
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                x as f32 / scale,
                y as f32 / scale,
            )));
        }
        if maximized {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
    }

    /// Рамка окна: скругление углов и цвет рамки средствами DWM (Windows 11).
    ///
    /// Вызывается один раз, в первом кадре: к этому моменту тема известна и цвет
    /// рамки совпадает с токеном `--bt-border`. Вне Windows и в тестах вызов ничего
    /// не делает — там рамку рисует приложение.
    fn setup_window_frame(&mut self, frame: &eframe::Frame) {
        use eframe::wgpu::rwh::HasWindowHandle as _;
        let Ok(handle) = frame.window_handle() else {
            return;
        };
        let raw = handle.as_raw();
        self.window_handle = Some(raw);
        crate::window::setup_dwm(raw, self.pal().border);
    }

    /// Размер и положение «обычного» окна: их окно получит при запуске. Развёрнутое
    /// окно запоминает только разворот, прижатое к краю (`arranged`) — ничего: его
    /// размер — половина экрана, а не выбор пользователя.
    fn track_window(&mut self, ctx: &egui::Context, arranged: bool) {
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
        } else if arranged {
            previous.map(|w| WindowState {
                maximized: false,
                ..w
            })
        } else {
            let scale = ctx.pixels_per_point();
            inner.map(|inner| WindowState {
                position: outer.map(|outer| {
                    [
                        (outer.min.x * scale).round() as i32,
                        (outer.min.y * scale).round() as i32,
                    ]
                }),
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
        let ctx = ui.ctx().clone();
        self.poll_background(&ctx);
        self.poll_view(&ctx);
        // Края — раньше шапки: нажатие на краю не должно достаться кнопкам под ним.
        titlebar::resize_edges(ui.ctx());
        let p = self.pal();
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, p.surface_0);
        let recording = self.screen == Screen::Recording && self.rec.is_some();
        let viewing = self.screen == Screen::Viewing;
        if recording {
            // Шапка, трасса, панель и лента стоят вплотную, как в UI v2.
            ui.spacing_mut().item_spacing.y = 0.0;
            self.recording_title_bar(ui);
        } else if viewing {
            ui.spacing_mut().item_spacing.y = 0.0;
            crate::viewscreen::title_bar(self, ui);
        } else {
            self.home_title_bar(ui);
        }
        self.failure_notice(ui);
        self.settings_warning(ui);
        if recording {
            self.recording_screen(ui);
            if self.live.is_some() {
                // Пульс точки «Стоп» и таймер: ~30 кадров в секунду только во время записи.
                ctx.request_repaint_after(live::FRAME);
            }
        } else if viewing {
            crate::viewscreen::error(self, ui);
            if self.viewing.is_some() {
                crate::viewscreen::notice(self, ui);
                crate::viewscreen::map(self, ui);
                crate::viewscreen::bar(self, ui);
                crate::viewscreen::feed(self, ui);
            }
        } else {
            egui::ScrollArea::vertical()
                .id_salt("screen")
                .auto_shrink(false)
                .show(ui, |ui| self.screen_body(ui));
        }
        self.profile_prompt(&ctx);
        self.note_prompt(&ctx);
        // Рамка окна — последней, поверх всего: иначе панели ленты перекрывали бы её.
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        crate::window::draw_frame(ui, &p, maximized);
    }

    /// Строка под шапкой о сброшенных настройках, как в Tauri-версии.
    fn settings_warning(&mut self, ui: &mut Ui) {
        let Some(warning) = &self.warning else {
            return;
        };
        let text = match warning {
            LoadWarning::Reset { reason } => t!("settings.warningReset", reason = reason),
            LoadWarning::Corrupt { copy: None } => t!("settings.warningCorruptNoCopy").to_owned(),
            LoadWarning::Corrupt { copy: Some(copy) } => {
                t!("settings.warningCorrupt", path = copy.display())
            }
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
            // Запись без экрана записи не бывает: запись идёт со своей шапкой.
            // Просмотр журнала — тоже: у него своя шапка и своя лента.
            Screen::Recording | Screen::Viewing => {}
        }
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

/// Источник сессии для журнала запуска.
fn describe(source: &Source) -> String {
    match source {
        Source::TcpServer { listen } => format!("tcp_server {listen}"),
        Source::TcpClient { addr, .. } => format!("tcp_client {addr}"),
        Source::Serial { settings, .. } => format!("serial {} {}", settings.port, settings.baud),
    }
}

impl eframe::App for BytetapApp {
    /// Каждый кадр, и когда окно свёрнуто: итоги задач записи и закрытие окна.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_session(ctx);
        self.handle_close(ctx);
    }

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
        // Смоук окна о сбое в CI (NFR-3): паника главного потока в первом кадре.
        if !self.ready && std::env::var_os("BYTETAP_TEST_PANIC").is_some_and(|v| v == "1") {
            panic!("BYTETAP_TEST_PANIC=1: test panic in the first frame");
        }
        self.follow_system_theme(&ctx);
        if self.ready {
            // В первом кадре окно ещё не встало на запомненное место.
            self.track_window(&ctx, crate::placement::arranged(frame));
        } else {
            self.restore_window(&ctx);
            // Рамка окна: скругление и цвет рамки средствами DWM (Windows 11).
            self.setup_window_frame(frame);
        }
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
