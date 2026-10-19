/**
 * Тема приложения: системная по умолчанию, переключатель запоминается (AC13).
 *
 * Системная тема — отсутствие атрибута `data-theme`: значения приходят из
 * `design/tokens-system.css` по `prefers-color-scheme`. Переключатель ставит
 * `data-theme="light|dark"` на `<html>`.
 *
 * Выбранная тема дополнительно кешируется в localStorage: `main.ts` читает кеш
 * синхронно до монтирования, поэтому окно не вспыхивает чужой темой, пока загружаются
 * настройки из `settings.json`. Встроенный скрипт в `index.html` запрещён CSP.
 */

import type { ThemeMode } from "./settings/settings";

/** Ключ кеша темы: только он читается синхронно при старте. */
export const THEME_KEY = "bytetap.theme";

function isThemeMode(value: string | null): value is ThemeMode {
  return value === "system" || value === "light" || value === "dark";
}

/** Тема из кеша для синхронного применения до монтирования. */
export function cachedTheme(): ThemeMode | null {
  try {
    const value = globalThis.localStorage?.getItem(THEME_KEY) ?? null;
    return isThemeMode(value) ? value : null;
  } catch {
    return null;
  }
}

export function applyTheme(mode: ThemeMode, root: HTMLElement = document.documentElement): void {
  if (mode === "system") delete root.dataset.theme;
  else root.dataset.theme = mode;
  try {
    globalThis.localStorage?.setItem(THEME_KEY, mode);
  } catch {
    // Приватный режим браузера: тема применится, но не запомнится.
  }
}

/**
 * Тема, которая сейчас на экране: системная разворачивается по `prefers-color-scheme`.
 *
 * Переключатель в шапке показывает глиф темы, в которую переведёт (Sun/Moon),
 * поэтому ему нужна фактическая тема, а не режим.
 */
export function resolvedTheme(mode: ThemeMode): "light" | "dark" {
  if (mode !== "system") return mode;
  const dark =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches;
  return dark ? "dark" : "light";
}
