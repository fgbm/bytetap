import { describe, expect, it } from "vitest";
import {
  densityBars,
  fractionFromX,
  frameRect,
  impulseHeight,
  impulses,
  TRACE_WINDOW_US,
  windowRange,
} from "./model";

describe("окно трассы", () => {
  it("до заполнения окна держит 0…60 с, потом скользит", () => {
    expect(windowRange(0)).toEqual({ fromUs: 0, toUs: TRACE_WINDOW_US });
    expect(windowRange(10_000_000)).toEqual({ fromUs: 0, toUs: TRACE_WINDOW_US });
    expect(windowRange(120_000_000)).toEqual({
      fromUs: 60_000_000,
      toUs: 120_000_000,
    });
  });
});

describe("импульсы", () => {
  it("высота растёт с длиной фрагмента и остаётся в долях", () => {
    expect(impulseHeight(0)).toBeLessThan(impulseHeight(512));
    expect(impulseHeight(512)).toBeLessThan(impulseHeight(4096));
    expect(impulseHeight(4096)).toBeLessThanOrEqual(1);
    expect(impulseHeight(10_000_000)).toBeLessThanOrEqual(1);
  });

  it("раскладывает точки окна в доли ширины и выбрасывает чужие", () => {
    const points = [
      { monoUs: 0, len: 10 },
      { monoUs: 30_000_000, len: 512 },
      { monoUs: 60_000_000, len: 20 },
      { monoUs: 90_000_000, len: 20 },
    ];

    const result = impulses(points, 0, TRACE_WINDOW_US);
    expect(result).toHaveLength(3);
    expect(result[0].x).toBe(0);
    expect(result[1].x).toBeCloseTo(0.5);
    expect(result[2].x).toBe(1);
    expect(result[1].height).toBeCloseTo(impulseHeight(512));
  });

  it("на пустом окне не рисует ничего", () => {
    expect(impulses([{ monoUs: 1, len: 1 }], 10, 10)).toEqual([]);
  });
});

describe("плотность сессии", () => {
  it("нормирует корзины к максимуму", () => {
    const bars = densityBars([0, 100, 200, 50], 4);
    expect(bars).toEqual([0, 0.5, 1, 0.25]);
  });

  it("на пустом журнале отдаёт нули нужной длины", () => {
    expect(densityBars([], 5)).toEqual([0, 0, 0, 0, 0]);
    expect(densityBars([0, 0, 0], 3)).toEqual([0, 0, 0]);
  });
});

describe("позиция указателя и рамка видимой области", () => {
  it("переводит координату клика в долю ширины", () => {
    const rect = { left: 100, width: 600 };
    expect(fractionFromX(100, rect)).toBe(0);
    expect(fractionFromX(400, rect)).toBeCloseTo(0.5);
    expect(fractionFromX(1000, rect)).toBe(1);
    expect(fractionFromX(0, rect)).toBe(0);
    expect(fractionFromX(10, { left: 0, width: 0 })).toBe(0);
  });

  it("строит рамку по видимой области и ограничивает её", () => {
    expect(frameRect(null)).toEqual({ left: 0, width: 1 });
    const frame = frameRect({ start: 0.4, end: 0.6 });
    expect(frame.left).toBeCloseTo(0.4);
    expect(frame.width).toBeCloseTo(0.2);
    expect(frameRect({ start: -1, end: 2 })).toEqual({ left: 0, width: 1 });
    expect(frameRect({ start: 0.8, end: 0.2 })).toEqual({ left: 0.8, width: 0 });
  });
});
