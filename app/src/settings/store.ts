/**
 * Состояние настроек приложения: последняя конфигурация, тема, профили (AC11).
 *
 * Единственный экземпляр на приложение; компоненты читают его через `useSettings()`.
 * Запись откладывается, чтобы ввод в поля не дёргал диск на каждую букву.
 */

import { reactive } from "vue";
import { bridgeKind } from "../api/bridge";
import type { Encoding } from "../feed/bytes";
import { applyTheme } from "../theme";
import {
  defaultSettings,
  findProfile,
  lastRunOf,
  normalizeSettings,
  removeProfile,
  type AppConfig,
  type FeedViewMode,
  type LastRun,
  type Profile,
  type Settings,
  type ThemeMode,
  upsertProfile,
} from "./settings";
import {
  CorruptSettingsError,
  createSettingsStorage,
  preserveCorruptSettings,
  type SettingsStorage,
} from "./storage";

const SAVE_DELAY_MS = 400;

const storage: SettingsStorage = createSettingsStorage(bridgeKind());

/** Почему настройки не прочитались: интерфейс работает на значениях по умолчанию. */
export interface SettingsWarning {
  code: "reset" | "corrupt";
  /** Куда положена копия испорченного файла. */
  path: string;
}

const state = reactive<
  Settings & { loaded: boolean; warning: SettingsWarning | null }
>({
  ...defaultSettings(),
  loaded: false,
  warning: null,
});

let saveTimer: ReturnType<typeof setTimeout> | null = null;

function snapshot(): Settings {
  return {
    config: { ...state.config, serial: { ...state.config.serial } },
    theme: state.theme,
    profiles: state.profiles.map((profile) => ({
      ...profile,
      config: { ...profile.config, serial: { ...profile.config.serial } },
    })),
    feed: { ...state.feed },
    lastRun: state.lastRun === null ? null : { ...state.lastRun },
  };
}

function scheduleSave(): void {
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    void storage.save(snapshot());
  }, SAVE_DELAY_MS);
}

/** Немедленная запись: перед закрытием окна и в тестах. */
export async function flushSettings(): Promise<void> {
  if (saveTimer !== null) {
    clearTimeout(saveTimer);
    saveTimer = null;
  }
  await storage.save(snapshot());
}

/**
 * Читает сохранённые настройки и никогда не бросает: ошибка чтения — строка предупреждения
 * и значения по умолчанию, а не оборванная загрузка окружения (AC11). Испорченный файл
 * перед перезаписью копируется рядом: `settings.corrupt-<дата>.json`.
 */
export async function loadSettings(): Promise<void> {
  let raw: unknown = null;
  try {
    raw = await storage.load();
    state.warning = null;
  } catch (error) {
    const corrupt = error instanceof CorruptSettingsError;
    const path = corrupt ? ((await preserveCorruptSettings(bridgeKind())) ?? "") : "";
    state.warning = corrupt ? { code: "corrupt", path } : { code: "reset", path: "" };
  }
  const loaded = normalizeSettings(raw, state.config.logDir);
  state.config = loaded.config;
  state.theme = loaded.theme;
  state.profiles = loaded.profiles;
  state.feed = loaded.feed;
  state.lastRun = loaded.lastRun;
  state.loaded = true;
  applyTheme(state.theme);
  if (state.warning !== null) void flushSettings();
}

/** Закрыть предупреждение о настройках. */
export function dismissSettingsWarning(): void {
  state.warning = null;
}

/** Стартовая логика: папка журналов по умолчанию, если своих настроек ещё нет. */
export function ensureLogDir(dir: string): void {
  if (state.config.logDir.trim() === "" && dir !== "") {
    state.config.logDir = dir;
    scheduleSave();
  }
}

export function updateConfig(patch: Partial<AppConfig>): void {
  state.config = { ...state.config, ...patch };
  scheduleSave();
}

/** Поля COM-порта правятся по одному: вложенный объект не должен теряться. */
export function updateSerial(patch: Partial<AppConfig["serial"]>): void {
  state.config = { ...state.config, serial: { ...state.config.serial, ...patch } };
  scheduleSave();
}

/**
 * Старт состоялся: карточка этого сценария выделяется на стартовом экране,
 * а к описанию добавляется «последний раз: …» (UI v2).
 */
export function rememberLastRun(config: AppConfig = state.config): void {
  state.lastRun = lastRunOf(config);
  scheduleSave();
}

export function setTheme(theme: ThemeMode): void {
  state.theme = theme;
  applyTheme(theme);
  scheduleSave();
}

/** Вид ленты (текст или HEX) запоминается в настройках (FR-26, AC11). */
export function setFeedView(view: FeedViewMode): void {
  state.feed.view = view;
  scheduleSave();
}

/** Кодировка отображения запоминается в настройках (FR-27, AC11). */
export function setFeedEncoding(encoding: Encoding): void {
  state.feed.encoding = encoding;
  scheduleSave();
}

export function saveProfile(name: string): void {
  state.profiles = upsertProfile(state.profiles, name, state.config);
  scheduleSave();
}

export function deleteProfile(name: string): void {
  state.profiles = removeProfile(state.profiles, name);
  scheduleSave();
}

export function applyProfile(name: string): boolean {
  const profile: Profile | null = findProfile(state.profiles, name);
  if (profile === null) return false;
  state.config = { ...profile.config, serial: { ...profile.config.serial } };
  scheduleSave();
  // Смена профиля — заметное решение: пишем сразу, а не через отложенный таймер (AC11).
  void flushSettings();
  return true;
}

/** Только для тестов: вернуть настройки к значениям по умолчанию. */
export function resetSettings(): void {
  Object.assign(state, defaultSettings(), { loaded: false, warning: null });
}

export function useSettings() {
  return {
    state,
    loadSettings,
    ensureLogDir,
    updateConfig,
    updateSerial,
    rememberLastRun,
    setTheme,
    setFeedView,
    setFeedEncoding,
    saveProfile,
    deleteProfile,
    applyProfile,
    flushSettings,
    dismissSettingsWarning,
  };
}

export type { LastRun };
