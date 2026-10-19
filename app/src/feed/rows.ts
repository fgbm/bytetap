/**
 * Строки ленты: фрагмент или служебное событие в представлении экрана (FR-26).
 *
 * Модель строки — чистые данные: время `HH:MM:SS.mmm`, направление, соединение,
 * иконка и класс. Текст фрагмента собирается сегментами по кодировке, служебные
 * строки переводятся через i18n и не хранят русских слов в коде.
 */

import type { FeedEvent } from "../api/types";
import { formatTime } from "../format";
import type { Segment } from "../../../shared/control-chips";
import { eventSegments, type Encoding } from "./bytes";

/** Иконка строки; Lucide-компонент выбирает по ней шаблон. */
export type RowIcon = "a2l" | "l2a" | "open" | "close" | "error" | "info" | "note" | "stopped";

/** Перевод: тот же `t` из vue-i18n, но без его перегрузок. */
export type Translate = (key: string, params?: Record<string, unknown>) => string;

export interface FeedRow {
  /** Порядковый номер события в буфере — ключ строки и позиция прокрутки. */
  index: number;
  kind: FeedEvent["type"];
  /** CSS-класс строки: `a2l`, `l2a`, `syn`, `ev`, `wn`. */
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

/** Причина закрытия — коды формата журнала. */
const CLOSE_REASON_KEYS: Record<string, string> = {
  eof: "feed.reasonEof",
  reset: "feed.reasonReset",
  stop: "feed.reasonStop",
  error: "feed.reasonError",
  timeout: "feed.reasonTimeout",
};

function localized(codes: Record<string, string>, code: string, t: Translate): string {
  const key = codes[code];
  return key === undefined ? code : t(key);
}

function closeText(event: Extract<FeedEvent, { type: "close" }>, t: Translate): string {
  if (event.by === "" && event.reason === "") return t("feed.closed");
  return t("feed.closedBy", {
    by: localized(CLOSED_BY_KEYS, event.by, t),
    reason: localized(CLOSE_REASON_KEYS, event.reason, t),
  });
}

function connLabel(conn: number | undefined): string {
  return conn === undefined ? "" : `#${conn}`;
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
        cls: event.dir + (event.synthetic ? " syn" : ""),
        conn: connLabel(event.conn),
        icon: event.dir,
        synthetic: event.synthetic,
        segments: eventSegments(event, encoding),
      };
    case "open":
      return {
        ...base,
        conn: connLabel(event.conn),
        icon: "open",
        text: t("feed.opened", { peer: event.peer, local: event.local }),
      };
    case "close":
      return { ...base, conn: connLabel(event.conn), icon: "close", text: closeText(event, t) };
    case "error":
      return {
        ...base,
        cls: "wn",
        conn: connLabel(event.conn),
        icon: "error",
        text: event.message,
      };
    case "info":
      return { ...base, conn: connLabel(event.conn), icon: "info", text: event.message };
    case "note":
      return { ...base, icon: "note", text: event.text };
    case "stopped":
      return { ...base, icon: "stopped", text: t("feed.stopped") };
    default:
      return base;
  }
}
