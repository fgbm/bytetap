//! Состояние приложения и каркас экранов.
//!
//! Экраны — сценарии, шаг режима, запись, просмотр журнала. Сценарии и шаг — шаг 2
//! этапа U1 (`scenarios.rs`, `step.rs`), запись — шаг 3 (`recording.rs`, `live.rs`),
//! просмотр — шаг 4 (`viewscreen.rs` с индексом `journal.rs` и переводом доли времени
//! в событие `viewing.rs`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytetap_core::journal::{LogDirWarningKind, RecentJournal, Stats};
use bytetap_core::session::SessionConfig;
use bytetap_core::transport::Source;
use bytetap_core::transport::serial::PortInfo;
use chrono::{DateTime, FixedOffset};
use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke, Theme, Ui, vec2};

use crate::background::{self, Interface, Loaded, Task};
use crate::errors::StartError;
use crate::form::FormState;
#[cfg(feature = "test-hooks")]
use crate::hooks;
use crate::icons::Icon;
use crate::journal::{
    JournalCache, JournalOpen, NoteWritten, PageDone, PageError, PageJob, PageLoad, PageReader,
};
use crate::live::{self, Action, JournalFailure, LiveSession, Phase, StopOutcome};
use crate::recording::{PULSE_MAX_PERIOD, RecordingFixture, RecordingView};
use crate::settings::{LoadWarning, SessionMode, Settings, Store, ThemeMode, WindowState};
use crate::theme::tokens::{SPACE_2, SPACE_3, SPACE_5, TEXT_XS};
use crate::theme::{self, Palette, mix};
use crate::viewing::JournalView;
use crate::{diag, t, titlebar, widgets};

/// Сколько последних записей показывает стартовый экран.
pub const RECENT_SHOWN: usize = 3;

/// Окно закрыто принудительно, не дождавшись остановки записи: `main` после выхода
/// показывает системное сообщение — журнал мог быть дописан не полностью.
pub static FORCED_EXIT: AtomicBool = AtomicBool::new(false);

/// Текст последнего системного сообщения о неполном журнале: тест проверяет его, а
/// пользователь видит окно с той же причиной (U1-AC17).
pub static CLOSE_NOTICE: Mutex<Option<String>> = Mutex::new(None);

/// Сообщение о неполном журнале при закрытии окна: `main` показывает его системным
/// окном уже после выхода из окна приложения.
///
/// Показывать его из окна нельзя: системное окно модально, и на Windows оно
/// заблокирует поток окна до нажатия «ОК» (U1-AC17).
pub fn take_close_notice() -> Option<String> {
    CLOSE_NOTICE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// Тестовый хук смоука записи (CI): `tcp_server:127.0.0.1:0` — запись стартует сразу.
/// Читается только вместе с [`HOOKS_ENV`], как и остальные тестовые ключи.
#[cfg(feature = "test-hooks")]
pub const AUTOSTART_ENV: &str = "BYTETAP_TEST_AUTOSTART";

/// Тестовый хук паники первого кадра (CI, NFR-3): окно о сбое и запись в журнал
/// запуска. Читается только вместе с [`HOOKS_ENV`].
#[cfg(feature = "test-hooks")]
pub const PANIC_ENV: &str = "BYTETAP_TEST_PANIC";

/// Тестовый хук снимков для согласования (CI, U1-AC21): экран и тема снимка,
/// `BYTETAP_TEST_SCREEN=recording-hex-dark`. Читается только вместе с
/// [`HOOKS_ENV`]: случайная переменная окружения в чужой оболочке не переключит
/// приложение на синтетический экран и не запишет журнал в чужую папку.
#[cfg(feature = "test-hooks")]
pub const SCREEN_ENV: &str = "BYTETAP_TEST_SCREEN";

/// Второй ключ тестовых хуков: без него [`SCREEN_ENV`] не читается вовсе.
#[cfg(feature = "test-hooks")]
pub const HOOKS_ENV: &str = "BYTETAP_TEST_HOOKS";

/// Журнал для снимка просмотра: файл, который откроется на экране просмотра.
/// Читается только вместе с [`HOOKS_ENV`], как и остальные тестовые ключи.
#[cfg(feature = "test-hooks")]
pub const JOURNAL_ENV: &str = "BYTETAP_TEST_JOURNAL";

/// Данные окружения для снимков и тестов вместо фонового опроса системы.
#[derive(Clone, Debug)]
pub struct Fixtures {
    pub interfaces: Vec<Interface>,
    pub ports: Vec<PortInfo>,
    pub recents: Vec<RecentJournal>,
    /// «Сейчас» для «сегодня 10:15».
    pub now: DateTime<FixedOffset>,
}

/// Окружение снимков для согласования (U1-AC21): те же порты, адреса и последние
/// записи, что у эталонов `egui_kittest`, чтобы снимок в exe сравнивался с эталоном.
#[must_use]
#[cfg(feature = "test-hooks")]
pub fn review_fixtures() -> Fixtures {
    let journal = |name: &str, started_at: &str, analyzer: &str, minutes: u64| RecentJournal {
        path: format!(r"C:\Users\operator\Documents\bytetap\{name}").into(),
        started_at: started_at.to_owned(),
        analyzer: analyzer.to_owned(),
        duration_us: minutes * 60 * 1_000_000,
        mode: "tcp_server".to_owned(),
    };
    Fixtures {
        interfaces: vec![
            Interface {
                name: "Ethernet".into(),
                addr: "192.168.1.10".into(),
            },
            Interface {
                name: "Wi-Fi".into(),
                addr: "10.0.0.5".into(),
            },
        ],
        ports: vec![PortInfo {
            name: "COM3".into(),
            description: "USB-SERIAL CH340".into(),
        }],
        recents: vec![
            journal(
                "bytetap-20261019101500-tcp_server.jsonl",
                "2026-10-19T10:15:00+03:00",
                "ANALYZER",
                42,
            ),
            journal(
                "bytetap-20261018180200-tcp_client.jsonl",
                "2026-10-18T18:02:00+03:00",
                "ANALYZER-2",
                21,
            ),
            journal(
                "bytetap-20261017094000-serial.jsonl",
                "2026-10-17T09:40:00+03:00",
                "",
                8,
            ),
        ],
        now: chrono::DateTime::parse_from_rfc3339("2026-10-19T12:00:00+03:00")
            .expect("review fixtures time"),
    }
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

/// Куда уйдёт текст диалога комментария.
///
/// Цель задаётся при открытии диалога и живёт вместе с ним: раньше флаг «комментарий к
/// журналу» оставался после закрытия диалога, и следующий комментарий во время записи
/// молча уходил в просмотр (FR-23, U1-AC17, U1-AC19).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteTarget {
    /// В идущую сессию: `note` в журнале записи.
    Session,
    /// В остановленный журнал на просмотре.
    Journal(PathBuf),
}

/// Диалог комментария: цель, набранный текст и ошибка дописывания.
#[derive(Clone, Debug)]
pub struct NoteDialog {
    /// Куда уйдёт комментарий.
    pub target: NoteTarget,
    /// Набранный текст.
    pub text: String,
    /// Ошибка дописывания: диалог остаётся открытым с этой строкой.
    pub error: Option<String>,
    /// Запись идёт в фоне: повторное «Дописать» не запускает вторую.
    pub sending: bool,
    /// Номер отправки, которую ждёт этот диалог; 0 — отправки нет.
    ///
    /// Диалог закрывают и открывают заново, пока комментарий ложится в файл: ответ
    /// прежней отправки не должен ни закрыть новый диалог, ни показать в нём свою
    /// ошибку. Поэтому ответ сверяется с номером, а не с самим фактом открытого диалога.
    pub request: u64,
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
    /// Пул чтения страниц журнала: 1–2 потока с очередью, а не поток на страницу.
    view_reader: Option<PageReader>,
    /// Дописывание комментария и повторное открытие сводки — в фоне.
    ///
    /// Вместе с задачей хранится номер отправки: ответ трогает только тот диалог,
    /// который эту отправку начал.
    view_note: Option<(u64, Task<Result<NoteWritten, PageError>>)>,
    /// Номер последней отправки комментария: выдаётся [`BytetapApp::send_journal_note`].
    note_request: u64,
    /// Кеш индексов журналов: общий на приложение, индекс переживает просмотр.
    ///
    /// Кеш ядра потокобезопасен сам: внешнего замка нет, поток окна его не держит.
    pub(crate) journal_cache: Arc<JournalCache>,
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
    /// Открыт диалог комментария: цель и набранный текст.
    pub(crate) note: Option<NoteDialog>,
    /// Закрытие окна ждёт остановки записи с этого момента.
    closing: Option<Instant>,
    /// Остановка не уложилась в предел: следующее закрытие не отменяется.
    force_close: bool,
    /// Ошибка остановки записи: строка на стартовом экране (как `stop_failed` в UI v2).
    pub(crate) stop_error: Option<String>,
    /// Итоги последней записи: строка на стартовом экране (U1-AC17).
    pub(crate) stop_stats: Option<Stats>,
    /// Снимки и тесты: данные окружения даны заранее, фоновых опросов нет.
    fixtures: bool,
    pub(crate) now: Option<DateTime<FixedOffset>>,
    /// Настройки изменены и ещё не записаны.
    dirty: bool,
    /// Запомненное положение окна (физические пиксели) и разворот: ставятся в первом
    /// кадре, когда известен масштаб монитора окна.
    restore: Option<(Option<[i32; 2]>, bool)>,
    /// Окно приложения: нужно, чтобы настроить DWM — скругление углов и скрытая рамка.
    window_handle: Option<eframe::wgpu::rwh::RawWindowHandle>,
    /// Рамка DWM применена после первого кадра, когда окно уже видимо.
    frame_applied: bool,
    /// Активно ли окно в прошлом кадре: по смене активности DWM настраивается повторно.
    window_focused: Option<bool>,
    /// Что о смене активности уже записано в журнал запуска: `[потеря, возвращение]`.
    activation_logged: [bool; 2],
    /// Отказ повторной настройки DWM уже записан в журнал запуска: окно переключается
    /// часто, и строка на каждое переключение забивала бы журнал.
    reapply_refusal_logged: bool,
    /// Скругляет ли своя рамка углы в этом кадре: DWM скруглил их у этого окна, окно не
    /// развёрнуто и не прижато к краю, кадр рисует не программный растр (`window.rs`).
    frame_rounded: bool,
    /// Тестовый хук снимков: меню, которое надо открыть на следующем кадре.
    hook_menu: Option<&'static str>,
    /// Кадров нарисовано с начала записи: итог уходит в журнал запуска при остановке
    /// (NFR-5: сколько кадров стоила сессия).
    frames: u64,
    /// Цена кадра записи и её сглаживание: по ней окно выбирает период следующего
    /// (`live::next_period`). Меряется только во время записи: в простое окно спит по
    /// своему таймеру, и промежуток между кадрами — не цена кадра.
    frame_cost: live::FrameBudget,
    /// Промежуток, который окно попросило в прошлом кадре: без него не отделить
    /// ожидание по таймеру от работы над кадром.
    frame_delay: Option<Duration>,
    /// Сколько прошло от начала прошлого кадра до запроса следующего: кадр рисуется
    /// после этого запроса, и без него не видно, уложился ли он в своё ожидание.
    frame_request: Option<Duration>,
    /// Когда начался прошлый кадр — по часам, а не по `unstable_dt` egui.
    frame_started: Option<Instant>,
    /// Когда начался текущий кадр: по нему считается время до запроса перерисовки.
    frame_began: Option<Instant>,
    /// Сколько длился прошлый кадр: от начала `ui` до её конца, то есть работа окна.
    frame_work: Duration,
    /// Размер окна в точках и свёрнутость в прошлом кадре: по их смене цена кадра
    /// записи забывается (разворот, восстановление, свёртывание).
    frame_window: Option<([u32; 2], bool)>,
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
            view_reader: None,
            view_note: None,
            note_request: 0,
            journal_cache: Arc::new(JournalCache::default()),
            pending: None,
            runtime: None,
            phase: Phase::Idle,
            start_task: None,
            live: None,
            stop_task: None,
            rec: None,
            journal_failure: None,
            recorded_path: None,
            note: None,
            closing: None,
            force_close: false,
            stop_error: None,
            stop_stats: None,
            fixtures: false,
            now: None,
            dirty: false,
            restore: None,
            window_handle: None,
            frame_applied: false,
            window_focused: None,
            activation_logged: [false; 2],
            reapply_refusal_logged: false,
            frame_rounded: false,
            hook_menu: None,
            frames: 0,
            frame_cost: live::FrameBudget::default(),
            frame_delay: None,
            frame_request: None,
            frame_started: None,
            frame_began: None,
            frame_work: Duration::ZERO,
            frame_window: None,
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
    #[cfg(feature = "test-hooks")]
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

    /// Экран по имени тестового хука (CI, [`SCREEN_ENV`]): `<экран>-<тема>`, например
    /// `start-light`, `serial-more-dark`, `recording-hex-light`, `viewing-text-dark`.
    ///
    /// Окружение синтетическое (те же данные, что у снимков `egui_kittest`), чтобы
    /// снимок в exe совпадал с эталоном на любой машине. Запись на экране
    /// `recording-*` — пустая лента: строки приходят, если CI запускает сессию хуком
    /// [`AUTOSTART_ENV`] и шлёт в неё сообщения.
    #[must_use]
    #[cfg(feature = "test-hooks")]
    pub fn with_test_screen(mut self, ctx: &egui::Context, spec: &str) -> Self {
        let (name, dark) = match spec.rsplit_once('-') {
            Some((name, "dark")) => (name, true),
            Some((name, "light")) => (name, false),
            _ => (spec, false),
        };
        diag::info(format!("{SCREEN_ENV}: экран {name}, тёмная тема {dark}"));
        // Тема задана явно: иначе системная тема машины перебила бы её в первом кадре.
        self.settings.theme = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        self.set_dark(ctx, dark);
        // Хук ничего не пишет на диск: ни настроек, ни выбранной темы.
        self.store = None;
        self = self.with_fixtures(review_fixtures());
        match name {
            "start" => self.screen = Screen::Scenarios,
            "server" => self.show_step(SessionMode::TcpServer, false),
            "client" => self.show_step(SessionMode::TcpClient, false),
            "serial" => self.show_step(SessionMode::Serial, false),
            "serial-more" => self.show_step(SessionMode::Serial, true),
            "errors" => {
                // Ошибка старта по коду: адреса прослушивания нет на машине.
                self.show_step(SessionMode::TcpServer, true);
                self.settings.config.listen_host = "eth0".into();
                self.start_error = Some(
                    crate::errors::StartError::new(
                        crate::errors::ErrorCode::AddressUnavailable,
                        "Cannot assign requested address (os error 99)",
                    )
                    .with_param("addr", "eth0"),
                );
            }
            "recording-text" | "recording-hex" => {
                self.settings.feed.view = if name == "recording-hex" {
                    crate::settings::FeedView::Hex
                } else {
                    crate::settings::FeedView::Text
                };
                self.rec = Some(RecordingView::detached(self.settings.config.clone()));
                self.phase = Phase::Recording;
                self.screen = Screen::Recording;
            }
            "failure" => {
                self.journal_failure = Some(JournalFailure {
                    code: crate::live::JournalFailureCode::DiskFull,
                    message: "No space left on device (os error 28)".into(),
                });
                self.recorded_path =
                    Some(format!(r"{}\bytetap-review.jsonl", self.settings.config.log_dir).into());
                self.phase = Phase::Failed;
            }
            "viewing-text" | "viewing-hex" => {
                self.settings.feed.view = if name == "viewing-hex" {
                    crate::settings::FeedView::Hex
                } else {
                    crate::settings::FeedView::Text
                };
                match hooks::var(JOURNAL_ENV) {
                    Some(path) if !path.is_empty() => self.open_journal(ctx, path.into()),
                    _ => diag::error(format!(
                        "{SCREEN_ENV}: для {name} нужен {JOURNAL_ENV}=<путь к журналу>"
                    )),
                }
            }
            "menu-journals" => self.hook_menu = Some("bt-journals-menu"),
            "menu-record" => {
                self.rec = Some(RecordingView::detached(self.settings.config.clone()));
                self.phase = Phase::Recording;
                self.screen = Screen::Recording;
                self.hook_menu = Some("bt-recording-menu");
            }
            "note" => {
                self.rec = Some(RecordingView::detached(self.settings.config.clone()));
                self.phase = Phase::Recording;
                self.screen = Screen::Recording;
                self.open_note(NoteTarget::Session);
                if let Some(note) = &mut self.note {
                    note.text = t!("review.noteText").to_owned();
                }
            }
            "profile" => {
                self.show_step(SessionMode::Serial, true);
                self.profile_prompt = Some(t!("review.profileName").to_owned());
            }
            _ => diag::error(format!("{SCREEN_ENV}: неизвестный экран `{spec}`")),
        }
        self
    }

    /// Шаг режима с полями из настроек; `more` раскрывает «Дополнительно».
    /// Нужен только снимкам по хуку `BYTETAP_TEST_SCREEN` (feature `test-hooks`).
    #[cfg(feature = "test-hooks")]
    fn show_step(&mut self, mode: SessionMode, more: bool) {
        self.settings.config.mode = mode;
        self.form = FormState::new(&self.settings.config);
        self.form.more = more;
        self.screen = Screen::Step(mode);
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

    /// Ошибка остановки записи: видна на стартовом экране (U1-AC17).
    pub fn stop_error(&self) -> Option<&str> {
        self.stop_error.as_deref()
    }

    /// Итоги последней записи: видны на стартовом экране (U1-AC17).
    pub fn stop_stats(&self) -> Option<&Stats> {
        self.stop_stats.as_ref()
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

    /// Журнал последней записи: со стартового экрана его можно открыть сразу.
    #[must_use]
    pub fn with_recorded_path(mut self, path: PathBuf) -> Self {
        self.recorded_path = Some(path);
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
        self.view_reader = None;
        // Ответ дописывания в прежний журнал больше не нужен: задача бросается вместе
        // с приёмником ответа, и сводка нового журнала им не обновится (U1-AC19).
        self.view_note = None;
        // Диалог комментария к журналу закрывается вместе с журналом, как и по «×».
        if self
            .note
            .as_ref()
            .is_some_and(|note| matches!(note.target, NoteTarget::Journal(_)))
        {
            self.note = None;
        }
        self.screen = Screen::Viewing;
        let cache = Arc::clone(&self.journal_cache);
        self.view_open = Some(Task::spawn(ctx, move || JournalOpen::open(&cache, &path)));
    }

    /// Закрыть просмотр журнала: возвращает стартовый экран.
    pub(crate) fn close_journal(&mut self) {
        self.viewing = None;
        self.view_error = None;
        self.view_open = None;
        self.view_reader = None;
        self.view_note = None;
        // Диалог комментария к журналу закрывается вместе с журналом.
        if self
            .note
            .as_ref()
            .is_some_and(|note| matches!(note.target, NoteTarget::Journal(_)))
        {
            self.note = None;
        }
        self.screen = Screen::Scenarios;
    }

    /// Открытый на просмотр журнал.
    pub fn viewing(&self) -> Option<&JournalView> {
        self.viewing.as_ref()
    }

    /// Открытый на просмотр журнал — изменяемый: тесты двигают прокрутку и страницы.
    pub fn viewing_mut(&mut self) -> Option<&mut JournalView> {
        self.viewing.as_mut()
    }

    /// Путь открытого на просмотр журнала.
    pub fn viewing_path(&self) -> Option<&PathBuf> {
        self.viewing.as_ref().map(|view| &view.path)
    }

    /// Ошибка открытия журнала.
    pub fn view_error(&self) -> Option<&str> {
        self.view_error.as_deref()
    }

    /// Открыть диалог комментария: цель задаётся здесь и живёт вместе с диалогом.
    pub(crate) fn open_note(&mut self, target: NoteTarget) {
        self.note = Some(NoteDialog {
            target,
            text: String::new(),
            error: None,
            sending: false,
            request: 0,
        });
    }

    /// Диалог комментария к открытому журналу.
    pub(crate) fn open_journal_note(&mut self) {
        let Some(view) = &self.viewing else {
            return;
        };
        // Файл — тот, который читает источник страниц, а не подпись в шапке.
        let path = view.source.path().to_path_buf();
        self.open_note(NoteTarget::Journal(path));
    }

    /// Дописать комментарий в журнал (FR-23): файл и повторное открытие сводки — в
    /// фоне, поток окна не ждёт ввод-вывод.
    ///
    /// Возвращает номер отправки: по нему ответ находит свой диалог.
    pub(crate) fn append_journal_note(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        text: &str,
    ) -> u64 {
        let cache = Arc::clone(&self.journal_cache);
        let text = text.to_owned();
        self.note_request += 1;
        let request = self.note_request;
        self.view_note = Some((
            request,
            Task::spawn(ctx, move || {
                JournalOpen::append_and_reopen(&cache, &path, &text)
            }),
        ));
        request
    }

    /// Отправить комментарий из диалога: диалог ждёт ответа своего номера.
    pub(crate) fn send_journal_note(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        text: &str,
    ) -> u64 {
        if let Some(note) = &mut self.note {
            note.text = text.to_owned();
            note.sending = true;
            note.error = None;
        }
        let request = self.append_journal_note(ctx, path, text);
        if let Some(note) = &mut self.note {
            note.request = request;
        }
        request
    }

    /// Перечитать журнал после комментария: сводка и лента видят новое событие.
    ///
    /// Экран не пересоздаётся: прокрутка и загруженные страницы остаются на месте.
    fn apply_opened(&mut self, opened: JournalOpen) {
        diag::info(format!(
            "журнал открыт: {} событий, {}",
            opened.summary.events,
            opened.path.display()
        ));
        let total = opened.summary.events;
        match &mut self.viewing {
            // Журнал уже открыт: обновляем сводку и длины, экран остаётся.
            Some(view) => {
                view.summary = opened.summary;
                view.first_event = opened.first_event;
                view.source.set_lengths(opened.lengths);
                view.source.extend(total);
                view.invalidate();
            }
            None => {
                self.viewing = Some(JournalView::new(
                    opened.path,
                    opened.summary,
                    opened.lengths,
                    opened.first_event,
                ));
            }
        }
    }

    /// Переход к событию по доле времени на карте сессии.
    pub(crate) fn seek_journal(&mut self, fraction: f32) {
        let Some(view) = &mut self.viewing else {
            return;
        };
        view.seek = Some(view.event_at_fraction(fraction));
    }

    /// Ответ дописывания относится к тому журналу, который открыт сейчас.
    ///
    /// Пока комментарий ложился в файл, человек мог закрыть журнал и открыть другой:
    /// ответ для A не обновляет сводку B. Диалог тоже должен быть про этот журнал —
    /// цель задаётся при его открытии и живёт вместе с ним (U1-AC19).
    fn note_answer_fits(&self, opened: &JournalOpen) -> bool {
        let dialog_fits = match self.note.as_ref().map(|note| &note.target) {
            Some(NoteTarget::Journal(path)) => *path == opened.path,
            Some(NoteTarget::Session) => false,
            // Диалог закрыли (отмена): ответ всё равно просил тот, кто его открыл.
            None => true,
        };
        dialog_fits
            && self
                .viewing
                .as_ref()
                .is_some_and(|view| view.path == opened.path)
    }

    /// Попросить последнюю страницу журнала: дописанный комментарий виден и в ленте,
    /// когда её прокрутить до конца, а не только строкой под шапкой (U1-AC19).
    fn request_last_page(&mut self, ctx: &egui::Context) {
        let Some(total) = self.viewing.as_ref().map(|view| view.source.total()) else {
            return;
        };
        if total > 0 {
            self.request_pages(ctx, total - 1, total);
        }
    }

    /// Рамка видимой области ленты на карте сессии.
    pub(crate) fn set_visible(&mut self, visible: Option<(f32, f32)>) {
        if let Some(view) = &mut self.viewing {
            view.visible = visible;
        }
    }

    /// Попросить страницы журнала, покрывающие видимые события `[from, to)`.
    pub(crate) fn request_pages(&mut self, ctx: &egui::Context, from: usize, to: usize) {
        let Some(view) = &mut self.viewing else {
            return;
        };
        let missing = view.source.missing_pages(from, to);
        if missing.is_empty() {
            return;
        }
        let reading = self
            .view_reader
            .as_ref()
            .is_some_and(|reader| reader.path() == view.source.path());
        if !reading {
            self.view_reader = Some(PageReader::new(
                ctx,
                Arc::clone(&self.journal_cache),
                view.source.path().to_path_buf(),
            ));
        }
        let mut dead = false;
        {
            let Some(reader) = &mut self.view_reader else {
                return;
            };
            for (page, start) in missing {
                let generation = view.source.request_page(page);
                let queued = reader.request(PageJob {
                    page,
                    start,
                    count: crate::journal::PAGE_SIZE,
                    generation,
                });
                if !queued {
                    // Очередь пула закрыта: ответа не будет, и «…» не должен висеть вечно.
                    diag::error(format!(
                        "страница журнала {page} не поставлена в очередь: пул чтения закрыт"
                    ));
                    view.source.page_lost(page, generation);
                    dead = true;
                }
            }
        }
        if dead {
            // Пул без живых потоков не оживёт: следующий запрос страниц заведёт новый.
            self.view_reader = None;
        }
    }

    /// Через сколько повторить чтение страниц, которые ждут повтора.
    pub(crate) fn retry_pages(&self) -> Option<Duration> {
        self.viewing
            .as_ref()
            .and_then(|view| view.source.next_retry())
    }

    /// Итоги фоновых задач просмотра: открытый журнал, страницы и комментарий.
    fn poll_view(&mut self, ctx: &egui::Context) {
        if let Some(task) = &mut self.view_open
            && let Some(result) = task.poll()
        {
            self.view_open = None;
            match result {
                Ok(opened) => {
                    self.apply_opened(opened);
                    // Первая страница нужна сразу: лента иначе пустая.
                    self.request_pages(ctx, 0, 1);
                }
                Err(err) => {
                    diag::error(format!("журнал не открылся: {err}"));
                    self.view_error = Some(t!("journal.openFailed", reason = err.message));
                }
            }
        }
        if let Some((request, task)) = &mut self.view_note
            && let Some(result) = task.poll()
        {
            let request = *request;
            self.view_note = None;
            // Ответ трогает диалог, только если это тот же его экземпляр: диалог
            // закрывают и открывают заново, пока комментарий ложится в файл.
            let mine = self
                .note
                .as_ref()
                .is_some_and(|note| note.sending && note.request == request);
            match result {
                Ok(written) => {
                    // Диалог закрывается, как только комментарий лёг в файл: сбой
                    // перечитывания сводки — не сбой записи (U1-AC19).
                    let fits = written
                        .reopened
                        .as_ref()
                        .is_some_and(|opened| self.note_answer_fits(opened));
                    if mine {
                        self.note = None;
                        // Комментарий лёг в файл: прежняя строка об ошибке просмотра
                        // больше не про этот журнал.
                        self.view_error = None;
                    }
                    match written.reopened {
                        Some(opened) if fits => {
                            diag::info("комментарий добавлен в журнал");
                            self.apply_opened(opened);
                            self.view_error = None;
                            // Последняя страница — сразу: дописанный комментарий виден
                            // в ленте, когда её прокрутить до конца (U1-AC19).
                            self.request_last_page(ctx);
                        }
                        Some(opened) => diag::info(format!(
                            "ответ для {} отброшен: открыт другой журнал",
                            opened.path.display()
                        )),
                        None => {
                            let reason = written.reread_error.unwrap_or_default();
                            diag::error(format!(
                                "комментарий записан, журнал не перечитан: {reason}"
                            ));
                            // Причина — строкой под шапкой просмотра: комментарий уже
                            // в файле, и закрывать ради неё диалог незачем.
                            self.view_error = Some(t!("journal.noteRereadFailed", reason = reason));
                        }
                    }
                }
                Err(err) => {
                    diag::error(format!("комментарий не записан: {err}"));
                    // Диалог не закрывается молча: причина — строкой в диалоге. Без
                    // своего диалога та же строка идёт под шапку просмотра.
                    let text = t!("journal.noteFailed", reason = err.message);
                    if mine {
                        if let Some(note) = &mut self.note {
                            note.sending = false;
                            note.error = Some(text);
                        }
                    } else {
                        self.view_error = Some(text);
                    }
                }
            }
        }
        let mut done: Vec<PageDone> = Vec::new();
        if let Some(reader) = &self.view_reader {
            while let Some(page) = reader.poll() {
                done.push(page);
            }
        }
        for page in done {
            let Some(view) = &mut self.viewing else {
                continue;
            };
            match page.result {
                Ok(events) => match view.source.store_page(page.page, page.generation, events) {
                    PageLoad::Stored => {}
                    PageLoad::Short => diag::error(format!(
                        "страница журнала {} короче индекса: перечитываем",
                        page.page
                    )),
                    PageLoad::Stale => {}
                },
                Err(err) => {
                    let exhausted = view.source.page_failed(page.page, page.generation);
                    diag::error(format!(
                        "страница журнала не прочитана: {err}{}",
                        if exhausted {
                            " (попытки исчерпаны)"
                        } else {
                            ""
                        }
                    ));
                }
            }
            // Лента перерисуется в следующем кадре: страница пришла или отмечена.
            ctx.request_repaint();
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
        self.stop_error = None;
        self.stop_stats = None;
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
        self.frames = 0;
        self.frame_cost.clear();
        self.frame_delay = None;
        self.frame_request = None;
        self.frame_started = None;
        self.frame_began = None;
        self.frame_work = Duration::ZERO;
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
        // Диалог комментария к сессии закрывается вместе с записью.
        if self
            .note
            .as_ref()
            .is_some_and(|note| matches!(note.target, NoteTarget::Session))
        {
            self.note = None;
        }
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
                    self.stop_error = None;
                    self.stop_stats = None;
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
        // Сессия закончилась сама (отказ журнала, падение задачи): окно замечает это и
        // освобождает слот — иначе запись «висит» в интерфейсе до «Стоп» (U1-AC17).
        if self.live.is_some()
            && self.stop_task.is_none()
            && self
                .live
                .as_ref()
                .is_some_and(live::LiveSession::is_finished)
        {
            diag::info("сессия ядра закончилась сама: запись останавливается");
            self.stop_recording(ctx);
        }
        if let Some(task) = &mut self.stop_task
            && let Some(outcome) = task.poll()
        {
            self.stop_task = None;
            crate::recording::log_outcome(&outcome);
            // Кадры записи: частота перерисовки в живом окне — рядом с итогами сессии.
            if let Some(rec) = &self.rec {
                let seconds = rec.elapsed().as_secs_f64().max(0.001);
                diag::info(format!(
                    "кадров за запись: {}, {:.1} в секунду",
                    self.frames,
                    self.frames as f64 / seconds
                ));
                // Цена кадра и период: по ним видно, почему кадров столько (NFR-5).
                // Цена — последний подтверждённый замер: сглаженная оценка к концу
                // записи тает, когда кадры идут без событий.
                let period = live::next_period(
                    crate::gpu::software(),
                    self.frame_cost.cost().max(self.frame_cost.measured()),
                );
                diag::info(format!(
                    "цена кадра записи: {} мс, период запроса кадра {} мс (работа {} мс, ожидание презентации {} мс)",
                    self.frame_cost.measured().as_millis(),
                    period.as_millis(),
                    self.frame_cost.work().as_millis(),
                    self.frame_cost.present().as_millis()
                ));
            }
            self.recorded_path = Some(outcome.journal_path.clone());
            if let Some(failure) = outcome.failure {
                self.phase = live::reduce(self.phase, Action::JournalFailed);
                self.journal_failure = Some(failure);
            } else if let Some(error) = outcome.error {
                // Ошибка остановки без отказа журнала: её видно на стартовом экране,
                // как `stop_failed` в UI v2 (U1-AC17).
                diag::error(format!("остановка записи: {error}"));
                self.phase = live::reduce(self.phase, Action::StopFailed);
                self.stop_error = Some(error);
            } else {
                self.phase = live::reduce(self.phase, Action::Stopped);
                // Итоги записи — на стартовом экране: сколько соединений и байт (U1-AC17).
                self.stop_stats = outcome.stats.clone();
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
    ///
    /// Пока запись идёт, каждый пришедший запрос закрытия отменяется: окно ждёт
    /// остановку, а не закрывается посреди неё. Событие закрытия живёт один проход
    /// (eframe очищает события вьюпорта перед кадром), занятого цикла нет, поэтому
    /// запрос во втором проходе — новый: без `CancelClose` в нём окно закрылось бы,
    /// не дописав `session_end` (U1-AC17).
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
            // Записи нет: закрытие не отменено, но отказ журнала или ошибка остановки,
            // оставшиеся на экране, уходят в системное сообщение до выхода из окна.
            if requested {
                self.show_close_notice();
            }
            return;
        };
        if !self.session_busy() {
            diag::info("запись остановлена, окно закрывается");
            self.closing = None;
            self.show_close_notice();
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
            // Кадр раз в 100 мс: окно ждёт остановки, но не гоняет кадры зря.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Журнал мог быть дописан не полностью: причина уходит в системное сообщение.
    ///
    /// Причина — отказ журнала, пришедший при остановке или раньше (сессия могла
    /// остановиться сама, пока окно было свёрнуто, а строку причины человек не видел
    /// и не закрыл), или ошибка остановки: и в том и в другом случае `session_end` мог
    /// не дойти до журнала.
    ///
    /// Сообщение показывает `main` уже после выхода из окна: системное окно модально,
    /// и из окна оно заблокировало бы поток окна до нажатия «ОК» (U1-AC17).
    fn show_close_notice(&mut self) {
        let reason = match (self.journal_failure.take(), self.stop_error.take()) {
            (Some(failure), _) => crate::recording::failure_reason(failure.code).to_owned(),
            (None, Some(error)) => error,
            (None, None) => return,
        };
        let text = t!("record.journalIncomplete", reason = reason);
        diag::error(format!("окно закрывается: {text}"));
        *CLOSE_NOTICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
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

    /// Тема: палитра и стиль egui. Рамку окна рисует приложение цветом `--bt-border`,
    /// поэтому смены темы DWM не касается: своя рамка видна в обеих темах.
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

    /// Обработчик окна для DWM: запоминается в первом кадре, когда он уже есть.
    fn capture_window_handle(&mut self, frame: &eframe::Frame) {
        use eframe::wgpu::rwh::HasWindowHandle as _;
        let Ok(handle) = frame.window_handle() else {
            return;
        };
        self.window_handle = Some(handle.as_raw());
    }

    /// Рамка окна: скругление углов и скрытая рамка DWM (Windows 11).
    ///
    /// Применяется, когда окно уже видимо, и только по номеру кадра
    /// (`cumulative_frame_nr`), а не по флагу прохода: в eframe 0.37 окно создаётся
    /// скрытым (`with_visible(false)`), кадр 0 рисуется в скрытое окно, и только потом
    /// `EpiIntegration::post_rendering` показывает его (`set_visible(true)`). Атрибуты,
    /// заданные до показа, DWM теряет — рамка появлялась только после смены темы
    /// (U1-AC18).
    ///
    /// Повтора для развёрнутого окна нет: `Maximized` при восстановлении проходит через
    /// `egui_winit::set_maximized` → `set_maximized_while_hidden`, который маскирует окно
    /// `DWMWA_CLOAK` и снимает маску сразу же, до первого кадра; `ViewportCommand::
    /// Maximized` на видимом окне (`toggle_maximized` в шапке) вызывает
    /// `window.set_maximized` вовсе без маскировки. К кадру 1 окно и так видимо и
    /// развёрнуто, поэтому настройка DWM применяется к нему один раз.
    ///
    /// Вне Windows и в тестах вызов ничего не делает — там рамку рисует приложение.
    fn apply_window_frame(&mut self, ctx: &egui::Context) {
        // Кадр 0 — тот, до показа окна: DWM ещё нечего настраивать.
        if ctx.cumulative_frame_nr() == 0 {
            return;
        }
        if !self.frame_applied {
            self.frame_applied = true;
            self.setup_window_frame();
        }
    }

    /// Вызов DWM: одна строка на факт пишет сам `window::setup_dwm` — скругление,
    /// запасной путь и скрытая рамка DWM, с HRESULT отказа. Итог по `DwmSetup` здесь не
    /// повторяется: журнал запуска показывает, что DWM принял, а не что мы попросили.
    fn setup_window_frame(&mut self) {
        let Some(handle) = self.window_handle else {
            diag::error("рамка окна: обработчик окна не получен — DWM не настроен");
            return;
        };
        let _ = crate::window::setup_dwm(handle, self.pal().border, crate::gpu::software());
    }

    /// Смена активности окна: DWM за свои атрибуты при таком состоянии не отвечает.
    ///
    /// По документации `DWMWA_BORDER_COLOR` цвет рамки на смену состояния выставляет
    /// приложение, а `DWMWA_COLOR_NONE` подавляет отрисовку рамки DWM всегда. Своя рамка
    /// от активности не зависит, но состояние DWM подтверждается повторно там, где DWM
    /// принял атрибут угла (`window::dwm_corner_accepted`); в журнал запуска идут первая
    /// потеря и первое возвращение фокуса за запуск и первый отказ HRESULT: окно
    /// переключается часто, и строка на каждое переключение забивала бы журнал запуска.
    fn track_window_activation(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused);
        let previous = self.window_focused;
        if previous == focused {
            return;
        }
        self.window_focused = focused;
        // Первое наблюдение состояния DWM не трогает: настройка идёт после первого кадра.
        // Повторять настройку есть что только там, где DWM принял атрибут угла: на
        // Windows 10 атрибута нет, и скругления не появится ни от какого повтора.
        if previous.is_none() || !self.frame_applied || !crate::window::dwm_corner_accepted() {
            return;
        }
        let Some(focused) = focused else {
            return;
        };
        let Some(handle) = self.window_handle else {
            return;
        };
        if let Some(code) = crate::window::reapply_dwm(handle, crate::gpu::software())
            && !self.reapply_refusal_logged
        {
            self.reapply_refusal_logged = true;
            diag::error(format!(
                "рамка окна: повторная настройка DWM: HRESULT {code:#x}"
            ));
        }
        if crate::window::activation_to_log(focused, &mut self.activation_logged) {
            diag::info(format!(
                "рамка окна: окно {} — состояние DWM подтверждено повторно",
                if focused {
                    "активно"
                } else {
                    "неактивно"
                }
            ));
        }
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
        // Фон окна: проход отрисовки очищает окно цветом `clear_color`, но заливка
        // нужна — цвет из очистки и из вершины egui переводится в sRGB по-разному, и
        // без заливки снимки экранов расходятся на один разряд канала (U1-AC24).
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
        self.stop_error_notice(ui);
        self.settings_warning(ui);
        if recording {
            let added = self.recording_screen(ui);
            if self.live.is_some() {
                // Период кадра выбирается по цене прошлых кадров: аппаратный растр
                // держит 30 кадров в секунду, WARP — 10 и реже (NFR-5, U1-AC24).
                let period = self.frame_period();
                live::set_repaint_period(period);
                // Новые строки — по периоду кадра. Без новых событий трассу и ленту
                // перерисовывать незачем, но пульс «Стоп» идёт непрерывно, как
                // CSS-анимация в UI v2: на аппаратном растре кадр всё равно приходит раз
                // в [`live::FRAME`] (пульс и таймер), а на WARP — к смене секунды
                // таймера, и там пульса нет (U1-AC24).
                //
                // Свёрнутого окна здесь нет: eframe 0.37 у свёрнутого окна зовёт только
                // `logic`, кадра не строит, и эта ветка до него не доходит (находка
                // контрольного ревью шага 5).
                let delay = if self.frame_cost.take_probe() {
                    // Пробная пара: цена кадра стареет, и раз в несколько секунд её
                    // проверяет кадр с ожиданием минимального периода.
                    live::COST_PROBE_DELAY
                } else if added > 0 {
                    period
                } else if crate::gpu::software() {
                    let elapsed = self
                        .rec
                        .as_ref()
                        .map_or(Duration::ZERO, |rec| rec.elapsed());
                    live::next_second(elapsed).max(live::FRAME)
                } else {
                    live::FRAME
                };
                self.frame_delay = Some(delay);
                ctx.request_repaint_after(delay);
                // Отсюда и до конца кадра — растр, отправка и представление: остаток
                // кадра рисуется уже после запроса (`live::present_wait`).
                self.frame_request = self.frame_began.map(|began| began.elapsed());
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
        self.note_dialog(&ctx);
        // Рамка окна — последней, поверх всего: иначе панели ленты перекрывали бы её.
        // Рисует её приложение на всех экранах и при любом состоянии окна: рамка DWM
        // гасится сменой активности, поэтому DWM получает `DWMWA_COLOR_NONE` (см. window.rs).
        // Развёрнутое окно периметра не имеет — рамки нет.
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !maximized {
            crate::window::draw_frame(ui, &p, self.frame_rounded);
        }
    }

    /// Итоги или ошибка остановки записи: строка под шапкой на стартовом экране
    /// (U1-AC17).
    ///
    /// Как `stop_failed` в UI v2: запись не началась заново, но человек видит, что
    /// остановка сорвалась и почему; при удачной остановке — итоги записи.
    fn stop_error_notice(&mut self, ui: &mut Ui) {
        if self.screen == Screen::Recording {
            return;
        }
        let error = self.stop_error.clone();
        let stats = self.stop_stats.clone();
        if error.is_none() && stats.is_none() {
            return;
        }
        let p = self.pal();
        let (text, danger) = match (&error, &stats) {
            (Some(error), _) => (t!("record.stopFailed", reason = error), true),
            (None, Some(stats)) => {
                let text = t!(
                    "record.stopped",
                    connections = stats.connections,
                    to_lis = crate::recording::format_bytes(stats.bytes_analyzer_to_lis),
                    to_analyzer = crate::recording::format_bytes(stats.bytes_lis_to_analyzer)
                );
                (text, false)
            }
            (None, None) => return,
        };
        egui::Frame::new()
            .fill(mix(
                p.surface_0,
                if danger { p.danger } else { p.text_muted },
                0.12,
            ))
            .inner_margin(egui::Margin::symmetric(SPACE_3 as i8, SPACE_2 as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let color = if danger { p.danger } else { p.text_muted };
                    ui.label(RichText::new(text).size(TEXT_XS).color(color));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, &p, Icon::X, 14.0, t!("settings.dismiss"))
                            .clicked()
                        {
                            self.stop_error = None;
                            self.stop_stats = None;
                        }
                    });
                });
            });
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter()
            .hline(line.x_range(), line.center().y, Stroke::new(1.0, p.border));
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

    /// Период кадра записи: на аппаратном растре — [`live::FRAME`], на WARP — по цене
    /// прошлых кадров.
    pub(crate) fn frame_period(&self) -> Duration {
        live::next_period(crate::gpu::software(), self.frame_cost.cost())
    }

    /// Пульс точки «Стоп»: он расходится, только когда следующий кадр придёт не позже
    /// [`crate::recording::PULSE_MAX_PERIOD`], — то есть на аппаратном растре.
    ///
    /// На WARP кадр стоит сотни миллисекунд, а без событий окно просыпается к смене
    /// секунды: фаза кольца к каждому кадру случайна, и точка стоит статично — это
    /// отступление от UI v2, где пульс непрерывный (см. `docs/dev/design.md`).
    pub(crate) fn pulse(&self) -> bool {
        !crate::gpu::software()
            && self
                .frame_delay
                .is_none_or(|delay| delay <= PULSE_MAX_PERIOD)
    }

    /// Разворот, восстановление и свёртывание окна: оценка цены кадра забывается.
    ///
    /// Кадр после смены размера рисует другую раскладку, а кадр после свёрнутого окна
    /// приходит через секунды. Оценка и запрошенный в прошлом кадре период — тоже:
    /// `frame_delay` сбрасывается, а промежуток до кадра после смены ценой не считается
    /// (`frame_started` забывается). Подтверждённая цена кадра остаётся: с неё окно
    /// начинает заново.
    fn reset_frame_cost_on_window_change(&mut self, ctx: &egui::Context) {
        let window = ctx.input(|i| {
            let viewport = i.viewport();
            let size = viewport.inner_rect.map_or([0; 2], |rect| {
                [
                    rect.width().round().max(0.0) as u32,
                    rect.height().round().max(0.0) as u32,
                ]
            });
            (size, viewport.minimized.unwrap_or(false))
        });
        if self.frame_window.replace(window) == Some(window) {
            return;
        }
        self.frame_cost.reset();
        self.frame_delay = None;
        self.frame_request = None;
        self.frame_started = None;
    }

    /// Цена кадра записи: время самого кадра — работа окна плюс ожидание презентации.
    ///
    /// Промежуток между кадрами годится для этого, только когда окно выждало свой
    /// период целиком: раннее пробуждение (фрагмент ленты, ответ задачи) говорит о
    /// событии, а не о кадре, и в цену не идёт (см. [`live::present_wait`]). Работа
    /// меряется по часам, а не по `unstable_dt` egui: в тестах и на первом кадре он
    /// равен предсказанному, а не прожитому. `started` — начало кадра, `work` — его
    /// длительность.
    fn measure_frame_cost(
        &mut self,
        started: Instant,
        delay: Option<Duration>,
        requested: Option<Duration>,
        work: Duration,
    ) {
        let spacing = self
            .frame_started
            .replace(started)
            .map(|last| started.saturating_duration_since(last));
        self.frame_work = work;
        // Только запись: в простое окно спит по своему таймеру, и промежуток между
        // кадрами — не цена кадра. Последняя цена записи остаётся: её читает итог в
        // журнале запуска, который пишется уже после остановки.
        if self.live.is_none() {
            return;
        }
        // Свёрнутое окно кадров не рисует: промежуток в секунды — не цена кадра. Предел
        // берётся от самого ожидания, а не общий: на WARP ожидание и так доходит до
        // секунды, и общий предел пропускал бы в цену паузу свёрнутого окна.
        let spacing = spacing.filter(|spacing| {
            delay.is_none_or(|delay| {
                *spacing <= requested.unwrap_or(Duration::ZERO) + live::max_gap(delay)
            })
        });
        self.frame_cost.sample(spacing, delay, requested, work);
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

/// Проба цены кадра (`probe.rs`) просит кадр без интерфейса: только фон.
#[must_use]
#[cfg(feature = "test-hooks")]
fn probe_background_only() -> bool {
    crate::probe::enabled() && crate::probe::mode() == crate::probe::EMPTY
}

/// Без feature `test-hooks` пробы нет: кадр рисуется целиком, как в релизе.
#[must_use]
#[cfg(not(feature = "test-hooks"))]
fn probe_background_only() -> bool {
    false
}

impl eframe::App for BytetapApp {
    /// Каждый кадр, и когда окно свёрнуто: итоги задач записи и закрытие окна.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.reset_frame_cost_on_window_change(ctx);
        self.poll_session(ctx);
        self.handle_close(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(feature = "test-hooks")]
        let probe = crate::probe::frame_start();
        // Ожидание и запрос прошлого кадра: `draw` ниже поставит новые, а цена кадра
        // считается по тому, что действовало в измеряемом промежутке.
        let frame_delay = self.frame_delay;
        let frame_request = self.frame_request;
        let frame_started = Instant::now();
        self.frame_began = Some(frame_started);
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
        #[cfg(feature = "test-hooks")]
        if !self.ready && hooks::flag(crate::app::PANIC_ENV) {
            panic!("{}: test panic in the first frame", crate::app::PANIC_ENV);
        }
        self.follow_system_theme(&ctx);
        // Прижатое к краю и развёрнутое окно: DWM такие углы не скругляет, и своя рамка
        // там квадратная (`window::rounded_frame`).
        let arranged = if self.ready {
            let arranged = crate::placement::arranged(frame);
            // В первом кадре окно ещё не встало на запомненное место.
            self.track_window(&ctx, arranged);
            arranged
        } else {
            self.restore_window(&ctx);
            false
        };
        if self.window_handle.is_none() {
            self.capture_window_handle(frame);
        }
        // Рамка окна: скругление и скрытая рамка DWM — на первом видимом кадре, когда
        // окно уже показано (иначе DWM теряет атрибуты).
        self.apply_window_frame(&ctx);
        self.track_window_activation(&ctx);
        // Скругление своей рамки — по фактическому состоянию DWM: если настройка не
        // применилась (Windows 10, отказ HRESULT, программный растр), углы квадратные.
        self.frame_rounded = crate::window::rounded_frame(
            crate::window::dwm_rounded(),
            arranged,
            crate::gpu::software(),
        );
        // Проба цены кадра (`probe.rs`): в режиме `empty` рисуется только фон — столько
        // стоит кадр без интерфейса.
        if probe_background_only() {
            let p = self.pal();
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0, p.surface_0);
        } else {
            self.draw(ui);
        }
        // Снимок меню (CI, `BYTETAP_TEST_SCREEN=menu-*`): кнопка уже нарисована,
        // меню открывается следующим кадром — его и снимает CI.
        if let Some(menu) = self.hook_menu.take() {
            egui::Popup::open_id(&ctx, egui::Id::unique(menu));
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.dirty && self.screen != Screen::Step(self.settings.config.mode) {
            self.save_settings();
        }
        self.frames += 1;
        if !self.ready {
            self.ready = true;
            diag::info("интерфейс готов");
        }
        // Работа над кадром — время до `frame_end`: она и есть та часть цены кадра,
        // которую окно знает про себя (остальное — ожидание презентации).
        self.measure_frame_cost(
            frame_started,
            frame_delay,
            frame_request,
            frame_started.elapsed(),
        );
        #[cfg(feature = "test-hooks")]
        crate::probe::frame_end(&ctx, frame, probe);
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

    /// Тестовый хук снимков показывает тот экран, который назван в переменной, и ту
    /// тему, которой кончается имя (U1-AC21).
    #[test]
    fn the_review_hook_shows_the_named_screen_and_theme() {
        let ctx = egui::Context::default();
        for (spec, screen, dark) in [
            ("start-light", Screen::Scenarios, false),
            ("start-dark", Screen::Scenarios, true),
            ("server-light", Screen::Step(SessionMode::TcpServer), false),
            ("client-dark", Screen::Step(SessionMode::TcpClient), true),
            (
                "serial-more-light",
                Screen::Step(SessionMode::Serial),
                false,
            ),
            ("errors-dark", Screen::Step(SessionMode::TcpServer), true),
            ("recording-text-light", Screen::Recording, false),
            ("recording-hex-dark", Screen::Recording, true),
            ("menu-record-light", Screen::Recording, false),
            ("note-dark", Screen::Recording, true),
            ("profile-light", Screen::Step(SessionMode::Serial), false),
            ("failure-dark", Screen::Scenarios, true),
        ] {
            let app =
                BytetapApp::new(&ctx, Settings::default(), None, None).with_test_screen(&ctx, spec);
            assert_eq!(app.screen, screen, "{spec}");
            assert_eq!(app.dark(), dark, "{spec}");
        }
        // Просмотр без журнала и неизвестное имя экран не меняют.
        for spec in ["viewing-text-light", "no-such-screen-light"] {
            let app =
                BytetapApp::new(&ctx, Settings::default(), None, None).with_test_screen(&ctx, spec);
            assert_eq!(app.screen, Screen::Scenarios, "{spec}");
        }
        // «Дополнительно» и диалоги раскрыты, «…» открывается на следующем кадре.
        let app = BytetapApp::new(&ctx, Settings::default(), None, None)
            .with_test_screen(&ctx, "serial-more-light");
        assert!(app.form.more, "«Дополнительно» раскрыто");
        let app = BytetapApp::new(&ctx, Settings::default(), None, None)
            .with_test_screen(&ctx, "note-light");
        assert!(app.note.is_some(), "диалог комментария открыт");
        let app = BytetapApp::new(&ctx, Settings::default(), None, None)
            .with_test_screen(&ctx, "menu-journals-dark");
        assert_eq!(app.hook_menu, Some("bt-journals-menu"));
    }

    /// Журнал на диске: заголовок сессии и один фрагмент. Путь и сводка — то, что
    /// приходит из фонового открытия журнала.
    fn journal_file(dir: &std::path::Path, name: &str) -> (PathBuf, JournalOpen) {
        journal_file_rows(dir, name, 1)
    }

    /// Журнал с `rows` фрагментами: страниц столько, сколько их покрывает индекс.
    fn journal_file_rows(dir: &std::path::Path, name: &str, rows: usize) -> (PathBuf, JournalOpen) {
        use bytetap_core::journal::{Autoresponder, Data, Dir, Event, Host, Mode, Session};

        let path = dir.join(name);
        let header = Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            serde_json::json!({"listen": "127.0.0.1:5000"}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
        text.push('\n');
        for index in 0..rows {
            let data = Event::Data(Data::new(
                "2026-10-19T10:15:03.000000+03:00",
                index as u64 + 1,
                1,
                Dir::AnalyzerToLis,
                false,
                b"A",
            ));
            text.push_str(&serde_json::to_string(&data).expect("событие"));
            text.push('\n');
        }
        std::fs::write(&path, text).expect("журнал");
        let cache = JournalCache::default();
        let opened = JournalOpen::open(&cache, &path).expect("журнал открывается");
        (path, opened)
    }

    /// Экран просмотра на готовой сводке: без фонового открытия.
    fn show(app: &mut BytetapApp, opened: &JournalOpen) {
        app.viewing = Some(JournalView::new(
            opened.path.clone(),
            opened.summary.clone(),
            opened.lengths.clone(),
            opened.first_event.clone(),
        ));
        app.screen = Screen::Viewing;
    }

    /// Ответ дописывания применяется только к своему журналу и только к диалогу про
    /// него: иначе ответ для A обновил бы сводку открытого B (U1-AC19).
    #[test]
    fn a_comment_answer_is_applied_only_to_its_own_journal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_a, opened_a) = journal_file(dir.path(), "a.jsonl");
        let (_b, opened_b) = journal_file(dir.path(), "b.jsonl");
        let ctx = egui::Context::default();
        let mut app = BytetapApp::new(&ctx, Settings::default(), None, None);
        show(&mut app, &opened_a);
        app.open_note(NoteTarget::Journal(opened_a.path.clone()));
        assert!(app.note_answer_fits(&opened_a), "открыт A, диалог про A");
        // Открыли B, а ответ пришёл для A: чужой журнал не трогаем.
        show(&mut app, &opened_b);
        assert!(!app.note_answer_fits(&opened_a));
        // Диалог всё ещё про A — ответ для B его тоже не касается.
        assert!(!app.note_answer_fits(&opened_b));
        app.open_note(NoteTarget::Journal(opened_b.path.clone()));
        assert!(app.note_answer_fits(&opened_b));
        // Комментарий к сессии — не к журналу.
        app.open_note(NoteTarget::Session);
        assert!(!app.note_answer_fits(&opened_b));
    }

    /// Гонка: ответ дописывания для A приходит после открытия B и не трогает B.
    #[test]
    fn the_answer_of_a_comment_does_not_touch_another_journal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path_a, opened_a) = journal_file(dir.path(), "a.jsonl");
        let (path_b, opened_b) = journal_file(dir.path(), "b.jsonl");
        let ctx = egui::Context::default();
        let mut app = BytetapApp::new(&ctx, Settings::default(), None, None);
        show(&mut app, &opened_a);
        app.open_note(NoteTarget::Journal(path_a.clone()));
        app.append_journal_note(&ctx, path_a.clone(), "проба A");
        // Пока комментарий ложился в файл, открыли другой журнал.
        app.open_journal(&ctx, path_b.clone());
        for _ in 0..200 {
            app.poll_view(&ctx);
            if app.view_open.is_none() && app.viewing.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Комментарий в A всё равно записан: задачу открытия другого журнала это не
        // отменяет.
        let written = (0..200).any(|_| {
            let text = std::fs::read_to_string(&path_a).unwrap_or_default();
            if text.contains("проба A") {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
            false
        });
        assert!(written, "комментарий не попал в A");
        // Ни одного кадра ответа для A: сводка B остаётся своей.
        for _ in 0..10 {
            app.poll_view(&ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
        let view = app.viewing.as_ref().expect("журнал B открыт");
        assert_eq!(view.path, path_b);
        assert_eq!(
            view.summary.events, opened_b.summary.events,
            "сводка B не тронута ответом для A"
        );
        assert_eq!(view.source.total(), opened_b.summary.events);
        // Диалог закрылся вместе с журналом A: он был про A.
        assert!(app.note.is_none());
    }

    /// Мёртвый пул чтения пересоздаётся: страница не ждёт ответа, которого не будет.
    #[test]
    fn a_dead_page_reader_is_recreated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, opened) = journal_file_rows(dir.path(), "a.jsonl", 600);
        let ctx = egui::Context::default();
        let mut app = BytetapApp::new(&ctx, Settings::default(), None, None);
        show(&mut app, &opened);
        // Пул без живых потоков: очередь заданий закрыта.
        app.view_reader = Some(PageReader::dead(path.clone()));
        app.request_pages(&ctx, 0, 1);
        assert!(
            app.view_reader.is_none(),
            "мёртвый пул сброшен: следующий запрос заведёт новый"
        );
        // Вторая страница ещё не просилась: следующий запрос поднимает живой пул.
        app.request_pages(&ctx, 500, 501);
        assert!(app.view_reader.is_some(), "пул чтения заведён заново");
        for _ in 0..200 {
            app.poll_view(&ctx);
            let stored = app
                .viewing_mut()
                .is_some_and(|view| view.source.event(500).is_some());
            if stored {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("страница не пришла из нового пула");
    }

    /// Дождаться ответа дописывания: окно опрашивает задачу из кадра.
    fn wait_note(app: &mut BytetapApp, ctx: &egui::Context) {
        for _ in 0..300 {
            app.poll_view(ctx);
            if app.view_note.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("ответ дописывания не пришёл");
    }

    /// Ошибка дописывания остаётся в том диалоге, который отправил комментарий.
    #[test]
    fn a_note_error_lands_in_the_dialog_that_sent_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_a, opened_a) = journal_file(dir.path(), "a.jsonl");
        // Папки нет: дописывание сорвётся на открытии файла.
        let missing = dir.path().join("нет-папки").join("b.jsonl");
        let ctx = egui::Context::default();
        let mut app = BytetapApp::new(&ctx, Settings::default(), None, None);
        show(&mut app, &opened_a);
        app.open_note(NoteTarget::Journal(missing.clone()));
        app.note.as_mut().expect("диалог").text = "проба".into();
        let request = app.send_journal_note(&ctx, missing, "проба");
        assert!(app.note.as_ref().expect("диалог").sending);
        wait_note(&mut app, &ctx);
        let note = app.note.as_ref().expect("диалог не закрыт");
        assert_eq!(note.request, request);
        assert!(!note.sending, "диалог больше не ждёт ответа");
        assert!(note.error.is_some(), "причина — строкой в диалоге");
    }

    /// Отмена во время отправки: ответ первой записи не трогает новый диалог.
    ///
    /// Диалог закрыли, не дождавшись записи, и открыли заново с набранным текстом.
    /// Ответ первой отправки идёт в свой номер запроса, поэтому новый диалог остаётся
    /// открытым и с текстом, а причина первой ошибки — строкой под шапкой.
    #[test]
    fn a_cancelled_note_answer_does_not_touch_a_new_dialog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path_a, opened_a) = journal_file(dir.path(), "a.jsonl");
        let missing = dir.path().join("нет-папки").join("b.jsonl");
        let ctx = egui::Context::default();
        let mut app = BytetapApp::new(&ctx, Settings::default(), None, None);
        show(&mut app, &opened_a);
        app.open_note(NoteTarget::Journal(missing.clone()));
        let first = app.send_journal_note(&ctx, missing, "первый");
        // Отмена: диалог уходит, новый открыт уже с текстом.
        app.note = None;
        app.open_note(NoteTarget::Journal(path_a.clone()));
        app.note.as_mut().expect("диалог").text = "второй".into();
        wait_note(&mut app, &ctx);
        let note = app.note.as_ref().expect("новый диалог открыт");
        assert_ne!(note.request, first, "ответ был не про этот диалог");
        assert_eq!(note.text, "второй", "набранный текст на месте");
        assert!(!note.sending, "новый диалог не ждёт ответа");
        assert!(
            note.error.is_none(),
            "ошибка первой записи не попала в новый диалог"
        );
        assert!(
            app.view_error().is_some(),
            "причина первой записи — строкой под шапкой просмотра"
        );
    }
}
