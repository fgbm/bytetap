/**
 * Тексты ошибок по коду: интерфейс переводит `code`, а `message` из ядра оставляет
 * журналу и отладке (AC3). Ключи собраны здесь, чтобы тест i18n проверял их все.
 */

import type { BridgeErrorCode, JournalFailureCode, SessionConfig } from "../api/types";
import type { SessionError } from "./reducer";

export const ERROR_KEYS: Record<BridgeErrorCode, string> = {
  port_in_use: "errors.port_in_use",
  address_unavailable: "errors.address_unavailable",
  permission_denied: "errors.permission_denied",
  log_dir: "errors.log_dir",
  already_running: "errors.already_running",
  journal: "errors.journal",
  journal_open: "errors.journal_open",
  io: "errors.io",
};

/** Причины отказа журнала: код ядра → человеческая фраза для баннера AC15. */
export const FAILURE_KEYS: Record<JournalFailureCode, string> = {
  disk_full: "banner.journalFailureReasons.disk_full",
  permission_denied: "banner.journalFailureReasons.permission_denied",
  not_found: "banner.journalFailureReasons.not_found",
  network: "banner.journalFailureReasons.network",
  busy: "banner.journalFailureReasons.busy",
  timeout: "banner.journalFailureReasons.timeout",
  io: "banner.journalFailureReasons.io",
};

/** Ошибки, которые показываются у порта и адреса прослушивания. */
export const PORT_ERROR_CODES: BridgeErrorCode[] = [
  "port_in_use",
  "address_unavailable",
  "permission_denied",
];

/** Ошибки, которые показываются у папки журналов. Открытие файла — отдельный баннер. */
export const LOG_DIR_ERROR_CODES: BridgeErrorCode[] = ["log_dir"];

/** Подставляет то, что есть: параметры ядра важнее текущей конфигурации. */
export function errorParams(error: SessionError, cfg: SessionConfig): Record<string, string> {
  return {
    port: String(cfg.listenPort),
    addr: cfg.listenHost === "" ? "0.0.0.0" : cfg.listenHost,
    dir: cfg.logDir,
    path: cfg.logDir,
    message: error.message,
    ...error.params,
  };
}
