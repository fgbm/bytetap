import { describe, expect, it } from "vitest";
import { base64ByteLength, parseEncoding, textSegments, type Encoding } from "./bytes";

/** Байты CP1251 для кириллицы: «Привет». */
const CP1251_PRIVET = [0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2];

function text(bytes: readonly number[], encoding: Encoding): string {
  return textSegments(Uint8Array.from(bytes), encoding)
    .filter((segment) => !segment.chip)
    .map((segment) => segment.value)
    .join("");
}

describe("textSegments", () => {
  it("показывает управляющие байты плашками FR-22, а не символами", () => {
    const segments = textSegments(Uint8Array.from([0x0b, 0x4d, 0x53, 0x48, 0x0d, 0x1c]), "cp1251");

    expect(segments).toEqual([
      { chip: true, value: "<VT>" },
      { chip: false, value: "MSH" },
      { chip: true, value: "<CR>" },
      { chip: true, value: "<FS>" },
    ]);
  });

  it("не даёт тексту притвориться плашкой: `<` выводится как <x3C>", () => {
    const bytes = Array.from(new TextEncoder().encode("<STX>"));
    expect(textSegments(Uint8Array.from(bytes), "utf8")).toEqual([
      { chip: true, value: "<x3C>" },
      { chip: false, value: "STX>" },
    ]);
  });

  it("ASCII показывает байты ≥0x80 плашками <xHH>", () => {
    const segments = textSegments(Uint8Array.from([0x41, 0xff, 0x7f]), "ascii");
    expect(segments).toEqual([
      { chip: false, value: "A" },
      { chip: true, value: "<xFF>" },
      { chip: true, value: "<x7F>" },
    ]);
  });

  it("декодирует кириллицу в CP1251", () => {
    expect(text(CP1251_PRIVET, "cp1251")).toBe("Привет");
    expect(text(CP1251_PRIVET, "latin1")).not.toBe("Привет");
  });

  it("декодирует кириллицу в UTF-8", () => {
    const bytes = Array.from(new TextEncoder().encode("Привет"));
    expect(text(bytes, "utf8")).toBe("Привет");
  });

  it("декодирует GBK", () => {
    expect(text([0xc4, 0xe3, 0xba, 0xc3], "gbk")).toBe("你好");
  });

  it("Latin-1 показывает байты старшей половины латиницей", () => {
    expect(text([0xe9, 0x20, 0xf1], "latin1")).toBe("é ñ");
  });

  it("управляющие байты не мешают декодированию соседних кусков", () => {
    const bytes = [0x0b, ...CP1251_PRIVET, 0x0d];
    const segments = textSegments(Uint8Array.from(bytes), "cp1251");
    expect(segments.map((segment) => segment.value)).toEqual(["<VT>", "Привет", "<CR>"]);
  });

  it("пустой фрагмент не даёт сегментов", () => {
    expect(textSegments(new Uint8Array(0), "cp1251")).toEqual([]);
  });
});

describe("длина и настройки", () => {
  it("считает длину base64 в байтах", () => {
    expect(base64ByteLength(btoa("MLLP"))).toBe(4);
    expect(base64ByteLength(btoa("0123456789ab"))).toBe(12);
    expect(base64ByteLength("")).toBe(0);
  });

  it("не пропускает неизвестную кодировку из настроек", () => {
    expect(parseEncoding("cp1251")).toBe("cp1251");
    expect(parseEncoding("koi8-r")).toBeNull();
    expect(parseEncoding(42)).toBeNull();
  });
});
