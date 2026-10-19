/** Выборка событий ленты: все или только выбранное соединение (AC8). */

import type { FeedEvent } from "../api/types";
import type { FeedSource } from "./source";

/** Номера соединений в ленте по возрастанию — для чипов фильтра. */
export function connsOf(feed: FeedSource<FeedEvent>): number[] {
  const seen = new Set<number>();
  for (let index = 0; index < feed.length; index += 1) {
    const event = feed.at(index);
    if (event !== undefined && "conn" in event && typeof event.conn === "number") {
      seen.add(event.conn);
    }
  }
  return [...seen].sort((left, right) => left - right);
}

/** Сколько последних соединений показывается чипами: ряд не растёт с длиной сессии. */
export const CONN_CHIPS = 8;

/** Последние соединения ленты для панели фильтра. */
export function connChips(feed: FeedSource<FeedEvent>): number[] {
  return connsOf(feed).slice(-CONN_CHIPS);
}

/**
 * Индексы событий выбранного соединения; `null` — все.
 *
 * События без соединения (`note`, `stopped`) — контекст всей сессии, поэтому
 * остаются в ленте при любом фильтре.
 */
export function filterIndices(feed: FeedSource<FeedEvent>, conn: number | null): number[] {
  const indices: number[] = [];
  for (let index = 0; index < feed.length; index += 1) {
    if (conn === null) {
      indices.push(index);
      continue;
    }
    const event = feed.at(index);
    const eventConn = event !== undefined && "conn" in event ? event.conn : undefined;
    if (eventConn === undefined || eventConn === conn) indices.push(index);
  }
  return indices;
}
