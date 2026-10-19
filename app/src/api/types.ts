/**
 * Контракт моста M2 между интерфейсом и ядром.
 *
 * Формы повторяют `docs/dev/stages/M2.md` (раздел «Контракт моста») один в один:
 * команды Tauri называются так же, в snake_case, поля — в camelCase.
 */

/** Режим сессии. На M2 доступен только TCP-сервер «вместо ЛИС». */
export type SessionMode = "tcp_server";

/** Автоответчик. `hl7` — синтетический ACK, `none` — запись без ответов. */
export type Responder = "hl7" | "none";

/** Направление фрагмента: анализатор → ЛИС или ЛИС → анализатор. */
export type Dir = "a2l" | "l2a";

export interface SessionConfig {
  mode: SessionMode;
  listenHost: string;
  listenPort: number;
  responder: Responder;
  analyzer: string;
  note: string;
  logDir: string;
}

export interface StartInfo {
  journalPath: string;
  localAddr: string;
  startedAt: string;
}

export interface SessionStats {
  connections: number;
  bytesAnalyzerToLis: number;
  bytesLisToAnalyzer: number;
}

export interface SessionSummary {
  stats: SessionStats;
  journalPath: string;
}

/** IPv4-адрес ноутбука без loopback, имя интерфейса — рядом с адресом. */
export interface InterfaceInfo {
  name: string;
  addr: string;
}

export interface DataEvent {
  type: "data";
  t: string;
  monoUs: number;
  conn: number;
  dir: Dir;
  synthetic: boolean;
  b64: string;
}

export interface OpenEvent {
  type: "open";
  t: string;
  monoUs: number;
  conn: number;
  peer: string;
  local: string;
}

export interface CloseEvent {
  type: "close";
  t: string;
  monoUs: number;
  conn: number;
  by: string;
  reason: string;
}

export interface ErrorEvent {
  type: "error";
  t: string;
  monoUs: number;
  conn?: number;
  message: string;
}

export interface InfoEvent {
  type: "info";
  t: string;
  monoUs: number;
  conn?: number;
  message: string;
}

export interface NoteEvent {
  type: "note";
  t: string;
  text: string;
}

export interface StoppedEvent {
  type: "stopped";
  reason: "user" | "journal_failure";
  message?: string;
}

/** Дискриминированный union по `type`; те же события уходят в журнал. */
export type FeedEvent =
  | DataEvent
  | OpenEvent
  | CloseEvent
  | ErrorEvent
  | InfoEvent
  | NoteEvent
  | StoppedEvent;

export interface FeedBatch {
  events: FeedEvent[];
  dropped: number;
}

/** Коды ошибок команд; текст `message` — для журнала и отладки, не для экрана. */
export type BridgeErrorCode =
  | "port_in_use"
  | "address_unavailable"
  | "permission_denied"
  | "log_dir"
  | "already_running"
  | "journal"
  | "io";

export interface BridgeErrorPayload {
  code: BridgeErrorCode;
  message: string;
  params?: Record<string, string>;
}

/** Ошибка команды моста с кодом: интерфейс переводит по коду, а не по тексту. */
export class BridgeError extends Error {
  readonly code: BridgeErrorCode;
  readonly params: Record<string, string>;

  constructor(code: BridgeErrorCode, message: string, params: Record<string, string> = {}) {
    super(message);
    this.name = "BridgeError";
    this.code = code;
    this.params = params;
  }
}

export interface JournalSummary {
  path: string;
  header: Record<string, unknown>;
  events: number;
  durationUs: number;
  stats: SessionStats;
  /** Плотность байтов на 600 корзин — из неё рисуется трасса всей сессии. */
  density: { a2l: number[]; l2a: number[] };
}
