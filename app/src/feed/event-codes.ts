/**
 * Тексты событий журнала по коду: `message` в журнале английский, интерфейс берёт
 * по `code` русскую строку (FR-15). Список кодов — из `docs/reference/session-format.md`;
 * он же проверяется тестом i18n, поэтому новый код без перевода не пройдёт проверку.
 */

import type { Translate } from "./rows";

/** Коды `error` и `info`, которые пишет сессия и автоответчик. */
export type SessionEventCode =
  | "accept_failed"
  | "connect_failed"
  | "serial_open_failed"
  | "connection_read_failed"
  | "connection_write_failed"
  | "answer_timeout"
  | "journal_failure"
  | "protocol_undetected";

export const EVENT_KEYS: Record<SessionEventCode, string> = {
  accept_failed: "events.accept_failed",
  connect_failed: "events.connect_failed",
  serial_open_failed: "events.serial_open_failed",
  connection_read_failed: "events.connection_read_failed",
  connection_write_failed: "events.connection_write_failed",
  answer_timeout: "events.answer_timeout",
  journal_failure: "events.journal_failure",
  protocol_undetected: "events.protocol_undetected",
};

/**
 * Русский текст события по коду; `null` — кода нет или он неизвестен, тогда
 * показывается английское `message` из журнала.
 */
export function eventText(
  code: string | undefined,
  message: string,
  t: Translate,
): string {
  if (code === undefined) return message;
  const key = EVENT_KEYS[code as SessionEventCode];
  return key === undefined ? message : t(key);
}
