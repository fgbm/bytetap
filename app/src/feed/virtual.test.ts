import { describe, expect, it } from "vitest";
import {
  ANCHOR_SPAN_PX,
  applyScroll,
  DEFAULT_VIEWPORT_HEIGHT,
  enableAutoScroll,
  exactFirstRow,
  firstVisibleRow,
  HEX_ROW_HEIGHT,
  isAtBottom,
  listHeight,
  MAX_SCROLL_PX,
  rangeFromRow,
  scrollTopForRow,
  scrollTravel,
  TEXT_ROW_HEIGHT,
  visibleRange,
  visibleRows,
  type ScrollAnchor,
} from "./virtual";

describe("visibleRange", () => {
  it("в начале списка держит видимые строки и запас", () => {
    const range = visibleRange(1000, 0, 400, TEXT_ROW_HEIGHT);
    expect(range.start).toBe(0);
    // 400 / 28 = 15 видимых строк, одна частичная, 8 в запасе.
    expect(range.end).toBe(24);
  });

  it("сдвигается вместе с прокруткой", () => {
    const range = visibleRange(1000, 100 * TEXT_ROW_HEIGHT, 400, TEXT_ROW_HEIGHT);
    expect(range.start).toBe(92);
    expect(range.end).toBe(124);
  });

  it("не выходит за конец списка", () => {
    expect(visibleRange(3, 0, 400, TEXT_ROW_HEIGHT)).toEqual({ start: 0, end: 3 });
    expect(visibleRange(0, 0, 400, TEXT_ROW_HEIGHT)).toEqual({ start: 0, end: 0 });
  });

  it("работает с высотой строки HEX", () => {
    const range = visibleRange(600, 0, 400, HEX_ROW_HEIGHT);
    expect(range.end - range.start).toBeLessThan(40);
  });
});

describe("высота списка", () => {
  it("умножает число строк на высоту", () => {
    expect(listHeight(10_000, TEXT_ROW_HEIGHT)).toBe(280_000);
    expect(listHeight(0, TEXT_ROW_HEIGHT)).toBe(0);
  });

  it("не поднимает распорку выше предела прокрутки", () => {
    // 1 000 000 строк по 28 px — 28 млн пикселей, больше, чем держит браузер.
    expect(listHeight(1_000_000, TEXT_ROW_HEIGHT)).toBe(MAX_SCROLL_PX);
  });
});

describe("сжатая прокрутка длинного списка (AC7)", () => {
  const total = 1_000_000;
  const travel = scrollTravel(total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT);

  it("конец списка достижим и заканчивается последней строкой", () => {
    expect(travel).toBeGreaterThan(0);
    const bottom = visibleRange(
      total,
      travel,
      DEFAULT_VIEWPORT_HEIGHT,
      TEXT_ROW_HEIGHT,
    );
    expect(bottom.end).toBe(total);
    expect(bottom.start).toBeGreaterThan(0);
    expect(bottom.start).toBeLessThan(total);
  });

  it("номер строки переводится в пиксель и обратно без потери конца", () => {
    expect(scrollTopForRow(total - 1, total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT)).toBe(
      travel,
    );
    expect(
      firstVisibleRow(travel, total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT),
    ).toBe(total - visibleRows(DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT));
  });

  it("верх и низ списка считаются от краёв окна", () => {
    // Верх: нулевой пиксель — нулевая строка, даже если якорь остался от прошлой прокрутки.
    expect(
      firstVisibleRow(0, total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT, {
        top: 10_000,
        row: 1000,
      }),
    ).toBe(0);
    // Низ: последняя строка стоит у нижнего края окна, пустой полосы под списком нет.
    const visible = visibleRows(DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT);
    const bottom = firstVisibleRow(travel, total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT, {
      top: travel - 10,
      row: total - visible - 50,
    });
    expect(bottom).toBe(total - visible);
    expect(bottom + visible).toBe(total);
  });

  it("мелкие шаги прокрутки копятся в якоре, а не теряются", () => {
    // Двадцать шагов по 5 px — это 100 px, то есть 3–4 строки: якорь пересаживается
    // на каждом шаге и хранит дробную строку, поэтому доля шага не пропадает.
    const geometry = [total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT] as const;
    let top = 5_000_000;
    let anchor: ScrollAnchor = {
      top,
      row: exactFirstRow(top, ...geometry),
    };
    const start = firstVisibleRow(top, ...geometry, anchor);
    for (let step = 0; step < 20; step += 1) {
      top += 5;
      anchor = { top, row: exactFirstRow(top, ...geometry, anchor) };
    }
    const moved = firstVisibleRow(top, ...geometry, anchor) - start;
    expect(moved).toBeGreaterThanOrEqual(3);
    expect(moved).toBeLessThanOrEqual(4);
  });

  it("сжатый HEX у нижнего края показывает конец без пустой полосы", () => {
    const rows = 2_000_000;
    const hexTravel = scrollTravel(rows, DEFAULT_VIEWPORT_HEIGHT, HEX_ROW_HEIGHT);
    expect(hexTravel).toBeGreaterThan(0);
    const visible = visibleRows(DEFAULT_VIEWPORT_HEIGHT, HEX_ROW_HEIGHT);
    const first = firstVisibleRow(hexTravel, rows, DEFAULT_VIEWPORT_HEIGHT, HEX_ROW_HEIGHT);
    expect(first + visible).toBe(rows);
    expect(rangeFromRow(rows, first, DEFAULT_VIEWPORT_HEIGHT, HEX_ROW_HEIGHT).end).toBe(rows);
  });

  it("рядом с якорем колесо двигает строки по одной, а не пропорцией", () => {
    // Одна строка сжатого списка «стоит» доли пикселя: пропорция увела бы на 2–3 строки.
    const anchor = { top: 5_000_000, row: 500_000 };
    expect(
      firstVisibleRow(
        anchor.top + TEXT_ROW_HEIGHT,
        total,
        DEFAULT_VIEWPORT_HEIGHT,
        TEXT_ROW_HEIGHT,
        anchor,
      ),
    ).toBe(500_001);
    expect(
      firstVisibleRow(
        anchor.top + 3 * TEXT_ROW_HEIGHT,
        total,
        DEFAULT_VIEWPORT_HEIGHT,
        TEXT_ROW_HEIGHT,
        anchor,
      ),
    ).toBe(500_003);
    // Дальше якоря — прыжок: строка считается пропорцией.
    const jump = anchor.top + ANCHOR_SPAN_PX + TEXT_ROW_HEIGHT;
    expect(
      firstVisibleRow(jump, total, DEFAULT_VIEWPORT_HEIGHT, TEXT_ROW_HEIGHT, anchor),
    ).not.toBe(anchor.row + Math.round((ANCHOR_SPAN_PX + TEXT_ROW_HEIGHT) / TEXT_ROW_HEIGHT));
  });

  it("автопрокрутка и «у нижнего края» работают со сжатой распоркой", () => {
    expect(isAtBottom(travel, DEFAULT_VIEWPORT_HEIGHT, MAX_SCROLL_PX)).toBe(true);
    expect(applyScroll(travel, DEFAULT_VIEWPORT_HEIGHT, MAX_SCROLL_PX).follow).toBe(true);
    expect(applyScroll(travel / 2, DEFAULT_VIEWPORT_HEIGHT, MAX_SCROLL_PX).follow).toBe(false);
  });
});

describe("автопрокрутка (AC8)", () => {
  it("считает прокрутку у нижнего края", () => {
    expect(isAtBottom(600, 400, 1000)).toBe(true);
    expect(isAtBottom(590, 400, 1000)).toBe(false);
    expect(isAtBottom(597, 400, 1000)).toBe(true);
  });

  it("выключается при прокрутке вверх и включается у нижнего края", () => {
    expect(applyScroll(100, 400, 1000).follow).toBe(false);
    expect(applyScroll(600, 400, 1000).follow).toBe(true);
    expect(enableAutoScroll().follow).toBe(true);
  });
});
