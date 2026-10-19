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
  LogDirWarning,
  RecentJournal,
  SerialPort,
  SessionConfig,
  SessionStatus,
  SessionSummary,
  StartInfo,
} from "./types";
import { createMockBridge } from "./mock-bridge";
import { createTauriBridge } from "./tauri-bridge";

export interface Bridge {
  /** IPv4-адреса ноутбука без loopback. */
  listInterfaces(): Promise<InterfaceInfo[]>;
  /** Порты COM для формы режима «Кабель COM-порта» (FR-12). */
  listSerialPorts(): Promise<SerialPort[]>;
  /** Папка журналов по умолчанию. */
  defaultLogDir(): Promise<string>;
  /** Предупреждение о OneDrive или сетевом пути; `null` — папка в порядке. */
  logDirWarning(dir: string): Promise<LogDirWarning | null>;
  /**
   * Последние журналы папки для стартового экрана. `null` — команды в мосте ещё нет:
   * блок «Последние записи» скрывается, а не показывает ошибку.
   */
  listRecentJournals(dir: string, limit: number): Promise<RecentJournal[] | null>;
  /** Идёт ли запись и что вернул её старт: для перезагруженной страницы (AC1). */
  sessionStatus(): Promise<SessionStatus>;
  /**
   * Запускает сессию. События приходят пачками в `onBatch`; отписка не нужна —
   * поток закрывается вместе с командой `stopSession`.
   */
  startSession(cfg: SessionConfig, onBatch: (batch: FeedBatch) => void): Promise<StartInfo>;
  /**
   * Останавливает сессию. `generation` — поколение из `StartInfo`: мост остановит только
   * свою сессию, а сессию, запущенную после перезагрузки страницы, не тронет (NFR-5).
   */
  stopSession(generation?: number): Promise<SessionSummary>;
  addSessionNote(text: string): Promise<void>;
  openJournal(path: string): Promise<JournalSummary>;
  readJournal(path: string, from: number, count: number): Promise<FeedEvent[]>;
  /** Длины событий журнала: по ним лента считает высоты HEX-строк (AC5). */
  journalLengths(path: string): Promise<number[]>;
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
