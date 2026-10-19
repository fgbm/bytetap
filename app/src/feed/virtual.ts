/**
 * Виртуализация ленты: в DOM живут только видимые строки плюс небольшой запас
 * сверху и снизу (FR-28, AC7).
 *
 * Высоты строк постоянные у каждого вида, поэтому раскладка считается без
 * измерения элементов: позиция строки — её номер, умноженный на высоту.
 *
 * Распорка прокрутки ограничена [`MAX_SCROLL_PX`]: браузер не держит список выше
 * десяти миллионов пикселей. Длинный журнал поэтому сжимается: пиксель прокрутки
 * переводится в номер строки пропорцией, а рядом с якорем — смещением, чтобы шаг
 * колеса оставался шагом в одну строку.
 */

/** Высота строки текстового вида; равна токену `--bt-lane-height`. */
export const TEXT_ROW_HEIGHT = 28;

/** Высота строки HEX-дампа: 16 байт, кегль 12px, отступы 3px и разделитель. */
export const HEX_ROW_HEIGHT = 25;

/** Строк выше и ниже видимого окна: прокрутка без пустых полос. */
export const OVERSCAN_ROWS = 8;

/** Высота окна до первой раскладки — jsdom и первый кадр до измерения. */
export const DEFAULT_VIEWPORT_HEIGHT = 400;

/**
 * Предел высоты распорки: выше браузер прокручивает неточно или не прокручивает вовсе.
 *
 * 10 000 000 пикселей — это 357 000 строк текстового вида; журнал длиннее виден
 * через сжатое отображение прокрутки в номера строк.
 */
export const MAX_SCROLL_PX = 10_000_000;

/**
 * Пиксели вокруг якоря, где прокрутка считается точной.
 *
 * Пока прокрутка не ушла от якоря дальше, строка считается смещением на высоту строки:
 * колесо и небольшое перетаскивание двигают ленту по одной строке. Дальше — ползунок,
 * клавиши и seek — идёт пропорция по всей высоте.
 */
export const ANCHOR_SPAN_PX = 2_000;

/** Диапазон строк `[start, end)`. */
export interface RowRange {
  start: number;
  end: number;
}

/**
 * Якорь точной прокрутки: строка на пикселе `top`.
 *
 * Строка дробная — её округляет отрисовка, — иначе мелкие шаги прокрутки терялись бы
 * при каждой пересадке якоря.
 */
export interface ScrollAnchor {
  top: number;
  row: number;
}

/** Высота строки для расчётов: нулевая высота не должна делить на ноль. */
function rowStep(rowHeight: number): number {
  return Math.max(1, rowHeight);
}

/** Сжата ли распорка пределом: пиксель прокрутки дороже одной строки. */
export function isScaled(total: number, rowHeight: number): boolean {
  return Math.max(0, total) * rowStep(rowHeight) > MAX_SCROLL_PX;
}

/** Высота всего списка для подстановки в распорку прокрутки: не выше предела. */
export function listHeight(total: number, rowHeight: number): number {
  const full = Math.max(0, total) * rowStep(rowHeight);
  return Math.min(full, MAX_SCROLL_PX);
}

/** Пиксели, на которые можно прокрутить список: высота содержимого минус окно. */
export function scrollTravel(total: number, viewportHeight: number, rowHeight: number): number {
  return Math.max(0, listHeight(total, rowHeight) - Math.max(0, viewportHeight));
}

/** Номер строки, не выходящий за список. */
function clampRow(row: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(Math.max(0, total - 1), Math.max(0, row));
}

/** Сколько строк окна видно: частичная нижняя строка тоже видна. */
export function visibleRows(viewportHeight: number, rowHeight: number): number {
  return Math.max(1, Math.ceil(Math.max(0, viewportHeight) / rowStep(rowHeight)));
}

/**
 * Первая строка у нижнего края: последняя строка стоит у нижнего края окна.
 *
 * Считать краем последнюю строку первой видимой нельзя: под ней до нижнего края
 * окна осталась бы пустая полоса, а выше конца списка нечего рисовать.
 */
function lastFirstRow(total: number, viewportHeight: number, rowHeight: number): number {
  return Math.max(0, total - visibleRows(viewportHeight, rowHeight));
}

/**
 * Строка на пикселе прокрутки без округления.
 *
 * Дробная часть нужна якорю: шаг колеса или трекпада меньше строки копится от шага
 * к шагу, а не теряется при каждой пересадке якоря. Округляет только
 * [`firstVisibleRow`] — в момент отрисовки.
 */
export function exactFirstRow(
  scrollTop: number,
  total: number,
  viewportHeight: number,
  rowHeight: number,
  anchor: ScrollAnchor | null = null,
): number {
  if (total <= 0) return 0;
  const height = rowStep(rowHeight);
  const at = Math.max(0, scrollTop);
  // Верх списка — нулевая строка, даже если якорь остался от прошлой прокрутки.
  if (at <= 0) return 0;
  if (!isScaled(total, height)) return Math.floor(at / height);
  const travel = scrollTravel(total, viewportHeight, height);
  const last = lastFirstRow(total, viewportHeight, height);
  // Нижний край — последняя строка у нижнего края окна.
  if (travel > 0 && at >= travel) return last;
  if (anchor !== null && Math.abs(at - anchor.top) <= ANCHOR_SPAN_PX) {
    return Math.min(last, Math.max(0, anchor.row + (at - anchor.top) / height));
  }
  if (travel <= 0) return 0;
  return (at / travel) * last;
}

/**
 * Первая видимая строка при данной прокрутке.
 *
 * Без сжатия это просто `scrollTop / высота`. Со сжатой распоркой рядом с якорем
 * строка считается смещением от него (шаг колеса — одна строка), а далёкий прыжок
 * ползунком или seek — пропорцией по всей высоте.
 */
export function firstVisibleRow(
  scrollTop: number,
  total: number,
  viewportHeight: number,
  rowHeight: number,
  anchor: ScrollAnchor | null = null,
): number {
  return clampRow(
    Math.round(exactFirstRow(scrollTop, total, viewportHeight, rowHeight, anchor)),
    total,
  );
}

/** Пиксель прокрутки, на котором строка стоит первой видимой: обратное к [`firstVisibleRow`]. */
export function scrollTopForRow(
  row: number,
  total: number,
  viewportHeight: number,
  rowHeight: number,
): number {
  if (total <= 0) return 0;
  const height = rowStep(rowHeight);
  if (!isScaled(total, height)) return Math.max(0, row) * height;
  const travel = scrollTravel(total, viewportHeight, height);
  const last = lastFirstRow(total, viewportHeight, height);
  // У нижнего края стоит строка `last`, и всё, что ниже неё, прокручивается в конец.
  return Math.min(travel, (Math.max(0, row) / Math.max(1, last)) * travel);
}

/** Диапазон строк `[start, end)` вокруг первой видимой строки. */
export function rangeFromRow(
  total: number,
  firstVisible: number,
  viewportHeight: number,
  rowHeight: number,
  overscan = OVERSCAN_ROWS,
): RowRange {
  if (total <= 0) return { start: 0, end: 0 };
  const height = rowStep(rowHeight);
  const visible = Math.ceil(Math.max(0, viewportHeight) / height) + 1;
  const start = Math.max(0, firstVisible - overscan);
  const end = Math.min(total, firstVisible + visible + overscan);
  return { start, end: Math.max(start, end) };
}

/** Видимые строки при данной прокрутке. */
export function visibleRange(
  total: number,
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number,
  overscan = OVERSCAN_ROWS,
  anchor: ScrollAnchor | null = null,
): RowRange {
  const first = firstVisibleRow(scrollTop, total, viewportHeight, rowHeight, anchor);
  return rangeFromRow(total, first, viewportHeight, rowHeight, overscan);
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
