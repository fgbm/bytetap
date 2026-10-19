/**
 * Постраничное чтение журнала для ленты (FR-29, M2-AC10).
 *
 * Журнал бывает в сотни мегабайт, поэтому в память интерфейса он не загружается: лента
 * знает общее число событий из сводки, а строки приходят страницами по [`JOURNAL_PAGE`]
 * через `read_journal`. Загруженные страницы лежат в LRU на [`JOURNAL_PAGES`] штук —
 * прокрутка вперёд вытесняет старые, и память не растёт с длиной сессии.
 */

import type { FeedEvent } from "../api/types";

/** Сколько событий в одной странице журнала. */
export const JOURNAL_PAGE = 500;

/** Сколько страниц журнала держим в памяти. */
export const JOURNAL_PAGES = 20;

/** Загрузчик страницы: `from` — номер события, `count` — сколько просить. */
export type PageLoader = (from: number, count: number) => Promise<FeedEvent[]>;

interface Page {
  from: number;
  events: FeedEvent[];
}

export class JournalFeed {
  readonly path: string;
  /** Растёт, когда приходит страница: по нему лента пересчитывает видимые строки. */
  version = 0;

  private total: number;
  private readonly load: PageLoader;
  private readonly pages = new Map<number, Page>();
  private readonly pending = new Set<number>();
  private readonly failed = new Set<number>();
  /** Длины событий из индекса журнала: нужны для высот HEX-строк (AC5). */
  private lengths: number[];

  constructor(path: string, total: number, load: PageLoader, lengths: number[] = []) {
    this.path = path;
    this.total = Math.max(0, Math.floor(total));
    this.load = load;
    this.lengths = lengths.slice();
  }

  /** Длина события в байтах из индекса; `null` — индекс не дал её. */
  byteLength(index: number): number | null {
    const length = this.lengths[index];
    return length === undefined ? null : length;
  }

  get length(): number {
    return this.total;
  }

  /** Сколько страниц сейчас в памяти: для теста LRU и диагностики. */
  get pagesInMemory(): number {
    return this.pages.size;
  }

  /** Событие по порядку или `undefined`, если страница ещё едет. */
  at(index: number): FeedEvent | undefined {
    if (!Number.isInteger(index) || index < 0 || index >= this.total) return undefined;
    const page = Math.floor(index / JOURNAL_PAGE);
    const cached = this.touch(page);
    if (cached !== undefined) return cached.events[index - cached.from];
    this.request(page);
    return undefined;
  }

  /** Просит страницы, покрывающие строки `[start, end)` видимого окна. */
  ensure(start: number, end: number): void {
    if (end <= start || this.total === 0) return;
    const last = this.pageCount - 1;
    const first = Math.max(0, Math.floor(start / JOURNAL_PAGE));
    const to = Math.min(last, Math.floor((end - 1) / JOURNAL_PAGE));
    for (let page = first; page <= to; page += 1) this.request(page);
  }

  /** В журнал дописан комментарий: событий стало больше (FR-23). */
  extend(total: number): void {
    const next = Math.max(0, Math.floor(total));
    if (next <= this.total) return;
    this.total = next;
    // Комментарий — служебная строка: своей длины для HEX у него нет.
    while (this.lengths.length < next) this.lengths.push(0);
    // Новое событие всегда в последней странице; её и перечитываем, иначе дописанный
    // комментарий не виден ни в метаданных (последние `note`), ни в ленте до прокрутки.
    const last = this.pageCount - 1;
    this.pages.delete(last);
    this.pending.delete(last);
    this.failed.delete(last);
    this.version += 1;
    this.ensure(this.total - 1, this.total);
  }

  /** Последние комментарии журнала, новые первыми, — из уже загруженных страниц. */
  notes(limit = 3): string[] {
    const found: string[] = [];
    const indices = [...this.pages.keys()].sort((left, right) => right - left);
    for (const page of indices) {
      const cached = this.pages.get(page);
      if (cached === undefined) continue;
      for (let index = cached.events.length - 1; index >= 0; index -= 1) {
        const event = cached.events[index];
        if (event.type === "note") found.push(event.text);
        if (found.length >= limit) return found;
      }
    }
    return found;
  }

  /** Забыть загруженное: журнал закрыт или открыт другой. */
  clear(): void {
    this.pages.clear();
    this.pending.clear();
    this.failed.clear();
    this.total = 0;
    this.version += 1;
  }

  private get pageCount(): number {
    return Math.ceil(this.total / JOURNAL_PAGE);
  }

  /** Страница из кеша с отметкой «только что использована» (LRU). */
  private touch(page: number): Page | undefined {
    const found = this.pages.get(page);
    if (found === undefined) return undefined;
    this.pages.delete(page);
    this.pages.set(page, found);
    return found;
  }

  /** Загружает страницу один раз; отказ не повторяется, чтобы прокрутка не зациклилась. */
  private request(page: number): void {
    if (this.pages.has(page) || this.pending.has(page) || this.failed.has(page)) return;
    const from = page * JOURNAL_PAGE;
    const count = Math.min(JOURNAL_PAGE, this.total - from);
    if (count <= 0) return;
    this.pending.add(page);
    void this.load(from, count).then(
      (events) => {
        this.pending.delete(page);
        this.store(page, from, events);
      },
      () => {
        this.pending.delete(page);
        this.failed.add(page);
      },
    );
  }

  private store(page: number, from: number, events: FeedEvent[]): void {
    // Пока страница ехала, журнал могли закрыть: тогда её место уже не здесь.
    if (from >= this.total) return;
    this.pages.set(page, { from, events });
    while (this.pages.size > JOURNAL_PAGES) {
      const oldest = this.pages.keys().next().value;
      if (oldest === undefined) break;
      this.pages.delete(oldest);
    }
    this.version += 1;
  }
}
