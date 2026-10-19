/**
 * Кольцевой буфер ленты: 10 000 последних событий в памяти (FR-28, AC7).
 *
 * События пишутся в свободный слот, поэтому добавление — O(1): пачка не копирует
 * весь массив. `version` растёт на каждое изменение, и интерфейс подписывается
 * на него, а не на массив событий: глубокой реактивности у ленты нет.
 */

/** Ёмкость буфера ленты: 10 000 последних фрагментов и служебных событий (AC7). */
export const FEED_CAPACITY = 10_000;

/** Кольцевой буфер фиксированной ёмкости: старые события вытесняются новыми. */
export class EventRing<T> {
  private readonly slots: (T | undefined)[];
  private readonly size: number;
  private start = 0;
  private count = 0;

  constructor(capacity: number = FEED_CAPACITY) {
    this.size = Math.max(1, Math.floor(capacity));
    this.slots = new Array<T | undefined>(this.size);
  }

  get length(): number {
    return this.count;
  }

  get capacity(): number {
    return this.size;
  }

  /** Добавляет событие; при переполнении вытесняет самое старое. */
  push(item: T): void {
    this.slots[(this.start + this.count) % this.size] = item;
    if (this.count < this.size) this.count += 1;
    else this.start = (this.start + 1) % this.size;
  }

  pushMany(items: readonly T[]): void {
    for (const item of items) this.push(item);
  }

  /** Событие по порядку в буфере: 0 — самое старое. */
  at(index: number): T {
    if (!Number.isInteger(index) || index < 0 || index >= this.count) {
      throw new RangeError(`нет события ${index}: в буфере ${this.count}`);
    }
    return this.slots[(this.start + index) % this.size] as T;
  }

  clear(): void {
    this.slots.fill(undefined);
    this.start = 0;
    this.count = 0;
  }

  /** Снимок буфера по порядку: для тестов и разовой выгрузки. */
  toArray(): T[] {
    const out: T[] = [];
    for (let index = 0; index < this.count; index += 1) out.push(this.at(index));
    return out;
  }
}

/**
 * Буфер ленты вместе с номером версии.
 *
 * Компоненты читают `version` в computed, чтобы пересчитать видимые строки;
 * сами события остаются обычными объектами без реактивных прокси.
 */
export class FeedBuffer<T> {
  readonly ring: EventRing<T>;
  version = 0;

  constructor(capacity: number = FEED_CAPACITY) {
    this.ring = new EventRing<T>(capacity);
  }

  get length(): number {
    return this.ring.length;
  }

  get capacity(): number {
    return this.ring.capacity;
  }

  at(index: number): T {
    return this.ring.at(index);
  }

  pushMany(items: readonly T[]): void {
    if (items.length === 0) return;
    this.ring.pushMany(items);
    this.version += 1;
  }

  clear(): void {
    this.ring.clear();
    this.version += 1;
  }

  toArray(): T[] {
    return this.ring.toArray();
  }
}
