//! Строки ленты: вид «Текст» и «HEX» — общие для записи и просмотра журнала.
//!
//! Отрисовка строки одна и та же при записи и при просмотре: та же колонка времени,
//! тот же глиф стрелки, тот же `‹CR›` цветом `ctrl`, тот же контекстный меню строки.
//! Различается только источник событий, поэтому рисование и вынесено отдельно от
//! экранов.

use std::sync::Arc;

use bytetap_core::journal::{ClosedBy, Dir, LinkRole};
use eframe::egui::{
    self, Align, Align2, Color32, Rect, Role, Sense, Stroke, TextFormat, Ui, WidgetInfo, pos2,
    text::LayoutJob, text::TextWrapping, vec2,
};

use crate::feed::{self, DataPayload, FeedEvent, FeedKind, HexLine, Segment, Segments};
use crate::selection::MenuAction;
use crate::settings::{Encoding, ProxyMode, SessionMode};
use crate::theme::tokens::{SPACE_2, SPACE_3, TEXT_2XS, TEXT_DATA, TEXT_XS};
use crate::theme::{self, Palette};
use crate::{t, widgets};

/// Высота строки ленты в виде «Текст».
pub const TEXT_ROW_HEIGHT: f32 = 28.0;
/// Высота строки HEX-дампа.
pub const HEX_ROW_HEIGHT: f32 = 25.0;
/// Колонка времени.
pub const TIME_WIDTH: f32 = 94.0;
/// Колонка глифа направления.
pub const ARROW_WIDTH: f32 = 12.0;
/// Колонка номера соединения.
pub const CONN_WIDTH: f32 = 24.0;

/// Вид строки: цвет текста и шрифт служебных строк — как классы `svc`, `ev`, `err`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowClass {
    /// Фрагмент обмена: цвет направления.
    Data(Dir),
    /// Служебная строка `open`/`close`.
    Service,
    /// Событие `info` или комментарий.
    Event,
    /// Замечание, о котором стоит знать: цвет `warn`, а не спокойный `muted`.
    Warning,
    /// Ошибка.
    Error,
}

/// Какая пара сторон у соединения в ленте.
///
/// Прокси — единственный режим, где соединение это пара: `open` говорит, кто с кем
/// соединён, а `close` — кто закрыл (FR-5, FR-7, M4-В5). У остальных режимов пары нет и
/// строки остаются прежними.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pair {
    /// Не пара: обычное соединение анализатора с bytetap.
    #[default]
    None,
    /// Цель прокси — ЛИС (топология «анализатор — клиент»).
    TargetLis,
    /// Цель прокси — анализатор (топология «анализатор — сервер»).
    TargetAnalyzer,
    /// Прослушка двух портов: стороны — физические роли портов, а не топология (M5-В4).
    Monitor,
}

impl Pair {
    /// Пара по режиму и топологии настроек сценария.
    pub fn of(mode: SessionMode, role: ProxyMode) -> Self {
        match mode {
            SessionMode::Proxy => match role {
                ProxyMode::AnalyzerClient => Self::TargetLis,
                ProxyMode::AnalyzerServer => Self::TargetAnalyzer,
            },
            SessionMode::ComMonitor => Self::Monitor,
            SessionMode::TcpServer | SessionMode::TcpClient | SessionMode::Serial => Self::None,
        }
    }

    /// Соединение — пара двух сторон.
    pub fn is_pair(self) -> bool {
        self != Self::None
    }

    /// Кто на дальнем конце пары: цель, к которой bytetap подключился.
    fn target(self) -> &'static str {
        match self {
            Self::TargetLis => t!("feed.byLis"),
            Self::TargetAnalyzer => t!("feed.byAnalyzer"),
            Self::None | Self::Monitor => "",
        }
    }

    /// Кто пришёл на порт bytetap: сторона, противоположная цели.
    fn accepted(self) -> &'static str {
        match self {
            Self::TargetLis => t!("feed.byAnalyzer"),
            Self::TargetAnalyzer => t!("feed.byLis"),
            Self::None | Self::Monitor => "",
        }
    }
}

/// Текст служебной строки (у фрагмента текст идёт сегментами).
pub fn row_text(event: &FeedEvent, pair: Pair) -> (RowClass, String) {
    let conn = event.conn.unwrap_or_default();
    match &event.kind {
        FeedKind::Data(data) => (RowClass::Data(data.dir), String::new()),
        FeedKind::Open { peer, link } => {
            if pair == Pair::Monitor {
                // У прослушки обе стороны — порты: принятая сторона это порт анализатора,
                // `link` — порт ЛИС (M5-В8).
                let lis = link.as_ref().map_or("", |link| link.peer.as_str());
                (
                    RowClass::Service,
                    t!(
                        "feed.openedMonitor",
                        conn = conn,
                        analyzer = peer,
                        lis = lis
                    ),
                )
            } else if pair.is_pair() {
                // Направление пары — по роли исходящей стороны: роль стороны в журнале
                // не зависит от того, кто инициировал соединение (FR-5).
                let (from, to) = match link.as_ref().map(|link| link.role) {
                    Some(LinkRole::Lis) => (t!("feed.byAnalyzer"), t!("feed.byLis")),
                    Some(LinkRole::Analyzer) => (t!("feed.byLis"), t!("feed.byAnalyzer")),
                    None => (pair.accepted(), pair.target()),
                };
                (
                    RowClass::Service,
                    t!("feed.openedPair", conn = conn, from = from, to = to),
                )
            } else {
                (
                    RowClass::Service,
                    t!("feed.opened", conn = conn, peer = peer),
                )
            }
        }
        FeedKind::Close { by, reason } => {
            let text = if pair == Pair::Monitor {
                // У порта нет соединения, которое кто-то закрывает: пара закрывается,
                // когда пропал порт одной стороны или запись остановили (M5-В8).
                let what = match reason.as_str() {
                    "stop" => t!("feed.closedMonitorStop").to_owned(),
                    "error" => t!("feed.closedMonitorLost").to_owned(),
                    "eof" => t!("feed.closedMonitorEnd").to_owned(),
                    _ => t!("feed.closedMonitorOther", reason = reason.as_str()),
                };
                t!(
                    "feed.closedMonitor",
                    conn = conn,
                    by = closed_by(*by),
                    what = what
                )
            } else if pair.is_pair() {
                match by {
                    ClosedBy::Analyzer => t!("feed.closedPairAnalyzer", conn = conn),
                    ClosedBy::Lis => t!("feed.closedPairLis", conn = conn),
                    // Причина видна, когда закрыл сам bytetap: срок ожидания и сброс —
                    // не его решение, а то, что сделала вторая сторона.
                    ClosedBy::Bytetap => match reason.as_str() {
                        // Срок дожития берётся из ядра: второй копии числа в
                        // интерфейсе быть не должно (FR-7).
                        "timeout" => t!(
                            "feed.closedPairAppTimeout",
                            conn = conn,
                            seconds = bytetap_core::session::PROXY_LINGER.as_secs()
                        ),
                        "reset" => t!("feed.closedPairAppReset", conn = conn),
                        _ => t!("feed.closedPairApp", conn = conn),
                    },
                }
            } else {
                t!("feed.closedSimple", conn = conn, by = closed_by(*by))
            };
            (RowClass::Service, text)
        }
        FeedKind::Error {
            code,
            peer,
            message,
        } => {
            // Повтор открытия портов в прослушке — состояние, а не новая ошибка обмена:
            // пропавший порт назван строкой выше, и дальше запись идёт (M5-В8).
            let class = if pair == Pair::Monitor && code.as_deref() == Some("serial_open_failed") {
                RowClass::Event
            } else {
                RowClass::Error
            };
            (
                class,
                event_text(code.as_deref(), message, pair, None, peer.as_deref()),
            )
        }
        FeedKind::Info {
            code,
            message,
            repeats,
        } => {
            // Переполнение очереди писателя — не ошибка обмена, но и не спокойная строка:
            // инженеру стоит знать, что журнал отстаёт (M4-AC12).
            let class = if matches!(
                code.as_deref(),
                Some("journal_backlog_warning" | "control_flow_not_forwarded")
            ) {
                RowClass::Warning
            } else {
                RowClass::Event
            };
            (
                class,
                event_text(code.as_deref(), message, pair, *repeats, None),
            )
        }
        FeedKind::Note { text } => (RowClass::Event, text.clone()),
    }
}

/// Кто закрыл соединение: анализатор, ЛИС или приложение.
pub fn closed_by(by: ClosedBy) -> &'static str {
    match by {
        ClosedBy::Analyzer => t!("feed.byAnalyzer"),
        ClosedBy::Lis => t!("feed.byLis"),
        ClosedBy::Bytetap => t!("feed.byApp"),
    }
}

/// Текст события `error`/`info` по коду; неизвестный код — английский `message` журнала.
///
/// `repeats` — число схлопнутых повторов из поля `info`, `peer` — адрес отвергнутой
/// стороны из поля `error` (оба — формат `/2`); свои фразы с ними есть у замечаний о
/// серии ошибок источника и у отказа по фильтру, остальные их не читают.
pub fn event_text(
    code: Option<&str>,
    message: &str,
    pair: Pair,
    repeats: Option<u32>,
    peer: Option<&str>,
) -> String {
    if let Some(text) = repeating_text(code, repeats) {
        return text;
    }
    // Прослушка: сторона ошибки — порт, и строка называет именно его (M5-В8).
    if pair == Pair::Monitor
        && let Some(text) = monitor_text(code, peer)
    {
        return text;
    }
    // Адрес отвергнутого — из поля события, а не из английского `message` (FR-8).
    if code == Some("proxy_peer_rejected")
        && let Some(addr) = peer
    {
        return t!("events.proxy_peer_rejected_addr", addr = addr);
    }
    let text = match code {
        Some("accept_failed") => t!("events.accept_failed"),
        Some("connect_failed") => t!("events.connect_failed"),
        Some("serial_open_failed") => t!("events.serial_open_failed"),
        Some("connection_read_failed") => t!("events.connection_read_failed"),
        Some("connection_write_failed") => t!("events.connection_write_failed"),
        Some("connection_task_failed") => t!("events.connection_task_failed"),
        Some("answer_timeout") => t!("events.answer_timeout"),
        Some("journal_failure") => t!("events.journal_failure"),
        // Отказ журнала по переполнению очереди: в ленте та же фраза, что у остальных
        // причин, а подробная причина — в строке под шапкой (M4-AC12).
        Some("journal_backlog") => t!("events.journal_failure"),
        Some("protocol_undetected") => t!("events.protocol_undetected"),
        Some("proxy_connect_failed") => proxy_failure_text(pair, true),
        Some("proxy_connect_timeout") => proxy_failure_text(pair, false),
        Some("proxy_target_is_self") => t!("events.proxy_target_is_self"),
        Some("proxy_peer_rejected") => t!("events.proxy_peer_rejected"),
        Some("journal_backlog_warning") => t!("events.journal_backlog_warning"),
        _ => message,
    };
    text.to_owned()
}

/// Строка ошибки или замечания прослушки: пропавший порт, повтор открытия, замечания
/// источника о таймере и линиях управления (M5-В3, M5-В8).
///
/// Имя пропавшего порта берётся из поля `peer` события: ядро пишет туда порт отказавшей
/// стороны, и строка называет его, не разбирая английский `message`. Журнал без поля
/// (старая запись) говорит «порт пропал» без имени.
fn monitor_text(code: Option<&str>, peer: Option<&str>) -> Option<String> {
    match code {
        Some("connection_read_failed" | "connection_write_failed") => Some(match peer {
            Some(port) => t!("events.monitor_port_lost", port = port),
            None => t!("events.monitor_port_lost_unnamed").to_owned(),
        }),
        // Повтор открытия идёт с паузой из настроек сценария: числа в тексте нет, потому
        // что в журнале пауза не записана.
        Some("serial_open_failed") => Some(t!("events.serial_open_failed_monitor").to_owned()),
        Some("timer_resolution") => Some(t!("events.timer_resolution").to_owned()),
        Some("control_flow_not_forwarded") => {
            Some(t!("events.control_flow_not_forwarded").to_owned())
        }
        _ => None,
    }
}

/// Фраза о серии ошибок источника с числом повторов: своя у каждого из трёх кодов.
///
/// Число в каталоге стоит в конце фразы — «повторов подряд было: 2», — чтобы русское
/// склонение не зависело от количества.
fn repeating_text(code: Option<&str>, repeats: Option<u32>) -> Option<String> {
    let repeats = repeats?;
    Some(match code {
        Some("source_retry_repeats") => t!("events.source_retry_repeats", repeats = repeats),
        Some("source_recovered") => t!("events.source_recovered", repeats = repeats),
        Some("source_failing_at_stop") => t!("events.source_failing_at_stop", repeats = repeats),
        _ => return None,
    })
}

/// Недоступная цель прокси: в строке названа вторая сторона — ЛИС или анализатор
/// (M4-AC6). У ЛИС название не склоняется, у анализатора — склоняется, поэтому для
/// каждой стороны своя фраза каталога.
fn proxy_failure_text(pair: Pair, refused: bool) -> &'static str {
    match (pair, refused) {
        (Pair::TargetLis, true) => t!("events.proxy_connect_failed_lis"),
        (Pair::TargetAnalyzer, true) => t!("events.proxy_connect_failed_analyzer"),
        (Pair::TargetLis, false) => t!("events.proxy_connect_timeout_lis"),
        (Pair::TargetAnalyzer, false) => t!("events.proxy_connect_timeout_analyzer"),
        // У прослушки нет цели прокси: эти коды сюда не приходят, и строка остаётся
        // прежней общей.
        (Pair::None | Pair::Monitor, true) => t!("events.proxy_connect_failed"),
        (Pair::None | Pair::Monitor, false) => t!("events.proxy_connect_timeout"),
    }
}

/// Глиф направления в строке.
pub fn glyph(dir: Dir) -> &'static str {
    match dir {
        Dir::AnalyzerToLis => "→",
        Dir::LisToAnalyzer => "←",
    }
}

/// Цвет направления в строке.
pub fn dir_color(p: &Palette, dir: Dir) -> Color32 {
    match dir {
        Dir::AnalyzerToLis => p.dir_a2l,
        Dir::LisToAnalyzer => p.dir_l2a,
    }
}

/// Формат текста в галлере: шрифт, цвет и выравнивание по середине строки.
pub fn text_format(font: egui::FontId, color: Color32) -> TextFormat {
    TextFormat {
        font_id: font,
        color,
        valign: Align::Center,
        ..Default::default()
    }
}

/// Текст в одну строку, обрезанный «…» по ширине.
pub fn truncated(ui: &Ui, mut job: LayoutJob, width: f32) -> Arc<egui::Galley> {
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    ui.painter().layout_job(job)
}

/// Строка ленты во всю ширину: выделенная — заливкой `selection`, наведение — фоном
/// `hover`, у строки роль элемента списка и признак выбора для AccessKit (L1-AC8).
///
/// Роль `ListItem` вместо `Label` шага 1: у ленты появилось выделение строк, и диктор
/// должен читать «выбрано» у каждой строки, а не только у открытой в панели (Л-В8).
fn row_frame(
    ui: &mut Ui,
    p: &Palette,
    height: f32,
    name: &str,
    selected: bool,
) -> (Rect, egui::Response) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, name));
    // Выбран ли элемент списка — свойство `selected`, а не `toggled`: `WidgetInfo::
    // selected` egui переводит в `toggled`, и диктор читал бы строку как переключатель.
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_selected(selected));
    if selected {
        ui.painter().rect_filled(rect, 0, p.selection);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0, p.hover);
    }
    (rect, response)
}

/// Текст фрагмента одной строкой с глифами `‹CR›` и «автоответ».
fn data_job(p: &Palette, segments: &Segments, synthetic: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let ctrl = p.ctrl.gamma_multiply(0.55);
    for segment in segments.iter() {
        match segment {
            Segment::Text(text) => {
                job.append(text, 0.0, text_format(theme::mono(TEXT_DATA), p.text));
            }
            Segment::Ctrl(byte, count) => {
                job.append(
                    &feed::ctrl_glyph(byte).repeat(count as usize),
                    0.0,
                    text_format(theme::mono(TEXT_2XS), ctrl),
                );
            }
            Segment::Escaped(text) => {
                job.append(
                    &text
                        .chars()
                        .next()
                        .map(feed::escaped_glyph)
                        .unwrap_or_default(),
                    0.0,
                    text_format(theme::mono(TEXT_2XS), ctrl),
                );
            }
        }
    }
    if synthetic {
        job.append(
            t!("feed.synthetic"),
            6.0,
            text_format(theme::sans(TEXT_2XS), p.dir_l2a),
        );
    }
    job
}

/// Строка «Текст»: время · стрелка · №соединения · данные · «автоответ».
///
/// `selected` — событие выделено блоком: строка залита, а в AccessKit у неё признак
/// выбора. `count` — сколько строк выделено: правый клик по выделенной строке даёт
/// меню на всё выделение (L1, шаг 2). Возвращается ответ строки: по клику экран
/// выбирает событие.
#[allow(clippy::too_many_arguments)]
pub fn text_row(
    ui: &mut Ui,
    p: &Palette,
    event: &FeedEvent,
    multi: bool,
    pair: Pair,
    encoding: Encoding,
    selected: bool,
    count: usize,
    action: &mut MenuAction,
) -> egui::Response {
    let (class, text) = row_text(event, pair);
    let segments = event.data().map(|data| data.segments(encoding));
    // Подпись строки для AccessKit — начало фрагмента: повторы глифов целиком не
    // разворачиваются, подпись строится каждый кадр (U1-AC16).
    let body = segments
        .as_deref()
        .map_or_else(|| text.clone(), |s| feed::preview(s, feed::LABEL_CHARS));
    let name = match (class, event.conn) {
        (RowClass::Data(dir), Some(conn)) if multi => {
            format!("{} {} #{conn} {body}", event.time, glyph(dir))
        }
        (RowClass::Data(dir), _) => format!("{} {} {body}", event.time, glyph(dir)),
        _ => format!("{} {body}", event.time),
    };
    let (rect, response) = row_frame(ui, p, TEXT_ROW_HEIGHT, &name, selected);
    let painter = ui.painter().clone();
    let cy = rect.center().y;
    let x0 = rect.left() + SPACE_3;
    let right = rect.right() - SPACE_3;
    let service = class == RowClass::Service;
    let size = if service { TEXT_XS } else { TEXT_DATA };
    painter.text(
        pos2(x0, cy),
        Align2::LEFT_CENTER,
        &event.time,
        theme::mono(size),
        p.text_faint,
    );
    let mut x = x0 + TIME_WIDTH + SPACE_2;
    match (class, event.data()) {
        (RowClass::Data(dir), Some(data)) => {
            painter.text(
                pos2(x, cy),
                Align2::LEFT_CENTER,
                glyph(dir),
                theme::mono_medium(TEXT_DATA),
                dir_color(p, dir),
            );
            x += ARROW_WIDTH + SPACE_2;
            if multi {
                painter.text(
                    pos2(x, cy),
                    Align2::LEFT_CENTER,
                    format!("#{}", event.conn.unwrap_or_default()),
                    theme::mono(TEXT_2XS),
                    p.text_faint,
                );
                x += CONN_WIDTH + SPACE_2;
            }
            let job = data_job(
                p,
                segments.as_deref().unwrap_or(&Segments::EMPTY),
                data.synthetic,
            );
            let galley = truncated(ui, job, right - x);
            painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, p.text);
        }
        _ => {
            let color = match class {
                RowClass::Error => p.danger,
                RowClass::Warning => p.warn,
                _ => p.text_muted,
            };
            let job =
                LayoutJob::single_section(text.clone(), text_format(theme::mono(size), color));
            let galley = truncated(ui, job, right - x);
            let end = x + galley.size().x;
            painter.galley(pos2(x, cy - galley.size().y / 2.0), galley, color);
            // Служебная строка: линия начинается за текстом и уходит до края.
            if service && end + SPACE_2 + 12.0 <= right {
                painter.hline(end + SPACE_2..=right, cy, Stroke::new(1.0, p.border_faint));
            }
        }
    }
    row_menu(
        &response, p, event, &body, encoding, selected, count, action,
    );
    response
}

/// Строка HEX-дампа: время и стрелка только у первой строки события.
///
/// Все строки одного события выбираются вместе: в HEX-виде событие занимает несколько
/// строк, а панель открывает событие, а не строку (L1-AC5).
#[allow(clippy::too_many_arguments)]
pub fn hex_row(
    ui: &mut Ui,
    p: &Palette,
    event: &FeedEvent,
    line: usize,
    multi: bool,
    pair: Pair,
    encoding: Encoding,
    selected: bool,
    count: usize,
    action: &mut MenuAction,
) -> egui::Response {
    let (class, text) = row_text(event, pair);
    let bytes = event.data().map_or(&[][..], |data| data.bytes.as_slice());
    let dump: HexLine = feed::hex_line(bytes, line);
    let hex: Vec<&str> = dump.bytes.iter().map(|(value, _)| value.as_str()).collect();
    let name = format!(
        "{} {} {} {}",
        if line == 0 { event.time.as_str() } else { "" },
        dump.offset,
        hex.join(" "),
        dump.ascii
    );
    let (rect, response) = row_frame(ui, p, HEX_ROW_HEIGHT, name.trim(), selected);
    let painter = ui.painter().clone();
    let font = theme::mono(TEXT_XS);
    let y = rect.top() + 3.0;
    let x0 = rect.left() + SPACE_3;
    let first = line == 0;
    if first {
        painter.text(
            pos2(x0, y),
            Align2::LEFT_TOP,
            &event.time,
            font.clone(),
            p.text_faint,
        );
    }
    let mut x = x0 + TIME_WIDTH + SPACE_2;
    if first && let RowClass::Data(dir) = class {
        painter.text(
            pos2(x, y),
            Align2::LEFT_TOP,
            glyph(dir),
            theme::mono_medium(TEXT_XS),
            dir_color(p, dir),
        );
    }
    x += ARROW_WIDTH + SPACE_2;
    if multi {
        if first && let Some(conn) = event.conn {
            painter.text(
                pos2(x, y),
                Align2::LEFT_TOP,
                format!("#{conn}"),
                font.clone(),
                p.text_faint,
            );
        }
        x += CONN_WIDTH + SPACE_2;
    }
    let (byte_color, ascii_color) = match class {
        RowClass::Data(_) => (p.text, p.text_muted),
        RowClass::Error => (p.danger, p.danger),
        RowClass::Warning => (p.warn, p.warn),
        RowClass::Event | RowClass::Service => (p.text_muted, p.text_muted),
    };
    let mut job = LayoutJob::default();
    job.append(&dump.offset, 0.0, text_format(font.clone(), p.text_faint));
    for (index, (value, control)) in dump.bytes.iter().enumerate() {
        let fmt = if *control && matches!(class, RowClass::Data(_)) {
            text_format(theme::mono_medium(TEXT_XS), p.ctrl)
        } else {
            text_format(font.clone(), byte_color)
        };
        job.append(value, if index == 0 { SPACE_2 } else { 2.0 }, fmt.clone());
        job.append(" ", 0.0, fmt);
    }
    job.append(&dump.ascii, SPACE_2, text_format(font, ascii_color));
    let galley = truncated(ui, job, rect.right() - SPACE_3 - x);
    painter.galley(pos2(x, y), galley, p.text);
    row_menu(
        &response, p, event, &text, encoding, selected, count, action,
    );
    response
}

/// Строка, чья страница ещё едет: заполнитель той же высоты, чтобы лента не сдвигалась.
pub fn pending_row(ui: &mut Ui, p: &Palette, height: f32) {
    let text = t!("journal.rowPending");
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, false, text));
    ui.painter().text(
        pos2(
            rect.left() + SPACE_3 + TIME_WIDTH + SPACE_2,
            rect.center().y,
        ),
        Align2::LEFT_CENTER,
        text,
        theme::mono(TEXT_XS),
        p.text_faint,
    );
}

/// Строка ошибки: страницу не удалось прочитать после всех попыток.
pub fn error_row(ui: &mut Ui, p: &Palette, height: f32) {
    let text = t!("journal.rowUnreadable");
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Alert, false, text));
    ui.painter().text(
        pos2(
            rect.left() + SPACE_3 + TIME_WIDTH + SPACE_2,
            rect.center().y,
        ),
        Align2::LEFT_CENTER,
        text,
        theme::mono(TEXT_XS),
        p.danger,
    );
}

/// Контекстное меню строки: текст, HEX и Base64 фрагмента (U1-AC16).
///
/// Текст для копирования строится по клику и целиком, без предела показа в строке:
/// строка ленты и её подпись для AccessKit показывают начало фрагмента, а скопировать
/// инженеру нужно весь фрагмент (U1-AC16).
///
/// Правый клик по выделенной строке — меню на всё выделение: «Скопировать N строк» и
/// те же строки в HEX и Base64. Действие выполняет экран: у него список событий
/// выделения целиком, а у строки — только она сама (L1, шаг 2).
#[allow(clippy::too_many_arguments)]
fn row_menu(
    response: &egui::Response,
    p: &Palette,
    event: &FeedEvent,
    text: &str,
    encoding: Encoding,
    selected: bool,
    count: usize,
    action: &mut MenuAction,
) {
    response.context_menu(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        if selected && count > 1 {
            let rows = t!("feed.copyRows", count = count);
            if widgets::menu_item(ui, p, &rows, true).clicked() {
                *action = MenuAction::CopyText;
            }
            let hex = t!("feed.copyRowsHex", count = count);
            if widgets::menu_item(ui, p, &hex, true).clicked() {
                *action = MenuAction::CopyHex;
            }
            let base64 = t!("feed.copyRowsBase64", count = count);
            if widgets::menu_item(ui, p, &base64, true).clicked() {
                *action = MenuAction::CopyBase64;
            }
            if widgets::menu_item(ui, p, t!("feed.clearSelection"), true).clicked() {
                *action = MenuAction::Clear;
            }
            return;
        }
        if widgets::menu_item(ui, p, t!("feed.copyText"), true).clicked() {
            let copied = match event.data() {
                Some(data) => data.full_text(encoding),
                None => text.to_owned(),
            };
            ui.ctx().copy_text(copied);
        }
        if let Some(data) = event.data() {
            if widgets::menu_item(ui, p, t!("feed.copyHex"), true).clicked() {
                ui.ctx().copy_text(feed::hex_string(&data.bytes));
            }
            if widgets::menu_item(ui, p, t!("feed.copyBase64"), true).clicked() {
                ui.ctx().copy_text(feed::base64_string(&data.bytes));
            }
        }
    });
}

/// Событие ленты из строки журнала: `None` — заголовок `session` или итоги.
pub fn event_of(event: &bytetap_core::journal::Event) -> Option<FeedEvent> {
    FeedEvent::from_session(event)
}

/// Байты фрагмента для меню копирования.
pub fn payload_bytes(data: &DataPayload) -> &[u8] {
    &data.bytes
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::{Link, LinkRole};

    use super::*;

    fn event(kind: FeedKind) -> FeedEvent {
        FeedEvent {
            time: "10:15:03.000".into(),
            mono_us: Some(0),
            conn: Some(1),
            kind,
        }
    }

    fn link(role: LinkRole) -> Link {
        Link {
            role,
            peer: "10.0.0.5:5000".into(),
            local: "192.168.1.10:50231".into(),
        }
    }

    /// Пара прокси: `open` называет обе стороны по роли исходящего сокета, `close` —
    /// кто закрыл (M4-В5, M4-AC13).
    #[test]
    fn a_pair_row_names_both_sides_and_who_closed() {
        let opened = event(FeedKind::Open {
            peer: "192.168.1.51:49152".into(),
            link: Some(link(LinkRole::Lis)),
        });
        assert_eq!(
            row_text(&opened, Pair::TargetLis).1,
            "#1 открыто: анализатор → ЛИС"
        );

        // Топология «анализатор — сервер»: входящая сторона — ЛИС, цель — анализатор.
        let opened = event(FeedKind::Open {
            peer: "10.0.0.5:5000".into(),
            link: Some(link(LinkRole::Analyzer)),
        });
        assert_eq!(
            row_text(&opened, Pair::TargetAnalyzer).1,
            "#1 открыто: ЛИС → анализатор"
        );

        for (by, reason, text) in [
            (ClosedBy::Lis, "eof", "#1 ЛИС закрыла соединение"),
            (ClosedBy::Analyzer, "eof", "#1 анализатор закрыл соединение"),
            (ClosedBy::Bytetap, "stop", "#1 bytetap закрыл соединение"),
            (
                ClosedBy::Bytetap,
                "timeout",
                "#1 bytetap закрыл соединение: вторая сторона не ответила за 5 с",
            ),
            (
                ClosedBy::Bytetap,
                "reset",
                "#1 bytetap закрыл соединение: сброс другой стороной",
            ),
        ] {
            let close = event(FeedKind::Close {
                by,
                reason: reason.into(),
            });
            assert_eq!(row_text(&close, Pair::TargetLis).1, text);
        }
    }

    /// Остальные режимы говорят как раньше: одно соединение, «отключился · ЛИС».
    #[test]
    fn the_other_modes_keep_their_rows() {
        let opened = event(FeedKind::Open {
            peer: "192.168.1.51:49152".into(),
            link: None,
        });
        assert_eq!(
            row_text(&opened, Pair::None).1,
            "#1 подключился 192.168.1.51:49152"
        );
        let close = event(FeedKind::Close {
            by: ClosedBy::Lis,
            reason: "eof".into(),
        });
        assert_eq!(row_text(&close, Pair::None).1, "#1 отключился · ЛИС");
    }

    /// Строки прослушки: открытие пары портов, закрытие со стороной и причиной, пропажа
    /// порта по имени и замечания источника (M5-В8, M5-В3).
    #[test]
    fn the_monitor_rows_name_the_ports() {
        let opened = event(FeedKind::Open {
            peer: "COM3".into(),
            link: Some(Link {
                role: LinkRole::Lis,
                peer: "COM4".into(),
                local: String::new(),
            }),
        });
        assert_eq!(
            row_text(&opened, Pair::Monitor),
            (
                RowClass::Service,
                "#1 открыто: анализатор COM3 → ЛИС COM4".into()
            )
        );

        // Закрытие: у порта нет соединения, которое кто-то закрывает, — есть сторона и
        // причина (М5-В8).
        for (by, reason, text) in [
            (ClosedBy::Lis, "error", "#1 закрыто: ЛИС, порт пропал"),
            (
                ClosedBy::Analyzer,
                "error",
                "#1 закрыто: анализатор, порт пропал",
            ),
            (ClosedBy::Bytetap, "stop", "#1 закрыто: bytetap, стоп"),
            (
                ClosedBy::Analyzer,
                "eof",
                "#1 закрыто: анализатор, конец данных",
            ),
        ] {
            let close = event(FeedKind::Close {
                by,
                reason: reason.into(),
            });
            assert_eq!(row_text(&close, Pair::Monitor).1, text);
        }

        // Пропавший порт назван в строке ошибки; журнал без поля говорит без имени.
        let lost = |port: Option<&str>| {
            event(FeedKind::Error {
                code: Some("connection_read_failed".into()),
                peer: port.map(str::to_owned),
                message: "monitor read failed: The device is not connected".into(),
            })
        };
        let (class, text) = row_text(&lost(Some("COM4")), Pair::Monitor);
        assert_eq!(class, RowClass::Error);
        assert_eq!(text, "COM4 пропал — жду подключения");
        assert_eq!(
            row_text(&lost(None), Pair::Monitor).1,
            "Порт пропал — жду подключения"
        );

        // Повтор открытия и замечания источника: свои фразы по кодам (M5-AC5, М5-В3).
        let open_failed = event(FeedKind::Error {
            code: Some("serial_open_failed".into()),
            peer: None,
            message: "serial port open failed".into(),
        });
        let (class, text) = row_text(&open_failed, Pair::Monitor);
        assert_eq!(
            class,
            RowClass::Event,
            "повтор — состояние, не ошибка обмена"
        );
        assert!(text.contains("повторяю"), "{text}");

        let timer = event(FeedKind::Info {
            code: Some("timer_resolution".into()),
            message: "the system timer resolution is raised".into(),
            repeats: None,
        });
        let (class, text) = row_text(&timer, Pair::Monitor);
        assert_eq!(class, RowClass::Event);
        assert!(text.contains("таймера"), "{text}");

        let flow = event(FeedKind::Info {
            code: Some("control_flow_not_forwarded".into()),
            message: "control flow rtscts on COM3 does not pass through bytetap".into(),
            repeats: None,
        });
        let (class, text) = row_text(&flow, Pair::Monitor);
        assert_eq!(class, RowClass::Warning, "о линиях стоит знать");
        assert!(text.contains("насквозь"), "{text}");

        // Пара из настроек сценария: прослушка — всегда пара портов.
        assert_eq!(
            Pair::of(SessionMode::ComMonitor, ProxyMode::AnalyzerClient),
            Pair::Monitor
        );
    }

    /// Недоступная цель: строка называет вторую сторону пары и остаётся `danger`, а не
    /// английским текстом журнала (M4-AC6).
    #[test]
    fn an_unreachable_target_is_named_by_the_pair() {
        let error = |code: &str| {
            event(FeedKind::Error {
                code: Some(code.into()),
                peer: None,
                message: "connect failed".into(),
            })
        };
        for (pair, code, text) in [
            (
                Pair::TargetLis,
                "proxy_connect_failed",
                "Не удалось подключиться к ЛИС — соединение закрыто, жду следующее.",
            ),
            (
                Pair::TargetLis,
                "proxy_connect_timeout",
                "ЛИС не ответила за отведённое время — соединение закрыто, жду следующее.",
            ),
            (
                Pair::TargetAnalyzer,
                "proxy_connect_failed",
                "Не удалось подключиться к анализатору — соединение закрыто, жду следующее.",
            ),
        ] {
            let (class, row) = row_text(&error(code), pair);
            assert_eq!(class, RowClass::Error);
            assert_eq!(row, text);
        }
        // Пара из настроек сценария: топология задаёт сторону цели.
        assert_eq!(
            Pair::of(SessionMode::Proxy, ProxyMode::AnalyzerServer),
            Pair::TargetAnalyzer
        );
        assert_eq!(
            Pair::of(SessionMode::Serial, ProxyMode::AnalyzerClient),
            Pair::None
        );
    }

    /// Коды ядра из ревью M4 показываются по-русски и своим цветом: петля на себя —
    /// `danger`, отставание журнала — `warn`, а не спокойный `muted` (M4-AC6, M4-AC12).
    #[test]
    fn the_new_core_codes_have_their_rows() {
        let error = |code: &str| {
            event(FeedKind::Error {
                code: Some(code.into()),
                peer: None,
                message: "core text".into(),
            })
        };
        let (class, text) = row_text(&error("proxy_target_is_self"), Pair::TargetLis);
        assert_eq!(class, RowClass::Error);
        assert_eq!(text, "Цель указывает на сам bytetap — соединение закрыто.");

        let (class, text) = row_text(&error("accept_failed"), Pair::TargetLis);
        assert_eq!(class, RowClass::Error);
        assert_eq!(text, "Не удалось принять соединение — повторяю.");

        // Отказ по адресу — не сломанный приём: строка называет именно причину, а адрес
        // отвергнутого берёт из поля события, а не из английского `message` (FR-8).
        let (class, text) = row_text(&error("proxy_peer_rejected"), Pair::TargetLis);
        assert_eq!(class, RowClass::Error);
        assert_eq!(
            text,
            "Соединение с неразрешённого адреса закрыто — жду следующее."
        );
        let rejected = event(FeedKind::Error {
            code: Some("proxy_peer_rejected".into()),
            peer: Some("192.0.2.7".into()),
            message: "proxy peer 192.0.2.7 is not allowed: only 192.168.1.51 may connect".into(),
        });
        let (class, text) = row_text(&rejected, Pair::TargetLis);
        assert_eq!(class, RowClass::Error);
        assert_eq!(
            text,
            "Соединение с неразрешённого адреса 192.0.2.7 закрыто — жду следующее."
        );

        let warning = event(FeedKind::Info {
            code: Some("journal_backlog_warning".into()),
            message: "journal writer backlog".into(),
            repeats: None,
        });
        let (class, text) = row_text(&warning, Pair::TargetLis);
        assert_eq!(class, RowClass::Warning);
        assert!(!text.starts_with("events."), "{text}");

        // Отказ журнала: в ленте та же строка, что у прочих причин отказа.
        let (class, text) = row_text(&error("journal_backlog"), Pair::None);
        assert_eq!(class, RowClass::Error);
        assert_eq!(text, "Запись в журнал прервана.");
        // Неизвестный код по-прежнему показывает английский текст журнала.
        let unknown = event(FeedKind::Info {
            code: Some("other_code".into()),
            message: "core text".into(),
            repeats: None,
        });
        assert_eq!(
            row_text(&unknown, Pair::None),
            (RowClass::Event, "core text".into())
        );
    }
}
