/**
 * Строки ленты: фрагмент или служебное событие в представлении экрана (FR-26).
 *
 * Модель строки — чистые данные: время `HH:MM:SS.mmm`, направление, соединение,
 * иконка и класс. Текст фрагмента собирается сегментами по кодировке, служебные
 * строки переводятся через i18n и не хранят русских слов в коде.
 *
 * UI v2: управляющие символы — не плашки, а `‹CR›` (глиф в угловых скобках),
 * поэтому сегмент-чип здесь же получает угловые скобки-глифы.
 */

import type { FeedEvent } from "../api/types";
import { formatTime } from "../format";
import type { Segment } from "../../../shared/control-chips";
import { eventSegments, type Encoding } from "./bytes";
import { eventText } from "./event-codes";

/** Иконка строки; Lucide-компонент выбирает по ней шаблон. */
export type RowIcon = "a2l" | "l2a" | "open" | "close" | "error" | "info" | "note" | "stopped";

/** Перевод: тот же `t` из vue-i18n, но без его перегрузок. */
export type Translate = (key: string, params?: Record<string, unknown>) => string;

export interface FeedRow {
  /** Порядковый номер события в буфере — ключ строки и позиция прокрутки. */
  index: number;
  kind: FeedEvent["type"];
  /** CSS-класс строки: `a2l`, `l2a`, `svc`, `ev`, `err`. */
  cls: string;
  time: string;
  conn: string;
  icon: RowIcon;
  synthetic: boolean;
  /** Готовый текст служебной строки; у фрагмента пуст — текст идёт сегментами. */
  text: string;
  /** Сегменты текста фрагмента (FR-22, FR-27). */
  segments: Segment[];
  event: FeedEvent;
}

/** Кто закрыл соединение — коды формата журнала. */
const CLOSED_BY_KEYS: Record<string, string> = {
  analyzer: "feed.byAnalyzer",
  lis: "feed.byLis",
  bytetap: "feed.byApp",
};

function localized(codes: Record<string, string>, code: string, t: Translate): string {
  const key = codes[code];
  return key === undefined ? code : t(key);
}

/** «#1 подключился 192.168.1.51:49152» и «#1 отключился · анализатор» (UI v2). */
function closeText(event: Extract<FeedEvent, { type: "close" }>, t: Translate): string {
  if (event.by === "") return t("feed.closed", { conn: event.conn });
  return t("feed.closedSimple", {
    conn: event.conn,
    by: localized(CLOSED_BY_KEYS, event.by, t),
  });
}

function connLabel(conn: number | undefined): string {
  return conn === undefined ? "" : `#${conn}`;
}

/** `<CR>` → `‹CR›`: глиф вместо плашки, фоном управляющие символы больше не выделяются. */
function glyph(segment: Segment): Segment {
  if (!segment.chip) return segment;
  return { chip: true, value: `‹${segment.value.slice(1, -1)}›` };
}

/**
 * Строка события вместе с текстом фрагмента по выбранной кодировке.
 * Кеш декодирования живёт в `bytes.ts`, поэтому повторные вызовы дешёвы.
 */
export function eventRow(
  event: FeedEvent,
  index: number,
  t: Translate,
  encoding: Encoding,
): FeedRow {
  const base: FeedRow = {
    index,
    kind: event.type,
    cls: "ev",
    time: "t" in event ? formatTime(event.t) : "",
    conn: "",
    icon: "info",
    synthetic: false,
    text: "",
    segments: [],
    event,
  };

  switch (event.type) {
    case "data":
      return {
        ...base,
        cls: event.dir,
        conn: connLabel(event.conn),
        icon: event.dir,
        synthetic: event.synthetic,
        segments: eventSegments(event, encoding).map(glyph),
      };
    case "open":
      return {
        ...base,
        cls: "svc",
        conn: connLabel(event.conn),
        icon: "open",
        text: t("feed.opened", { conn: event.conn, peer: event.peer }),
      };
    case "close":
      return { ...base, cls: "svc", conn: connLabel(event.conn), icon: "close", text: closeText(event, t) };
    case "error":
      return {
        ...base,
        cls: "err",
        conn: connLabel(event.conn),
        icon: "error",
        text: eventText(event.code, event.message, t),
      };
    case "info":
      return {
        ...base,
        conn: connLabel(event.conn),
        icon: "info",
        text: eventText(event.code, event.message, t),
      };
    case "note":
      return { ...base, icon: "note", text: event.text };
    case "stopped":
      return { ...base, icon: "stopped", text: t("feed.stopped") };
    default:
      return base;
  }
}
