/**
 * Чистый редьюсер состояний сессии: `idle → starting → recording → stopping → idle`,
 * плюс `failed` (отказ журнала) и `viewing` (просмотр журнала, шаг 4).
 *
 * Ни DOM, ни таймеров, ни моста — только состояние и действия, поэтому проверяется vitest.
 */

import type {
  BridgeErrorCode,
  FeedBatch,
  JournalSummary,
  SessionStats,
  SessionSummary,
  StartInfo,
} from "../api/types";
import { base64ByteLength } from "../feed/bytes";

export type SessionStatus =
  | "idle"
  | "starting"
  | "recording"
  | "stopping"
  | "failed"
  | "viewing";

/** Ошибка команды в состоянии: код для перевода, текст — для отладки. */
export interface SessionError {
  code: BridgeErrorCode;
  message: string;
  params: Record<string, string>;
}

export interface SessionState {
  status: SessionStatus;
  /** Запущенная сессия: путь к журналу, адрес прослушивания, время старта. */
  startInfo: StartInfo | null;
  /** Накопленная статистика; после стопа заменяется итогами `stop_session`. */
  stats: SessionStats;
  /** Сколько событий ленты отброшено переполненной очередью интерфейса (AC16). */
  dropped: number;
  /** Ошибка старта или остановки; показывается у поля и в баннере. */
  error: SessionError | null;
  /** Отказ журнала: сессия остановлена, записанное сохранено (AC15). */
  journalFailure: { message: string } | null;
  /** Итоги последней остановленной сессии. */
  summary: SessionSummary | null;
  /** Открытый журнал в режиме просмотра. */
  journal: JournalSummary | null;
}

export type SessionAction =
  | { type: "start_requested" }
  | { type: "started"; info: StartInfo }
  | { type: "start_failed"; error: SessionError }
  | { type: "stop_requested" }
  | { type: "stopped"; summary: SessionSummary | null }
  | { type: "stop_failed"; error: SessionError }
  | { type: "journal_failed"; message: string }
  | { type: "failure_dismissed" }
  | { type: "batch"; batch: FeedBatch }
  | { type: "journal_opened"; journal: JournalSummary }
  | { type: "journal_closed" }
  | { type: "error_dismissed" }
  | { type: "command_failed"; error: SessionError };

export const EMPTY_STATS: SessionStats = {
  connections: 0,
  bytesAnalyzerToLis: 0,
  bytesLisToAnalyzer: 0,
};

export function initialSessionState(): SessionState {
  return {
    status: "idle",
    startInfo: null,
    stats: { ...EMPTY_STATS },
    dropped: 0,
    error: null,
    journalFailure: null,
    summary: null,
    journal: null,
  };
}

/** Счётчики байтов и соединений по пачке событий. */
function addStats(base: SessionStats, batch: FeedBatch): SessionStats {
  const next = { ...base };
  for (const event of batch.events) {
    if (event.type === "data") {
      const bytes = base64ByteLength(event.b64);
      if (event.dir === "a2l") next.bytesAnalyzerToLis += bytes;
      else next.bytesLisToAnalyzer += bytes;
    }
    if (event.type === "open") next.connections += 1;
  }
  return next;
}

/** Применяет действие и возвращает новое состояние; неизвестный переход не меняет состояние. */
export function reduceSession(state: SessionState, action: SessionAction): SessionState {
  switch (action.type) {
    case "start_requested":
      if (state.status !== "idle" && state.status !== "failed" && state.status !== "viewing") {
        return state;
      }
      return {
        ...initialSessionState(),
        status: "starting",
      };

    case "started":
      if (state.status !== "starting") return state;
      return { ...state, status: "recording", startInfo: action.info, error: null };

    case "start_failed":
      if (state.status !== "starting" && state.status !== "recording") return state;
      return { ...state, status: "idle", startInfo: null, error: action.error };

    case "stop_requested":
      if (state.status !== "recording") return state;
      return { ...state, status: "stopping" };

    case "stopped":
      if (state.status !== "stopping" && state.status !== "failed") return state;
      return {
        ...state,
        status: "idle",
        startInfo: null,
        summary: action.summary,
        error: null,
        journalFailure: null,
        stats: action.summary ? { ...action.summary.stats } : state.stats,
      };

    case "stop_failed":
      return { ...state, status: "failed", error: action.error };

    case "journal_failed":
      return {
        ...state,
        status: "failed",
        startInfo: null,
        journalFailure: { message: action.message },
      };

    case "failure_dismissed":
      if (state.status !== "failed") return state;
      return { ...state, status: "idle", journalFailure: null, error: null };

    case "batch":
      if (state.status !== "recording" && state.status !== "failed") return state;
      return {
        ...state,
        stats: addStats(state.stats, action.batch),
        dropped: state.dropped + action.batch.dropped,
      };

    case "journal_opened":
      if (state.status === "recording" || state.status === "starting" || state.status === "stopping") {
        return state;
      }
      return { ...state, status: "viewing", journal: action.journal, journalFailure: null };

    case "journal_closed":
      if (state.status !== "viewing") return state;
      return { ...state, status: "idle", journal: null };

    case "error_dismissed":
      return state.error ? { ...state, error: null } : state;

    case "command_failed":
      return { ...state, error: action.error };

    default:
      return state;
  }
}
