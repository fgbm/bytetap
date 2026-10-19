/**
 * Тема приложения: системная по умолчанию, переключатель запоминается (AC13).
 *
 * Системная тема — отсутствие атрибута `data-theme`: значения приходят из
 * `design/tokens-system.css` по `prefers-color-scheme`. Переключатель ставит
 * `data-theme="light|dark"` на `<html>`.
 */

import type { ThemeMode } from "./settings/settings";

export function applyTheme(mode: ThemeMode, root: HTMLElement = document.documentElement): void {
  if (mode === "system") delete root.dataset.theme;
  else root.dataset.theme = mode;
}

/** Цикл переключателя: системная → светлая → тёмная → системная. */
export function nextTheme(mode: ThemeMode): ThemeMode {
  if (mode === "system") return "light";
  if (mode === "light") return "dark";
  return "system";
}
