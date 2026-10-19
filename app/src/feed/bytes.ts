/**
 * Байты фрагмента и их показ текстом: плашки управляющих символов (FR-22)
 * и кодировки отображения (FR-27).
 *
 * Кодировка влияет только на показ: в журнал всегда пишутся исходные байты.
 * Текст режется на плашки до декодирования, поэтому управляющий байт внутри
 * многобайтовой последовательности никогда не попадёт в декодер.
 */

import { segments, type Segment } from "../../../shared/control-chips";
import type { DataEvent } from "../api/types";

/** Кодировки отображения текста (FR-27). */
export type Encoding = "ascii" | "cp1251" | "utf8" | "gbk" | "latin1";

/** Порядок кодировок в панели ленты. */
export const ENCODINGS: readonly Encoding[] = ["ascii", "cp1251", "utf8", "gbk", "latin1"];

/** Ключи подписей кодировок в локали. */
export const ENCODING_KEYS: Record<Encoding, string> = {
  ascii: "feed.encAscii",
  cp1251: "feed.encCp1251",
  utf8: "feed.encUtf8",
  gbk: "feed.encGbk",
  latin1: "feed.encLatin1",
};

/** Метки `TextDecoder`; ASCII разбирается собственной функцией. */
const DECODER_ENCODINGS: Record<Exclude<Encoding, "ascii">, string> = {
  cp1251: "windows-1251",
  utf8: "utf-8",
  gbk: "gbk",
  latin1: "iso-8859-1",
};

/** Сколько байт фрагмента показывается в строке ленты: остальное заменяется на «…». */
export const TEXT_LIMIT_BYTES = 2048;

/** Имена управляющих байтов по FR-22; остальные управляющие — `<xHH>`. */
const CONTROL_NAMES: Record<number, string> = {
  0x02: "<STX>",
  0x03: "<ETX>",
  0x04: "<EOT>",
  0x05: "<ENQ>",
  0x06: "<ACK>",
  0x0a: "<LF>",
  0x0b: "<VT>",
  0x0d: "<CR>",
  0x15: "<NAK>",
  0x17: "<ETB>",
  0x1c: "<FS>",
};

const decoders = new Map<Encoding, TextDecoder>();

/** Управляющий байт: по FR-22 показывается плашкой, а не текстом. */
export function isControlByte(byte: number): boolean {
  return byte < 0x20 || byte === 0x7f;
}

/** Форма байта в тексте: имя управляющего символа или `<xHH>`. */
export function controlToken(byte: number): string {
  return CONTROL_NAMES[byte] ?? `<x${byte.toString(16).toUpperCase().padStart(2, "0")}>`;
}

/** Base64 → байты; битые данные дают пустой фрагмент, а не исключение. */
export function base64ToBytes(b64: string): Uint8Array {
  let text: string;
  try {
    text = atob(b64);
  } catch {
    return new Uint8Array(0);
  }
  const bytes = new Uint8Array(text.length);
  for (let index = 0; index < text.length; index += 1) {
    bytes[index] = text.charCodeAt(index) & 0xff;
  }
  return bytes;
}

/** Длина фрагмента в байтах без декодирования base64. */
export function base64ByteLength(b64: string): number {
  const padding = b64.endsWith("==") ? 2 : b64.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((b64.length * 3) / 4) - padding);
}

function decoderFor(encoding: Encoding): TextDecoder | null {
  if (encoding === "ascii") return null;
  const cached = decoders.get(encoding);
  if (cached !== undefined) return cached;
  try {
    const created = new TextDecoder(DECODER_ENCODINGS[encoding], { fatal: false });
    decoders.set(encoding, created);
    return created;
  } catch {
    // Среда без нужной таблицы кодировки: байты показываются как `<xHH>`.
    return null;
  }
}

function printableAsciiRun(run: readonly number[]): string {
  let out = "";
  for (const byte of run) {
    // `<` экранируется по FR-22, иначе текст прибора притворится плашкой.
    out += byte === 0x3c ? "<x3C>" : String.fromCharCode(byte);
  }
  return out;
}

function decodedRun(run: readonly number[], encoding: Encoding): string {
  const decoder = decoderFor(encoding);
  const bytes = Uint8Array.from(run);
  const text = decoder === null ? null : decoder.decode(bytes);
  if (text === null) return run.map(controlToken).join("");
  return text.replace(/</g, "<x3C>");
}

/**
 * Текст фрагмента плашками: управляющие байты `<0x20` и `0x7F` (в ASCII ещё
 * и `≥0x80`) — плашки, печатные куски — декодированные выбранной кодировкой.
 */
export function textSegments(
  bytes: Uint8Array,
  encoding: Encoding,
  limit = TEXT_LIMIT_BYTES,
): Segment[] {
  const end = Math.min(bytes.length, Math.max(0, limit));
  const parts: string[] = [];
  let run: number[] = [];

  const flush = (): void => {
    if (run.length === 0) return;
    parts.push(encoding === "ascii" ? printableAsciiRun(run) : decodedRun(run, encoding));
    run = [];
  };

  for (let index = 0; index < end; index += 1) {
    const byte = bytes[index];
    if (isControlByte(byte) || (encoding === "ascii" && byte >= 0x80)) {
      flush();
      parts.push(controlToken(byte));
    } else {
      run.push(byte);
    }
  }
  flush();

  if (bytes.length > end) parts.push("…");
  return segments(parts.join(""));
}

const cache = new WeakMap<DataEvent, Map<Encoding, Segment[]>>();

/** Текст фрагмента с кешем на событие: строка декодируется один раз на кодировку. */
export function eventSegments(event: DataEvent, encoding: Encoding): Segment[] {
  let byEncoding = cache.get(event);
  if (byEncoding === undefined) {
    byEncoding = new Map<Encoding, Segment[]>();
    cache.set(event, byEncoding);
  }
  const cached = byEncoding.get(encoding);
  if (cached !== undefined) return cached;
  const value = textSegments(base64ToBytes(event.b64), encoding);
  byEncoding.set(encoding, value);
  return value;
}

/** Проверка значения из настроек: неизвестная кодировка не проходит. */
export function parseEncoding(value: unknown): Encoding | null {
  return typeof value === "string" && (ENCODINGS as readonly string[]).includes(value)
    ? (value as Encoding)
    : null;
}
