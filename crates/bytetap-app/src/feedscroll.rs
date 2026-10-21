//! Прокрутка ленты в единицах строк: своя полоса прокрутки (U1-AC19).
//!
//! Лента журнала бывает в миллионы строк: 4 млн строк HEX при 25 px — это 100 000 000
//! пикселей. `ScrollArea::show_rows` держит смещение в `f32`, а на такой длине шаг
//! мантиссы — 8 px: в конце журнала строки дрожат и наезжают друг на друга на 2–4 px.
//! Поэтому у ленты просмотра своя прокрутка: смещение — номер первой строки и доля
//! внутри неё в `f64`, а строки рисуются от верхнего края видимой области, поэтому
//! координаты отрисовки всегда маленькие.
//!
//! Прокрутка — колесом над лентой (и над полосой тоже), полосой справа (перетаскивание
//! ползунка, клик по дорожке) и клавишами PageUp/PageDown/Home/End, когда фокус не в поле
//! ввода: иначе Home и End уводили бы каретку в тексте.

use std::ops::Range;

use eframe::egui::{
    self, Align, CursorIcon, Id, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};

use crate::t;
use crate::theme::Palette;
use crate::widgets;

/// Ширина полосы прокрутки.
const BAR_WIDTH: f32 = 10.0;
/// Ширина ползунка.
const THUMB_WIDTH: f32 = 6.0;
/// Наименьшая высота ползунка: за него можно взяться мышью.
const THUMB_MIN: f32 = 28.0;

/// Состояние прокрутки ленты: смещение в строках и перетаскивание ползунка.
#[derive(Clone, Copy, Debug, Default)]
pub struct FeedScroll {
    /// Номер первой строки сверху и доля внутри неё: 3.5 — половина строки 3 скрыта.
    offset: f64,
    /// Ползунок тянут: за какой точкой он взят, в пикселях от его верха.
    grab: Option<f32>,
}

/// Что видно в ленте после прокрутки.
#[derive(Clone, Debug)]
pub struct ScrollOut {
    /// Строки, которые надо нарисовать, — с запасом в одну сверху и снизу.
    pub range: Range<usize>,
    /// Первая видимая строка.
    pub first: usize,
    /// Сколько строк помещается в ленте (целое вверх).
    pub visible: usize,
    /// Видимая область ленты без полосы прокрутки.
    pub inner: Rect,
}

impl FeedScroll {
    /// Смещение в строках: номер первой строки и доля внутри неё.
    #[must_use]
    pub fn offset(&self) -> f64 {
        self.offset
    }

    /// Первая видимая строка журнала из `rows`.
    #[must_use]
    pub fn first_row(&self, rows: usize) -> usize {
        let first = self.offset.floor();
        if !first.is_finite() || first <= 0.0 {
            return 0;
        }
        (first as u64).min(rows.saturating_sub(1) as u64) as usize
    }

    /// Поставить строку `row` первой видимой: переход по карте сессии.
    pub fn set_top(&mut self, row: usize) {
        self.offset = row as f64;
    }

    /// Предел смещения в строках: последняя строка ленты видна до конца.
    ///
    /// В высоту `height` помещается `height / row_height` строк — число дробное, если
    /// высота ленты не кратна строке. Целое `rows − ceil(...)` оставляло низ последней
    /// строки за краем ленты: до неё было не доскроллить, а `End` показывал строку
    /// обрезанной на долю строки. Поэтому предел дробный, а `ceil` остаётся только у
    /// диапазона отрисовки.
    #[must_use]
    pub fn max_offset(rows: usize, row_height: f32, height: f32) -> f64 {
        let fit = f64::from(height) / f64::from(row_height);
        (rows as f64 - fit).max(0.0)
    }

    /// Прокрутить к концу: `rows` строк в ленте высотой `height`.
    pub fn set_bottom(&mut self, rows: usize, row_height: f32, height: f32) {
        self.offset = Self::max_offset(rows, row_height, height);
    }

    /// Лента на всю доступную высоту: строки, полоса прокрутки и видимое окно.
    ///
    /// `add` рисует строки диапазона в дочернем `Ui`, начало координат которого —
    /// верх видимой области: абсолютные координаты строк не растут с журналом.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        p: &Palette,
        id: Id,
        rows: usize,
        row_height: f32,
        add: impl FnOnce(&mut Ui, Range<usize>),
    ) -> ScrollOut {
        let height = ui.available_height().max(row_height);
        // Сама лента фокуса не берёт: Tab идёт к ползунку полосы, а клавиши ленты
        // работают, когда фокус не в поле ввода (U1-AC25).
        let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        // Полоса нужна, когда строки не помещаются целиком: `rows·h > h_ленты`. Высота
        // последней страницы бывает больше высоты ленты на доли строки — это уже прокрутка.
        let fit = f64::from(outer.height()) / f64::from(row_height);
        let bar = rows as f64 > fit;
        let visible = (fit.ceil() as usize).max(1);
        let max_offset = (rows as f64 - fit).max(0.0);
        let inner = Rect::from_min_max(
            outer.min,
            pos2(
                outer.right() - if bar { BAR_WIDTH } else { 0.0 },
                outer.bottom(),
            ),
        );

        // Колесо — над всей лентой, включая полосу: `ui.rect_contains_pointer` смотрит
        // только на слой, а не на виджеты поверх (ползунок перехватил бы наведение).
        if ui.rect_contains_pointer(outer) {
            self.wheel(ui, row_height, max_offset);
        }
        // Клавиши — когда фокус не в поле ввода и не на другом виджете экрана: в
        // диалоге комментария Home и End принадлежат тексту, а не ленте. На ползунке
        // полосы они прокручивают ленту — иначе фокус на полосе отнял бы клавиши.
        let thumb = id.with("thumb");
        if ui.memory(|m| m.focused().is_none_or(|focused| focused == thumb)) {
            self.keys(ui, visible, max_offset);
        }
        self.offset = self.offset.clamp(0.0, max_offset);

        let first = self.first_row(rows);
        let fraction = (self.offset - first as f64) as f32;
        let top = inner.top() - fraction * row_height;
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(inner.left(), top),
                    pos2(inner.right(), inner.bottom() + row_height),
                ))
                .layout(Layout::top_down(Align::Min)),
        );
        child.spacing_mut().item_spacing.y = 0.0;
        child.set_clip_rect(ui.clip_rect().intersect(inner));
        let range = first..(first + visible + 2).min(rows);
        add(&mut child, range.clone());
        if bar {
            self.bar(ui, p, id, outer, visible, rows, max_offset);
        }
        ScrollOut {
            range,
            first,
            visible,
            inner,
        }
    }

    /// Колесо над лентой: смещение в строках, а не в пикселях.
    fn wheel(&mut self, ui: &Ui, row_height: f32, max_offset: f64) {
        let delta = ui.input(|i| i.smooth_scroll_delta.y);
        if delta != 0.0 {
            self.offset -= f64::from(delta) / f64::from(row_height);
            self.offset = self.offset.clamp(0.0, max_offset);
        }
    }

    /// Клавиши: страница, начало и конец.
    fn keys(&mut self, ui: &Ui, visible: usize, max_offset: f64) {
        let page = visible.saturating_sub(1).max(1) as f64;
        let (down, up, home, end) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
            )
        });
        if down {
            self.offset += page;
        }
        if up {
            self.offset -= page;
        }
        if home {
            self.offset = 0.0;
        }
        if end {
            self.offset = max_offset;
        }
        self.offset = self.offset.clamp(0.0, max_offset);
    }

    /// Полоса прокрутки справа: ползунок и дорожка.
    fn bar(
        &mut self,
        ui: &mut Ui,
        p: &Palette,
        id: Id,
        outer: Rect,
        visible: usize,
        rows: usize,
        max_offset: f64,
    ) {
        let track = Rect::from_min_max(
            pos2(outer.right() - BAR_WIDTH, outer.top()),
            pos2(outer.right(), outer.bottom()),
        );
        // Ползунок — доля видимого от всего журнала: строк помещается `rows − max_offset`,
        // дробное число, если высота ленты не кратна строке.
        let fit = (rows as f64 - max_offset).clamp(0.0, rows as f64);
        let thumb_height = (track.height() * fit as f32 / rows as f32)
            .max(THUMB_MIN)
            .min(track.height());
        let span = (track.height() - thumb_height).max(0.0);
        let place = |offset: f64| -> f32 {
            let t = if max_offset > 0.0 {
                (offset / max_offset).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            track.top() + span * t
        };
        let thumb = Rect::from_min_size(
            pos2(
                track.left() + (BAR_WIDTH - THUMB_WIDTH) / 2.0,
                place(self.offset),
            ),
            vec2(THUMB_WIDTH, thumb_height),
        );
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let thumb_response = ui.interact(thumb, id.with("thumb"), Sense::drag());
        thumb_response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::ScrollBar, true, t!("feed.scrollBar"));
            info.value = Some(if max_offset > 0.0 {
                (self.offset / max_offset).clamp(0.0, 1.0)
            } else {
                0.0
            });
            info
        });
        if thumb_response.drag_started()
            && let Some(pos) = pointer
        {
            self.grab = Some((pos.y - thumb.top()).clamp(0.0, thumb.height()));
        }
        if thumb_response.dragged()
            && span > 0.0
            && let Some(pos) = pointer
        {
            let grab = self.grab.unwrap_or(thumb_height / 2.0);
            let t = f64::from(((pos.y - grab - track.top()) / span).clamp(0.0, 1.0));
            self.offset = t * max_offset;
        }
        if thumb_response.drag_stopped() {
            self.grab = None;
        }
        // Клик по дорожке — страница вверх или вниз.
        let track_response = ui.interact(track, id.with("track"), Sense::click());
        track_response
            .widget_info(|| WidgetInfo::labeled(Role::ScrollBar, true, t!("feed.scrollTrack")));
        if track_response.clicked()
            && let Some(pos) = pointer
            && !thumb.contains(pos)
        {
            let page = visible.saturating_sub(1).max(1) as f64;
            self.offset += if pos.y < thumb.top() { -page } else { page };
            self.offset = self.offset.clamp(0.0, max_offset);
        }
        let visuals = ui.visuals();
        let color = if thumb_response.hovered() || thumb_response.dragged() {
            visuals.widgets.hovered.fg_stroke.color
        } else {
            visuals.widgets.inactive.fg_stroke.color
        };
        ui.painter().rect_filled(track, 0, visuals.extreme_bg_color);
        ui.painter()
            .rect_filled(thumb, egui::CornerRadius::same(3), color);
        // Фокус с клавиатуры: кольцо вокруг ползунка в цвете `ctrl`.
        widgets::focus_ring(
            ui,
            p,
            &thumb_response,
            thumb.shrink(1.0),
            egui::CornerRadius::same(3),
        );
        if thumb_response.hovered() || thumb_response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4 млн строк HEX при 25 px — 100 млн пикселей: у `f32` шаг мантиссы там 8 px.
    const HEX_ROWS: usize = 4_000_000;
    const HEX_ROW: f32 = 25.0;

    #[test]
    fn the_row_offset_is_exact_at_the_end_of_a_huge_feed() {
        let mut scroll = FeedScroll::default();
        for row in [0, 2_000_000, HEX_ROWS - 1] {
            scroll.set_top(row);
            assert_eq!(scroll.first_row(HEX_ROWS), row, "строка {row}");
        }
        // Доля внутри строки не теряется: смещение — в строках, а не в пикселях.
        scroll.set_top(3_999_990);
        assert_eq!(scroll.first_row(HEX_ROWS), 3_999_990);
        assert!((scroll.offset() - 3_999_990.0).abs() < 1e-9);
    }

    #[test]
    fn the_pixel_offset_of_f32_wobbles_where_the_row_offset_does_not() {
        // Так считает `ScrollArea::show_rows`: пиксели во `f32`.
        let exact = 3_999_990.5_f64 * f64::from(HEX_ROW);
        let wobble = (exact as f32 as f64 - exact).abs();
        assert!(
            wobble >= 2.0,
            "у f32 на 100 млн пикселей ошибка {wobble} px — строки дрожат"
        );
        // Смещение в строках считается точно: та же точка даёт ту же строку.
        let mut scroll = FeedScroll::default();
        scroll.set_top(3_999_990);
        assert_eq!(scroll.first_row(HEX_ROWS), 3_999_990);
        assert!(scroll.offset() < HEX_ROWS as f64);
    }

    #[test]
    fn the_offset_at_the_end_keeps_the_last_row_whole() {
        // Высота ленты не кратна строке: 500 / 18 = 27.78 строки. Целое смещение
        // `rows − ceil(...)` оставляло низ последней строки за краем ленты.
        let height = 500.0;
        let row_height = 18.0;
        let fit = f64::from(height) / f64::from(row_height);
        let rows = 1_000_000;
        let mut scroll = FeedScroll::default();
        scroll.set_bottom(rows, row_height, height);
        assert!(
            (scroll.offset() + fit - rows as f64).abs() < 1e-9,
            "низ последней строки — по низу ленты: смещение {}",
            scroll.offset()
        );
        assert!(
            scroll.offset() - scroll.offset().floor() > 0.0,
            "смещение дробное: {}",
            scroll.offset()
        );
        assert_eq!(scroll.first_row(rows), rows - 28);
    }

    #[test]
    fn a_journal_a_hair_taller_than_the_feed_scrolls_to_the_end() {
        // 28 строк по 18 px при ленте 500 px: помещается 27.78 — последняя строка уже
        // не влезает, и лента обязана прокрутиться, хоть целых строк и хватает.
        let height = 500.0;
        let row_height = 18.0;
        let rows = 28;
        let mut scroll = FeedScroll::default();
        scroll.set_bottom(rows, row_height, height);
        assert!(scroll.offset() > 0.0, "смещение {}", scroll.offset());
        assert!(
            (scroll.offset() + f64::from(height) / f64::from(row_height) - rows as f64).abs()
                < 1e-9
        );
        assert_eq!(scroll.first_row(rows), 0);
    }

    #[test]
    fn a_feed_shorter_than_its_rows_does_not_scroll() {
        // 10 строк по 18 px в ленте 500 px: прокрутки нет, полосы тоже.
        assert_eq!(FeedScroll::max_offset(10, 18.0, 500.0), 0.0);
        // Ровно по высоте — тоже без прокрутки.
        assert_eq!(FeedScroll::max_offset(25, 20.0, 500.0), 0.0);
    }

    #[test]
    fn the_visible_row_is_clamped_to_the_feed() {
        // Смещение за концом ленты: первая строка — последняя.
        let scroll = FeedScroll {
            offset: 1e9,
            ..FeedScroll::default()
        };
        assert_eq!(scroll.first_row(10), 9);
        // Отрицательное смещение — начало.
        let scroll = FeedScroll {
            offset: -5.0,
            ..FeedScroll::default()
        };
        assert_eq!(scroll.first_row(10), 0);
    }
}
