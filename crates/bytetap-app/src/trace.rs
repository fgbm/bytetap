//! Трасса записи: две дорожки над лентой — импульс на каждый фрагмент в бегущем окне
//! последних 60 с по `mono_us` (M3-UI-AC5, как в v0.1.0: `app/src/trace/model.ts` и
//! `TraceView.vue`).
//!
//! Модель — чистые функции в долях дорожки, экран только умножает их на пиксели.

use bytetap_core::journal::Dir;
use eframe::egui::{Align2, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::feed::FeedRing;
use crate::theme::tokens::{SPACE_2, SPACE_3, TEXT_SM};
use crate::theme::{self, Palette};

/// Ширина бегущего окна во время записи: последние 60 с.
pub const WINDOW_US: u64 = 60_000_000;

/// Высота дорожки.
pub const LANE_HEIGHT: f32 = 22.0;

/// Отступы трассы сверху и снизу, как `.bt-trace` (6px и 8px).
const PAD_TOP: f32 = 6.0;
const PAD_BOTTOM: f32 = 8.0;

/// Ширина подписи дорожки (глиф стрелки).
const LABEL_WIDTH: f32 = 16.0;

/// Нижняя граница высоты импульса: столбик короче 30% дорожки не читается.
const IMPULSE_MIN: f32 = 0.3;

/// Длина фрагмента, на которой импульс занимает всю дорожку.
const IMPULSE_FULL_LEN: f32 = 4096.0;

/// Высота трассы целиком.
pub fn height() -> f32 {
    PAD_TOP + 2.0 * LANE_HEIGHT + PAD_BOTTOM
}

/// Высота импульса по длине фрагмента — от 30% до 100% дорожки: логарифм различает
/// 3 байта и 3 КБ, а нижняя граница держит короткие ENQ и ACK видимыми.
pub fn impulse_height(len: usize) -> f32 {
    let share = ((len.max(1) as f32).log10() / IMPULSE_FULL_LEN.log10()).min(1.0);
    IMPULSE_MIN + (1.0 - IMPULSE_MIN) * share
}

/// Окно «последние 60 с»: до заполнения — от нуля, дальше скользит.
pub fn window_range(now_us: u64) -> (u64, u64) {
    let to = now_us.max(WINDOW_US);
    (to - WINDOW_US, to)
}

/// Импульс в долях дорожки: `x` — положение, `height` — высота.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impulse {
    pub x: f32,
    pub height: f32,
}

/// Импульсы фрагментов направления `dir`, попавших в окно `[from, to]`.
///
/// `mono_us` растёт по буферу, поэтому обход идёт с конца и заканчивается на первом
/// фрагменте старше окна.
pub fn impulses(ring: &FeedRing, dir: Dir, from: u64, to: u64) -> Vec<Impulse> {
    let span = to.saturating_sub(from);
    if span == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for event in ring.iter().rev() {
        let (Some(data), Some(mono_us)) = (event.data(), event.mono_us) else {
            continue;
        };
        if data.dir != dir {
            continue;
        }
        if mono_us < from {
            break;
        }
        if mono_us > to {
            continue;
        }
        out.push(Impulse {
            x: (mono_us - from) as f32 / span as f32,
            height: impulse_height(data.bytes.len()),
        });
    }
    out.reverse();
    out
}

/// Последний известный `mono_us`: окно не отстаёт от данных.
pub fn latest_mono_us(ring: &FeedRing) -> u64 {
    ring.iter()
        .rev()
        .find_map(|event| event.mono_us)
        .unwrap_or_default()
}

/// Рисует трассу во всю ширину `ui`: фон, подписи, базовая линия и импульсы 2px.
pub fn show(ui: &mut Ui, p: &Palette, ring: &FeedRing, now_us: u64) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height()), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0, p.trace_bg);
    let (from, to) = window_range(now_us.max(latest_mono_us(ring)));
    let left = rect.left() + SPACE_3 + LABEL_WIDTH + SPACE_2;
    let width = (rect.right() - SPACE_3 - left).max(0.0);
    for (lane, (dir, glyph, color)) in [
        (Dir::AnalyzerToLis, "→", p.dir_a2l),
        (Dir::LisToAnalyzer, "←", p.dir_l2a),
    ]
    .into_iter()
    .enumerate()
    {
        let top = rect.top() + PAD_TOP + LANE_HEIGHT * lane as f32;
        painter.text(
            pos2(
                rect.left() + SPACE_3 + LABEL_WIDTH / 2.0,
                top + LANE_HEIGHT / 2.0,
            ),
            Align2::CENTER_CENTER,
            glyph,
            theme::mono(TEXT_SM),
            p.text_muted,
        );
        // Базовая линия — токен border: дорожка читается как ось графика.
        let bottom = top + LANE_HEIGHT - 1.5;
        painter.hline(left..=left + width, bottom, Stroke::new(1.0, p.border));
        let color = color.gamma_multiply(0.9);
        for impulse in impulses(ring, dir, from, to) {
            // Столбик 2px; ниже 6px он сливается с базовой линией.
            let x = (left + impulse.x * width).round();
            let h = (impulse.height * (LANE_HEIGHT - 2.0)).max(6.0);
            painter.rect_filled(
                Rect::from_min_max(pos2(x - 1.0, bottom - h), pos2(x + 1.0, bottom)),
                0,
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::feed::{DataPayload, FeedEvent, FeedKind};

    fn data(mono_us: u64, dir: Dir, len: usize) -> Arc<FeedEvent> {
        Arc::new(FeedEvent {
            time: String::new(),
            mono_us: Some(mono_us),
            conn: Some(1),
            kind: FeedKind::Data(DataPayload::new(dir, false, vec![b'x'; len])),
        })
    }

    #[test]
    fn the_impulse_height_grows_with_the_log_of_the_length() {
        assert!((impulse_height(0) - 0.3).abs() < 1e-6);
        assert!((impulse_height(1) - 0.3).abs() < 1e-6);
        assert!((impulse_height(4096) - 1.0).abs() < 1e-6);
        assert!((impulse_height(1_000_000) - 1.0).abs() < 1e-6);
        assert!(impulse_height(3) < impulse_height(3000));
    }

    #[test]
    fn the_window_slides_after_the_first_minute() {
        assert_eq!(window_range(0), (0, WINDOW_US));
        assert_eq!(window_range(30_000_000), (0, WINDOW_US));
        assert_eq!(window_range(90_000_000), (30_000_000, 90_000_000));
    }

    #[test]
    fn impulses_are_placed_by_time_and_split_by_direction() {
        let mut ring = FeedRing::new(100);
        ring.push(data(10_000_000, Dir::AnalyzerToLis, 3));
        ring.push(data(40_000_000, Dir::LisToAnalyzer, 1));
        ring.push(data(70_000_000, Dir::AnalyzerToLis, 4096));
        ring.push(data(90_000_000, Dir::AnalyzerToLis, 10));
        let (from, to) = window_range(latest_mono_us(&ring));
        assert_eq!((from, to), (30_000_000, 90_000_000));
        let a2l = impulses(&ring, Dir::AnalyzerToLis, from, to);
        assert_eq!(a2l.len(), 2, "фрагмент старше окна не рисуется");
        assert!((a2l[0].x - 40.0 / 60.0).abs() < 1e-6);
        assert!((a2l[0].height - 1.0).abs() < 1e-6);
        assert!((a2l[1].x - 1.0).abs() < 1e-6);
        let l2a = impulses(&ring, Dir::LisToAnalyzer, from, to);
        assert_eq!(l2a.len(), 1);
        assert!((l2a[0].x - 1.0 / 6.0).abs() < 1e-6);
        assert!((l2a[0].height - 0.3).abs() < 1e-6);
    }
}
