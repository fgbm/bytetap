/**
 * Расчёт трассы: импульсы бегущего окна и плотность всей сессии в координаты (AC9).
 *
 * Чистые функции без canvas: экран только умножает доли на ширину дорожки,
 * поэтому раскладка проверяется vitest без браузера.
 */

/** Ширина бегущего окна во время записи: последние ~60 с. */
export const TRACE_WINDOW_US = 60_000_000;

/** Число корзин плотности в журнале (`JournalSummary.density`). */
export const DENSITY_BINS = 600;

/** Фрагмент для трассы: когда пришёл и сколько в нём байт. */
export interface Point {
  monoUs: number;
  len: number;
}

/** Импульс в долях дорожки: `x` — положение, `height` — высота. */
export interface Impulse {
  x: number;
  height: number;
}

/** Нижняя граница высоты импульса: столбик короче 30% дорожки не читается. */
const IMPULSE_MIN = 0.3;

/** Длина фрагмента, на которой импульс занимает всю дорожку. */
const IMPULSE_FULL_LEN = 4096;

/**
 * Высота импульса по длине фрагмента — от 30% до 100% высоты дорожки.
 *
 * Логарифм различает 3 байта и 3 КБ, а нижняя граница держит короткие служебные
 * кадры (ENQ, ACK) видимыми: иначе ритм обмена пропадает в плоской полосе.
 */
export function impulseHeight(len: number): number {
  const share = Math.min(1, Math.log10(Math.max(1, len)) / Math.log10(IMPULSE_FULL_LEN));
  return IMPULSE_MIN + (1 - IMPULSE_MIN) * share;
}

/** Окно «последние 60 с»: до заполнения окна — от нуля, дальше — скользит. */
export function windowRange(nowUs: number, span = TRACE_WINDOW_US): { fromUs: number; toUs: number } {
  const toUs = Math.max(span, nowUs);
  return { fromUs: toUs - span, toUs };
}

/** Импульсы точек, попавших в окно `[fromUs, toUs]`, в долях ширины. */
export function impulses(points: readonly Point[], fromUs: number, toUs: number): Impulse[] {
  const span = toUs - fromUs;
  if (span <= 0) return [];
  const out: Impulse[] = [];
  for (const point of points) {
    if (point.monoUs < fromUs || point.monoUs > toUs) continue;
    out.push({ x: (point.monoUs - fromUs) / span, height: impulseHeight(point.len) });
  }
  return out;
}

/** Плотность корзин → доли высоты дорожки; пустой журнал даёт нули. */
export function densityBars(values: readonly number[], bins = DENSITY_BINS): number[] {
  const out = new Array<number>(bins).fill(0);
  const max = values.reduce((top, value) => Math.max(top, value), 0);
  if (max <= 0) return out;
  for (let index = 0; index < bins; index += 1) {
    const value = values[index] ?? 0;
    out[index] = Math.min(1, Math.max(0, value / max));
  }
  return out;
}

/** Доля ширины по координате указателя внутри области трассы. */
export function fractionFromX(x: number, rect: { left: number; width: number }): number {
  if (rect.width <= 0) return 0;
  return Math.min(1, Math.max(0, (x - rect.left) / rect.width));
}

/**
 * Номер события для доли времени на трассе журнала (AC9).
 *
 * Плотность нарисована по времени, а лента считает строки событиями. Таблица
 * `firstEvent` связывает их: `firstEvent[b]` — первое событие корзины `b`, поэтому
 * доля 0.99 попадает в событие, а не в пропорцию номеров строк.
 */
export function eventAtFraction(
  firstEvent: readonly number[],
  total: number,
  fraction: number,
  bins = DENSITY_BINS,
): number {
  if (total <= 0 || firstEvent.length === 0) return 0;
  const point = Math.min(bins, Math.round(Math.min(1, Math.max(0, fraction)) * bins));
  // Правая граница сессии событий не содержит: переходим к последнему.
  return Math.max(0, Math.min(total - 1, firstEvent[point] ?? total));
}

/**
 * Доля времени для номера события: корзина, в которой оно первое (AC9).
 *
 * Обратное к [`eventAtFraction`]: рамка видимой области показывает время первой
 * видимой строки, а не её место в списке строк.
 */
export function fractionAtEvent(
  firstEvent: readonly number[],
  index: number,
  bins = DENSITY_BINS,
): number {
  if (firstEvent.length === 0) return 0;
  let low = 0;
  let high = bins;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if ((firstEvent[middle] ?? Number.POSITIVE_INFINITY) <= index) low = middle;
    else high = middle - 1;
  }
  return low / bins;
}

/** Рамка видимой области в долях ширины. */
export interface BrushRect {
  left: number;
  width: number;
}

/** Рамка видимой области ленты; без окна — вся ширина. */
export function frameRect(window: { start: number; end: number } | null): BrushRect {
  if (window === null) return { left: 0, width: 1 };
  const start = Math.min(1, Math.max(0, window.start));
  const end = Math.min(1, Math.max(start, window.end));
  return { left: start, width: end - start };
}
