/**
 * Реализация моста поверх Tauri v2.
 *
 * Всё локальное (FR-33): файловые команды ядра — `pick_journal`, `pick_log_dir`,
 * `open_log_dir` (диалог и проводник на стороне Rust), настройки и профили —
 * plugin-store в `settings.json`. Сети приложение не касается.
 */

import { Channel, invoke } from "@tauri-apps/api/core";
import type { Bridge } from "./bridge";
import {
  BridgeError,
  type BridgeErrorCode,
  type BridgeErrorPayload,
  type FeedBatch,
  type FeedEvent,
  type InterfaceInfo,
  type JournalSummary,
  type LogDirWarning,
  type SessionConfig,
  type SessionStatus,
  type SessionSummary,
  type StartInfo,
} from "./types";

const CODES: BridgeErrorCode[] = [
  "port_in_use",
  "address_unavailable",
  "permission_denied",
  "log_dir",
  "already_running",
  "journal",
  "journal_open",
  "io",
];

function isBridgeErrorPayload(value: unknown): value is BridgeErrorPayload {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as { code?: unknown; message?: unknown };
  return (
    typeof candidate.code === "string" &&
    CODES.includes(candidate.code as BridgeErrorCode) &&
    typeof candidate.message === "string"
  );
}

/** Команда Tauri отвергает промис объектом `{code, message, params}`; переводим его в `BridgeError`. */
export function toBridgeError(error: unknown): BridgeError {
  if (error instanceof BridgeError) return error;
  if (isBridgeErrorPayload(error)) {
    return new BridgeError(error.code, error.message, error.params ?? {});
  }
  const message = error instanceof Error ? error.message : String(error);
  return new BridgeError("io", message);
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw toBridgeError(error);
  }
}

export function createTauriBridge(): Bridge {
  return {
    listInterfaces: () => call<InterfaceInfo[]>("list_interfaces"),
    defaultLogDir: () => call<string>("default_log_dir"),
    logDirWarning: (dir) => call<LogDirWarning | null>("log_dir_warning", { dir }),
    sessionStatus: () => call<SessionStatus>("session_status"),

    async startSession(cfg: SessionConfig, onBatch: (batch: FeedBatch) => void) {
      const channel = new Channel<FeedBatch>();
      channel.onmessage = onBatch;
      return call<StartInfo>("start_session", { cfg, channel });
    },

    stopSession: (generation) => call<SessionSummary>("stop_session", { generation }),
    addSessionNote: (text) => call<void>("add_session_note", { text }),

    openJournal: (path) => call<JournalSummary>("open_journal", { path }),
    readJournal: (path, from, count) => call<FeedEvent[]>("read_journal", { path, from, count }),
    journalLengths: (path) => call<number[]>("journal_lengths", { path }),
    appendNote: (path, text) => call<void>("append_note", { path, text }),

    async pickJournal() {
      return (await call<string | null>("pick_journal")) ?? null;
    },

    async pickLogDir() {
      return (await call<string | null>("pick_log_dir")) ?? null;
    },

    openLogDir: async (dir) => {
      await call<void>("open_log_dir", { dir });
    },
  };
}
