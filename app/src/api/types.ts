/**
 * Контракт моста между интерфейсом и ядром.
 *
 * Формы повторяют `docs/dev/stages/M2.md` и `M3.md` (раздел «Контракт моста») один
 * в один: команды Tauri называются так же, в snake_case, поля — в camelCase.
 */

/**
 * Режим сессии. Прокси и прослушка COM появятся в M4 и M5 и в интерфейсе не показываются:
 * в макете v2 нет несуществующих режимов.
 */
export type SessionMode = "tcp_server" | "tcp_client" | "serial";

/** Автоответчик: `auto` решает по первому байту, `hl7` — MLLP, `astm` — E1381. */
export type Responder = "auto" | "hl7" | "astm" | "none";

/** Чётность COM-порта. */
export type SerialParity = "none" | "even" | "odd" | "mark" | "space";

/** Управление потоком COM-порта. */
export type SerialFlow = "none" | "rtscts" | "xonxoff";

/** Настройки порта FR-11: фактические значения попадают в заголовок журнала. */
export interface SerialSettings {
  port: string;
  baud: number;
  dataBits: 7 | 8;
  parity: SerialParity;
  stopBits: 1 | 2;
  flow: SerialFlow;
  dtr: boolean;
  rts: boolean;
}

/**
 * Параметры сессии. Поля чужого режима необязательны, лишние ядро отвергает
 * до создания журнала, поэтому интерфейс отправляет только свой режим.
 */
export interface SessionConfig {
  mode: SessionMode;
  listenHost?: string;
  listenPort?: number;
  connectHost?: string;
  connectPort?: number;
  reconnectSec?: number;
  serial?: SerialSettings;
  responder: Responder;
  astmNakOnBadChecksum: boolean;
  analyzer: string;
  note: string;
  logDir: string;
}

/** Порт из `list_serial_ports`: имя для открытия и понятное описание (FR-12). */
export interface SerialPort {
  name: string;
  description: string;
}

/** Запись в папке журналов для стартового экрана (`list_recent_journals`). */
export interface RecentJournal {
  path: string;
  startedAt: string;
  analyzer: string;
  durationUs: number;
  mode: string;
}

export interface StartInfo {
  /** Номер поколения сессии: «Стоп» попадает только в свою сессию (NFR-5). */
  generation: number;
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
  /** Код из `session-format.md`: интерфейс берёт по нему русскую строку. */
  code?: string;
}

export interface InfoEvent {
  type: "info";
  t: string;
  monoUs: number;
  conn?: number;
  message: string;
  code?: string;
}

export interface NoteEvent {
  type: "note";
  t: string;
  text: string;
}

export interface StoppedEvent {
  type: "stopped";
  reason: "user" | "journal_failure";
  /** Вид отказа журнала: интерфейс переводит его, а `message` оставляет в подробностях. */
  code?: JournalFailureCode;
  message?: string;
}

/** Направление фрагмента: анализатор → ЛИС или ЛИС → анализатор. */
export type Dir = "a2l" | "l2a";

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
  /** Счётчики ядра на момент пачки: шапка берёт их, а не считает по ленте. */
  stats?: SessionStats;
}

/** Коды ошибок команд; текст `message` — для журнала и отладки, не для экрана. */
export type BridgeErrorCode =
  | "port_in_use"
  | "address_unavailable"
  | "address_invalid"
  | "permission_denied"
  | "log_dir"
  | "already_running"
  | "journal"
  | "journal_open"
  | "port_not_found"
  | "port_busy"
  | "port_settings"
  | "serial"
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
  /**
   * Индекс первого события в каждой из 601 точки корзин (AC9).
   *
   * По нему рамка видимой области и переход из трассы считаются по времени,
   * а не по номерам строк.
   */
  firstEvent: number[];
}

/** Причина отказа журнала: код из моста, интерфейс переводит её через `ru.json`. */
export type JournalFailureCode =
  | "disk_full"
  | "permission_denied"
  | "not_found"
  | "network"
  | "busy"
  | "timeout"
  | "io";

/** Предупреждение о папке журналов: код и подстановки текста (FR-19). */
export interface LogDirWarning {
  code: "onedrive" | "network";
  params: Record<string, string>;
}

/** Состояние записи для перезагруженной страницы (M2-AC1). */
export interface SessionStatus {
  running: boolean;
  /** Параметры идущей сессии; `null`, если мост занят запуском или остановкой. */
  startInfo: StartInfo | null;
}
