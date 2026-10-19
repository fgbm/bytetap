/**
 * HEX-дамп фрагмента по 16 байт: смещение, байты с подсветкой управляющих,
 * ASCII-колонка (FR-26, AC5).
 *
 * Строка собирается по требованию — виртуализатору нужна только видимая, —
 * поэтому `hexDump` обходит фрагмент целиком, а `hexLine` отдаёт одну строку.
 */

/** Байт в строке дампа: значение и признак управляющего байта. */
export interface HexByte {
  value: string;
  control: boolean;
}

/** Строка дампа: смещение, до 16 байт и их ASCII-колонка. */
export interface HexLine {
  offset: string;
  bytes: HexByte[];
  ascii: string;
}

export const HEX_BYTES_PER_LINE = 16;

function hexByte(byte: number): HexByte {
  return {
    value: byte.toString(16).toUpperCase().padStart(2, "0"),
    control: byte < 0x20 || byte === 0x7f,
  };
}

function formatLine(slice: Uint8Array, offset: number): HexLine {
  const bytes: HexByte[] = [];
  let ascii = "";
  for (const byte of slice) {
    bytes.push(hexByte(byte));
    ascii += byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : ".";
  }
  return { offset: offset.toString(16).toUpperCase().padStart(4, "0"), bytes, ascii };
}

/** Сколько строк дампа занимает фрагмент; пустой фрагмент — тоже строка. */
export function hexLineCount(length: number): number {
  return Math.max(1, Math.ceil(Math.max(0, length) / HEX_BYTES_PER_LINE));
}

/** Одна строка дампа по номеру строки. */
export function hexLine(bytes: Uint8Array, line: number): HexLine {
  const offset = Math.max(0, Math.floor(line)) * HEX_BYTES_PER_LINE;
  return formatLine(bytes.subarray(offset, offset + HEX_BYTES_PER_LINE), offset);
}

/** Полный дамп фрагмента. */
export function hexDump(bytes: Uint8Array): HexLine[] {
  const lines: HexLine[] = [];
  for (let offset = 0; offset < bytes.length; offset += HEX_BYTES_PER_LINE) {
    lines.push(hexLine(bytes, offset / HEX_BYTES_PER_LINE));
  }
  if (lines.length === 0) lines.push(hexLine(bytes, 0));
  return lines;
}
