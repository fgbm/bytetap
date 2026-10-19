/**
 * Тексты ошибок по коду: интерфейс переводит `code`, а `message` из ядра оставляет
 * журналу и отладке (AC3). Ключи собраны здесь, чтобы тест i18n проверял их все.
 *
 * UI v2: у ошибки есть поле формы, под которым она показывается красным текстом
 * (12px, без рамок-баннеров).
 */

import type { BridgeErrorCode, JournalFailureCode, SessionMode } from "../api/types";
import type { SessionError } from "./reducer";

export const ERROR_KEYS: Record<BridgeErrorCode, string> = {
  port_in_use: "errors.port_in_use",
  address_unavailable: "errors.address_unavailable",
  address_invalid: "errors.address_invalid",
  permission_denied: "errors.permission_denied",
  log_dir: "errors.log_dir",
  already_running: "errors.already_running",
  journal: "errors.journal",
  journal_open: "errors.journal_open",
  port_not_found: "errors.port_not_found",
  port_busy: "errors.port_busy",
  port_settings: "errors.port_settings",
  serial: "errors.serial",
  io: "errors.io",
};

/** Причины отказа журнала: код ядра → человеческая фраза для строки отказа. */
export const FAILURE_KEYS: Record<JournalFailureCode, string> = {
  disk_full: "banner.journalFailureReasons.disk_full",
  permission_denied: "banner.journalFailureReasons.permission_denied",
  not_found: "banner.journalFailureReasons.not_found",
  network: "banner.journalFailureReasons.network",
  busy: "banner.journalFailureReasons.busy",
  timeout: "banner.journalFailureReasons.timeout",
  io: "banner.journalFailureReasons.io",
};

/** Поле формы, под которым показывается ошибка; `start` — под кнопкой «Старт». */
export type ErrorField = "port" | "address" | "serial" | "logDir" | "start";

/**
 * Куда отнести ошибку команды.
 *
 * Порт TCP у сервера и порт COM — разные поля, поэтому режим тоже нужен:
 * `port_in_use` у сервера — это занятый порт прослушивания, у COM-порта — занятый
 * последовательный порт.
 */
export function errorField(code: BridgeErrorCode, mode: SessionMode): ErrorField {
  switch (code) {
    case "port_in_use":
      return mode === "serial" ? "serial" : "port";
    case "permission_denied":
      return mode === "serial" ? "serial" : "port";
    case "address_unavailable":
    case "address_invalid":
      return "address";
    case "port_not_found":
    case "port_busy":
    case "port_settings":
    case "serial":
      return "serial";
    case "log_dir":
    case "journal_open":
      return "logDir";
    default:
      return "start";
  }
}

/** Подставляет то, что есть: параметры ядра важнее текущей конфигурации. */
export function errorParams(
  error: SessionError,
  cfg: { listenPort: number; listenHost: string; connectHost: string; connectPort: number; logDir: string },
): Record<string, string> {
  return {
    port: String(cfg.listenPort),
    addr: cfg.listenHost === "" ? "0.0.0.0" : cfg.listenHost,
    host: cfg.connectHost,
    connectPort: String(cfg.connectPort),
    dir: cfg.logDir,
    path: cfg.logDir,
    message: error.message,
    ...error.params,
  };
}
