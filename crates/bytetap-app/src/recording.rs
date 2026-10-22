//! Экран записи (M3-UI-AC4…AC8, U1-AC14…AC17): шапка с «Стоп», сводкой и счётчиками,
//! строка отказа журнала, трасса, панель ленты и виртуализированная лента.
//!
//! Лента — зеркало общего буфера [`live::SharedFeed`]: каждый кадр окно забирает только
//! новые события (без копирования всей ленты) и дописывает их в свой индекс строк. Кадр
//! рисует только видимые строки (`show_rows`); высота строки фиксирована, в HEX-виде
//! событие занимает по строке на 16 байт.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytetap_core::journal::{JournalFailureReason, Stats};
use bytetap_core::session::SessionCounters;
use eframe::egui::{
    self, Align, Color32, CursorIcon, Frame, Id, Key, Layout, Popup, Rect, RectAlign, RichText,
    Role, ScrollArea, Sense, Stroke, TextEdit, Ui, UiBuilder, WidgetInfo, epaint::Shadow, pos2,
    text::LayoutJob, vec2,
};
use tokio::sync::watch;

use crate::app::{BytetapApp, NoteTarget};
use crate::feed::{self, Detected, ENCODINGS, FeedEvent, FeedKind, FeedRing};
use crate::feedrows::{
    HEX_ROW_HEIGHT, Pair, TEXT_ROW_HEIGHT, hex_row, text_format, text_row, truncated,
};
use crate::icons::{self, Icon};
use crate::live::{self, JournalFailure, JournalFailureCode, SharedFeed};
use crate::settings::{AppConfig, Encoding, FeedView, ProxyMode, Responder, SessionMode};
use crate::theme::tokens::{
    RADIUS_CHIP, RADIUS_PANEL, RADIUS_RECORD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_2XS,
    TEXT_BASE, TEXT_DATA, TEXT_XS,
};
use crate::theme::{self, Palette, mix, radius};
use crate::{diag, feedrows, t, trace, widgets};

/// Высота панели над лентой.
const BAR_HEIGHT: f32 = 32.0;
/// Период пульса точки «Стоп» (`--bt-pulse-duration`).
const PULSE_PERIOD: f64 = 1.6;

/// Период кадра, после которого пульс точки «Стоп» не рисуется.
///
/// Программный растр (WARP) рисует кадр сотни миллисекунд, а окно просит его реже
/// секунды: фаза пульса к каждому кадру случайна, и кольцо мигает вместо того, чтобы
/// расходиться. Тогда точка стоит статично — отступление от UI v2, где пульс идёт
/// непрерывной CSS-анимацией ([`crate::app::BytetapApp::pulse`]).
pub(crate) const PULSE_MAX_PERIOD: Duration = Duration::from_millis(200);

/// Индекс строк ленты: номер события и номер его первой строки.
///
/// Номера строк сквозные с начала индекса: вытеснение старых событий снимает записи
/// спереди, новые дописываются сзади. Строка экрана — смещение от первой записи.
#[derive(Debug, Default)]
pub struct RowIndex {
    entries: VecDeque<(u64, u64)>,
    end: u64,
    hex: bool,
    filter: Option<u32>,
}

impl RowIndex {
    pub fn new(hex: bool, filter: Option<u32>) -> Self {
        Self {
            hex,
            filter,
            ..Self::default()
        }
    }

    /// Дописывает событие, если оно проходит фильтр соединения.
    pub fn push(&mut self, seq: u64, event: &FeedEvent) {
        if !feed::passes(event, self.filter) {
            return;
        }
        let lines = if self.hex { event.hex_lines() } else { 1 };
        self.entries.push_back((seq, self.end));
        self.end += lines as u64;
    }

    /// Снимает события старше `first_seq`: их уже нет в буфере.
    pub fn evict_before(&mut self, first_seq: u64) {
        while self
            .entries
            .front()
            .is_some_and(|(seq, _)| *seq < first_seq)
        {
            self.entries.pop_front();
        }
    }

    /// Сколько строк в ленте.
    pub fn rows(&self) -> usize {
        self.entries
            .front()
            .map_or(0, |(_, start)| (self.end - start) as usize)
    }

    /// Событие и номер строки внутри него для строки экрана `row`.
    pub fn locate(&self, row: usize) -> Option<(u64, usize)> {
        let base = self.entries.front()?.1;
        let line = base + row as u64;
        if line >= self.end {
            return None;
        }
        let index = self.entries.partition_point(|(_, start)| *start <= line) - 1;
        let (seq, start) = self.entries[index];
        Some((seq, (line - start) as usize))
    }

    /// Индекс заново: другой вид или фильтр.
    pub fn rebuild(ring: &FeedRing, hex: bool, filter: Option<u32>) -> Self {
        let mut index = Self::new(hex, filter);
        for (seq, event) in ring.since(0) {
            index.push(seq, event);
        }
        index
    }
}

/// Что показывает экран записи: зеркало ленты, индекс строк, прокрутка и сводка.
pub struct RecordingView {
    shared: SharedFeed,
    feed: FeedRing,
    /// Сколько событий каждого соединения в буфере: фильтр и колонка номера.
    conns: BTreeMap<u32, usize>,
    detected: Option<Detected>,
    rows: RowIndex,
    dropped: u64,
    /// Догон новых строк; прокрутка вверх выключает.
    follow: bool,
    /// Сколько событий пришло после прокрутки вверх — «↓ N новых».
    pending: u64,
    filter: Option<u32>,
    jump_bottom: bool,
    counters: Option<SessionCounters>,
    fixed_stats: Stats,
    started: Instant,
    fixed_elapsed: Option<Duration>,
    /// Настройки, с которыми запись запущена: сводка режима в шапке.
    pub(crate) config: AppConfig,
    /// Адреса сторон пары прокси из последнего `open` (FR-5, M4-AC13): сводка двух
    /// сторон в шапке. До первого соединения оба пусты.
    sides: (Option<String>, Option<String>),
    /// Локальный адрес исходящего сокета к цели из последнего `open`: с него вторая
    /// сторона видит bytetap (FR-8).
    outgoing_local: Option<String>,
    /// Фактический адрес слушателя TCP-сервера или адрес анализатора.
    pub(crate) local_addr: String,
    pub(crate) journal_path: Option<PathBuf>,
    failure: Option<watch::Receiver<Option<JournalFailureReason>>>,
}

/// Синтетическая запись для снимков и тестов: события, счётчики и время без сессии.
#[derive(Clone, Debug, Default)]
pub struct RecordingFixture {
    pub events: Vec<bytetap_core::journal::Event>,
    pub stats: Stats,
    pub elapsed: Duration,
    pub local_addr: String,
    pub journal_path: Option<PathBuf>,
    pub dropped: u64,
}

impl RecordingView {
    fn new(shared: SharedFeed, config: AppConfig, local_addr: String) -> Self {
        Self {
            shared,
            feed: FeedRing::default(),
            conns: BTreeMap::new(),
            detected: None,
            rows: RowIndex::default(),
            dropped: 0,
            follow: true,
            pending: 0,
            filter: None,
            jump_bottom: false,
            counters: None,
            fixed_stats: Stats::default(),
            started: Instant::now(),
            fixed_elapsed: None,
            config,
            sides: (None, None),
            outgoing_local: None,
            local_addr,
            journal_path: None,
            failure: None,
        }
    }

    /// Экран идущей сессии.
    pub fn live(session: &live::LiveSession, config: AppConfig) -> Self {
        let mut view = Self::new(session.feed(), config, session.local_addr().to_owned());
        view.counters = Some(session.counters());
        view.journal_path = Some(session.journal_path().to_path_buf());
        view.failure = Some(session.failure_watch());
        view
    }

    /// Экран без сессии: старт без runtime (тесты) — пустая лента.
    pub fn detached(config: AppConfig) -> Self {
        Self::new(SharedFeed::default(), config, String::new())
    }

    pub fn fixture(fixture: RecordingFixture, config: AppConfig) -> Self {
        let shared = SharedFeed::default();
        {
            let mut ring = live::lock(&shared);
            for event in &fixture.events {
                if let Some(event) = FeedEvent::from_session(event) {
                    ring.push(Arc::new(event));
                }
            }
            ring.lagged(fixture.dropped);
        }
        let mut view = Self::new(shared, config, fixture.local_addr);
        view.fixed_stats = fixture.stats;
        view.fixed_elapsed = Some(fixture.elapsed);
        view.journal_path = fixture.journal_path;
        view
    }

    /// Общий буфер ленты: тесты дописывают в него события.
    pub fn shared(&self) -> SharedFeed {
        Arc::clone(&self.shared)
    }

    pub fn stats(&self) -> Stats {
        self.counters
            .as_ref()
            .map_or_else(|| self.fixed_stats.clone(), SessionCounters::snapshot)
    }

    pub fn elapsed(&self) -> Duration {
        self.fixed_elapsed.unwrap_or_else(|| self.started.elapsed())
    }

    /// Отказ журнала, о котором сообщило ядро.
    pub fn failure(&self) -> Option<JournalFailure> {
        let watch = self.failure.as_ref()?;
        watch.borrow().as_ref().map(JournalFailure::new)
    }

    /// Фактический адрес слушателя TCP-сервера или адрес анализатора.
    pub fn local_addr(&self) -> &str {
        &self.local_addr
    }

    pub fn follow(&self) -> bool {
        self.follow
    }

    pub fn pending(&self) -> u64 {
        self.pending
    }

    pub fn rows(&self) -> usize {
        self.rows.rows()
    }

    pub fn len(&self) -> usize {
        self.feed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.feed.is_empty()
    }

    /// Номера соединений в буфере по возрастанию.
    pub fn conns(&self) -> Vec<u32> {
        self.conns.keys().copied().collect()
    }

    pub fn set_filter(&mut self, filter: Option<u32>) {
        if self.filter != filter {
            self.filter = filter;
            self.rows = RowIndex::rebuild(&self.feed, self.rows.hex, filter);
        }
    }

    /// Забирает из общего буфера только новые события; возвращает, сколько пришло.
    pub fn sync(&mut self, hex: bool) -> u64 {
        if self.rows.hex != hex {
            self.rows = RowIndex::rebuild(&self.feed, hex, self.filter);
        }
        let shared = live::lock(&self.shared);
        self.dropped = shared.dropped();
        if self.feed.next_seq() < shared.first_seq() {
            // Окно отстало больше чем на буфер: зеркало начинается заново с общего.
            self.feed = FeedRing::starting_at(shared.first_seq());
            self.conns.clear();
            self.rows = RowIndex::new(hex, self.filter);
        }
        let mut added = 0;
        let new: Vec<(u64, Arc<FeedEvent>)> = shared
            .since(self.feed.next_seq())
            .map(|(seq, event)| (seq, Arc::clone(event)))
            .collect();
        drop(shared);
        for (seq, event) in new {
            if self.detected.is_none() {
                self.detected = feed::detect(&event);
            }
            self.remember_sides(&event);
            if let Some(conn) = event.conn {
                *self.conns.entry(conn).or_default() += 1;
            }
            self.rows.push(seq, &event);
            if let Some(evicted) = self.feed.push(event)
                && let Some(conn) = evicted.conn
                && let Some(count) = self.conns.get_mut(&conn)
            {
                *count -= 1;
                if *count == 0 {
                    self.conns.remove(&conn);
                }
            }
            added += 1;
        }
        self.rows.evict_before(self.feed.first_seq());
        if self
            .filter
            .is_some_and(|conn| !self.conns.contains_key(&conn))
        {
            // Выбранное соединение ушло из буфера — фильтр снимается.
            self.set_filter(None);
        }
        if !self.follow {
            self.pending += added;
        }
        added
    }

    /// Сводка режима в шапке: «жду на :5000 · HL7 (авто)», «COM5 · 9600 8N1»,
    /// «анализатор 192.168.1.51 ⇄ bytetap :50025 ⇄ ЛИС 10.0.0.5:5000».
    pub fn summary(&self, stats: &Stats) -> String {
        let cfg = &self.config;
        match cfg.mode {
            SessionMode::TcpClient => t!(
                "record.connectedTo",
                addr = format!("{}:{}", cfg.connect_host, cfg.connect_port),
                responder = self.responder_name()
            ),
            SessionMode::Serial => format!(
                "{} · {} {}",
                cfg.serial.port,
                cfg.serial.baud,
                crate::form::serial_format(&cfg.serial)
            ),
            SessionMode::TcpServer => {
                // Порт 0 выбирает система: в сводке — тот, что занят на самом деле.
                let port = self.listen_port();
                if stats.connections > 0 {
                    t!(
                        "record.onPort",
                        port = port,
                        responder = self.responder_name()
                    )
                } else {
                    t!(
                        "record.waitingOn",
                        port = port,
                        responder = self.responder_name()
                    )
                }
            }
            // Сводка прокси — две стороны и bytetap посередине (M4-AC13): у остальных
            // режимов за запись одна вторая сторона, у пары их две.
            SessionMode::Proxy => pair_summary(
                cfg.proxy_role,
                &self.listen_port(),
                &format!("{}:{}", cfg.proxy_host, cfg.proxy_port),
                self.sides.0.as_deref(),
                self.sides.1.as_deref(),
            ),
        }
    }

    /// Порт прослушивания для сводки: «:50025», фактический, если система выбрала его
    /// сама (порт 0 в форме).
    fn listen_port(&self) -> String {
        let port = self.local_addr.rsplit_once(':').map_or_else(
            || self.config.listen_port.to_string(),
            |(_, port)| port.to_owned(),
        );
        format!(":{port}")
    }

    /// Сводка шапки галлеей: у прокси адреса моноширинным, подписи сторон muted,
    /// разделители faint — как в макете M4-4. У остальных режимов — одна строка.
    pub fn summary_job(&self, p: &Palette, stats: &Stats) -> LayoutJob {
        if self.config.mode != SessionMode::Proxy {
            return LayoutJob::single_section(
                self.summary(stats),
                text_format(theme::sans(TEXT_DATA), p.text_muted),
            );
        }
        let cfg = &self.config;
        let pieces = pair_pieces(
            cfg.proxy_role,
            &self.listen_port(),
            &format!("{}:{}", cfg.proxy_host, cfg.proxy_port),
            self.sides.0.as_deref(),
            self.sides.1.as_deref(),
        );
        let mut job = LayoutJob::default();
        for piece in &pieces {
            let format = match piece {
                PairPiece::Label(_) => text_format(theme::sans(TEXT_DATA), p.text_muted),
                PairPiece::Address(_) => text_format(theme::mono_medium(TEXT_DATA), p.text),
                PairPiece::Separator => text_format(theme::sans(TEXT_DATA), p.text_faint),
            };
            job.append(&piece.text(), 0.0, format);
        }
        job
    }

    /// Пара сторон ленты: у прокси строки говорят «открыто: анализатор → ЛИС» и «ЛИС
    /// закрыла соединение», у остальных режимов — как раньше.
    pub fn pair(&self) -> Pair {
        Pair::of(self.config.mode, self.config.proxy_role)
    }

    /// Адреса сторон пары из события `open`: принятая сторона — в `peer`, исходящая — в
    /// `link`. Роль стороны в журнале не зависит от того, кто инициировал соединение.
    fn remember_sides(&mut self, event: &FeedEvent) {
        let FeedKind::Open {
            peer,
            link: Some(link),
        } = &event.kind
        else {
            return;
        };
        self.outgoing_local = Some(link.local.clone());
        self.sides = match link.role {
            bytetap_core::journal::LinkRole::Lis => (Some(peer.clone()), Some(link.peer.clone())),
            bytetap_core::journal::LinkRole::Analyzer => {
                (Some(link.peer.clone()), Some(peer.clone()))
            }
        };
    }

    /// Локальный адрес исходящего сокета к цели из первого `open`: с него вторая
    /// сторона видит bytetap (FR-8).
    pub fn outgoing_local(&self) -> Option<&str> {
        self.outgoing_local.as_deref()
    }

    fn responder_name(&self) -> &'static str {
        match self.config.responder {
            Responder::Hl7 => t!("record.responderHl7"),
            Responder::Astm => t!("record.responderAstm"),
            Responder::None => t!("record.responderNone"),
            Responder::Auto => match self.detected {
                Some(Detected::Hl7) => t!("record.responderAutoHl7"),
                Some(Detected::Astm) => t!("record.responderAutoAstm"),
                _ => t!("record.responderAuto"),
            },
        }
    }
}

/// Кусок сводки пары прокси: подпись стороны, адрес или разделитель.
///
/// Из кусков собираются обе формы сводки — строка для AccessKit и подсказки и галлея
/// шапки с моноширинными адресами (M4-4): так текст и рисунок не расходятся.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PairPiece {
    /// Подпись стороны: «анализатор», «bytetap», «ЛИС» — muted.
    Label(String),
    /// Адрес или порт: моноширинный, цвет текста.
    Address(String),
    /// Разделитель «⇄» с пробелами по краям — faint.
    Separator,
}

impl PairPiece {
    /// Кусок как текст с пробелом после него: строки для подсказки и AccessKit.
    ///
    /// Пробел стоит в конце каждого куска, а не в начале разделителя: раскладка egui не
    /// выводит пробел, с которого начинается кусок, и «⇄» прилипал бы к адресу.
    fn text(&self) -> String {
        match self {
            Self::Label(text) | Self::Address(text) => format!("{text} "),
            Self::Separator => format!("{PAIR_SEPARATOR} "),
        }
    }
}

/// Адрес стороны без порта соединения: у принятой стороны порт случайный (его выбрала
/// операционная система), а в макете M4-4 показан адрес прибора или ЛИС.
fn without_port(address: &str) -> &str {
    match address.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|ch| ch.is_ascii_digit()) && !host.is_empty() => {
            host
        }
        _ => address,
    }
}

/// Разделитель сторон пары в сводке шапки.
const PAIR_SEPARATOR: char = '⇄';

/// Сводка пары прокси кусками: «анализатор 192.168.1.51 ⇄ bytetap :50025 ⇄ ЛИС
/// 10.0.0.5:5000».
///
/// Пока пары не было, известна одна сторона — цель из настроек и порт прослушивания:
/// адрес второй стороны прокси узнаёт только из её соединения. Порт отрезается у
/// принятой стороны — в обеих топологиях: у анализатора он случайный, у ЛИС порт
/// соединения тоже не тот, на котором она слушает. Порт цели остаётся: именно его
/// набирают в настройках второй стороны.
fn pair_pieces(
    role: ProxyMode,
    listen: &str,
    target: &str,
    analyzer: Option<&str>,
    lis: Option<&str>,
) -> Vec<PairPiece> {
    let analyzer_label = || PairPiece::Label(t!("feed.byAnalyzer").to_owned());
    let lis_label = || PairPiece::Label(t!("feed.byLis").to_owned());
    let bytetap = || PairPiece::Label(t!("feed.byApp").to_owned());
    match (analyzer, lis) {
        (Some(analyzer), Some(lis)) => {
            // Пара есть: принятая сторона пришла сама, её порт — соединения.
            let (analyzer, lis) = match role {
                ProxyMode::AnalyzerClient => (without_port(analyzer), lis),
                ProxyMode::AnalyzerServer => (analyzer, without_port(lis)),
            };
            vec![
                analyzer_label(),
                PairPiece::Address(analyzer.to_owned()),
                PairPiece::Separator,
                bytetap(),
                PairPiece::Address(listen.to_owned()),
                PairPiece::Separator,
                lis_label(),
                PairPiece::Address(lis.to_owned()),
            ]
        }
        _ => match role {
            ProxyMode::AnalyzerClient => vec![
                bytetap(),
                PairPiece::Address(listen.to_owned()),
                PairPiece::Separator,
                lis_label(),
                PairPiece::Address(target.to_owned()),
            ],
            ProxyMode::AnalyzerServer => vec![
                analyzer_label(),
                PairPiece::Address(target.to_owned()),
                PairPiece::Separator,
                bytetap(),
                PairPiece::Address(listen.to_owned()),
            ],
        },
    }
}

/// Сводка пары прокси строкой: ею пользуются шапка записи, шапка просмотра журнала и
/// подсказка обрезанной сводки.
pub(crate) fn pair_summary(
    role: ProxyMode,
    listen: &str,
    target: &str,
    analyzer: Option<&str>,
    lis: Option<&str>,
) -> String {
    let text: String = pair_pieces(role, listen, target, analyzer, lis)
        .iter()
        .map(PairPiece::text)
        .collect();
    text.trim_end().to_owned()
}

/// Байты: «512 Б», «10,2 КБ», «1,2 МБ» (FR-25, счётчики в шапке записи).
pub fn format_bytes(bytes: u64) -> String {
    const NBSP: char = '\u{a0}';
    let size = |value: f64, unit: &str| {
        let rounded = (value * 10.0).round() / 10.0;
        let number = if rounded.fract() == 0.0 {
            format!("{rounded:.0}")
        } else {
            format!("{rounded:.1}").replace('.', ",")
        };
        format!("{number}{NBSP}{unit}")
    };
    let value = bytes as f64;
    if value >= 1024f64.powi(3) {
        size(value / 1024f64.powi(3), t!("units.gigabytes"))
    } else if value >= 1024f64.powi(2) {
        size(value / 1024f64.powi(2), t!("units.megabytes"))
    } else if value >= 1024.0 {
        size(value / 1024.0, t!("units.kilobytes"))
    } else {
        format!("{bytes}{NBSP}{}", t!("units.bytes"))
    }
}

/// Причина отказа журнала человеческой фразой.
pub fn failure_reason(code: JournalFailureCode) -> &'static str {
    match code {
        JournalFailureCode::DiskFull => t!("banner.journalFailureReasons.disk_full"),
        JournalFailureCode::PermissionDenied => {
            t!("banner.journalFailureReasons.permission_denied")
        }
        JournalFailureCode::NotFound => t!("banner.journalFailureReasons.not_found"),
        JournalFailureCode::Network => t!("banner.journalFailureReasons.network"),
        JournalFailureCode::Busy => t!("banner.journalFailureReasons.busy"),
        JournalFailureCode::Timeout => t!("banner.journalFailureReasons.timeout"),
        JournalFailureCode::Backlog => t!("banner.journalFailureReasons.backlog"),
        JournalFailureCode::Io => t!("banner.journalFailureReasons.io"),
    }
}

/// Текст события `error`/`info` по коду; неизвестный код — английский `message` журнала.
pub fn event_text(code: Option<&str>, message: &str, pair: Pair) -> String {
    feedrows::event_text(code, message, pair)
}

fn encoding_label(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Ascii => t!("feed.encAscii"),
        Encoding::Cp1251 => t!("feed.encCp1251"),
        Encoding::Utf8 => t!("feed.encUtf8"),
        Encoding::Gbk => t!("feed.encGbk"),
        Encoding::Latin1 => t!("feed.encLatin1"),
    }
}

fn view_label(view: FeedView) -> &'static str {
    match view {
        FeedView::Text => t!("feed.viewText"),
        FeedView::Hex => t!("feed.viewHex"),
    }
}

impl BytetapApp {
    /// Шапка записи: бренд, «Стоп» с таймером, сводка; справа счётчики и меню «…».
    pub(crate) fn recording_title_bar(&mut self, ui: &mut Ui) {
        let Some(rec) = &self.rec else {
            return;
        };
        let stats = rec.stats();
        let summary = rec.summary(&stats);
        // У прокси сводка — из кусков: адреса моноширинным, подписи сторон muted (M4-4).
        let summary_job = rec.summary_job(&self.pal(), &stats);
        let elapsed = rec.elapsed().as_secs();
        let p = self.pal();
        let counters_width = counters_width(ui, &p, &stats);
        self.title_bar(
            ui,
            counters_width + SPACE_2 + widgets::ICON_BUTTON_WIDTH,
            |app, ui| {
                app.brand(ui);
                let p = app.pal();
                if stop_pill(ui, &p, &feed::format_duration(elapsed), app.pulse()).clicked() {
                    let ctx = ui.ctx().clone();
                    app.stop_recording(&ctx);
                }
                let width = ui.available_width().max(0.0);
                let job = summary_job.clone();
                // Сводка обрезается «…» по остатку шапки: в узком окне она не должна
                // выталкивать счётчики и кнопки окна за край (находка ревью шага 5).
                let full = ui.painter().layout_job(job.clone());
                let galley = truncated(ui, job, width);
                let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
                response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &summary));
                // Полный текст — во всплывающей подсказке: обрезанная строка его теряет.
                if full.size().x > width {
                    response.on_hover_text(&summary);
                }
                ui.painter().galley(rect.min, galley, p.text_muted);
            },
            |app, ui| {
                let p = app.pal();
                counters(ui, &p, &stats);
                app.recording_menu(ui, &p);
            },
        );
    }

    /// Меню «…» записи: комментарий, папка журналов, путь к журналу, тема.
    fn recording_menu(&mut self, ui: &mut Ui, p: &Palette) {
        let button = widgets::icon_button(ui, p, Icon::Ellipsis, 16.0, t!("record.menu"));
        let mut note = false;
        let mut folder = false;
        let mut copy = false;
        let mut theme = false;
        let journal = self.rec.as_ref().and_then(|rec| rec.journal_path.clone());
        let theme_text = if self.dark() {
            t!("theme.toLight")
        } else {
            t!("theme.toDark")
        };
        Popup::menu(&button)
            .id(Id::unique("bt-recording-menu"))
            .align(RectAlign::BOTTOM_END)
            .width(232.0)
            .gap(4.0)
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                note =
                    widgets::menu_item(ui, p, t!("record.addNote"), self.live.is_some()).clicked();
                folder = widgets::menu_item(ui, p, t!("header.openFolder"), true).clicked();
                copy =
                    widgets::menu_item(ui, p, t!("record.copyPath"), journal.is_some()).clicked();
                widgets::menu_separator(ui, p);
                theme = widgets::menu_item(ui, p, theme_text, true).clicked();
            });
        let ctx = ui.ctx().clone();
        if note {
            // Цель задаётся при открытии: комментарий в сессию, а не в просмотр.
            self.open_note(NoteTarget::Session);
        }
        if folder {
            self.open_log_dir(&ctx);
        }
        if copy && let Some(path) = journal {
            ctx.copy_text(path.display().to_string());
        }
        if theme {
            self.toggle_theme(&ctx);
        }
    }

    /// Отказ журнала: строка причины цветом `danger` и два текстовых действия.
    pub(crate) fn failure_notice(&mut self, ui: &mut Ui) {
        let Some(failure) = &self.journal_failure else {
            return;
        };
        let p = self.pal();
        let text = t!(
            "banner.journalFailureTitleWith",
            reason = failure_reason(failure.code)
        );
        let recorded = self.recorded_path.clone();
        let mut choose = false;
        let mut open = false;
        Frame::new()
            .inner_margin(egui::Margin::symmetric(SPACE_3 as i8, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_2;
                    let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
                    icons::paint(ui, Icon::CircleAlert, rect.center(), 15.0, p.danger);
                    let label = ui.label(RichText::new(&text).size(TEXT_XS).color(p.danger));
                    label.widget_info(|| WidgetInfo::labeled(Role::Alert, true, &text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = SPACE_2;
                        // Справа налево: «Открыть записанное» крайняя справа.
                        open = text_action(ui, &p, t!("banner.openRecorded"), recorded.is_some())
                            .clicked();
                        choose = text_action(ui, &p, t!("banner.chooseOtherDir"), true).clicked();
                    });
                });
            });
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            line.x_range(),
            line.center().y,
            Stroke::new(1.0, p.border_faint),
        );
        let ctx = ui.ctx().clone();
        if choose {
            self.pick_log_dir(&ctx);
        }
        if open && let Some(path) = recorded {
            self.dismiss_failure();
            self.open_journal(&ctx, path);
        }
    }

    /// Тело экрана записи под шапкой: трасса, панель ленты, лента.
    /// Экран записи: шапка, трасса, панель и лента. Возвращает, сколько событий пришло
    /// в ленту за этот кадр: без новых окно не просит следующий кадр так часто.
    pub(crate) fn recording_screen(&mut self, ui: &mut Ui) -> u64 {
        let p = self.pal();
        let hex = self.settings.feed.view == FeedView::Hex;
        let encoding = self.settings.feed.encoding;
        let Some(rec) = &mut self.rec else {
            return 0;
        };
        let added = rec.sync(hex);
        let now_us = u64::try_from(rec.elapsed().as_micros()).unwrap_or(u64::MAX);
        trace::show(ui, &p, &rec.feed, now_us);
        let mut view = self.settings.feed.view;
        let mut chosen_encoding = encoding;
        feed_bar(ui, &p, rec, &mut view, &mut chosen_encoding);
        feed_list(ui, &p, rec, hex, encoding, added);
        if view != self.settings.feed.view || chosen_encoding != encoding {
            let feed = &mut self.settings_mut().feed;
            feed.view = view;
            feed.encoding = chosen_encoding;
            self.save_settings();
        }
        added
    }

    /// Комментарий во время записи и в просмотре журнала (FR-23): Ctrl+Enter или
    /// «Добавить», Escape — отмена.
    ///
    /// Цель берётся из диалога: комментарий к сессии и комментарий к журналу — разные
    /// записи, и флаг «куда» не переживает закрытие диалога (U1-AC17, U1-AC19).
    pub(crate) fn note_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.note.as_ref() else {
            return;
        };
        let journal = matches!(dialog.target, NoteTarget::Journal(_));
        let mut text = dialog.text.clone();
        let error = dialog.error.clone();
        let sending = dialog.sending;
        let p = self.pal();
        let mut confirm = false;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::unique("bt-note-prompt"))
            .frame(
                Frame::new()
                    .fill(p.surface_0)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(radius(RADIUS_PANEL))
                    .inner_margin(SPACE_4),
            )
            .backdrop_color(p.text.gamma_multiply(0.35))
            .show(ctx, |ui| {
                ui.set_width(388.0);
                ui.spacing_mut().item_spacing.y = SPACE_3;
                ui.label(
                    RichText::new(if journal {
                        t!("journal.noteTitle")
                    } else {
                        t!("actions.noteTitle")
                    })
                    .font(theme::sans_semibold(TEXT_BASE))
                    .color(p.text),
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = SPACE_1;
                    let label = widgets::field_label(
                        ui,
                        &p,
                        if journal {
                            t!("journal.noteLabel")
                        } else {
                            t!("actions.note")
                        },
                    );
                    let id = Id::unique("bt-note-text");
                    let focused = ui.memory(|m| m.has_focus(id));
                    let response = ui
                        .add(
                            TextEdit::multiline(&mut text)
                                .id(id)
                                .font(theme::sans(TEXT_XS + 1.0))
                                .text_color(p.text)
                                .hint_text(
                                    RichText::new(if journal {
                                        t!("journal.notePlaceholder")
                                    } else {
                                        t!("actions.notePlaceholder")
                                    })
                                    .color(p.text_faint),
                                )
                                .frame(
                                    Frame::new()
                                        .fill(p.surface_0)
                                        .stroke(Stroke::new(
                                            1.0,
                                            if focused { p.ctrl } else { p.border },
                                        ))
                                        .corner_radius(radius(crate::theme::tokens::RADIUS_CONTROL))
                                        .inner_margin(egui::Margin::symmetric(9, 8)),
                                )
                                .desired_width(f32::INFINITY)
                                .desired_rows(4),
                        )
                        .labelled_by(label.id);
                    if ui.memory(|m| m.focused().is_none()) {
                        response.request_focus();
                    }
                    if ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)) {
                        confirm = true;
                    }
                });
                // Ошибка дописывания — строкой в диалоге: диалог не закрывается молча.
                if let Some(error) = &error {
                    let label = ui.label(RichText::new(error).size(TEXT_XS).color(p.danger));
                    label.widget_info(|| WidgetInfo::labeled(Role::Alert, true, error));
                }
                let filled = !text.trim().is_empty() && !sending;
                let add_label = if sending {
                    t!("actions.saving")
                } else if journal {
                    t!("journal.noteAdd")
                } else {
                    t!("actions.noteAdd")
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_2;
                    let buttons = widgets::confirm_button_width(ui, add_label)
                        + SPACE_2
                        + widgets::chip_button_width(ui, false, t!("actions.cancel"));
                    ui.add_space((ui.available_width() - buttons - SPACE_2).max(0.0));
                    confirm |= widgets::confirm_button(ui, &p, add_label, filled).clicked();
                    cancel =
                        widgets::chip_button(ui, &p, None, t!("actions.cancel"), true).clicked();
                });
            });
        if modal.should_close() {
            cancel = true;
        }
        if confirm && !text.trim().is_empty() && !sending {
            let target = self.note.as_ref().map(|note| note.target.clone());
            match target {
                Some(NoteTarget::Journal(path)) => {
                    // Файл и сводка — в фоне; диалог закроется, когда комментарий ляжет.
                    // Ответ придёт с номером этой отправки: диалог, открытый заново,
                    // он не тронет.
                    self.send_journal_note(ctx, path, &text);
                }
                Some(NoteTarget::Session) => {
                    self.add_note(&text);
                    self.note = None;
                }
                None => {}
            }
        } else if cancel {
            // Отмена: цель комментария уходит вместе с диалогом.
            self.note = None;
        } else if let Some(note) = &mut self.note {
            note.text = text;
        }
    }
}

/// Текстовое действие строки отказа: `danger`, подчёркивание при наведении.
fn text_action(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> egui::Response {
    let color = if enabled {
        p.danger
    } else {
        mix(p.surface_0, p.danger, 0.45)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::sans_medium(TEXT_XS), color);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(8.0, 6.0), sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    let hovered = enabled && response.hovered();
    let text_rect = Rect::from_center_size(rect.center(), galley.size());
    widgets::focus_ring(ui, p, &response, rect, radius(RADIUS_CHIP));
    ui.painter().galley(text_rect.min, galley, color);
    if hovered {
        ui.painter().hline(
            text_rect.x_range(),
            text_rect.bottom(),
            Stroke::new(1.0, color),
        );
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Пилюля «Стоп»: точка, слово и таймер, 28px.
///
/// `pulse` — рисовать ли пульс точки: на программном растре кадр идёт реже, чем за
/// [`PULSE_MAX_PERIOD`], и кольцо пульса скакало бы по случайной фазе — тогда точка
/// стоит статично (отступление от UI v2, `docs/dev/design.md`).
fn stop_pill(ui: &mut Ui, p: &Palette, timer: &str, pulse: bool) -> egui::Response {
    let label = ui.painter().layout_no_wrap(
        t!("record.stop").to_owned(),
        theme::sans_semibold(TEXT_XS),
        p.rec_fg,
    );
    let time = ui
        .painter()
        .layout_no_wrap(timer.to_owned(), theme::mono_medium(TEXT_XS), p.rec_fg);
    let width = 8.0 + 9.0 + 7.0 + label.size().x + 7.0 + time.size().x + 10.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
    let name = format!("{} {timer}", t!("record.stop"));
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    let corner = radius(RADIUS_RECORD.min(127.0));
    let fill = if response.hovered() {
        mix(p.rec_bg, p.rec_fg, 0.12)
    } else {
        p.rec_bg
    };
    ui.painter().rect_filled(rect, corner, fill);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            corner,
            Stroke::new(1.0, p.ctrl),
            egui::StrokeKind::Outside,
        );
    }
    let center = pos2(rect.left() + 8.0 + 4.5, rect.center().y);
    // Пульс как `bt-pulse`: кольцо растёт до 7px и гаснет за 70% периода. На редком
    // кадре (программный растр) фаза успевает перескочить, поэтому пульса нет.
    let phase = (ui.input(|i| i.time) % PULSE_PERIOD / PULSE_PERIOD) as f32;
    let grow = (phase / 0.7).min(1.0);
    if pulse && grow < 1.0 {
        ui.painter().circle_filled(
            center,
            4.5 + 7.0 * grow,
            p.rec.gamma_multiply(0.6 * (1.0 - grow)),
        );
    }
    ui.painter().circle_filled(center, 4.5, p.rec);
    let x = rect.left() + 8.0 + 9.0 + 7.0;
    let lw = label.size().x;
    ui.painter().galley(
        pos2(x, rect.center().y - label.size().y / 2.0),
        label,
        p.rec_fg,
    );
    ui.painter().galley(
        pos2(x + lw + 7.0, rect.center().y - time.size().y / 2.0),
        time,
        p.rec_fg,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Счётчики «→ 629 Б ← 195 Б 2 соед.»: стрелки цветом направления, моно 12px.
fn counters(ui: &mut Ui, p: &Palette, stats: &Stats) {
    let (galley, text) = counters_galley(ui, p, stats);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
    ui.painter().galley(rect.min, galley, p.text);
}

/// Ширина счётчиков: правую группу шапки ставят от правого края на неё.
fn counters_width(ui: &Ui, p: &Palette, stats: &Stats) -> f32 {
    counters_galley(ui, p, stats).0.size().x
}

/// Готовый текст и галлея счётчиков — одна и та же раскладка для меры и рисунка.
fn counters_galley(ui: &Ui, p: &Palette, stats: &Stats) -> (std::sync::Arc<egui::Galley>, String) {
    let font = theme::mono(TEXT_XS);
    let mut job = LayoutJob::default();
    job.append("→ ", 0.0, text_format(font.clone(), p.dir_a2l));
    job.append(
        &format_bytes(stats.bytes_analyzer_to_lis),
        0.0,
        text_format(font.clone(), p.text),
    );
    job.append("← ", SPACE_3, text_format(font.clone(), p.dir_l2a));
    job.append(
        &format_bytes(stats.bytes_lis_to_analyzer),
        0.0,
        text_format(font.clone(), p.text),
    );
    job.append(
        &format!("{} {}", stats.connections, t!("record.connShort")),
        SPACE_3,
        text_format(font, p.text_faint),
    );
    let text = format!(
        "→ {} ← {} {} {}",
        format_bytes(stats.bytes_analyzer_to_lis),
        format_bytes(stats.bytes_lis_to_analyzer),
        stats.connections,
        t!("record.connShort")
    );
    (ui.painter().layout_job(job), text)
}

/// Кнопка панели ленты: текст 12px `text-muted` и шеврон, 24px.
fn bar_button(ui: &mut Ui, p: &Palette, text: &str, name: &str) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::sans(TEXT_XS), p.text_muted);
    let (rect, response) = ui.allocate_exact_size(
        vec2(6.0 + galley.size().x + 4.0 + 12.0 + 6.0, 24.0),
        Sense::click(),
    );
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ComboBox, true, name);
        info.current_text_value = Some(text.to_owned());
        info
    });
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.painter().rect_filled(rect, radius(RADIUS_CHIP), p.hover);
    }
    widgets::focus_ring(ui, p, &response, rect, radius(RADIUS_CHIP));
    let color = if hovered { p.text } else { p.text_muted };
    let x = rect.left() + 6.0;
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley.clone(),
        color,
    );
    icons::paint(
        ui,
        Icon::ChevronDown,
        pos2(x + galley.size().x + 4.0 + 6.0, rect.center().y),
        12.0,
        color,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Панель 32px над лентой: вид, кодировка (только текст), пропущенное и фильтр.
fn feed_bar(
    ui: &mut Ui,
    p: &Palette,
    rec: &mut RecordingView,
    view: &mut FeedView,
    encoding: &mut Encoding,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAR_HEIGHT), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, p.border_faint),
    );
    let inner = rect.shrink2(vec2(SPACE_3, 0.0));
    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.spacing_mut().item_spacing.x = SPACE_2;
    let button = bar_button(&mut left, p, view_label(*view), t!("feed.viewGroup"));
    Popup::menu(&button)
        .id(Id::unique("bt-feed-view"))
        .align(RectAlign::BOTTOM_START)
        .gap(4.0)
        .width(160.0)
        .show(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for option in [FeedView::Text, FeedView::Hex] {
                if widgets::menu_item(ui, p, view_label(option), true).clicked() {
                    *view = option;
                }
            }
        });
    if *view == FeedView::Text {
        let button = bar_button(&mut left, p, encoding_label(*encoding), t!("feed.encoding"));
        Popup::menu(&button)
            .id(Id::unique("bt-feed-encoding"))
            .align(RectAlign::BOTTOM_START)
            .gap(4.0)
            .width(160.0)
            .show(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for option in ENCODINGS {
                    if widgets::menu_item(ui, p, encoding_label(option), true).clicked() {
                        *encoding = option;
                    }
                }
            });
    }

    // Правая часть панели — одной полосой слева направо от правого края: Tab идёт по
    // таблеткам соединений в том же порядке, что их видит глаз (U1-AC25).
    let conns = rec.conns();
    let mut chips: Vec<(Option<u32>, String)> = Vec::new();
    if conns.len() > 1 {
        let shown = &conns[conns.len().saturating_sub(feed::CONN_CHIPS)..];
        chips = shown.iter().map(|&c| (Some(c), format!("#{c}"))).collect();
        chips.insert(0, (None, t!("feed.connAll").to_owned()));
    }
    let dropped = (rec.dropped > 0).then(|| t!("feed.dropped", count = rec.dropped));
    let chips_width: f32 = chips
        .iter()
        .map(|(_, text)| conn_chip_width(ui, text))
        .sum::<f32>()
        + SPACE_1 * chips.len().saturating_sub(1) as f32;
    let dropped_width = dropped.as_ref().map_or(0.0, |text| {
        ui.painter()
            .layout_no_wrap(text.clone(), theme::sans(TEXT_2XS), p.text_faint)
            .size()
            .x
    });
    let width = chips_width
        + if dropped.is_some() {
            SPACE_2 + dropped_width
        } else {
            0.0
        };
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(
                pos2(inner.right() - width, inner.top()),
                inner.max,
            ))
            .layout(Layout::left_to_right(Align::Center)),
    );
    right.spacing_mut().item_spacing.x = SPACE_1;
    if let Some(text) = dropped {
        right.label(RichText::new(text).size(TEXT_2XS).color(p.text_faint));
        // Промежуток до таблеток — `SPACE_2`: `item_spacing` уже отсчитал `SPACE_1`.
        right.add_space(SPACE_2 - SPACE_1);
    }
    if !chips.is_empty() {
        let mut chosen = None;
        for (value, text) in chips {
            if conn_chip(&mut right, p, &text, rec.filter == value).clicked() {
                chosen = Some(value);
            }
        }
        if let Some(value) = chosen {
            rec.set_filter(value);
        }
    }
}

/// Ширина таблетки соединения: правую часть панели ставят от правого края на неё.
fn conn_chip_width(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), theme::mono(TEXT_2XS), Color32::PLACEHOLDER)
        .size()
        .x
        + 16.0
}

/// Таблетка соединения: моно 11px, выбранная — фон цвета текста.
fn conn_chip(ui: &mut Ui, p: &Palette, text: &str, on: bool) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), theme::mono(TEXT_2XS), Color32::PLACEHOLDER);
    let (rect, response) = ui.allocate_exact_size(
        vec2(galley.size().x + 16.0, galley.size().y + 4.0),
        Sense::click(),
    );
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, text));
    let (fill, color) = match (on, response.hovered()) {
        (true, _) => (p.text, p.surface_0),
        (false, true) => (p.hover, p.text),
        _ => (Color32::TRANSPARENT, p.text_muted),
    };
    let corner = radius(RADIUS_RECORD.min(127.0));
    ui.painter().rect_filled(rect, corner, fill);
    widgets::focus_ring(ui, p, &response, rect, corner);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Лента: видимые строки через `show_rows`, догон и таблетка «↓ N новых».
fn feed_list(
    ui: &mut Ui,
    p: &Palette,
    rec: &mut RecordingView,
    hex: bool,
    encoding: Encoding,
    added: u64,
) {
    let total = rec.rows.rows();
    let area = ui.available_rect_before_wrap();
    if total == 0 {
        ui.add_space(28.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(
                RichText::new(t!("feed.emptyTitle"))
                    .font(theme::sans_semibold(TEXT_XS))
                    .color(p.text_muted),
            );
            ui.label(
                RichText::new(t!("feed.emptyText"))
                    .size(TEXT_XS)
                    .color(p.text_muted),
            );
        });
        return;
    }
    let row_height = if hex { HEX_ROW_HEIGHT } else { TEXT_ROW_HEIGHT };
    let multi = rec.conns.len() > 1;
    let pair = rec.pair();
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut scroll = ScrollArea::vertical()
        .id_salt("bt-feed")
        .auto_shrink(false)
        .stick_to_bottom(true);
    if rec.jump_bottom {
        // Ниже конца содержимого: область прижмёт прокрутку к последней строке.
        scroll = scroll.vertical_scroll_offset(total as f32 * row_height + area.height());
        rec.jump_bottom = false;
    }
    let output = scroll.show_rows(ui, row_height, total, |ui, range| {
        for row in range {
            let Some((seq, line)) = rec.rows.locate(row) else {
                break;
            };
            let Some(event) = rec.feed.get(seq).cloned() else {
                continue;
            };
            if hex {
                hex_row(ui, p, &event, line, multi, pair, encoding);
            } else {
                text_row(ui, p, &event, multi, pair, encoding);
            }
        }
    });

    // Догон: внизу — следим; прокрутка вверх выключает догон и копит «новые».
    let max = (output.content_size.y - output.inner_rect.height()).max(0.0);
    let slack = if rec.follow {
        4.0 + added as f32 * row_height
    } else {
        4.0
    };
    if output.state.clamped_offset().y >= max - slack {
        rec.follow = true;
        rec.pending = 0;
    } else {
        rec.follow = false;
    }

    if !rec.follow && rec.pending > 0 {
        let text = t!("feed.newRows", count = rec.pending);
        let galley = ui
            .painter()
            .layout_no_wrap(text.clone(), theme::sans(TEXT_XS), p.rec_fg);
        let size = vec2(12.0 + 13.0 + 6.0 + galley.size().x + 12.0, 28.0);
        let pill = Rect::from_min_size(area.right_bottom() - size - vec2(SPACE_4, SPACE_4), size);
        let response = ui.interact(pill, Id::unique("bt-new-rows"), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &text));
        let corner = radius(RADIUS_RECORD.min(127.0));
        let shadow = Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: p.text.gamma_multiply(0.18),
        };
        ui.painter().add(shadow.as_shape(pill, corner));
        ui.painter().rect_filled(
            pill,
            corner,
            if response.hovered() {
                mix(p.rec_bg, p.rec_fg, 0.12)
            } else {
                p.rec_bg
            },
        );
        widgets::focus_ring(ui, p, &response, pill, corner);
        icons::paint(
            ui,
            Icon::ArrowDown,
            pos2(pill.left() + 12.0 + 6.5, pill.center().y),
            13.0,
            p.rec_fg,
        );
        ui.painter().galley(
            pos2(pill.left() + 31.0, pill.center().y - galley.size().y / 2.0),
            galley,
            p.rec_fg,
        );
        if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            rec.jump_bottom = true;
            rec.follow = true;
            rec.pending = 0;
            ui.ctx().request_repaint();
        }
    }
}

/// Итоги остановки в журнал запуска.
pub(crate) fn log_outcome(outcome: &live::StopOutcome) {
    match (&outcome.stats, &outcome.failure, &outcome.error) {
        (Some(stats), _, _) => diag::info(format!(
            "запись остановлена: {}, соединений {}, байт → {} ← {}",
            outcome.journal_path.display(),
            stats.connections,
            stats.bytes_analyzer_to_lis,
            stats.bytes_lis_to_analyzer
        )),
        (_, Some(failure), _) => diag::error(format!(
            "запись прервана отказом журнала {}: {:?}: {}",
            outcome.journal_path.display(),
            failure.code,
            failure.message
        )),
        (_, _, Some(error)) => diag::error(format!(
            "остановка записи {}: {error}",
            outcome.journal_path.display()
        )),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::Dir;

    use super::*;
    use crate::feed::{DataPayload, FeedKind};

    fn data(conn: u32, len: usize) -> Arc<FeedEvent> {
        Arc::new(FeedEvent {
            time: String::new(),
            mono_us: Some(0),
            conn: Some(conn),
            kind: FeedKind::Data(DataPayload::new(Dir::AnalyzerToLis, false, vec![b'x'; len])),
        })
    }

    fn note() -> Arc<FeedEvent> {
        Arc::new(FeedEvent {
            time: String::new(),
            mono_us: None,
            conn: None,
            kind: FeedKind::Note {
                text: String::new(),
            },
        })
    }

    #[test]
    fn the_hex_index_maps_rows_to_lines_of_events() {
        let mut ring = FeedRing::new(3);
        let mut index = RowIndex::new(true, None);
        for (seq, event) in [data(1, 20), data(1, 0), data(1, 40)]
            .into_iter()
            .enumerate()
        {
            index.push(seq as u64, &event);
            ring.push(event);
        }
        assert_eq!(index.rows(), 2 + 1 + 3);
        assert_eq!(index.locate(0), Some((0, 0)));
        assert_eq!(index.locate(1), Some((0, 1)));
        assert_eq!(index.locate(2), Some((1, 0)));
        assert_eq!(index.locate(5), Some((2, 2)));
        assert_eq!(index.locate(6), None);
        // Вытеснение первого события снимает его строки спереди.
        let event = data(1, 1);
        index.push(3, &event);
        ring.push(event);
        index.evict_before(ring.first_seq());
        assert_eq!(index.rows(), 1 + 3 + 1);
        assert_eq!(index.locate(0), Some((1, 0)));
        let text = RowIndex::rebuild(&ring, false, None);
        assert_eq!(text.rows(), 3);
        assert_eq!(text.locate(2), Some((3, 0)));
    }

    #[test]
    fn the_filter_keeps_one_connection_and_the_session_events() {
        let mut ring = FeedRing::default();
        for event in [data(1, 1), data(2, 1), note(), data(1, 1)] {
            ring.push(event);
        }
        let index = RowIndex::rebuild(&ring, false, Some(1));
        assert_eq!(index.rows(), 3);
        let seqs: Vec<u64> = (0..3).map(|row| index.locate(row).unwrap().0).collect();
        assert_eq!(seqs, [0, 2, 3]);
    }

    #[test]
    fn the_view_takes_only_new_events_from_the_shared_feed() {
        let shared = SharedFeed::default();
        let mut view = RecordingView::new(
            Arc::clone(&shared),
            AppConfig::with_log_dir(""),
            String::new(),
        );
        live::lock(&shared).push(data(1, 1));
        assert_eq!(view.sync(false), 1);
        assert_eq!(view.sync(false), 0, "второй раз нового нет");
        {
            let mut ring = live::lock(&shared);
            ring.push(data(2, 1));
            ring.lagged(3);
        }
        assert_eq!(view.sync(false), 1);
        assert_eq!(view.dropped, 3);
        assert_eq!(view.conns(), [1, 2]);
        view.set_filter(Some(2));
        assert_eq!(view.rows(), 1);
        // Прокрутка вверх: новые события копятся для «↓ N новых».
        view.follow = false;
        live::lock(&shared).push(data(2, 1));
        view.sync(false);
        assert_eq!(view.pending(), 1);
    }

    #[test]
    fn a_view_far_behind_restarts_from_the_shared_feed() {
        let shared = Arc::new(std::sync::Mutex::new(FeedRing::new(4)));
        let mut view = RecordingView::new(
            Arc::clone(&shared),
            AppConfig::with_log_dir(""),
            String::new(),
        );
        for _ in 0..10 {
            live::lock(&shared).push(data(1, 1));
        }
        assert_eq!(view.sync(false), 4);
        assert_eq!(view.rows(), 4);
        assert_eq!(view.feed.first_seq(), 6);
    }

    #[test]
    fn bytes_are_shown_with_units() {
        assert_eq!(format_bytes(0), "0\u{a0}Б");
        assert_eq!(format_bytes(629), "629\u{a0}Б");
        assert_eq!(format_bytes(10_445), "10,2\u{a0}КБ");
        assert_eq!(format_bytes(2048), "2\u{a0}КБ");
        assert_eq!(format_bytes(1_258_291), "1,2\u{a0}МБ");
        assert_eq!(format_bytes(3_650_722_202), "3,4\u{a0}ГБ");
    }

    #[test]
    fn the_summary_follows_the_mode() {
        let mut cfg = AppConfig::with_log_dir("");
        cfg.listen_port = 0;
        let mut view = RecordingView::detached(cfg.clone());
        view.local_addr = "127.0.0.1:50123".into();
        let stats = Stats::default();
        assert_eq!(view.summary(&stats), "жду на :50123 · авто");
        live::lock(&view.shared).push(data(1, 0));
        view.detected = Some(Detected::Hl7);
        let connected = Stats {
            connections: 1,
            ..Stats::default()
        };
        assert_eq!(view.summary(&connected), "подключено · :50123 · HL7 (авто)");
        cfg.mode = SessionMode::Serial;
        cfg.serial.port = "COM5".into();
        assert_eq!(
            RecordingView::detached(cfg.clone()).summary(&stats),
            "COM5 · 9600 8N1"
        );
        cfg.mode = SessionMode::TcpClient;
        cfg.responder = Responder::Astm;
        assert_eq!(
            RecordingView::detached(cfg).summary(&stats),
            "подключено к 192.168.1.50:5000 · ASTM"
        );
    }

    #[test]
    fn codes_have_texts() {
        assert_eq!(
            event_text(Some("connect_failed"), "x", Pair::None),
            "Не удалось подключиться к анализатору — повторяю."
        );
        assert_eq!(event_text(Some("unknown"), "raw", Pair::None), "raw");
        assert_eq!(event_text(None, "raw", Pair::None), "raw");
        // Недоступная цель прокси названа второй стороной пары (M4-AC6).
        assert_eq!(
            event_text(Some("proxy_connect_failed"), "x", Pair::TargetLis),
            "Не удалось подключиться к ЛИС — соединение закрыто, жду следующее."
        );
        assert_eq!(
            event_text(Some("proxy_connect_timeout"), "x", Pair::TargetAnalyzer),
            "Анализатор не ответил за отведённое время — соединение закрыто, жду следующее."
        );
        assert_eq!(
            failure_reason(JournalFailureCode::DiskFull),
            "на диске нет места"
        );
    }

    /// Сводка прокси: до первого соединения — порт и цель, после — обе стороны пары.
    ///
    /// Порт отрезается у принятой стороны — она пришла сама, и порт у неё случайный;
    /// порт цели остаётся: его набирают в настройках второй стороны (M4-4).
    #[test]
    fn the_proxy_summary_shows_both_sides() {
        let mut cfg = AppConfig::with_log_dir("C:/logs");
        cfg.mode = SessionMode::Proxy;
        cfg.proxy_host = "10.0.0.5".into();
        cfg.proxy_port = 5000;
        let mut view = RecordingView::detached(cfg.clone());
        view.local_addr = "0.0.0.0:50025".into();
        let stats = Stats::default();
        assert_eq!(view.summary(&stats), "bytetap :50025 ⇄ ЛИС 10.0.0.5:5000");

        let link = |role, peer: &str, local: &str| {
            Some(bytetap_core::journal::Link {
                role,
                peer: peer.into(),
                local: local.into(),
            })
        };
        let open = |peer: &str, link| FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: Some(0),
            conn: Some(1),
            kind: FeedKind::Open {
                peer: peer.into(),
                link,
            },
        };
        use bytetap_core::journal::LinkRole;

        // «Анализатор — клиент»: пришёл анализатор, порт его соединения не показывается.
        view.remember_sides(&open(
            "192.168.1.51:49152",
            link(LinkRole::Lis, "10.0.0.5:5000", "192.168.1.10:50231"),
        ));
        assert_eq!(
            view.summary(&stats),
            "анализатор 192.168.1.51 ⇄ bytetap :50025 ⇄ ЛИС 10.0.0.5:5000"
        );
        assert_eq!(
            view.outgoing_local(),
            Some("192.168.1.10:50231"),
            "адрес, с которого вторая сторона видела bytetap (FR-8)"
        );

        // Топология «анализатор — сервер»: ЛИС приходит сама, цель — анализатор.
        cfg.proxy_role = ProxyMode::AnalyzerServer;
        cfg.proxy_host = "192.168.1.51".into();
        let mut view = RecordingView::detached(cfg);
        view.local_addr = "0.0.0.0:50025".into();
        assert_eq!(
            view.summary(&stats),
            "анализатор 192.168.1.51:5000 ⇄ bytetap :50025",
            "порт цели остаётся, пока пары нет"
        );

        // Пара есть: порт отрезается теперь у ЛИС, порт анализатора-цели остаётся.
        view.remember_sides(&open(
            "10.0.1.20:51234",
            link(
                LinkRole::Analyzer,
                "192.168.1.51:5000",
                "192.168.1.10:50231",
            ),
        ));
        assert_eq!(
            view.summary(&stats),
            "анализатор 192.168.1.51:5000 ⇄ bytetap :50025 ⇄ ЛИС 10.0.1.20"
        );
    }
}
