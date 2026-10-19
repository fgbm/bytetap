/**
 * Настройки и профили: чистые функции поверх сохранённого объекта.
 *
 * В Tauri настройки лежат в `settings.json` через plugin-store, в mock и тестах —
 * в localStorage. Схема терпит старые и испорченные значения: всё, чего нет,
 * заменяется значением по умолчанию (AC11).
 */

import type { Responder, SessionConfig } from "../api/types";
import { parseEncoding, type Encoding } from "../feed/bytes";

export type ThemeMode = "system" | "light" | "dark";

/** Вид ленты: текст с плашками или HEX-дамп (FR-26). */
export type FeedViewMode = "text" | "hex";

/** Выбор вида и кодировки ленты — запоминается вместе с настройками (AC11). */
export interface FeedSettings {
  view: FeedViewMode;
  encoding: Encoding;
}

/** Пустой адрес — слушать на всех интерфейсах; так его понимает ядро (`listen_host`). */
export const ALL_INTERFACES = "";
export const DEFAULT_PORT = 5000;

export interface Profile {
  name: string;
  config: SessionConfig;
}

export interface Settings {
  config: SessionConfig;
  theme: ThemeMode;
  profiles: Profile[];
  feed: FeedSettings;
}

export function defaultConfig(logDir = ""): SessionConfig {
  return {
    mode: "tcp_server",
    listenHost: ALL_INTERFACES,
    listenPort: DEFAULT_PORT,
    responder: "hl7",
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
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function responder(value: unknown, fallback: Responder): Responder {
  return value === "hl7" || value === "none" ? value : fallback;
}

export function normalizeConfig(raw: unknown, fallback: SessionConfig): SessionConfig {
  if (!isRecord(raw)) return { ...fallback };
  const port = typeof raw.listenPort === "number" ? raw.listenPort : Number(raw.listenPort);
  return {
    mode: "tcp_server",
    listenHost: text(raw.listenHost, fallback.listenHost),
    listenPort: Number.isInteger(port) && port >= 1 && port <= 65535 ? port : fallback.listenPort,
    responder: responder(raw.responder, fallback.responder),
    analyzer: text(raw.analyzer, fallback.analyzer),
    note: text(raw.note, fallback.note),
    logDir: text(raw.logDir, fallback.logDir),
  };
}

function normalizeProfile(raw: unknown, fallback: SessionConfig): Profile | null {
  if (!isRecord(raw)) return null;
  const name = text(raw.name, "").trim();
  if (name === "") return null;
  return { name, config: normalizeConfig(raw.config, fallback) };
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
  return { config, theme, profiles, feed: normalizeFeed(raw.feed) };
}

/** Сохраняет профиль под именем: одноимённый заменяется, остальные остаются на месте. */
export function upsertProfile(profiles: Profile[], name: string, config: SessionConfig): Profile[] {
  const trimmed = name.trim();
  if (trimmed === "") return profiles;
  const next: Profile = { name: trimmed, config: { ...config } };
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
