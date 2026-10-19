/**
 * Мост к ядру: один интерфейс, две реализации.
 *
 * `tauriBridge` вызывает команды Tauri и получает события через Channel, `mockBridge`
 * отдаёт те же типы и синтетический обмен без окна и ядра. Выбор — по `VITE_BRIDGE`
 * или по отсутствию `window.__TAURI_INTERNALS__` (браузер, тесты, `npm run dev`).
 */

import type {
  FeedBatch,
  FeedEvent,
  InterfaceInfo,
  JournalSummary,
  SessionConfig,
  SessionSummary,
  StartInfo,
} from "./types";
import { createMockBridge } from "./mock-bridge";
import { createTauriBridge } from "./tauri-bridge";

export interface Bridge {
  /** IPv4-адреса ноутбука без loopback. */
  listInterfaces(): Promise<InterfaceInfo[]>;
  /** Папка журналов по умолчанию. */
  defaultLogDir(): Promise<string>;
  /** Предупреждение о OneDrive или сетевом пути; `null` — папка в порядке. */
  logDirWarning(dir: string): Promise<string | null>;
  /**
   * Запускает сессию. События приходят пачками в `onBatch`; отписка не нужна —
   * поток закрывается вместе с командой `stopSession`.
   */
  startSession(cfg: SessionConfig, onBatch: (batch: FeedBatch) => void): Promise<StartInfo>;
  stopSession(): Promise<SessionSummary>;
  addSessionNote(text: string): Promise<void>;
  openJournal(path: string): Promise<JournalSummary>;
  readJournal(path: string, from: number, count: number): Promise<FeedEvent[]>;
  appendNote(path: string, text: string): Promise<void>;
  pickJournal(): Promise<string | null>;
  pickLogDir(): Promise<string | null>;
  openLogDir(dir: string): Promise<void>;
}

/** Работает ли приложение внутри Tauri (в браузере и тестах — нет). */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export type BridgeKind = "tauri" | "mock";

/** `VITE_BRIDGE=mock` включает заглушку даже внутри Tauri (демонстрация без ядра). */
export function bridgeKind(): BridgeKind {
  const mode = import.meta.env.VITE_BRIDGE;
  if (mode === "mock") return "mock";
  if (mode === "tauri") return "tauri";
  return isTauri() ? "tauri" : "mock";
}

function createBridge(): Bridge {
  return bridgeKind() === "tauri" ? createTauriBridge() : createMockBridge();
}

/** Мост приложения. Тесты и компоненты могут принимать `Bridge` параметром. */
export const bridge: Bridge = createBridge();
