/**
 * Хранилище настроек: `settings.json` в папке данных приложения внутри Tauri,
 * localStorage в mock и тестах. Схема одна, меняется только носитель (AC11).
 */

import type { BridgeKind } from "../api/bridge";
import type { Settings } from "./settings";

export const SETTINGS_FILE = "settings.json";
export const SETTINGS_KEY = "bytetap.settings";
/** Ключ копии испорченных настроек в mock-хранилище. */
export const CORRUPT_KEY = "bytetap.settings.corrupt";

export interface SettingsStorage {
  load(): Promise<unknown>;
  save(settings: Settings): Promise<void>;
}

/** Сохранение есть, но прочитать его нельзя: файл испорчен, а не отсутствует. */
export class CorruptSettingsError extends Error {
  constructor(message = "настройки не читаются") {
    super(message);
    this.name = "CorruptSettingsError";
  }
}

export function localSettingsStorage(storage?: Storage): SettingsStorage {
  return {
    async load() {
      const store = storage ?? globalThis.localStorage;
      if (!store) return null;
      const raw = store.getItem(SETTINGS_KEY);
      if (raw === null) return null;
      try {
        return JSON.parse(raw) as unknown;
      } catch {
        throw new CorruptSettingsError();
      }
    },
    async save(settings) {
      const store = storage ?? globalThis.localStorage;
      store?.setItem(SETTINGS_KEY, JSON.stringify(settings));
    },
  };
}

/** plugin-store грузится лениво: в mock и тестах Tauri-плагин не нужен. */
export function tauriSettingsStorage(file = SETTINGS_FILE): SettingsStorage {
  return {
    async load() {
      const { load } = await import("@tauri-apps/plugin-store");
      try {
        const store = await load(file, { autoSave: false });
        return (await store.get<unknown>("settings")) ?? null;
      } catch (error) {
        // Плагин падает и на испорченном JSON: это не «настроек нет» (AC11).
        throw new CorruptSettingsError(String(error));
      }
    },
    async save(settings) {
      const { load } = await import("@tauri-apps/plugin-store");
      const store = await load(file, { autoSave: false });
      await store.set("settings", settings);
      await store.save();
    },
  };
}

/**
 * Копия испорченного `settings.json` рядом с оригиналом (AC11).
 *
 * В Tauri копию делает Rust: у интерфейса нет доступа к файлам, а настройки сейчас
 * будут перезаписаны значениями по умолчанию. В mock копия ложится в localStorage.
 */
export async function preserveCorruptSettings(kind: BridgeKind): Promise<string | null> {
  if (kind === "tauri") {
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      return await invoke<string | null>("preserve_corrupt_settings");
    } catch {
      return null;
    }
  }
  const raw = globalThis.localStorage?.getItem(SETTINGS_KEY);
  if (raw === undefined || raw === null) return null;
  const copy = `${CORRUPT_KEY}-${new Date().toISOString().replace(/[:.]/g, "-")}`;
  globalThis.localStorage?.setItem(copy, raw);
  return copy;
}

export function createSettingsStorage(kind: BridgeKind): SettingsStorage {
  return kind === "tauri" ? tauriSettingsStorage() : localSettingsStorage();
}
