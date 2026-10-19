/**
 * Настройки и профили: чистые функции поверх сохранённого объекта.
 *
 * В Tauri настройки лежат в `settings.json` через plugin-store, в mock и тестах —
 * в localStorage. Схема терпит старые и испорченные значения: всё, чего нет,
 * заменяется значением по умолчанию (AC11).
 *
 * `AppConfig` — полная форма всех трёх режимов: поля, которые сейчас не нужны,
 * остаются на месте, чтобы переключение сценария не теряло ввод. В мост уходит
 * `sessionConfigFor`: только поля выбранного режима, лишние ядро отвергает.
 */

import type {
  Responder,
  SerialFlow,
  SerialParity,
  SerialSettings,
  SessionConfig,
  SessionMode,
} from "../api/types";
import { parseEncoding, type Encoding } from "../feed/bytes";

export type ThemeMode = "system" | "light" | "dark";

/** Вид ленты: текст с управляющими символами или HEX-дамп (FR-26). */
export type FeedViewMode = "text" | "hex";

/** Выбор вида и кодировки ленты — запоминается вместе с настройками (AC11). */
export interface FeedSettings {
  view: FeedViewMode;
  encoding: Encoding;
}

/** Пустой адрес — слушать на всех интерфейсах; так его понимает ядро (`listen_host`). */
export const ALL_INTERFACES = "";
export const DEFAULT_PORT = 5000;
export const DEFAULT_CONNECT_HOST = "192.168.1.50";
export const DEFAULT_BAUD = 9600;
export const DEFAULT_RECONNECT_SEC = 5;

/** Типовые скорости COM-порта FR-11; остальные значения — «другая…». */
export const BAUD_RATES: readonly number[] = [
  300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200,
];

export const BAUD_MIN = 1;
export const BAUD_MAX = 4_000_000;

/** Полная конфигурация всех режимов: поля чужого режима не теряются при переключении. */
export interface AppConfig {
  mode: SessionMode;
  listenHost: string;
  listenPort: number;
  connectHost: string;
  connectPort: number;
  reconnectSec: number;
  serial: SerialSettings;
  responder: Responder;
  astmNakOnBadChecksum: boolean;
  analyzer: string;
  note: string;
  logDir: string;
}

/** Последний старт: карточка сценария показывает его порт (UI v2). */
export interface LastRun {
  mode: SessionMode;
  /** Порт режима: TCP-порт сервера и клиента, имя порта для COM. */
  port: string;
  /** Адрес анализатора для TCP-клиента; в остальных режимах пуст. */
  host: string;
  baud: number;
}

export interface Profile {
  name: string;
  config: AppConfig;
}

export interface Settings {
  config: AppConfig;
  theme: ThemeMode;
  profiles: Profile[];
  feed: FeedSettings;
  lastRun: LastRun | null;
}

export function defaultSerial(port = ""): SerialSettings {
  return {
    port,
    baud: DEFAULT_BAUD,
    dataBits: 8,
    parity: "none",
    stopBits: 1,
    flow: "none",
    dtr: true,
    rts: true,
  };
}

export function defaultConfig(logDir = ""): AppConfig {
  return {
    mode: "tcp_server",
    listenHost: ALL_INTERFACES,
    listenPort: DEFAULT_PORT,
    connectHost: DEFAULT_CONNECT_HOST,
    connectPort: DEFAULT_PORT,
    reconnectSec: DEFAULT_RECONNECT_SEC,
    serial: defaultSerial(),
    responder: "auto",
    astmNakOnBadChecksum: false,
    analyzer: "",
    note: "",
    logDir,
  };
}

export function defaultSettings(logDir = ""): Settings {
  return {
    config: defaultConfig(logDir),
    theme: "system",
    profiles: [],
    feed: { view: "text", encoding: "cp1251" },
    lastRun: null,
  };
}

/**
 * Конфигурация для `start_session`: только поля выбранного режима.
 *
 * Ядро отвергает лишние поля до создания журнала, поэтому общая форма настроек
 * ужимается здесь, а не в мосте.
 */
export function sessionConfigFor(cfg: AppConfig): SessionConfig {
  const common = {
    mode: cfg.mode,
    responder: cfg.responder,
    astmNakOnBadChecksum: cfg.astmNakOnBadChecksum,
    analyzer: cfg.analyzer,
    note: cfg.note,
    logDir: cfg.logDir,
  };
  if (cfg.mode === "tcp_client") {
    return {
      ...common,
      connectHost: cfg.connectHost,
      connectPort: cfg.connectPort,
      reconnectSec: cfg.reconnectSec,
    };
  }
  if (cfg.mode === "serial") {
    return { ...common, serial: { ...cfg.serial } };
  }
  return { ...common, listenHost: cfg.listenHost, listenPort: cfg.listenPort };
}

const PARITY_LETTER: Record<SerialParity, string> = {
  none: "N",
  even: "E",
  odd: "O",
  mark: "M",
  space: "S",
};

/** Формат порта FR-11: `8N1`, `7E2` — то же, что `--format` в CLI. */
export function serialFormat(serial: SerialSettings): string {
  return `${serial.dataBits}${PARITY_LETTER[serial.parity]}${serial.stopBits}`;
}

/** Карточка сценария показывает порт последнего старта этого режима. */
export function lastRunOf(cfg: AppConfig): LastRun {
  if (cfg.mode === "tcp_client") {
    return {
      mode: cfg.mode,
      port: String(cfg.connectPort),
      host: cfg.connectHost,
      baud: 0,
    };
  }
  if (cfg.mode === "serial") {
    return { mode: cfg.mode, port: cfg.serial.port, host: "", baud: cfg.serial.baud };
  }
  return { mode: cfg.mode, port: String(cfg.listenPort), host: "", baud: 0 };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function int(value: unknown, fallback: number, min: number, max: number): number {
  const number = typeof value === "number" ? value : Number(value);
  if (!Number.isInteger(number) || number < min || number > max) return fallback;
  return number;
}

function mode(value: unknown, fallback: SessionMode): SessionMode {
  return value === "tcp_client" || value === "serial" || value === "tcp_server" ? value : fallback;
}

function responder(value: unknown, fallback: Responder): Responder {
  return value === "auto" || value === "hl7" || value === "astm" || value === "none"
    ? value
    : fallback;
}

const PARITIES: readonly SerialParity[] = ["none", "even", "odd", "mark", "space"];
const FLOWS: readonly SerialFlow[] = ["none", "rtscts", "xonxoff"];

function serial(raw: unknown, fallback: SerialSettings): SerialSettings {
  if (!isRecord(raw)) return { ...fallback };
  return {
    port: text(raw.port, fallback.port),
    baud: int(raw.baud, fallback.baud, BAUD_MIN, BAUD_MAX),
    dataBits: raw.dataBits === 7 ? 7 : 8,
    parity: PARITIES.find((value) => value === raw.parity) ?? fallback.parity,
    stopBits: raw.stopBits === 2 ? 2 : 1,
    flow: FLOWS.find((value) => value === raw.flow) ?? fallback.flow,
    dtr: typeof raw.dtr === "boolean" ? raw.dtr : fallback.dtr,
    rts: typeof raw.rts === "boolean" ? raw.rts : fallback.rts,
  };
}

export function normalizeConfig(raw: unknown, fallback: AppConfig): AppConfig {
  if (!isRecord(raw)) return { ...fallback, serial: { ...fallback.serial } };
  return {
    mode: mode(raw.mode, fallback.mode),
    listenHost: text(raw.listenHost, fallback.listenHost),
    listenPort: int(raw.listenPort, fallback.listenPort, 1, 65535),
    connectHost: text(raw.connectHost, fallback.connectHost),
    connectPort: int(raw.connectPort, fallback.connectPort, 1, 65535),
    reconnectSec: int(raw.reconnectSec, fallback.reconnectSec, 0, 3600),
    serial: serial(raw.serial, fallback.serial),
    responder: responder(raw.responder, fallback.responder),
    astmNakOnBadChecksum:
      typeof raw.astmNakOnBadChecksum === "boolean"
        ? raw.astmNakOnBadChecksum
        : fallback.astmNakOnBadChecksum,
    analyzer: text(raw.analyzer, fallback.analyzer),
    note: text(raw.note, fallback.note),
    logDir: text(raw.logDir, fallback.logDir),
  };
}

function normalizeProfile(raw: unknown, fallback: AppConfig): Profile | null {
  if (!isRecord(raw)) return null;
  const name = text(raw.name, "").trim();
  if (name === "") return null;
  return { name, config: normalizeConfig(raw.config, fallback) };
}

/** Последний старт: неизвестный режим забывается, порт остаётся строкой. */
export function normalizeLastRun(raw: unknown): LastRun | null {
  if (!isRecord(raw)) return null;
  if (raw.mode !== "tcp_server" && raw.mode !== "tcp_client" && raw.mode !== "serial") return null;
  return {
    mode: raw.mode,
    port: text(raw.port, ""),
    host: text(raw.host, ""),
    baud: int(raw.baud, 0, 0, BAUD_MAX),
  };
}

/** Вид и кодировка ленты: неизвестные значения заменяются значениями по умолчанию. */
export function normalizeFeed(raw: unknown): FeedSettings {
  const base: FeedSettings = { view: "text", encoding: "cp1251" };
  if (!isRecord(raw)) return base;
  return {
    view: raw.view === "hex" ? "hex" : "text",
    encoding: parseEncoding(raw.encoding) ?? base.encoding,
  };
}

export function normalizeSettings(raw: unknown, logDir = ""): Settings {
  const base = defaultSettings(logDir);
  if (!isRecord(raw)) return base;
  const theme = raw.theme === "light" || raw.theme === "dark" ? raw.theme : "system";
  const config = normalizeConfig(raw.config, base.config);
  const profiles = Array.isArray(raw.profiles)
    ? raw.profiles
        .map((profile) => normalizeProfile(profile, config))
        .filter((profile): profile is Profile => profile !== null)
    : [];
  return {
    config,
    theme,
    profiles,
    feed: normalizeFeed(raw.feed),
    lastRun: normalizeLastRun(raw.lastRun),
  };
}

/** Сохраняет профиль под именем: одноимённый заменяется, остальные остаются на месте. */
export function upsertProfile(profiles: Profile[], name: string, config: AppConfig): Profile[] {
  const trimmed = name.trim();
  if (trimmed === "") return profiles;
  const next: Profile = { name: trimmed, config: { ...config, serial: { ...config.serial } } };
  const index = profiles.findIndex((profile) => profile.name === trimmed);
  if (index === -1) return [...profiles, next];
  const copy = [...profiles];
  copy[index] = next;
  return copy;
}

export function removeProfile(profiles: Profile[], name: string): Profile[] {
  return profiles.filter((profile) => profile.name !== name);
}

export function findProfile(profiles: Profile[], name: string): Profile | null {
  return profiles.find((profile) => profile.name === name) ?? null;
}
