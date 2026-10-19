/** Представление события ленты текстом: до шага 3 — просто читаемый фрагмент. */

import type { FeedEvent } from "../api/types";

const CONTROL: Record<number, string> = {
  2: "<STX>",
  3: "<ETX>",
  4: "<EOT>",
  5: "<ENQ>",
  6: "<ACK>",
  10: "<LF>",
  11: "<VT>",
  13: "<CR>",
  28: "<FS>",
};

/** Base64 → строка байт (по символу на байт); кодировки отображения — шаг 3. */
export function decodeBase64(b64: string): string {
  try {
    return atob(b64);
  } catch {
    return "";
  }
}

/** Управляющие символы показываются плашками-именами, остальные — как есть. */
export function previewText(text: string, limit = 200): string {
  let out = "";
  for (const char of text) {
    const code = char.charCodeAt(0);
    if (CONTROL[code] !== undefined) out += CONTROL[code];
    else if (code < 32) out += `<x${code.toString(16).padStart(2, "0")}>`;
    else out += char;
    if (out.length >= limit) return `${out.slice(0, limit)}…`;
  }
  return out;
}

/** Текст события для простого списка шага 2; служебные строки — без данных. */
export function eventText(event: FeedEvent): string {
  switch (event.type) {
    case "data":
      return previewText(decodeBase64(event.b64));
    case "error":
    case "info":
      return event.message;
    case "note":
      return event.text;
    case "close":
      return event.reason;
    default:
      return "";
  }
}
