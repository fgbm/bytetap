/**
 * Метаданные заголовка журнала для полосы просмотра (FR-29, M2-AC10).
 *
 * Заголовок `session` приходит из `open_journal` как JSON: здесь он разбирается в поля,
 * которые показывает полоса под предупреждением о персональных данных. Чистые функции:
 * ни Vue, ни моста, поэтому проверяются vitest без окна.
 */

/** Значения режима в заголовке журнала (`docs/reference/session-format.md`). */
export const MODE_KEYS: Record<string, string> = {
  tcp_server: "journal.modeTcpServer",
  tcp_client: "journal.modeTcpClient",
  proxy: "journal.modeProxy",
  com_server: "journal.modeComServer",
  com_monitor: "journal.modeComMonitor",
  com_tcp: "journal.modeComTcp",
};

/** Значения автоответчика в заголовке журнала. */
export const RESPONDER_KEYS: Record<string, string> = {
  hl7_mllp: "journal.responderHl7",
  astm_e1381: "journal.responderAstm",
  none: "journal.responderNone",
  auto: "journal.responderAuto",
};

export interface JournalMeta {
  /** Модель анализатора из заголовка; пустая строка — не указана. */
  analyzer: string;
  /** Режим из заголовка; пустая строка — поля нет. */
  mode: string;
  /** Профиль автоответчика из заголовка. */
  responder: string;
  /** Адрес прослушивания из `settings.listen`. */
  listen: string;
  /** Порт из `settings.listen`; пустая строка — адреса нет. */
  port: string;
  /** Начало сессии по системным часам, RFC 3339. */
  startedAt: string;
  /** Комментарий, указанный при старте. */
  note: string;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** Поле заголовка из JSON: `session` — объект, всё остальное — «нет значения». */
export function journalMeta(header: Record<string, unknown>): JournalMeta {
  const settings = header.settings;
  const listen =
    typeof settings === "object" && settings !== null
      ? text((settings as Record<string, unknown>).listen)
      : "";
  return {
    analyzer: text(header.analyzer),
    mode: text(header.mode),
    responder: text(header.autoresponder),
    listen,
    port: listen.slice(listen.lastIndexOf(":") + 1),
    startedAt: text(header.started_at),
    note: text(header.note),
  };
}

/** Имя файла журнала без папки: путь приходит и из Windows, и из Unix. */
export function journalFileName(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut === -1 ? path : path.slice(cut + 1);
}

/** Длительность сессии из микросекунд: «00:42:18», с часами всегда. */
export function journalDuration(durationUs: number): string {
  const total = Math.max(0, Math.floor(durationUs / 1_000_000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (value: number): string => String(value).padStart(2, "0");
  return `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`;
}

/** Комментарии журнала в одну строку: сначала из заголовка, затем дописанные. */
export function journalNotes(headerNote: string, appended: readonly string[]): string {
  return [headerNote, ...[...appended].reverse()]
    .filter((note) => note.trim() !== "")
    .join(" · ");
}
