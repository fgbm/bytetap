/**
 * Форматирование чисел, времени и таймера сессии для строки состояния и кнопки «Стоп».
 *
 * Числа и даты идут через vue-i18n (`n()` и `d()`): разряды, десятичная запятая и порядок
 * даты — свойства локали, а не константы интерфейса. Единицы измерения — ключи `ru.json`.
 */

import { i18n } from "./i18n";

/** Неразрывный пробел: число и единица измерения не переносятся на новую строку. */
const NBSP = "\u00a0";

function number(value: number, format: "integer" | "oneDecimal"): string {
  return i18n.global.n(value, format);
}

/** Размер с единицей: целые значения без дробной части, дробные — с одной цифрой. */
function size(value: number, unit: string): string {
  const rounded = Math.round(value * 10) / 10;
  const format = Number.isInteger(rounded) ? "integer" : "oneDecimal";
  return `${number(rounded, format)}${NBSP}${unit}`;
}

/** Байты: «512 Б», «10,2 КБ», «1,2 МБ», «3,4 ГБ» (FR-25, счётчики в шапке записи). */
export function formatBytes(bytes: number): string {
  const safe = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  if (safe >= 1024 ** 3) return size(safe / 1024 ** 3, i18n.global.t("units.gigabytes"));
  if (safe >= 1024 ** 2) return size(safe / 1024 ** 2, i18n.global.t("units.megabytes"));
  if (safe >= 1024) return size(safe / 1024, i18n.global.t("units.kilobytes"));
  return `${number(Math.round(safe), "integer")}${NBSP}${i18n.global.t("units.bytes")}`;
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

/** Дата и время для заголовка журнала: «19.10.2026, 10:15:00» по правилам локали. */
export function formatDateTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return i18n.global.d(date, "journal");
}

/** Начало записи для списка последних журналов: «сегодня 10:15», «вчера 18:02», «17.10 09:40». */
export function formatRecentTime(iso: string, now = new Date()): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  const day = (value: Date): number =>
    new Date(value.getFullYear(), value.getMonth(), value.getDate()).getTime();
  const days = Math.round((day(now) - day(date)) / 86_400_000);
  if (days === 0) return `${i18n.global.t("units.today")} ${time}`;
  if (days === 1) return `${i18n.global.t("units.yesterday")} ${time}`;
  return `${pad(date.getDate())}.${pad(date.getMonth() + 1)} ${time}`;
}

/** Длительность записи словами: «42 мин», «2 ч 05 мин»; пустой журнал — пустая строка. */
export function formatDurationSpan(us: number): string {
  const total = Math.max(0, Math.floor(us / 1_000_000));
  if (total === 0) return "";
  const minutes = Math.floor(total / 60);
  const minutesKey = i18n.global.t("units.minutes");
  if (minutes < 60) return `${Math.max(1, minutes)}${NBSP}${minutesKey}`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return `${hours}${NBSP}${i18n.global.t("units.hours")} ${pad(rest)}${NBSP}${minutesKey}`;
}
