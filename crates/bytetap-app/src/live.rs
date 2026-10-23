//! Запись без моста: прямые вызовы `bytetap_core::session` (U1-AC13).
//!
//! Перенос логики моста Tauri-версии (v0.1.0, `app/src-tauri/src/{bridge,batch,feed,error}.rs`)
//! без IPC: старт сессии с кодами ошибок у полей формы, приёмник событий в кольцевой
//! буфер ленты, комментарий, остановка с итогами и отказ журнала по коду.
//!
//! Сессия живёт в runtime `tokio` приложения. Приёмник — задача того же runtime: он
//! забирает всё, что накопилось в подписке `broadcast`, кладёт в общий буфер под одним
//! замком и просит перерисовку не чаще [`FRAME`], сколько бы фрагментов ни пришло.
//! Отставший приёмник (`Lagged`) теряет события ленты, но не счётчики: их окно берёт
//! из [`SessionCounters`], а потерянное показывает как «пропущено в ленте: N».

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytetap_core::journal::{JournalFailureReason, OsFamily, Stats};
use bytetap_core::session::{
    Session, SessionConfig, SessionCounters, SessionEvent, SessionHandle, journal_failure_reason,
};
use bytetap_core::transport::serial::SerialSettings;
use bytetap_core::transport::{
    MonitorSide, Source, is_monitor_same_port, is_proxy_target_is_self, monitor_side,
};
use eframe::egui;
use tokio::sync::{broadcast, watch};

use crate::errors::{ErrorCode, StartError};
use crate::feed::{FeedEvent, FeedRing};

/// Не чаще одной перерисовки на столько: ~30 кадров в секунду во время записи.
///
/// На аппаратном растре это ещё и единственный период кадра записи: кадр там дешёвый,
/// и пульс «Стоп» идёт непрерывно, как CSS-анимация в UI v2.
pub const FRAME: Duration = Duration::from_millis(33);

/// Потолок кадров на программном растре (WARP): 10 кадров в секунду.
///
/// Кадр 1200×800 на WARP стоит ~160 мс и несколько ядер: 30 кадров в секунду ноутбук без
/// драйвера GPU не вытянет, а ленте чаще и не нужно — она догоняет буфером.
pub const FRAME_SOFTWARE: Duration = Duration::from_millis(100);

/// Самый редкий кадр записи: раз в секунду.
///
/// Столько же окно ждёт кадр без новых событий — тогда по нему идут только таймер и
/// пульс «Стоп», а трасса и лента остаются прежними. Реже лента перестаёт догонять на
/// глаз, хотя данные и не теряются: их держит буфер.
pub const FRAME_SLOWEST: Duration = Duration::from_secs(1);

/// Ожидание до следующей целой секунды записи: `1 с − subsec(elapsed)`.
///
/// Таймер шапки показывает `elapsed` целыми секундами. Кадр к границе секунды приходит
/// тогда, когда подпись действительно меняется: иначе кадр без событий опаздывает на
/// доли секунды и на загруженном окне перескакивает через секунду — «00:07» сменяется
/// «00:09». Пульс «Стоп» идёт тем же кадром.
#[must_use]
pub fn next_second(elapsed: Duration) -> Duration {
    FRAME_SLOWEST.saturating_sub(Duration::from_micros(u64::from(elapsed.subsec_micros())))
}

/// Период кадров записи: на аппаратном растре — фиксированный, на WARP — по цене
/// прошлых кадров.
///
/// На аппаратном растре период всегда [`FRAME`]: кадр стоит единицы миллисекунд, и
/// оценивать его незачем. Оценка там только вредила — одна задержка (свёрнутое окно,
/// перетаскивание, разворот) поднимала её, а кадры, выждавшие своё ожидание целиком,
/// ничего нового о цене не говорят, и период залипал на секунде до «Стоп».
///
/// `cost` — цена кадра на WARP (см. [`FrameBudget`]). Кадр стоит столько, сколько он
/// рисуется, поэтому ждать между кадрами надо заметно больше: три бюджета кадра на кадр
/// оставляют окну четверть ядра, а лента всё равно догоняет буфером. Замер на WARP:
/// кадр 206 мс при ожидании 620 мс — это 97 % одного ядра вместо 306 % при 30 запросах
/// кадра в секунду.
#[must_use]
pub fn next_period(software: bool, cost: Duration) -> Duration {
    if !software {
        return FRAME;
    }
    cost.saturating_mul(3).clamp(FRAME_SOFTWARE, FRAME_SLOWEST)
}

/// Предел промежутка, который ещё считается кадром, а не паузой: вдвое больше
/// запрошенного ожидания плюс запас на пробуждение окна.
///
/// Свёрнутое и перекрытое окно кадров не рисует, и промежуток в секунды — не цена
/// кадра. Предел берётся от самого ожидания, а не общий: на WARP ожидание и так доходит
/// до секунды, и общий предел пропускал бы в цену паузу свёрнутого окна — та и поднимала
/// период до секунды до «Стоп» (находка повторного ревью шага 5).
#[must_use]
pub fn max_gap(delay: Duration) -> Duration {
    delay.saturating_mul(2) + GAP_SLACK
}

/// Запас на пробуждение по таймеру в [`max_gap`]: система будит окно на десятки
/// миллисекунд позже запрошенного.
const GAP_SLACK: Duration = Duration::from_millis(250);

/// Затухание цены кадра на кадре, который выждал своё ожидание целиком.
///
/// Такой кадр говорит только, что цена не больше ожидания. Оценка тает до подтверждённой
/// цены, но не ниже: период держит тройной запас над ценой кадра, и опускать его до
/// ожидания, в которое кадр еле влезает, значит отдать окну всё ядро. Замеры на WARP:
/// с полом (прогон 39718258600) цена кадра 205 мс, период 617 мс, 2.3 кадра/с и 170 %
/// одного ядра; без пола (39715728617) период сползал к 100 мс и запись отдавала 258 %.
const COST_DECAY: f32 = 0.875;

/// Предел роста цены кадра за один кадр: не больше чем вдвое.
///
/// Выброс (своп, сборка мусора, чужой процесс на том же ядре) не должен уводить период
/// в секунду с одного замера: настоящая медлительность подтвердится следующими кадрами.
/// Тот же предел держит и пол оценки: кадр, выждавший ожидание целиком, не возвращает
/// цену к прежней одним кадром.
const COST_GROWTH: f32 = 2.0;

/// Через сколько подтверждённая цена кадра стареет: раз в столько её проверяет пробная
/// пара кадров.
///
/// Кадры идут своим периодом и выжидают его целиком, поэтому замер цены им недоступен:
/// уложившийся в ожидание кадр говорит только, что цена не больше ожидания. Без проверки
/// подтверждённая цена становится храповиком — пара медленных кадров (нагрузка CPU на
/// пару секунд, своп) поднимает её, и период стоит у секунды до «Стоп» (находка
/// контрольного ревью шага 5: на WARP после нагрузки 2–3 с период оставался секундой).
pub const COST_PROBE_EVERY: Duration = Duration::from_secs(5);

/// Ожидание пробного кадра: минимальный период WARP.
///
/// Пробная пара идёт с этим ожиданием. Кадр, который рисуется дольше, его перерастает —
/// и ожидание презентации видно замером; кадр, который уложился, ограничивает цену
/// сверху. В обоих случаях подтверждённая цена заменяется, а не усредняется: она замер,
/// а не среднее.
pub const COST_PROBE_DELAY: Duration = FRAME_SOFTWARE;

/// Сколько кадров подряд идёт проверка цены: цена подтверждается парой замеров.
const COST_PROBE_FRAMES: u8 = 2;

/// Допуск на пробуждение по таймеру: система будит окно на десятки миллисекунд позже
/// запрошенного, и этот разброс — не остаток кадра.
const PRESENT_SLACK: Duration = Duration::from_millis(50);

/// Ожидание презентации по замеру кадра: сколько кадр рисовался после того, как окно
/// попросило следующий.
///
/// Окно просит кадр посреди своей работы — через `requested` от начала кадра, — и ждёт
/// `delay`. Значит, промежуток между началами кадров — это `requested + max(delay, P)`,
/// где `P` — остаток кадра: растр, отправка и представление. Если промежуток больше
/// `requested + delay` (с допуском [`PRESENT_SLACK`] на пробуждение по таймеру), кадр не
/// уложился в своё ожидание, и `P` из него видно целиком: `spacing − requested`.
///
/// `None` — замер цены не даёт: окно выждало свой период целиком (тогда видно только,
/// что `P ≤ delay`), или проснулось раньше по событию (фрагмент ленты, ответ задачи).
/// Такой промежуток в цену кадра не идёт: после кадра без событий (ожидание
/// [`FRAME_SLOWEST`]) событие через 500 мс записалось бы кадром в 500 мс, и период ушёл
/// бы в секунду даже на быстром растре.
#[must_use]
pub fn present_wait(spacing: Duration, delay: Duration, requested: Duration) -> Option<Duration> {
    let overrun = spacing.saturating_sub(requested + delay + PRESENT_SLACK);
    (!overrun.is_zero()).then(|| spacing.saturating_sub(requested))
}

/// Цена кадра записи: работа над кадром плюс ожидание презентации, со сглаживанием.
///
/// Цена кадра — это время самого кадра: сколько окно рисует и сколько ждёт презентацию.
/// Промежуток между кадрами годится для этого, только когда окно выждало свой период
/// целиком; в остальных случаях он говорит о событии, а не о кадре (см.
/// [`present_wait`]).
///
/// Оценка нужна только на программном растре: на аппаратном период фиксирован
/// ([`next_period`]). Цена подтверждается парой замеров подряд, растёт не больше чем
/// вдвое за кадр ([`COST_GROWTH`]) и тает на кадрах, выждавших своё ожидание целиком, но
/// не ниже подтверждённой цены ([`COST_DECAY`]): так одна задержка не залипает в
/// периоде, а тройной запас периода над ценой кадра остаётся. Подтверждённая цена стареет:
/// раз в [`COST_PROBE_EVERY`] её проверяет пробная пара кадров с ожиданием
/// [`COST_PROBE_DELAY`], и результат заменяет её. Сбрасывается оценка после разворота,
/// восстановления и свёртывания окна: [`reset`](Self::reset).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameBudget {
    /// Сколько заняла работа над только что нарисованным кадром.
    work: Duration,
    /// Подтверждённое ожидание презентации: растр, отправка и представление кадра.
    present: Duration,
    /// Сглаженная цена кадра: по ней окно выбирает период.
    cost: Duration,
    /// Замер, ждущий подтверждения следующим кадром.
    ///
    /// Разовую задержку (своп, пауза свёрнутого или перетаскиваемого окна) подтвердить
    /// нечем: следующий кадр после неё укладывается в щедрое ожидание и замера не даёт.
    pending: Option<Duration>,
    /// Время записи с прошлой проверки подтверждённой цены.
    since_probe: Duration,
    /// Сколько пробных кадров осталось запросить у окна.
    probes_left: u8,
}

impl FrameBudget {
    /// Цена кадра для [`next_period`].
    #[must_use]
    pub fn cost(&self) -> Duration {
        self.cost
    }

    /// Цена последнего подтверждённого замера: работа над кадром и ожидание презентации.
    ///
    /// Её печатает журнал запуска при остановке: сглаженная оценка к концу записи тает,
    /// а эта цена — то, что действительно стоило окну.
    #[must_use]
    pub fn measured(&self) -> Duration {
        self.work + self.present
    }

    /// Работа окна над последним кадром.
    #[must_use]
    pub fn work(&self) -> Duration {
        self.work
    }

    /// Ожидание презентации: растр, отправка и представление кадра.
    #[must_use]
    pub fn present(&self) -> Duration {
        self.present
    }

    /// Забыть оценку: разворот, восстановление, свёртывание окна или запись на просмотр
    /// с другим размером.
    ///
    /// Кадр после смены размера рисует другую раскладку, а кадр после свёрнутого окна
    /// приходит через секунды: прежняя оценка к ним не относится. Подтверждённая цена
    /// кадра остаётся — с неё окно начинает заново и не тратит кадры на догадку, — и её
    /// проверка тоже начинается заново.
    pub fn reset(&mut self) {
        self.cost = Duration::ZERO;
        self.pending = None;
        self.since_probe = Duration::ZERO;
        self.probes_left = 0;
    }

    /// Забыть всё: новая запись. Прежняя цена кадра к новой сессии не относится.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Пробный кадр: цена стареет, и раз в [`COST_PROBE_EVERY`] её проверяет пара кадров
    /// с ожиданием [`COST_PROBE_DELAY`].
    ///
    /// Зовёт окно перед каждым кадром записи: `true` — ждать [`COST_PROBE_DELAY`], а
    /// [`sample`](Self::sample) заменит подтверждённую цену замером или верхней оценкой.
    pub fn take_probe(&mut self) -> bool {
        if self.probes_left == 0 {
            return false;
        }
        self.probes_left -= 1;
        true
    }

    /// Учесть кадр: `spacing` — промежуток от прошлого кадра (`None` — кадр первый или
    /// после паузы, промежуток ценой не считается), `delay` — ожидание, которое окно
    /// просило в прошлом кадре (`None` — прошлый кадр прошёл без таймера), `requested` —
    /// сколько прошло от начала того кадра до запроса следующего, `work` — сколько
    /// заняла работа над только что нарисованным кадром.
    pub fn sample(
        &mut self,
        spacing: Option<Duration>,
        delay: Option<Duration>,
        requested: Option<Duration>,
        work: Duration,
    ) {
        self.work = work;
        // Возраст подтверждённой цены: раз в несколько секунд её проверяет пробная пара.
        if let Some(spacing) = spacing {
            self.since_probe += spacing;
        }
        if self.since_probe >= COST_PROBE_EVERY {
            self.since_probe = Duration::ZERO;
            self.probes_left = COST_PROBE_FRAMES;
        }
        let measured = match (spacing, delay, requested) {
            (Some(spacing), Some(delay), Some(requested)) => {
                present_wait(spacing, delay, requested)
            }
            _ => None,
        };
        if let Some(present) = measured {
            // Цена подтверждается парой замеров подряд: разовая задержка (своп, пауза
            // свёрнутого или перетаскиваемого окна) следующего замера не получает — после
            // неё кадр укладывается в ожидание, — и в цену не идёт.
            let Some(previous) = self.pending.take() else {
                self.pending = Some(present);
                return;
            };
            // Подтверждённая цена — замер, а не среднее: прежняя цена после медленной пары
            // заменяется следующей подтверждённой, иначе среднее тянет за собой старое
            // значение, и период остаётся у предела до «Стоп» (находка контрольного
            // ревью шага 5).
            self.present = previous.min(present);
            let cost = self.measured();
            // Первый замер после сброса — сразу настоящая цена; дальше вверх не больше
            // чем вдвое за кадр, а вниз — медленно: медленный кадр виден по первому же
            // замеру, быстрый после одного медленного (загрузка шрифтов, первый показ)
            // режим не переключает.
            self.cost = if self.cost.is_zero() {
                cost
            } else if cost >= self.cost {
                cost.min(self.cost.mul_f32(COST_GROWTH))
            } else {
                self.cost.mul_f32(COST_DECAY) + cost.mul_f32(0.125)
            };
            return;
        }
        // Кадр уложился в своё ожидание: ждущий замер не подтвердился, а цена кадра не
        // больше ожидания — пробный кадр с минимальным периодом ограничивает её сверху,
        // когда она упала.
        self.pending = None;
        if waited_fully(spacing, delay, requested) {
            if let Some(delay) = delay {
                self.present = self.present.min(delay.saturating_add(PRESENT_SLACK));
            }
            // Оценка тает к подтверждённой цене, но пол ограничен пределом роста: старая
            // цена не возвращает её к себе одним кадром, иначе период держится у предела
            // до «Стоп».
            let floor = self.measured().min(self.cost.mul_f32(COST_GROWTH));
            self.cost = self.cost.mul_f32(COST_DECAY).max(floor);
        }
    }
}

/// Кадр выждал своё ожидание целиком: окно не проснулось раньше по событию.
fn waited_fully(
    spacing: Option<Duration>,
    delay: Option<Duration>,
    requested: Option<Duration>,
) -> bool {
    match (spacing, delay) {
        (Some(spacing), Some(delay)) => {
            spacing.saturating_add(PRESENT_SLACK)
                >= delay.saturating_add(requested.unwrap_or(Duration::ZERO))
        }
        _ => false,
    }
}

/// Период, которым приёмник будит окно: его ставит окно из [`frame_period`].
static FRAME_PERIOD_US: AtomicU64 = AtomicU64::new(FRAME.as_micros() as u64);

/// Период перерисовки для приёмника ленты: пока окно не сказало иначе — [`FRAME`].
#[must_use]
pub fn repaint_period() -> Duration {
    Duration::from_micros(FRAME_PERIOD_US.load(Ordering::Relaxed))
}

/// Задать период перерисовки: окно зовёт это каждый кадр записи.
pub fn set_repaint_period(period: Duration) {
    let micros = u64::try_from(period.as_micros()).unwrap_or(u64::MAX);
    FRAME_PERIOD_US.store(micros, Ordering::Relaxed);
}

/// Предел ожидания остановки при закрытии окна (NFR-5): у писателя журнала свой
/// предел в 10 с, 15 с — запас на очередь и медленную машину.
pub const STOP_WAIT: Duration = Duration::from_secs(15);

/// Общий кольцевой буфер ленты: приёмник пишет, окно читает только новое.
pub type SharedFeed = Arc<Mutex<FeedRing>>;

pub fn lock(feed: &SharedFeed) -> std::sync::MutexGuard<'_, FeedRing> {
    feed.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Код отказа журнала для строки под шапкой: вид отказа, а не текст системы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalFailureCode {
    DiskFull,
    PermissionDenied,
    NotFound,
    Network,
    Busy,
    Timeout,
    /// Очередь писателя переполнена (M4-AC12): не диск, а объём, который сессия успела
    /// накопить, поэтому своей причиной, а не «ошибка записи».
    Backlog,
    Io,
}

impl JournalFailureCode {
    /// Вид отказа по причине от ядра: сначала свой код причины (`journal_backlog` у
    /// переполненной очереди писателя), затем код ОС (таблица по семейству ОС — 5 в
    /// Windows и в Linux значит разное), затем вид ошибки.
    pub fn from_failure(reason: &JournalFailureReason) -> Self {
        match reason.code() {
            Some("journal_backlog") => Self::Backlog,
            Some(_) => Self::Io,
            None => match reason.raw_os_error() {
                Some(code) => Self::from_os_code(reason, code),
                None => Self::from_kind(reason.kind()),
            },
        }
    }

    fn from_kind(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::StorageFull => Self::DiskFull,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::TimedOut => Self::Timeout,
            _ => Self::Io,
        }
    }

    fn from_os_code(reason: &JournalFailureReason, code: i32) -> Self {
        match reason.os() {
            OsFamily::Windows => match code {
                // ERROR_DISK_FULL и ERROR_HANDLE_DISK_FULL.
                112 | 39 => Self::DiskFull,
                // ERROR_ACCESS_DENIED.
                5 => Self::PermissionDenied,
                // ERROR_FILE_NOT_FOUND и ERROR_PATH_NOT_FOUND.
                2 | 3 => Self::NotFound,
                // ERROR_BAD_NETPATH, ERROR_DEV_NOT_EXIST, ERROR_UNEXP_NET_ERR, ERROR_NETNAME_DELETED.
                53 | 55 | 59 | 64 => Self::Network,
                // ERROR_SHARING_VIOLATION и ERROR_LOCK_VIOLATION.
                32 | 33 => Self::Busy,
                _ => Self::from_kind(reason.kind()),
            },
            OsFamily::Unix => match code {
                // ENOSPC.
                28 => Self::DiskFull,
                // EACCES и EPERM.
                13 | 1 => Self::PermissionDenied,
                // ENOENT.
                2 => Self::NotFound,
                _ => Self::from_kind(reason.kind()),
            },
        }
    }
}

/// Отказ журнала: код для строки под шапкой и текст для журнала запуска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalFailure {
    pub code: JournalFailureCode,
    pub message: String,
}

impl JournalFailure {
    pub fn new(reason: &JournalFailureReason) -> Self {
        Self {
            code: JournalFailureCode::from_failure(reason),
            message: reason.to_string(),
        }
    }
}

/// Код ошибки старта по ошибке ядра.
///
/// Ядро сначала занимает адрес или открывает порт, потом создаёт журнал, поэтому у
/// TCP-сервера всё, что не про адрес, — про журнал.
pub fn start_error(err: &io::Error, source: &Source) -> StartError {
    // Петля прокси на себя — ошибка настройки, а не про журнал: ошибка старта есть только
    // у цели, заданной адресом (M4-AC6), и встаёт под «Порт цели».
    if is_proxy_target_is_self(err) {
        return StartError::new(ErrorCode::ProxyTargetSelf, err.to_string());
    }
    match source {
        // Прокси занимает порт прослушивания так же, как TCP-сервер: занятый порт —
        // `port_in_use` под полем порта, всё остальное — про журнал (M4-AC13).
        Source::TcpServer { listen } | Source::Proxy { listen, .. } => {
            let port = listen.port().to_string();
            match ErrorCode::from_io_kind(err.kind()) {
                ErrorCode::PortInUse => {
                    StartError::new(ErrorCode::PortInUse, err.to_string()).with_param("port", port)
                }
                ErrorCode::Io => StartError::new(ErrorCode::Journal, err.to_string()),
                code => StartError::new(code, err.to_string()).with_param("port", port),
            }
        }
        Source::TcpClient { .. } => StartError::new(ErrorCode::Io, err.to_string()),
        Source::Serial { settings, .. } => serial_error(err, settings, port_listed),
        // Прослушка: ошибка старта несёт сторону отказавшего порта, поэтому текст и поле
        // берутся у того порта, который не открылся (M5-AC3). Один и тот же порт с двух
        // сторон — своя настройка, а не «порт занят» от собственного второго открытия.
        Source::ComMonitor { analyzer, lis, .. } => {
            let settings = match monitor_side(err) {
                Some(MonitorSide::Lis) => lis,
                Some(MonitorSide::Analyzer) | None => analyzer,
            };
            if is_monitor_same_port(err) {
                return StartError::new(ErrorCode::MonitorSamePort, err.to_string())
                    .with_param("port", &settings.port);
            }
            serial_error(err, settings, port_listed)
        }
    }
}

/// Ошибка старта COM-порта: нет порта, занят, нет прав, неверные настройки.
///
/// `serialport` отдаёт `NotFound` и для отсутствующего порта, и для занятого: различие —
/// по списку портов (`listed`).
pub fn serial_error(
    err: &io::Error,
    settings: &SerialSettings,
    listed: impl Fn(&str) -> bool,
) -> StartError {
    let code = match err.kind() {
        io::ErrorKind::InvalidInput => ErrorCode::PortSettings,
        io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        io::ErrorKind::NotFound if listed(&settings.port) => ErrorCode::PortBusy,
        io::ErrorKind::NotFound => ErrorCode::PortNotFound,
        _ => ErrorCode::Serial,
    };
    StartError::new(code, err.to_string()).with_param("port", &settings.port)
}

fn port_listed(port: &str) -> bool {
    bytetap_core::transport::serial::list_ports().is_ok_and(|ports| {
        ports
            .iter()
            .any(|info| info.name.eq_ignore_ascii_case(port))
    })
}

/// Идущая сессия: дескриптор ядра, приёмник ленты и общий буфер.
pub struct LiveSession {
    handle: SessionHandle,
    pump: tokio::task::JoinHandle<()>,
    feed: SharedFeed,
    journal_path: PathBuf,
    local_addr: String,
}

impl std::fmt::Debug for LiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSession")
            .field("journal_path", &self.journal_path)
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

/// Запускает сессию и приёмник событий.
///
/// Папка журналов создаётся до старта: недоступная папка — ошибка `log_dir`, а не отказ
/// журнала посреди работы (FR-19). Ошибка источника (занятый порт, нет COM-порта) —
/// ошибка старта до создания файла журнала (U1-AC11).
///
/// # Errors
///
/// Код ошибки и подстановки для текста под полем формы.
pub async fn start(config: SessionConfig, ctx: egui::Context) -> Result<LiveSession, StartError> {
    if let Some(dir) = &config.log_dir {
        std::fs::create_dir_all(dir).map_err(|err| {
            StartError::new(ErrorCode::LogDir, err.to_string())
                .with_param("dir", dir.to_string_lossy())
        })?;
    }
    let source = config.source.clone();
    let (handle, events) = Session::start_with_feed(config)
        .await
        .map_err(|err| start_error(&err, &source))?;
    let feed = SharedFeed::default();
    let pump = tokio::spawn(pump(events, Arc::clone(&feed), ctx));
    Ok(LiveSession {
        journal_path: handle.journal_path().to_path_buf(),
        local_addr: handle.local_addr().to_owned(),
        handle,
        pump,
        feed,
    })
}

/// Приёмник ленты: всё, что накопилось в подписке, — в буфер под одним замком.
///
/// Заканчивается, когда сессия выгрузила отправителя событий (остановка, отказ журнала).
///
/// Пачка ограничена [`crate::feed::FEED_CAPACITY`]: буфер ленты столько и вмещает, а
/// лишнее всё равно вытеснило бы старое. Ограничение не даёт приёмнику разобрать всю
/// очередь подписчика, если она вдруг окажется большой.
///
/// `Lagged` — ровно потерянные события ленты: подписка вытесняет старейшие события, а
/// итоги сессии (`session_end`) идут последними и в вытеснение не попадают. Заголовок
/// сессии в подписку не попадает вовсе: он пишется в журнал напрямую. Ошибку отказа
/// журнала `fail_journal` шлёт в подписку сам, и она тоже доходит до ленты. Поэтому
/// «пропущено в ленте: N» — точный счёт, а не оценка (U1-AC13).
pub async fn pump(
    mut events: broadcast::Receiver<SessionEvent>,
    feed: SharedFeed,
    ctx: egui::Context,
) {
    let mut batch = Vec::new();
    loop {
        let mut lagged = 0;
        let mut closed = false;
        match events.recv().await {
            Ok(event) => batch.extend(FeedEvent::from_session(&event)),
            Err(broadcast::error::RecvError::Lagged(count)) => lagged += count,
            Err(broadcast::error::RecvError::Closed) => closed = true,
        }
        while !closed && batch.len() < crate::feed::FEED_CAPACITY {
            match events.try_recv() {
                Ok(event) => batch.extend(FeedEvent::from_session(&event)),
                Err(broadcast::error::TryRecvError::Lagged(count)) => lagged += count,
                Err(broadcast::error::TryRecvError::Closed) => closed = true,
                Err(broadcast::error::TryRecvError::Empty) => break,
            }
        }
        if !batch.is_empty() || lagged > 0 {
            // `Arc` создаётся до взятия замка: под замком только кладём готовые события.
            let ready: Vec<Arc<FeedEvent>> = batch.drain(..).map(Arc::new).collect();
            let mut ring = lock(&feed);
            ring.lagged(lagged);
            for event in ready {
                ring.push(event);
            }
            drop(ring);
            // Окно само решает, как часто просить кадр: на WARP это 10 кадров в секунду,
            // на медленном кадре — реже (`frame_period`).
            ctx.request_repaint_after(repaint_period());
        }
        if closed {
            ctx.request_repaint();
            return;
        }
    }
}

/// Итоги остановки: статистика ядра или отказ журнала.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopOutcome {
    pub stats: Option<Stats>,
    pub failure: Option<JournalFailure>,
    /// Прочая ошибка остановки: текст для журнала запуска.
    pub error: Option<String>,
    pub journal_path: PathBuf,
}

impl LiveSession {
    pub fn feed(&self) -> SharedFeed {
        Arc::clone(&self.feed)
    }

    pub fn counters(&self) -> SessionCounters {
        self.handle.counters()
    }

    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    /// Для TCP-сервера — фактический адрес слушателя (при порте 0 выбран системой).
    pub fn local_addr(&self) -> &str {
        &self.local_addr
    }

    pub fn failure_watch(&self) -> watch::Receiver<Option<JournalFailureReason>> {
        self.handle.journal_failure()
    }

    /// Сессия ядра закончилась сама: отказ журнала, падение задачи или иное завершение.
    ///
    /// Окно замечает это и останавливает запись: слот освобождается без «Стоп», как в
    /// UI v2 (слот занятой завершившейся сессии освобождается — v0.1.0,
    /// `app/src-tauri/src/bridge.rs`).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Комментарий в идущую сессию (FR-23): событие `note` в журнал и ленту.
    ///
    /// # Errors
    ///
    /// Сессия уже остановлена.
    pub fn note(&self, text: &str) -> io::Result<()> {
        self.handle.note(text.trim())
    }

    /// Останавливает сессию: журнал получает `session_end`, лента — последние события.
    ///
    /// Сначала ядро закрывает журнал, потом приёмник дописывает хвост ленты: итоги не
    /// приходят раньше событий.
    pub async fn stop(self) -> StopOutcome {
        let Self {
            handle,
            pump,
            journal_path,
            ..
        } = self;
        let failure_watch = handle.journal_failure();
        let result = handle.stop().await;
        let _ = pump.await;
        let mut outcome = StopOutcome {
            stats: None,
            failure: None,
            error: None,
            journal_path,
        };
        match result {
            Ok(stats) => outcome.stats = Some(stats),
            Err(err) => match journal_failure_reason(&err) {
                Some(reason) => outcome.failure = Some(JournalFailure::new(reason)),
                None => outcome.error = Some(err.to_string()),
            },
        }
        if outcome.failure.is_none()
            && let Some(reason) = failure_watch.borrow().as_ref()
        {
            outcome.failure = Some(JournalFailure::new(reason));
        }
        outcome
    }
}

/// Фаза записи — как в v0.1.0 (`app/src/session/reducer.ts`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Записи нет.
    #[default]
    Idle,
    Starting,
    Recording,
    Stopping,
    /// Журнал отказал: запись остановлена, под шапкой строка с причиной.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    StartRequested,
    Started,
    StartFailed,
    StopRequested,
    Stopped,
    /// Остановка завершилась ошибкой без отказа журнала: видно на стартовом экране.
    StopFailed,
    JournalFailed,
    FailureDismissed,
}

/// Переход фазы; недопустимое действие оставляет фазу как есть.
pub fn reduce(phase: Phase, action: Action) -> Phase {
    use Action as A;
    use Phase as P;
    match (phase, action) {
        (P::Idle | P::Failed, A::StartRequested) => P::Starting,
        (P::Starting, A::Started) => P::Recording,
        (P::Starting, A::StartFailed) => P::Idle,
        (P::Recording, A::StopRequested) => P::Stopping,
        (P::Stopping, A::Stopped) => P::Idle,
        (P::Recording | P::Stopping, A::StopFailed) => P::Failed,
        (P::Recording | P::Stopping, A::JournalFailed) => P::Failed,
        (P::Failed, A::FailureDismissed) => P::Idle,
        (phase, _) => phase,
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use bytetap_core::transport::ProxyTargetIsSelf;
    use bytetap_core::transport::serial::SerialFormat;

    use crate::errors::ErrorField;
    use crate::settings::SessionMode;
    use bytetap_core::transport::ProxyRole;

    /// Период кадра записи: на аппаратном растре он фиксирован, на WARP — по цене
    /// прошлых кадров, и медленный кадр задаёт своё ожидание.
    #[test]
    fn the_frame_period_follows_the_cost_of_a_frame() {
        // Аппаратный растр: 30 кадров в секунду при любой цене кадра. Оценка цены там
        // не нужна, и период от неё не зависит — иначе одна задержка залипала бы.
        assert_eq!(next_period(false, Duration::from_millis(4)), FRAME);
        assert_eq!(next_period(false, Duration::from_millis(500)), FRAME);
        assert_eq!(next_period(false, FRAME_SLOWEST), FRAME);
        // WARP: 10 кадров в секунду, а медленный кадр задаёт своё ожидание.
        assert_eq!(next_period(true, Duration::from_millis(4)), FRAME_SOFTWARE);
        // Кадр 160 мс на WARP: ждать втрое дольше, чем он рисуется.
        assert_eq!(
            next_period(true, Duration::from_millis(160)),
            Duration::from_millis(480)
        );
        // Медленнее предела — ждать не дольше секунды: лента должна идти.
        assert_eq!(next_period(true, Duration::from_secs(1)), FRAME_SLOWEST);
        // Кадр без новых событий — тот же предел: раз в секунду.
        assert_eq!(next_period(true, Duration::from_secs(10)), FRAME_SLOWEST);
        // Приёмник будит окно тем же периодом, который поставило окно.
        set_repaint_period(FRAME_SOFTWARE);
        assert_eq!(repaint_period(), FRAME_SOFTWARE);
        set_repaint_period(FRAME);
        assert_eq!(repaint_period(), FRAME);
    }

    /// Цена кадра — время самого кадра: работа плюс ожидание презентации. Кадр WARP
    /// стоит 160 мс, хотя работа над ним — 0.5 мс.
    #[test]
    fn the_cost_of_a_frame_is_the_time_of_the_frame_itself() {
        let work = Duration::from_micros(500);
        let requested = Duration::from_millis(2);
        let mut budget = FrameBudget::default();
        // Первый кадр: промежутка нет, цены тоже.
        budget.sample(None, None, None, work);
        assert_eq!(budget.cost(), Duration::ZERO);
        // Окно попросило кадр через 2 мс работы и ждало 100 мс, а следующий кадр
        // начался через 202 мс: кадр не уложился в ожидание, и его остаток — растр и
        // представление — 200 мс. Одного замера мало: цена подтверждается парой.
        let overran = |budget: &mut FrameBudget| {
            budget.sample(
                Some(Duration::from_millis(202)),
                Some(FRAME_SOFTWARE),
                Some(requested),
                work,
            );
        };
        assert_eq!(
            present_wait(Duration::from_millis(202), FRAME_SOFTWARE, requested),
            Some(Duration::from_millis(200))
        );
        overran(&mut budget);
        assert_eq!(budget.present(), Duration::ZERO, "один замер — не цена");
        overran(&mut budget);
        assert_eq!(budget.present(), Duration::from_millis(200));
        assert_eq!(budget.measured(), Duration::from_millis(200) + work);
        assert_eq!(budget.cost(), Duration::from_millis(200) + work);
        // Окно выждало свой период целиком: видно только, что остаток кадра не больше
        // ожидания, — такой промежуток цену не уточняет, а оценка тает до подтверждённой
        // цены, не ниже: период держит тройной запас над ценой кадра (находка повторного
        // ревью шага 5: раньше оценка только росла, и период залипал на секунде до «Стоп»).
        assert_eq!(
            present_wait(
                Duration::from_millis(606),
                Duration::from_millis(600),
                requested
            ),
            None
        );
        for _ in 0..10 {
            budget.sample(
                Some(Duration::from_millis(606)),
                Some(Duration::from_millis(600)),
                Some(requested),
                work,
            );
        }
        assert_eq!(budget.cost(), budget.measured(), "оценка не ниже замера");
        // Пауза (свёрнутое окно) в цену кадра не идёт и оценку не трогает.
        let before = budget.cost();
        budget.sample(None, Some(FRAME), Some(requested), work);
        assert_eq!(budget.cost(), before);
    }

    /// Храповик периода: задержка окна не держит период до «Стоп».
    ///
    /// Свёрнутое, перетаскиваемое и развёрнутое окно дают промежуток в полторы секунды
    /// при ожидании в 33 мс. Раньше он записывался ценой кадра, период уходил в секунду,
    /// а кадры, выждавшие период целиком, держали оценку до остановки — в том числе на
    /// GPU. Теперь такая задержка приходит одна, подтверждения не получает и цену не
    /// поднимает.
    #[test]
    fn one_long_gap_does_not_stick_the_frame_period() {
        let work = Duration::from_micros(500);
        let requested = Duration::from_millis(2);
        let price = Duration::from_millis(192);
        let mut budget = FrameBudget::default();
        budget.sample(None, None, None, work);
        budget.sample(
            Some(Duration::from_millis(1500)),
            Some(FRAME),
            Some(requested),
            work,
        );
        // Кадры идут своим периодом и выжидают его целиком: период WARP — норма
        // (программный растр — кадр не чаще 10 раз в секунду), GPU — FRAME.
        let mut delay = next_period(true, budget.cost());
        assert_eq!(delay, FRAME_SOFTWARE, "задержка не подняла период");
        for _ in 0..30 {
            budget.sample(Some(delay + requested), Some(delay), Some(requested), work);
            delay = next_period(true, budget.cost());
        }
        assert_eq!(
            delay,
            FRAME_SOFTWARE,
            "период WARP вернулся к норме: цена {:?}",
            budget.measured()
        );
        // Запись на WARP идёт: два замера подряд подтверждают цену кадра, период — три
        // цены, — а пауза окна после них по-прежнему не поднимает его в секунду.
        let mut budget = FrameBudget::default();
        for _ in 0..2 {
            budget.sample(
                Some(requested + price),
                Some(FRAME_SOFTWARE),
                Some(requested),
                work,
            );
        }
        let settled = next_period(true, budget.cost());
        assert_eq!(settled, (price + work) * 3, "период — три цены кадра");
        budget.sample(
            Some(Duration::from_millis(1500)),
            Some(settled),
            Some(requested),
            work,
        );
        let mut delay = next_period(true, budget.cost());
        assert!(
            delay <= settled + FRAME,
            "пауза не подняла период: {delay:?}"
        );
        for _ in 0..30 {
            budget.sample(Some(delay + requested), Some(delay), Some(requested), work);
            delay = next_period(true, budget.cost());
        }
        assert_eq!(
            delay,
            settled,
            "период вернулся к трём ценам кадра: {:?}",
            budget.measured()
        );
        // Аппаратный растр период не оценивает вовсе.
        let mut gpu = FrameBudget::default();
        let mut periods = Vec::new();
        gpu.sample(None, None, None, work);
        gpu.sample(
            Some(Duration::from_millis(1500)),
            Some(FRAME),
            Some(requested),
            work,
        );
        for _ in 0..30 {
            gpu.sample(Some(FRAME + requested), Some(FRAME), Some(requested), work);
            periods.push(next_period(false, gpu.cost()));
        }
        assert!(
            periods.iter().all(|period| *period == FRAME),
            "установившийся период на GPU — {FRAME:?}: {periods:?}"
        );
    }

    /// Пара медленных кадров не держит период до «Стоп»: цена кадра стареет.
    ///
    /// На WARP подтверждённая цена становилась храповиком: после пары кадров по секунде
    /// (нагрузка CPU на пару секунд, своп) период стоял у предела, потому что кадры,
    /// выждавшие свой период целиком, цены не уточняют, а пол оценки не давал ей
    /// опуститься ниже подтверждённой. Теперь раз в [`COST_PROBE_EVERY`] цену проверяет
    /// пара кадров с ожиданием [`COST_PROBE_DELAY`], и замер заменяет её (находка
    /// контрольного ревью шага 5).
    #[test]
    fn a_pair_of_slow_frames_does_not_stick_the_frame_period() {
        let work = Duration::from_micros(500);
        let requested = Duration::from_millis(2);
        let price = Duration::from_millis(200);
        let mut budget = FrameBudget::default();
        budget.sample(None, None, None, work);
        // Пара кадров по секунде: подтверждённая цена кадра — секунда.
        for _ in 0..2 {
            budget.sample(
                Some(requested + FRAME_SLOWEST),
                Some(FRAME_SOFTWARE),
                Some(requested),
                work,
            );
        }
        assert_eq!(budget.measured(), FRAME_SLOWEST + work);
        assert_eq!(next_period(true, budget.cost()), FRAME_SLOWEST);
        // Дальше минута кадров по 200 мс: кадр либо укладывается в ожидание, либо
        // перерастает его. Пробные кадры окно запрашивает так же, как приложение.
        let mut elapsed = Duration::ZERO;
        while elapsed < Duration::from_secs(60) {
            let delay = if budget.take_probe() {
                COST_PROBE_DELAY
            } else {
                next_period(true, budget.cost())
            };
            let spacing = requested + price.max(delay);
            budget.sample(Some(spacing), Some(delay), Some(requested), work);
            elapsed += spacing;
        }
        let period = next_period(true, budget.cost());
        assert!(
            period <= price.mul_f32(3.0) + FRAME,
            "период {period:?} не вернулся к трём ценам кадра: цена {:?}",
            budget.measured()
        );
        assert_eq!(
            budget.measured(),
            price + work,
            "цена кадра — настоящая, а не прежняя"
        );
    }

    /// Пол оценки цены кадра не обходит предел роста.
    ///
    /// Кадр, выждавший своё ожидание целиком, не уточняет цену — он только не выше её, — и
    /// прежний пол возвращал оценку к подтверждённой цене одним кадром, хотя замеров
    /// вверх не больше чем вдвое за кадр ([`COST_GROWTH`]). С таким полом пара кадров по
    /// три секунды снова держала период у предела.
    #[test]
    fn the_floor_of_the_cost_respects_the_growth_limit() {
        let work = Duration::from_micros(500);
        let requested = Duration::from_millis(2);
        let price = Duration::from_millis(200);
        let mut budget = FrameBudget::default();
        // Цена 200 мс установилась парой замеров.
        for _ in 0..2 {
            budget.sample(
                Some(requested + price),
                Some(COST_PROBE_DELAY),
                Some(requested),
                work,
            );
        }
        assert_eq!(budget.cost(), price + work);
        // Пара кадров по три секунды: и цена, и оценка растут не больше чем вдвое за кадр.
        let mut previous = budget.cost();
        for _ in 0..2 {
            budget.sample(
                Some(requested + Duration::from_secs(3)),
                Some(COST_PROBE_DELAY),
                Some(requested),
                work,
            );
            assert!(
                budget.cost() <= previous.mul_f32(COST_GROWTH),
                "цена {:?} после {:?} выросла больше чем вдвое",
                budget.cost(),
                previous
            );
            previous = budget.cost();
        }
        assert_eq!(budget.measured(), Duration::from_secs(3) + work);
        // Кадр, выждавший ожидание целиком, не возвращает оценку к цене одним кадром.
        budget.sample(
            Some(requested + Duration::from_secs(3)),
            Some(Duration::from_secs(3)),
            Some(requested),
            work,
        );
        assert!(
            budget.cost() <= previous.mul_f32(COST_GROWTH),
            "пол вернул цену с {previous:?} к {:?}",
            budget.cost()
        );
    }

    /// Выброс не поднимает оценку в потолок одним кадром.
    #[test]
    fn one_outlier_lifts_the_cost_by_no_more_than_twice() {
        let work = Duration::from_millis(1);
        let requested = Duration::from_millis(2);
        let delay = Duration::from_millis(66);
        // Кадр не уложился в ожидание на 30 мс: цена кадра — 146 мс.
        let spacing = delay + requested + PRESENT_SLACK + Duration::from_millis(30);
        let mut budget = FrameBudget::default();
        for _ in 0..12 {
            budget.sample(Some(spacing), Some(delay), Some(requested), work);
        }
        let settled = budget.cost();
        assert_eq!(settled, Duration::from_millis(146) + work);
        // Своп или чужой процесс: два кадра «рисовались» по две секунды.
        for _ in 0..2 {
            budget.sample(Some(FRAME_SLOWEST * 2), Some(delay), Some(requested), work);
        }
        assert!(
            budget.cost() <= settled * 2 + work,
            "выброс поднял цену с {settled:?} до {:?}",
            budget.cost()
        );
    }

    /// Смена окна забывает оценку, но не цену кадра: новой записи — новая цена.
    ///
    /// Оценка после разворота не годится (другая раскладка), а подтверждённая цена
    /// кадра остаётся: с неё окно начинает заново и не тратит кадры на догадку. Новая
    /// сессия забывает и её.
    #[test]
    fn a_window_change_forgets_the_estimate_but_keeps_the_price() {
        let work = Duration::from_millis(1);
        let requested = Duration::from_millis(2);
        let price = Duration::from_millis(180);
        let mut budget = FrameBudget::default();
        for _ in 0..2 {
            budget.sample(
                Some(requested + price),
                Some(FRAME_SOFTWARE),
                Some(requested),
                work,
            );
        }
        assert_eq!(budget.measured(), price + work);
        assert!(next_period(true, budget.cost()) > FRAME_SOFTWARE);
        budget.reset();
        assert_eq!(budget.cost(), Duration::ZERO, "оценка забыта");
        assert_eq!(budget.measured(), price + work, "цена кадра осталась");
        // После смены окна цена подтверждается заново: пара замеров — и период снова
        // три цены кадра.
        for _ in 0..2 {
            budget.sample(
                Some(requested + price),
                Some(FRAME_SOFTWARE),
                Some(requested),
                work,
            );
        }
        assert_eq!(next_period(true, budget.cost()), (price + work) * 3);
        // Новая запись забывает и цену.
        budget.clear();
        assert_eq!(budget.measured(), Duration::ZERO);
        assert_eq!(budget.cost(), Duration::ZERO);
    }

    /// Предел промежутка считается от ожидания: пауза свёрнутого окна в цену не идёт.
    #[test]
    fn the_gap_limit_grows_with_the_wait() {
        assert_eq!(max_gap(FRAME), Duration::from_millis(316));
        assert_eq!(max_gap(FRAME_SLOWEST), Duration::from_millis(2250));
        assert!(max_gap(FRAME) < Duration::from_secs(1));
    }

    /// Раннее пробуждение не становится ценой кадра (находка ревью шага 5).
    ///
    /// Запись без событий: окно просит кадр раз в секунду, кадр быстрый. Событие через
    /// 500 мс будит окно раньше срока; если записать этот промежуток в цену кадра,
    /// период уйдёт в секунду, и лента будет отставать на секунду даже на GPU.
    #[test]
    fn an_early_frame_does_not_become_the_cost_of_a_frame() {
        let work = Duration::from_micros(300);
        let requested = Duration::from_millis(1);
        let mut budget = FrameBudget::default();
        budget.sample(None, None, None, work);
        // Кадр без событий: спали секунду, кадр быстрый — цена кадра мала.
        budget.sample(
            Some(FRAME_SLOWEST + requested),
            Some(FRAME_SLOWEST),
            Some(requested),
            work,
        );
        assert!(
            budget.cost() <= Duration::from_millis(20),
            "кадр быстрого растра: {:?}",
            budget.cost()
        );
        assert_eq!(next_period(false, budget.cost()), FRAME);
        // Событие через 500 мс: окно проснулось раньше своего периода.
        budget.sample(
            Some(Duration::from_millis(500)),
            Some(FRAME_SLOWEST),
            Some(requested),
            work,
        );
        assert!(
            budget.cost() <= Duration::from_millis(20),
            "раннее пробуждение — не цена кадра: {:?}",
            budget.cost()
        );
        assert_eq!(
            next_period(false, budget.cost()),
            FRAME,
            "следующий период — 33 мс, а не секунда"
        );
        // И дальше: период остаётся прежним, пока события идут чаще секунды.
        for _ in 0..5 {
            budget.sample(Some(FRAME + requested), Some(FRAME), Some(requested), work);
        }
        assert_eq!(next_period(false, budget.cost()), FRAME);
    }

    /// Ожидание до следующей целой секунды записи: таймер шапки показывает `elapsed`
    /// целыми секундами, и кадр к границе не даёт подписи перескакивать.
    #[test]
    fn the_quiet_frame_waits_for_the_next_whole_second() {
        assert_eq!(next_second(Duration::ZERO), FRAME_SLOWEST);
        assert_eq!(
            next_second(Duration::from_millis(1500)),
            Duration::from_millis(500)
        );
        assert_eq!(
            next_second(Duration::from_millis(39_990)),
            Duration::from_millis(10)
        );
        // Секунды идут подряд, без пропусков и повторов: кадр к границе секунды.
        let mut elapsed = Duration::from_millis(200);
        let mut shown = vec![elapsed.as_secs()];
        for _ in 0..20 {
            elapsed += next_second(elapsed);
            shown.push(elapsed.as_secs());
        }
        assert_eq!(
            shown,
            (0..=20).collect::<Vec<_>>(),
            "каждая секунда показана ровно раз"
        );
    }

    #[test]
    fn the_phase_follows_the_session() {
        use Action as A;
        use Phase as P;
        assert_eq!(reduce(P::Idle, A::StartRequested), P::Starting);
        assert_eq!(reduce(P::Starting, A::Started), P::Recording);
        assert_eq!(reduce(P::Starting, A::StartFailed), P::Idle);
        assert_eq!(reduce(P::Recording, A::StopRequested), P::Stopping);
        assert_eq!(reduce(P::Stopping, A::Stopped), P::Idle);
        assert_eq!(reduce(P::Recording, A::JournalFailed), P::Failed);
        assert_eq!(reduce(P::Stopping, A::JournalFailed), P::Failed);
        assert_eq!(reduce(P::Failed, A::FailureDismissed), P::Idle);
        assert_eq!(reduce(P::Failed, A::StartRequested), P::Starting);
        // Недопустимое — без изменений: второй старт во время записи, стоп без записи.
        assert_eq!(reduce(P::Recording, A::StartRequested), P::Recording);
        assert_eq!(reduce(P::Stopping, A::StartRequested), P::Stopping);
        assert_eq!(reduce(P::Idle, A::StopRequested), P::Idle);
        assert_eq!(reduce(P::Idle, A::Stopped), P::Idle);
        assert_eq!(reduce(P::Idle, A::JournalFailed), P::Idle);
    }

    fn server() -> Source {
        Source::TcpServer {
            listen: "0.0.0.0:5000".parse::<SocketAddr>().unwrap(),
        }
    }

    #[test]
    fn core_errors_get_their_codes() {
        let error = start_error(&io::Error::from(io::ErrorKind::AddrInUse), &server());
        assert_eq!(error.code, ErrorCode::PortInUse);
        assert_eq!(error.params.get("port").map(String::as_str), Some("5000"));
        assert_eq!(
            start_error(&io::Error::from(io::ErrorKind::AddrNotAvailable), &server()).code,
            ErrorCode::AddressUnavailable
        );
        assert_eq!(
            start_error(&io::Error::from(io::ErrorKind::PermissionDenied), &server()).code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            start_error(&io::Error::other("journal not created"), &server()).code,
            ErrorCode::Journal,
            "у сервера остальное — журнал"
        );
        let client = Source::TcpClient {
            addr: "192.168.1.50:5000".into(),
            reconnect: Duration::from_secs(5),
        };
        assert_eq!(
            start_error(&io::Error::from(io::ErrorKind::AddrInUse), &client).code,
            ErrorCode::Io
        );

        // Петля прокси на себя — свой код и своё поле, а не «журнал» (M4-AC6).
        let proxy = Source::Proxy {
            listen: "0.0.0.0:50025".parse::<SocketAddr>().unwrap(),
            target: "127.0.0.1:50025".into(),
            role: ProxyRole::AnalyzerClient,
            retry: Duration::from_secs(5),
            allow_from: None,
        };
        let looped = io::Error::new(
            io::ErrorKind::InvalidInput,
            ProxyTargetIsSelf {
                addr: "127.0.0.1:50025".parse().unwrap(),
            },
        );
        let error = start_error(&looped, &proxy);
        assert_eq!(error.code, ErrorCode::ProxyTargetSelf);
        assert_eq!(error.field(SessionMode::Proxy), ErrorField::TargetPort);

        let settings = SerialSettings::new("COM5", 9600, SerialFormat::EIGHT_N_ONE);
        let serial =
            |kind, listed: bool| serial_error(&io::Error::from(kind), &settings, |_| listed);
        assert_eq!(
            serial(io::ErrorKind::InvalidInput, false).code,
            ErrorCode::PortSettings
        );
        assert_eq!(
            serial(io::ErrorKind::PermissionDenied, false).code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            serial(io::ErrorKind::NotFound, true).code,
            ErrorCode::PortBusy
        );
        assert_eq!(
            serial(io::ErrorKind::NotFound, false).code,
            ErrorCode::PortNotFound
        );
        let other = serial(io::ErrorKind::Other, false);
        assert_eq!(other.code, ErrorCode::Serial);
        assert_eq!(other.params.get("port").map(String::as_str), Some("COM5"));
    }

    fn reason(os: OsFamily, code: i32) -> JournalFailureReason {
        JournalFailureReason::from_code(io::ErrorKind::Other, Some(code), os)
    }

    #[test]
    fn a_journal_failure_reason_becomes_a_code() {
        let windows = |code| JournalFailureCode::from_failure(&reason(OsFamily::Windows, code));
        assert_eq!(windows(112), JournalFailureCode::DiskFull);
        assert_eq!(windows(39), JournalFailureCode::DiskFull);
        assert_eq!(windows(5), JournalFailureCode::PermissionDenied);
        assert_eq!(windows(2), JournalFailureCode::NotFound);
        assert_eq!(windows(3), JournalFailureCode::NotFound);
        for code in [53, 55, 59, 64] {
            assert_eq!(windows(code), JournalFailureCode::Network);
        }
        assert_eq!(windows(32), JournalFailureCode::Busy);
        assert_eq!(windows(33), JournalFailureCode::Busy);
        let unix = |code| JournalFailureCode::from_failure(&reason(OsFamily::Unix, code));
        assert_eq!(unix(28), JournalFailureCode::DiskFull);
        assert_eq!(unix(13), JournalFailureCode::PermissionDenied);
        assert_eq!(unix(1), JournalFailureCode::PermissionDenied);
        assert_eq!(unix(2), JournalFailureCode::NotFound);
        assert_eq!(unix(5), JournalFailureCode::Io, "EIO, а не «нет доступа»");
        let sem_timeout =
            JournalFailureReason::from_code(io::ErrorKind::TimedOut, Some(121), OsFamily::Windows);
        assert_eq!(
            JournalFailureCode::from_failure(&sem_timeout),
            JournalFailureCode::Timeout
        );
        let timeout = JournalFailureReason::from_error(&io::Error::new(
            io::ErrorKind::TimedOut,
            "journal not flushed",
        ));
        assert_eq!(
            JournalFailureCode::from_failure(&timeout),
            JournalFailureCode::Timeout
        );
        let other = JournalFailureReason::from_message("device refused the write");
        assert_eq!(
            JournalFailureCode::from_failure(&other),
            JournalFailureCode::Io
        );
    }

    /// Свой код причины важнее вида ошибки: переполненная очередь писателя — не «ошибка
    /// записи на диск», хотя ОС здесь ни при чём (M4-AC12).
    #[test]
    fn a_coded_failure_reason_keeps_its_own_code() {
        let backlog = JournalFailureReason::from_message("journal writer backlog is full")
            .with_code("journal_backlog");
        assert_eq!(
            JournalFailureCode::from_failure(&backlog),
            JournalFailureCode::Backlog
        );
        let coded =
            JournalFailureReason::from_code(io::ErrorKind::NotFound, Some(2), OsFamily::Unix)
                .with_code("something_else");
        assert_eq!(
            JournalFailureCode::from_failure(&coded),
            JournalFailureCode::Io
        );
    }

    fn note(index: usize) -> SessionEvent {
        Arc::new(bytetap_core::journal::Event::Note(
            bytetap_core::journal::Note {
                t: "2026-10-19T10:15:03.000+03:00".into(),
                mono_us: Some(index as u64),
                text: index.to_string(),
            },
        ))
    }

    /// 50 000 событий через подписку с маленькой очередью: буфер не растёт сверх
    /// предела, а всё, что не дошло, посчитано как пропущенное (U1-AC13).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lagging_pump_counts_what_it_missed() {
        let (sender, receiver) = broadcast::channel(64);
        let feed = SharedFeed::default();
        let pump = tokio::spawn(pump(receiver, Arc::clone(&feed), egui::Context::default()));
        const TOTAL: usize = 50_000;
        for index in 0..TOTAL {
            sender.send(note(index)).unwrap();
            if index % 5_000 == 0 {
                tokio::task::yield_now().await;
            }
        }
        drop(sender);
        pump.await.unwrap();
        let ring = lock(&feed);
        assert!(ring.len() <= crate::feed::FEED_CAPACITY);
        // Счёт точный: `Lagged` вытесняет старейшие события подписки, а всё, что лента
        // получила, пронумеровано; пропущенное и полученное вместе — всё отправленное.
        assert_eq!(ring.next_seq() + ring.dropped(), TOTAL as u64);
        assert!(ring.dropped() > 0, "очередь 64 при 50 000 событий отстаёт");
        assert!(
            matches!(&ring.iter().last().unwrap().kind, crate::feed::FeedKind::Note { text } if *text == (TOTAL - 1).to_string()),
            "последнее событие дошло"
        );
    }
}
