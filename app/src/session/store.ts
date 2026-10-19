/**
 * Состояние сессии приложения: старт, стоп, события ленты, таймер, просмотр журнала.
 *
 * Лента пока — простой массив с лимитом; шаг 3 заменит его кольцевым буфером на
 * 10 000 фрагментов и виртуализацией. Редьюсер состояний живёт отдельно
 * (`reducer.ts`) и проверяется vitest без Vue.
 */

import { ref } from "vue";
import { bridge } from "../api/bridge";
import {
  BridgeError,
  type BridgeErrorCode,
  type FeedBatch,
  type FeedEvent,
  type JournalSummary,
  type SessionConfig,
} from "../api/types";
import { initialSessionState, reduceSession, type SessionAction, type SessionError, type SessionState } from "./reducer";

/** Предел простого массива ленты до появления кольцевого буфера (шаг 3). */
export const FEED_LIMIT = 2000;

const state = ref<SessionState>(initialSessionState());
const events = ref<FeedEvent[]>([]);
const elapsedMs = ref(0);

let ticker: ReturnType<typeof setInterval> | null = null;

function dispatch(action: SessionAction): void {
  state.value = reduceSession(state.value, action);
}

function toSessionError(error: unknown): SessionError {
  if (error instanceof BridgeError) {
    return { code: error.code, message: error.message, params: error.params };
  }
  return {
    code: "io",
    message: error instanceof Error ? error.message : String(error),
    params: {},
  };
}

function appendEvents(incoming: FeedEvent[]): void {
  const merged = events.value.concat(incoming);
  events.value =
    merged.length > FEED_LIMIT ? merged.slice(merged.length - FEED_LIMIT) : merged;
}

function startTicker(startedAt: string): void {
  stopTicker();
  const started = Date.parse(startedAt);
  const tick = () => {
    elapsedMs.value = Number.isNaN(started) ? 0 : Math.max(0, Date.now() - started);
  };
  tick();
  ticker = setInterval(tick, 500);
}

function stopTicker(): void {
  if (ticker !== null) clearInterval(ticker);
  ticker = null;
}

function onBatch(batch: FeedBatch): void {
  dispatch({ type: "batch", batch });
  appendEvents(batch.events);

  const stopped = batch.events.find(
    (event) => event.type === "stopped" && event.reason === "journal_failure",
  );
  if (stopped && stopped.type === "stopped") {
    stopTicker();
    dispatch({ type: "journal_failed", message: stopped.message ?? "" });
  }
}

async function start(cfg: SessionConfig): Promise<void> {
  if (state.value.status === "starting" || state.value.status === "recording") return;
  events.value = [];
  elapsedMs.value = 0;
  dispatch({ type: "start_requested" });
  try {
    const info = await bridge.startSession(cfg, onBatch);
    dispatch({ type: "started", info });
    startTicker(info.startedAt);
  } catch (error) {
    dispatch({ type: "start_failed", error: toSessionError(error) });
  }
}

async function stop(): Promise<void> {
  if (state.value.status !== "recording") return;
  dispatch({ type: "stop_requested" });
  try {
    const summary = await bridge.stopSession();
    dispatch({ type: "stopped", summary });
  } catch (error) {
    dispatch({ type: "stop_failed", error: toSessionError(error) });
  } finally {
    stopTicker();
  }
}

async function addNote(text: string): Promise<void> {
  if (state.value.status !== "recording") return;
  try {
    await bridge.addSessionNote(text);
  } catch (error) {
    dispatch({ type: "command_failed", error: toSessionError(error) });
  }
}

async function openJournal(path: string): Promise<JournalSummary | null> {
  try {
    const journal = await bridge.openJournal(path);
    dispatch({ type: "journal_opened", journal });
    return journal;
  } catch (error) {
    dispatch({ type: "command_failed", error: toSessionError(error) });
    return null;
  }
}

function closeJournal(): void {
  dispatch({ type: "journal_closed" });
}

function dismissError(): void {
  dispatch({ type: "error_dismissed" });
}

function dismissFailure(): void {
  dispatch({ type: "failure_dismissed" });
}

/** Ошибка вспомогательной команды (открыть папку): показывается тем же баннером. */
function reportError(code: BridgeErrorCode, message: string): void {
  dispatch({ type: "command_failed", error: { code, message, params: {} } });
}

/** Только для тестов: сбросить сессию и ленту. */
export function resetSession(): void {
  stopTicker();
  state.value = initialSessionState();
  events.value = [];
  elapsedMs.value = 0;
}

export function useSession() {
  return {
    state,
    events,
    elapsedMs,
    start,
    stop,
    addNote,
    openJournal,
    closeJournal,
    dismissError,
    dismissFailure,
    reportError,
  };
}
