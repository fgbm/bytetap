import { describe, expect, it } from "vitest";
import { hexDump, hexLine, hexLineCount, HEX_BYTES_PER_LINE } from "./hex";

/** MLLP-начало: <VT>MSH|…<CR><FS><CR> — 20 байт. */
const FRAME = Uint8Array.from([
  0x0b, 0x4d, 0x53, 0x48, 0x7c, 0x5e, 0x7e, 0x5c,
  0x26, 0x7c, 0x41, 0x4e, 0x41, 0x4c, 0x59, 0x5a,
  0x0d, 0x1c, 0x0d, 0x41,
]);

describe("hexDump", () => {
  it("режет фрагмент по 16 байт со смещением и ASCII-колонкой", () => {
    const lines = hexDump(FRAME);

    expect(HEX_BYTES_PER_LINE).toBe(16);
    expect(lines).toHaveLength(2);
    expect(lines[0].offset).toBe("0000");
    expect(lines[0].bytes).toHaveLength(16);
    expect(lines[0].bytes[0]).toEqual({ value: "0B", control: true });
    expect(lines[0].bytes[1]).toEqual({ value: "4D", control: false });
    expect(lines[0].ascii).toBe(".MSH|^~\\&|ANALYZ");
    expect(lines[1].offset).toBe("0010");
    expect(lines[1].bytes).toHaveLength(4);
    expect(lines[1].ascii).toBe("...A");
  });

  it("подсвечивает управляющие байты, включая 0x7F", () => {
    const line = hexLine(Uint8Array.from([0x00, 0x1f, 0x7f, 0x20, 0x41]), 0);
    expect(line.bytes.map((byte) => byte.control)).toEqual([true, true, true, false, false]);
  });

  it("пустой фрагмент — одна пустая строка", () => {
    expect(hexDump(new Uint8Array(0))).toEqual([{ offset: "0000", bytes: [], ascii: "" }]);
  });

  it("считает строки дампа по длине", () => {
    expect(hexLineCount(0)).toBe(1);
    expect(hexLineCount(1)).toBe(1);
    expect(hexLineCount(16)).toBe(1);
    expect(hexLineCount(17)).toBe(2);
    expect(hexLineCount(512)).toBe(32);
  });

  it("отдаёт нужную строку без построения всего дампа", () => {
    expect(hexLine(FRAME, 1).offset).toBe("0010");
    expect(hexLine(FRAME, 5).bytes).toEqual([]);
  });
});
