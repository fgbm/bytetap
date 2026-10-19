/**
 * Хранилище настроек: `settings.json` в папке данных приложения внутри Tauri,
 * localStorage в mock и тестах. Схема одна, меняется только носитель (AC11).
 */

import type { BridgeKind } from "../api/bridge";
import type { Settings } from "./settings";

export const SETTINGS_FILE = "settings.json";
export const SETTINGS_KEY = "bytetap.settings";

export interface SettingsStorage {
  load(): Promise<unknown>;
  save(settings: Settings): Promise<void>;
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
        return null;
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
      const store = await load(file, { autoSave: false });
      return (await store.get<unknown>("settings")) ?? null;
    },
    async save(settings) {
      const { load } = await import("@tauri-apps/plugin-store");
      const store = await load(file, { autoSave: false });
      await store.set("settings", settings);
      await store.save();
    },
  };
}

export function createSettingsStorage(kind: BridgeKind): SettingsStorage {
  return kind === "tauri" ? tauriSettingsStorage() : localSettingsStorage();
}
