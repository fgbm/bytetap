//! Стартовый экран — выбор сценария (FR-25, U1-AC9), как `ScenariosView.vue`.
//!
//! Три карточки отвечают на один вопрос: как анализатор связан с ноутбуком. Карточка
//! последнего запуска обведена и показывает его порт. ↑/↓ ходят по карточкам, Enter
//! открывает выбранную. Ниже — до трёх последних записей папки журналов.

use bytetap_core::journal::RecentJournal;
use chrono::{DateTime, TimeZone};
use eframe::egui::{
    self, CursorIcon, Key, Modifiers, Rect, Role, Sense, Stroke, Ui, Vec2, WidgetInfo, pos2, vec2,
};

use crate::app::{BytetapApp, centered};
use crate::icons::{self, Icon};
use crate::settings::{LastRun, SessionMode};
use crate::t;
use crate::theme::tokens::{
    RADIUS_CONTROL, RADIUS_PANEL, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_2XS, TEXT_BASE, TEXT_SM,
    TEXT_XS,
};
use crate::theme::{self, Palette, radius};
use crate::widgets;

/// Карточки сценариев в порядке экрана.
pub const CARDS: [SessionMode; 3] = [
    SessionMode::TcpServer,
    SessionMode::TcpClient,
    SessionMode::Serial,
];

fn card_id(index: usize) -> egui::Id {
    egui::Id::unique(("bt-card", index))
}

pub fn card_index(mode: SessionMode) -> usize {
    CARDS.iter().position(|card| *card == mode).unwrap_or(0)
}

pub fn mode_title(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::TcpServer => t!("scenarios.serverTitle"),
        SessionMode::TcpClient => t!("scenarios.clientTitle"),
        SessionMode::Serial => t!("scenarios.serialTitle"),
    }
}

fn mode_description(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::TcpServer => t!("scenarios.serverDescription"),
        SessionMode::TcpClient => t!("scenarios.clientDescription"),
        SessionMode::Serial => t!("scenarios.serialDescription"),
    }
}

fn mode_icon(mode: SessionMode) -> Icon {
    match mode {
        SessionMode::TcpServer => Icon::MonitorDown,
        SessionMode::TcpClient => Icon::Network,
        SessionMode::Serial => Icon::Usb,
    }
}

/// «последний раз: порт 5000» — только у карточки последнего сценария.
pub fn last_detail(run: Option<&LastRun>, mode: SessionMode) -> Option<String> {
    let run = run.filter(|run| run.mode == mode)?;
    let detail = match run.mode {
        SessionMode::TcpClient => format!("{}:{}", run.host, run.port),
        SessionMode::Serial => format!("{} · {}", run.port, run.baud),
        SessionMode::TcpServer => t!("scenarios.lastPort", port = run.port),
    };
    Some(t!("scenarios.lastTime", detail = detail))
}

const NBSP: char = '\u{a0}';

/// Начало записи: «сегодня 10:15», «вчера 18:02», «17.10 09:40» — в поясе `now`.
pub fn recent_time<Tz: TimeZone>(started_at: &str, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Ok(started) = DateTime::parse_from_rfc3339(started_at) else {
        return String::new();
    };
    let local = started.with_timezone(&now.timezone());
    let time = local.format("%H:%M");
    match (now.date_naive() - local.date_naive()).num_days() {
        0 => format!("{} {time}", t!("units.today")),
        1 => format!("{} {time}", t!("units.yesterday")),
        _ => local.format("%d.%m %H:%M").to_string(),
    }
}

/// Длительность записи словами: «42 мин», «2 ч 05 мин»; пустой журнал — пустая строка.
pub fn duration_span(us: u64) -> String {
    let total = us / 1_000_000;
    if total == 0 {
        return String::new();
    }
    let minutes = total / 60;
    let unit = t!("units.minutes");
    if minutes < 60 {
        return format!("{}{NBSP}{unit}", minutes.max(1));
    }
    format!(
        "{}{NBSP}{} {:02}{NBSP}{unit}",
        minutes / 60,
        t!("units.hours"),
        minutes % 60
    )
}

/// Подпись под именем файла: «анализатор · сегодня 10:15 · 42 мин».
pub fn recent_meta<Tz: TimeZone>(journal: &RecentJournal, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut parts = Vec::new();
    if !journal.analyzer.is_empty() {
        parts.push(journal.analyzer.clone());
    }
    parts.push(recent_time(&journal.started_at, now));
    let span = duration_span(journal.duration_us);
    if !span.is_empty() {
        parts.push(span);
    }
    parts.join(" · ")
}

impl BytetapApp {
    fn recent_meta_now(&self, journal: &RecentJournal) -> String {
        match &self.now {
            Some(now) => recent_meta(journal, now),
            None => recent_meta(journal, &chrono::Local::now()),
        }
    }

    /// Клавиатура экрана: ↑/↓ — карточки, Enter — открыть. Не в полях ввода и не
    /// поверх всплывающих окон.
    fn scenario_keys(&mut self, ctx: &egui::Context) -> Option<SessionMode> {
        // Фокус с клавиатуры на кнопке шапки: Enter — её, а не карточки.
        let focused_elsewhere = ctx
            .memory(|m| m.focused())
            .is_some_and(|id| !(0..CARDS.len()).any(|index| card_id(index) == id));
        if ctx.egui_wants_keyboard_input()
            || egui::Popup::is_any_open(ctx)
            || self.profile_prompt.is_some()
            || focused_elsewhere
        {
            return None;
        }
        let (down, up, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        let total = CARDS.len();
        if down {
            self.selected_card = (self.selected_card + 1) % total;
        }
        if up {
            self.selected_card = (self.selected_card + total - 1) % total;
        }
        enter.then(|| CARDS[self.selected_card])
    }

    pub(crate) fn scenarios(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut open = self.scenario_keys(&ctx);
        let mut open_recent = None;
        let p = self.pal();
        centered(ui, 592.0, |ui| {
            ui.add_space(56.0);
            ui.label(
                egui::RichText::new(t!("scenarios.question"))
                    .size(TEXT_SM)
                    .color(p.text_muted),
            );
            ui.add_space(SPACE_4 - ui.spacing().item_spacing.y);
            ui.spacing_mut().item_spacing.y = SPACE_2;
            let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
            for (index, mode) in CARDS.into_iter().enumerate() {
                let last = last_detail(self.settings.last_run.as_ref(), mode);
                let is_last = last.is_some();
                let active = index == self.selected_card;
                let response = card(ui, &p, card_id(index), mode, last, is_last, active);
                if response.hovered() && moved {
                    self.selected_card = index;
                }
                if response.gained_focus() {
                    self.selected_card = index;
                }
                if response.clicked() {
                    open = Some(mode);
                }
            }

            let recents = self.recents.ready().cloned().unwrap_or_default();
            if !recents.is_empty() {
                ui.add_space(SPACE_6 - SPACE_2);
                ui.label(
                    egui::RichText::new(t!("scenarios.recent"))
                        .font(theme::sans_medium(TEXT_XS))
                        .color(p.text_muted),
                );
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(SPACE_2);
                // Во время записи журнал не открыть: лента занята сессией.
                let busy = self.session_busy();
                for journal in &recents {
                    let meta = self.recent_meta_now(journal);
                    let response = recent_row(ui, &p, journal, &meta);
                    if busy {
                        response.on_hover_text(t!("header.openJournalBusy"));
                    } else if response.clicked() {
                        open_recent = Some(journal.path.clone());
                    }
                }
            }
            // Запись только что остановилась: записанный журнал открывается сразу.
            if let Some(path) = self.recorded_path.clone()
                && !self.session_busy()
            {
                ui.add_space(SPACE_3);
                let name = path
                    .to_string_lossy()
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let label = format!("{} — {}", t!("banner.openRecorded"), name);
                if widgets::link_button(ui, &p, Icon::FileText, 15.0, &label, false).clicked() {
                    open_recent = Some(path);
                }
            }
            ui.add_space(SPACE_6);
        });
        if let Some(path) = open_recent {
            self.open_journal(&ctx, path);
        } else if let Some(mode) = open {
            self.open_step(mode);
        }
    }
}

/// Карточка сценария: плитка с иконкой, заголовок, описание и шеврон.
fn card(
    ui: &mut Ui,
    p: &Palette,
    id: egui::Id,
    mode: SessionMode,
    last: Option<String>,
    is_last: bool,
    active: bool,
) -> egui::Response {
    let title = mode_title(mode);
    let mut description = mode_description(mode).to_owned();
    if let Some(last) = last {
        description.push_str(" · ");
        description.push_str(&last);
    }
    let width = ui.available_width();
    let left_offset = 13.0 + 44.0 + SPACE_3;
    let text_width = width - left_offset - 13.0 - 16.0 - 8.0;
    let title_galley =
        ui.painter()
            .layout_no_wrap(title.to_owned(), theme::sans_semibold(TEXT_BASE), p.text);
    let desc_galley = ui.painter().layout(
        description.clone(),
        theme::sans(TEXT_XS),
        p.text_muted,
        text_width,
    );
    let text_height = title_galley.size().y + 2.0 + desc_galley.size().y;
    let height = (text_height + 2.0 * 13.0).max(70.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(
            Role::Button,
            true,
            active,
            format!("{title}. {description}"),
        )
    });
    let corner = radius(RADIUS_PANEL);
    let painter = ui.painter();
    if active || response.hovered() {
        painter.rect_filled(rect, corner, p.hover);
    }
    if is_last {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, p.text),
            egui::StrokeKind::Inside,
        );
    }
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            corner,
            Stroke::new(1.0, p.ctrl),
            egui::StrokeKind::Outside,
        );
    }
    let tile = Rect::from_min_size(
        pos2(rect.left() + 13.0, rect.center().y - 22.0),
        Vec2::splat(44.0),
    );
    painter.rect_filled(tile, corner, p.surface_1);
    icons::paint(ui, mode_icon(mode), tile.center(), 24.0, p.text);

    let left = tile.right() + SPACE_3;
    let chevron_x = rect.right() - 13.0 - 8.0;
    let top = rect.center().y - text_height / 2.0;
    let title_height = title_galley.size().y;
    ui.painter().galley(pos2(left, top), title_galley, p.text);
    ui.painter().galley(
        pos2(left, top + title_height + 2.0),
        desc_galley,
        p.text_muted,
    );
    icons::paint(
        ui,
        Icon::ChevronRight,
        pos2(chevron_x, rect.center().y),
        16.0,
        p.text_faint,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Строка последней записи: имя файла моноширинным и подпись; подсказка — полный путь.
fn recent_row(ui: &mut Ui, p: &Palette, journal: &RecentJournal, meta: &str) -> egui::Response {
    // Имя файла по обоим разделителям: путь Windows разбирается и на других ОС.
    let path = journal.path.to_string_lossy();
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned();
    let width = ui.available_width();
    let mut job =
        egui::text::LayoutJob::simple_singleline(name.clone(), theme::mono(TEXT_XS), p.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width - 20.0);
    let name_galley = ui.painter().layout_job(job);
    let meta_galley =
        ui.painter()
            .layout_no_wrap(meta.to_owned(), theme::sans(TEXT_2XS), p.text_faint);
    let height = 8.0 + name_galley.size().y + 2.0 + meta_galley.size().y + 8.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, format!("{name}. {meta}")));
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, radius(RADIUS_CONTROL), p.hover);
    }
    widgets::focus_ring(ui, p, &response, rect, radius(RADIUS_CONTROL));
    let name_height = name_galley.size().y;
    ui.painter().galley(
        pos2(rect.left() + 10.0, rect.top() + 8.0),
        name_galley,
        p.text,
    );
    ui.painter().galley(
        pos2(rect.left() + 10.0, rect.top() + 8.0 + name_height + 2.0),
        meta_galley,
        p.text_faint,
    );
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(journal.path.display().to_string())
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-19T12:00:00+03:00").unwrap()
    }

    #[test]
    fn the_start_time_is_told_relative_to_today() {
        assert_eq!(
            recent_time("2026-10-19T10:15:00.000000+03:00", &now()),
            "сегодня 10:15"
        );
        assert_eq!(
            recent_time("2026-10-18T18:02:00+03:00", &now()),
            "вчера 18:02"
        );
        assert_eq!(
            recent_time("2026-10-17T09:40:00+03:00", &now()),
            "17.10 09:40"
        );
        // Время показывается в поясе «сейчас», а не в поясе записи.
        assert_eq!(recent_time("2026-10-19T07:15:00Z", &now()), "сегодня 10:15");
        assert_eq!(recent_time("not a date", &now()), "");
    }

    #[test]
    fn the_duration_is_told_in_minutes_and_hours() {
        assert_eq!(duration_span(0), "");
        assert_eq!(duration_span(999_999), "");
        assert_eq!(duration_span(5_000_000), "1\u{a0}мин");
        assert_eq!(duration_span(42 * 60 * 1_000_000), "42\u{a0}мин");
        assert_eq!(
            duration_span((2 * 3600 + 5 * 60) * 1_000_000),
            "2\u{a0}ч 05\u{a0}мин"
        );
    }

    #[test]
    fn only_the_last_mode_shows_its_detail() {
        let run = LastRun {
            mode: SessionMode::Serial,
            port: "COM5".into(),
            host: String::new(),
            baud: 9600,
        };
        assert_eq!(
            last_detail(Some(&run), SessionMode::Serial).as_deref(),
            Some("последний раз: COM5 · 9600")
        );
        assert_eq!(last_detail(Some(&run), SessionMode::TcpServer), None);
        let run = LastRun {
            mode: SessionMode::TcpServer,
            port: "5000".into(),
            host: String::new(),
            baud: 0,
        };
        assert_eq!(
            last_detail(Some(&run), SessionMode::TcpServer).as_deref(),
            Some("последний раз: порт 5000")
        );
        let run = LastRun {
            mode: SessionMode::TcpClient,
            port: "6000".into(),
            host: "10.0.0.7".into(),
            baud: 0,
        };
        assert_eq!(
            last_detail(Some(&run), SessionMode::TcpClient).as_deref(),
            Some("последний раз: 10.0.0.7:6000")
        );
    }

    #[test]
    fn the_meta_line_joins_analyzer_time_and_duration() {
        let journal = RecentJournal {
            path: "bytetap.jsonl".into(),
            started_at: "2026-10-19T10:15:00+03:00".into(),
            analyzer: "ANALYZER".into(),
            duration_us: 42 * 60 * 1_000_000,
            mode: "tcp_server".into(),
        };
        assert_eq!(
            recent_meta(&journal, &now()),
            "ANALYZER · сегодня 10:15 · 42\u{a0}мин"
        );
        let open = RecentJournal {
            analyzer: String::new(),
            duration_us: 0,
            ..journal
        };
        assert_eq!(recent_meta(&open, &now()), "сегодня 10:15");
    }
}
