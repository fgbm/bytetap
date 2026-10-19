import { describe, expect, it } from "vitest";
import {
  applyScroll,
  enableAutoScroll,
  HEX_ROW_HEIGHT,
  isAtBottom,
  listHeight,
  TEXT_ROW_HEIGHT,
  visibleRange,
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
