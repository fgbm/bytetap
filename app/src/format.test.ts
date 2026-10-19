import { describe, expect, it } from "vitest";
import { formatBytes, formatDateTime, formatDuration, formatTime } from "./format";

const NBSP = "\u00a0";

describe("formatBytes", () => {
  it("показывает байты, килобайты, мегабайты и гигабайты как в счётчиках шапки", () => {
    expect(formatBytes(0)).toBe(`0${NBSP}Б`);
    expect(formatBytes(512)).toBe(`512${NBSP}Б`);
    expect(formatBytes(10_240)).toBe(`10${NBSP}КБ`);
    expect(formatBytes(10_450)).toBe(`10,2${NBSP}КБ`);
    expect(formatBytes(1_258_291)).toBe(`1,2${NBSP}МБ`);
    expect(formatBytes(3_650_000_000)).toBe(`3,4${NBSP}ГБ`);
  });

  it("отрицательные и нечисловые значения считает нулём", () => {
    expect(formatBytes(-5)).toBe(`0${NBSP}Б`);
    expect(formatBytes(Number.NaN)).toBe(`0${NBSP}Б`);
  });
});

describe("formatDuration", () => {
  it("до часа показывает минуты и секунды, после — часы", () => {
    expect(formatDuration(14_000)).toBe("00:14");
    expect(formatDuration(847_000)).toBe("14:07");
    expect(formatDuration(0)).toBe("00:00");
    expect(formatDuration(3_600_000)).toBe("01:00:00");
    expect(formatDuration(4_447_000)).toBe("01:14:07");
  });

  it("не уходит в минус", () => {
    expect(formatDuration(-1000)).toBe("00:00");
  });
});

describe("formatTime", () => {
  it("выводит время фрагмента с миллисекундами", () => {
    const iso = new Date(2026, 9, 19, 10, 15, 3, 4).toISOString();
    expect(formatTime(iso)).toBe("10:15:03.004");
  });

  it("на испорченной дате показывает прочерки", () => {
    expect(formatTime("не дата")).toBe("--:--:--.---");
  });
});

describe("formatDateTime", () => {
  it("выводит дату и время журнала через локаль", () => {
    const iso = new Date(2026, 9, 19, 10, 15, 0).toISOString();
    expect(formatDateTime(iso), "дата и время по правилам локали").toBe("19.10.2026, 10:15:00");
  });
});
