/** Форматирование чисел, времени и таймера сессии для строки состояния и кнопки «Стоп». */

/** Неразрывный пробел: группы разрядов и единица измерения не переносятся на новую строку. */
const NBSP = "\u00a0";

function group(value: string): string {
  return value.replace(/\B(?=(\d{3})+(?!\d))/g, NBSP);
}

/** Байты: «10 240 Б», «1,2 МБ», «3,4 ГБ» (FR-25, строка состояния). */
export function formatBytes(bytes: number): string {
  const safe = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  if (safe >= 1024 ** 3) return `${(safe / 1024 ** 3).toFixed(1).replace(".", ",")}${NBSP}ГБ`;
  if (safe >= 1024 ** 2) return `${(safe / 1024 ** 2).toFixed(1).replace(".", ",")}${NBSP}МБ`;
  return `${group(String(Math.round(safe)))}${NBSP}Б`;
}

function pad(value: number, width = 2): string {
  return String(value).padStart(width, "0");
}

/** Длительность: «14:07» до часа, «01:14:07» после. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  return hours > 0
    ? `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`
    : `${pad(minutes)}:${pad(seconds)}`;
}

/** Время фрагмента: «HH:MM:SS.mmm» по локальному времени. */
export function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "--:--:--.---";
  return (
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}` +
    `.${pad(date.getMilliseconds(), 3)}`
  );
}

/** Дата и время для заголовка журнала: «19.10.2026 10:15:00». */
export function formatDateTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return (
    `${pad(date.getDate())}.${pad(date.getMonth() + 1)}.${date.getFullYear()} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}
