/**
 * Состояние сессии приложения: старт, стоп, события ленты, таймер, просмотр журнала.
 *
 * Лента живёт в кольцевом буфере на 10 000 событий (FR-28): добавление пачки —
 * O(1) на событие, без копирования массива. Буфер обёрнут в `shallowReactive`,
 * поэтому реактивен только номер версии, а сами события остаются обычными объектами.
 * Редьюсер состояний живёт отдельно (`reducer.ts`) и проверяется vitest без Vue.
 */

import { shallowReactive, shallowRef, ref } from "vue";
import { bridge } from "../api/bridge";
import {
  BridgeError,
  type BridgeErrorCode,
  type FeedBatch,
  type FeedEvent,
  type JournalFailureCode,
  type JournalSummary,
  type SessionConfig,
  type SessionStatus,
} from "../api/types";
import { FEED_CAPACITY, FeedBuffer } from "../feed/ring";
import { JournalFeed } from "../journal/feed";
import { initialSessionState, reduceSession, type SessionAction, type SessionError, type SessionState } from "./reducer";

const state = ref<SessionState>(initialSessionState());

/** Кольцевой буфер ленты: 10 000 последних событий и номер версии для подписки. */
const feed = shallowReactive(new FeedBuffer<FeedEvent>(FEED_CAPACITY));

/**
 * Открытый журнал: строки читаются страницами (FR-29). `shallowReactive` нужен, чтобы
 * лента видела `version` при приходе страницы, а сами страницы остались обычными массивами.
 */
const journalFeed = shallowRef<JournalFeed | null>(null);

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
  feed.pushMany(incoming);
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

/** Код отказа журнала из ошибки команды: мост кладёт его в `params.failure`. */
function failureCodeOf(error: unknown): JournalFailureCode {
  const raw = error instanceof BridgeError ? error.params.failure : undefined;
  const known: JournalFailureCode[] = [
    "disk_full",
    "permission_denied",
    "not_found",
    "network",
    "busy",
    "timeout",
    "io",
  ];
  return known.find((code) => code === raw) ?? "io";
}

function onBatch(batch: FeedBatch): void {
  // Поколение берётся до `journal_failed`: это действие обнуляет `startInfo`.
  const generation = state.value.startInfo?.generation;
  dispatch({ type: "batch", batch });
  appendEvents(batch.events);

  const stopped = batch.events.find(
    (event) => event.type === "stopped" && event.reason === "journal_failure",
  );
  if (stopped && stopped.type === "stopped") {
    stopTicker();
    dispatch({
      type: "journal_failed",
      code: stopped.code ?? "io",
      message: stopped.message ?? "",
    });
    // Ядро уже остановило сессию, а мост сам освобождает слот; вызов остаётся
    // безвредным и на случай старого моста. Поколение — своё: чужую сессию не трогаем.
    // Ошибка ожидаема: причина в баннере (NFR-5).
    void bridge.stopSession(generation).catch(() => undefined);
  }
}

async function start(cfg: SessionConfig): Promise<void> {
  if (state.value.status === "starting" || state.value.status === "recording") return;
  feed.clear();
  journalFeed.value = null;
  elapsedMs.value = 0;
  dispatch({ type: "start_requested" });
  try {
    const info = await bridge.startSession(cfg, onBatch);
    dispatch({ type: "started", info, config: cfg });
    startTicker(info.startedAt);
  } catch (error) {
    dispatch({ type: "start_failed", error: toSessionError(error) });
  }
}

async function stop(): Promise<void> {
  if (state.value.status !== "recording") return;
  dispatch({ type: "stop_requested" });
  try {
    // Своё поколение: сессию, которую запустили после перезагрузки, «Стоп» не тронет.
    const summary = await bridge.stopSession(state.value.startInfo?.generation);
    dispatch({ type: "stopped", summary });
  } catch (error) {
    // Отказ журнала на финальной фиксации: это тот же баннер AC15, а не ошибка команды.
    if (error instanceof BridgeError && error.code === "journal") {
      dispatch({
        type: "journal_failed",
        code: failureCodeOf(error),
        message: error.message,
      });
    } else {
      dispatch({ type: "stop_failed", error: toSessionError(error) });
    }
  } finally {
    stopTicker();
  }
}

/**
 * Останавливает запись без записи ленты: кнопка «Остановить» в баннере «сессия уже идёт»
 * и сессия, пережившая перезагрузку страницы (AC1).
 */
async function stopAny(): Promise<void> {
  try {
    // Баннер «сессия уже идёт»: поколение известно из `session_status`, но остановка
    // разрешена и без него — страница могла не получить `StartInfo`.
    const summary = await bridge.stopSession(state.value.startInfo?.generation);
    dispatch({ type: "stopped", summary });
  } catch (error) {
    dispatch({ type: "command_failed", error: toSessionError(error) });
  }
  stopTicker();
}

/**
 * Подхватывает идущую сессию после перезагрузки страницы (AC1).
 *
 * Лента перезагрузки не переживает: канал событий принадлежал старой странице. Зато
 * «Стоп» работает, и сессия не остаётся сиротой в ядре.
 */
async function adopt(status: SessionStatus): Promise<boolean> {
  if (!status.running || status.startInfo === null) return false;
  if (state.value.status !== "idle" && state.value.status !== "viewing") return false;
  dispatch({ type: "resumed", info: status.startInfo });
  startTicker(status.startInfo.startedAt);
  return true;
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
    const lengths = await bridge.journalLengths(path).catch(() => []);
    const pages = shallowReactive(
      new JournalFeed(
        path,
        journal.events,
        (from, count) => bridge.readJournal(path, from, count),
        lengths,
      ),
    );
    journalFeed.value = pages;
    dispatch({ type: "journal_opened", journal });
    // Последняя страница нужна сразу: из неё берутся дописанные комментарии к журналу.
    pages.ensure(Math.max(0, journal.events - 1), journal.events);
    return journal;
  } catch (error) {
    dispatch({ type: "command_failed", error: toSessionError(error) });
    return null;
  }
}

/**
 * Дописывает комментарий в остановленный журнал и обновляет сводку (FR-23).
 *
 * Начало файла не переписывается: событие `note` уходит в конец, поэтому в ленте
 * становится на одно событие больше, а индекс журнала пересобирается по изменившемуся файлу.
 */
async function appendJournalNote(text: string): Promise<void> {
  const journal = state.value.journal;
  if (journal === null || journalFeed.value === null) return;
  try {
    await bridge.appendNote(journal.path, text);
    const summary = await bridge.openJournal(journal.path);
    // `extend` сам перечитывает последнюю страницу: новый комментарий виден сразу (FR-23).
    journalFeed.value.extend(summary.events);
    dispatch({ type: "journal_opened", journal: summary });
  } catch (error) {
    dispatch({ type: "command_failed", error: toSessionError(error) });
  }
}

function closeJournal(): void {
  journalFeed.value?.clear();
  journalFeed.value = null;
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
  feed.clear();
  journalFeed.value = null;
  elapsedMs.value = 0;
}

export function useSession() {
  return {
    state,
    feed,
    journalFeed,
    elapsedMs,
    start,
    stop,
    stopAny,
    adopt,
    addNote,
    openJournal,
    appendJournalNote,
    closeJournal,
    dismissError,
    dismissFailure,
    reportError,
  };
}
