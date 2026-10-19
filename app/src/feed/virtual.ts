/**
 * Виртуализация ленты: в DOM живут только видимые строки плюс небольшой запас
 * сверху и снизу (FR-28, AC7).
 *
 * Высоты строк постоянные у каждого вида, поэтому раскладка считается без
 * измерения элементов: позиция строки — её номер, умноженный на высоту.
 */

/** Высота строки текстового вида; равна токену `--bt-lane-height`. */
export const TEXT_ROW_HEIGHT = 28;

/** Высота строки HEX-дампа: 16 байт, кегль 12px, отступы 3px и разделитель. */
export const HEX_ROW_HEIGHT = 25;

/** Строк выше и ниже видимого окна: прокрутка без пустых полос. */
export const OVERSCAN_ROWS = 8;

/** Высота окна до первой раскладки — jsdom и первый кадр до измерения. */
export const DEFAULT_VIEWPORT_HEIGHT = 400;

/** Диапазон строк `[start, end)`. */
export interface RowRange {
  start: number;
  end: number;
}

/** Видимые строки при данной прокрутке. */
export function visibleRange(
  total: number,
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
  overscan = OVERSCAN_ROWS,
): RowRange {
  if (total <= 0) return { start: 0, end: 0 };
  const height = Math.max(1, rowHeight);
  const firstVisible = Math.floor(Math.max(0, scrollTop) / height);
  const visible = Math.ceil(Math.max(0, viewportHeight) / height) + 1;
  const start = Math.max(0, firstVisible - overscan);
  const end = Math.min(total, firstVisible + visible + overscan);
  return { start, end: Math.max(start, end) };
}

/** Высота всего списка для подстановки в распорку прокрутки. */
export function listHeight(total: number, rowHeight: number): number {
  return Math.max(0, total) * Math.max(1, rowHeight);
}

/** Порог «у нижнего края»: дробные пиксели прокрутки не должны выключать догон. */
export const BOTTOM_THRESHOLD = 4;

/** Стоит ли прокрутка у нижнего края. */
export function isAtBottom(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
  threshold = BOTTOM_THRESHOLD,
): boolean {
  return scrollHeight - (scrollTop + clientHeight) <= threshold;
}

/** Состояние автопрокрутки ленты (AC8). */
export interface AutoScroll {
  follow: boolean;
}

/**
 * Реакция на прокрутку: у нижнего края автопрокрутка включается, после ухода
 * вверх — выключается. Кнопка включает её принудительно.
 */
export function applyScroll(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
): AutoScroll {
  return { follow: isAtBottom(scrollTop, clientHeight, scrollHeight) };
}

/** Кнопка «Автопрокрутка»: включить догон и прыгнуть вниз. */
export function enableAutoScroll(): AutoScroll {
  return { follow: true };
}
