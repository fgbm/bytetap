/**
 * Заглушка моста для `npm run dev`, браузера и тестов.
 *
 * Отдаёт те же типы, что ядро, и синтетический HL7-обмен ANALYZER ↔ LIS ↔ PATIENT^TEST
 * по таймеру: два одновременных соединения анализатора, ORU^R01 в UTF-8 и в CP1251
 * (кириллица в имени пациента), синтетические ACK автоответчика, QRY.
 * Ошибки воспроизводятся сценарием из адреса страницы: `?scenario=port_in_use`
 * или `?scenario=journal_failure`. `?burst=1` (или `?scenario=burst`) заливает
 * ленту 12 000 фрагментов за 60 с — для проверки виртуализации и прокрутки.
 */

import type { Bridge } from "./bridge";
import {
  BridgeError,
  type Dir,
  type FeedBatch,
  type FeedEvent,
  type InterfaceInfo,
  type JournalSummary,
  type LogDirWarning,
  type SessionConfig,
  type StartInfo,
} from "./types";
import { base64ByteLength } from "../feed/bytes";

export const MOCK_INTERFACES: InterfaceInfo[] = [
  { name: "Ethernet", addr: "192.168.1.10" },
  { name: "Wi-Fi", addr: "10.0.0.5" },
];

const MOCK_LOG_DIR = "C:\\Users\\operator\\Documents\\bytetap";
const TICK_MS = 500;
const JOURNAL_FAILURE_AFTER_STEPS = 14;

export type MockScenario = "default" | "port_in_use" | "journal_failure" | "burst";

function scenarioFromLocation(): MockScenario {
  if (typeof window === "undefined") return "default";
  const params = new URLSearchParams(window.location.search);
  const value = params.get("scenario");
  if (value === "port_in_use" || value === "journal_failure" || value === "burst") return value;
  return params.get("burst") === "1" ? "burst" : "default";
}

function pad(value: number, width = 2): string {
  return String(value).padStart(width, "0");
}

function stamp(date: Date): string {
  return (
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
    `${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
  );
}

function journalName(at: Date): string {
  return `bytetap-${stamp(at)}-tcp_server.jsonl`;
}

function joinPath(dir: string, name: string): string {
  const separator = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith(separator) ? `${dir}${name}` : `${dir}${separator}${name}`;
}

/** Текст → байты CP1251: латиница как есть, кириллица 0xC0–0xFF, Ё и ё отдельно. */
function cp1251Bytes(text: string): number[] {
  const bytes: number[] = [];
  for (const char of text) {
    const code = char.codePointAt(0) ?? 0x3f;
    if (code < 0x80) bytes.push(code);
    else if (code >= 0x410 && code <= 0x44f) bytes.push(code - 0x410 + 0xc0);
    else if (code === 0x401) bytes.push(0xa8);
    else if (code === 0x451) bytes.push(0xb8);
    else bytes.push(0x3f);
  }
  return bytes;
}

function bytesToBase64(bytes: readonly number[]): string {
  let text = "";
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text);
}

/** MLLP-кадр HL7: <VT> … <CR><FS><CR>. */
function mllp(...segments: string[]): string {
  return `\u000b${segments.join("\r")}\r\u001c\r`;
}

function ackFrame(messageId: string, at: Date): string {
  const message = `ACK^R01|BT${stamp(at)}${pad(at.getMilliseconds(), 3)}`;
  return mllp(
    `MSH|^~\\&|LIS||ANALYZER|LAB|${stamp(at)}||${message}|P|2.3.1`,
    `MSA|AA|${messageId}`,
  );
}

function queryFrame(messageId: string, at: Date): string {
  return mllp(
    `MSH|^~\\&|ANALYZER|LAB|LIS||${stamp(at)}||QRY^Q02|${messageId}|P|2.3.1`,
    "QRD|20261019|R|I|MSG0002|||10^RD",
  );
}

function resultFrame(messageId: string, at: Date, sample: number): string {
  return mllp(
    `MSH|^~\\&|ANALYZER|LAB|LIS||${stamp(at)}||ORU^R01|${messageId}|P|2.3.1`,
    `PID|1||${pad(sample, 6)}||PATIENT^TEST`,
    "OBX|1|NM|GLU^Glucose||5.4|mmol/L|3.9-6.1|N|||F",
    "OBX|2|NM|NA^Sodium||140|mmol/L|136-145|N|||F",
  );
}

/** Тот же результат, но имя пациента и показатель — кириллицей в CP1251 (FR-27). */
function resultFrameCp1251(messageId: string, at: Date, sample: number): string {
  return mllp(
    `MSH|^~\\&|ANALYZER|LAB|LIS||${stamp(at)}||ORU^R01|${messageId}|P|2.3.1`,
    `PID|1||${pad(sample, 6)}||ПАЦИЕНТ^ТЕСТ`,
    "OBX|1|NM|GLU^Глюкоза||5.4|mmol/L|3.9-6.1|N|||F",
  );
}

interface MockSession {
  cfg: SessionConfig;
  startInfo: StartInfo;
  startedAtMono: number;
  /** Первое соединение цикла. */
  conn: number;
  /** Второе соединение цикла: обмен идёт параллельно. */
  second: number;
  step: number;
  sample: number;
  bytesA2l: number;
  bytesL2a: number;
  connections: number;
  timer: ReturnType<typeof setInterval> | null;
  onBatch: (batch: FeedBatch) => void;
  failed: boolean;
}

/** Сколько событий в журнале заглушки и какова его длительность. */
const MOCK_JOURNAL_EVENTS = 128;
const MOCK_JOURNAL_DURATION_US = 2_538_000_000;

/** Событие журнала заглушки по номеру: тот же обмен, что в живой ленте (FR-29). */
function mockJournalEvent(index: number): FeedEvent {
  const monoUs = Math.round((index / MOCK_JOURNAL_EVENTS) * MOCK_JOURNAL_DURATION_US);
  const when = new Date(Date.UTC(2026, 9, 19, 7, 15, 0) + Math.round(monoUs / 1000));
  const t = when.toISOString();
  const conn = Math.floor(index / 9) + 1;
  const messageId = `J${pad(index, 4)}`;
  const bytes = (text: string): string => bytesToBase64(Array.from(new TextEncoder().encode(text)));

  switch (index % 9) {
    case 0:
      return {
        type: "open",
        t,
        monoUs,
        conn,
        peer: `192.168.1.${50 + conn}:49152`,
        local: "0.0.0.0:5000",
      };
    case 1:
      return {
        type: "data",
        t,
        monoUs,
        conn,
        dir: "a2l",
        synthetic: false,
        b64: bytes(resultFrame(messageId, when, index)),
      };
    case 2:
      return {
        type: "data",
        t,
        monoUs,
        conn,
        dir: "a2l",
        synthetic: false,
        b64: bytesToBase64(cp1251Bytes(resultFrameCp1251(messageId, when, index))),
      };
    case 3:
    case 6:
      return {
        type: "data",
        t,
        monoUs,
        conn,
        dir: "l2a",
        synthetic: true,
        b64: bytes(ackFrame(messageId, when)),
      };
    case 4:
      return {
        type: "data",
        t,
        monoUs,
        conn,
        dir: "a2l",
        synthetic: false,
        b64: bytes(resultFrame(`${messageId}B`, when, index + 1)),
      };
    case 5:
      return {
        type: "data",
        t,
        monoUs,
        conn,
        dir: "a2l",
        synthetic: false,
        b64: bytes(queryFrame(messageId, when)),
      };
    case 7:
      return {
        type: "info",
        t,
        monoUs,
        conn,
        message: "Автоответ: ACK отправлен",
      };
    default:
      return { type: "close", t, monoUs, conn, by: "analyzer", reason: "eof" };
  }
}

export function createMockBridge(scenario: MockScenario = scenarioFromLocation()): Bridge {
  let session: MockSession | null = null;
  let lastJournalPath: string | null = null;
  let journalDir = MOCK_LOG_DIR;
  /** Комментарии, дописанные в журнал заглушки: их сразу видно в просмотре (FR-23). */
  const journalNotes: FeedEvent[] = [];

  function emit(target: MockSession, events: FeedEvent[], dropped = 0): void {
    if (events.length === 0) return;
    // Счётчики — как у моста: строка состояния берёт их, а не считает по ленте.
    target.onBatch({
      events,
      dropped,
      stats: {
        connections: target.connections,
        bytesAnalyzerToLis: target.bytesA2l,
        bytesLisToAnalyzer: target.bytesL2a,
      },
    });
  }

  function dataEvent(
    target: MockSession,
    conn: number,
    dir: Dir,
    bytes: readonly number[],
    synthetic: boolean,
  ): FeedEvent {
    if (dir === "a2l") target.bytesA2l += bytes.length;
    else target.bytesL2a += bytes.length;
    return {
      type: "data",
      t: new Date().toISOString(),
      monoUs: Math.round((performance.now() - target.startedAtMono) * 1000),
      conn,
      dir,
      synthetic,
      b64: bytesToBase64(bytes),
    };
  }

  function textData(
    target: MockSession,
    conn: number,
    dir: Dir,
    text: string,
    synthetic: boolean,
  ): FeedEvent {
    return dataEvent(target, conn, dir, Array.from(new TextEncoder().encode(text)), synthetic);
  }

  function cp1251Data(target: MockSession, conn: number, dir: Dir, text: string): FeedEvent {
    return dataEvent(target, conn, dir, cp1251Bytes(text), false);
  }

  function at(target: MockSession): { t: string; monoUs: number } {
    return {
      t: new Date().toISOString(),
      monoUs: Math.round((performance.now() - target.startedAtMono) * 1000),
    };
  }

  function openEvent(target: MockSession, conn: number, peerHost: number): FeedEvent {
    return {
      type: "open",
      ...at(target),
      conn,
      peer: `192.168.1.${peerHost}:${49152 + target.step}`,
      local: target.startInfo.localAddr,
    };
  }

  function closeEvent(target: MockSession, conn: number): FeedEvent {
    return { type: "close", ...at(target), conn, by: "analyzer", reason: "eof" };
  }

  /**
   * Один шаг обмена: два одновременных соединения анализатора, результаты в UTF-8
   * и CP1251, синтетический автоответ, запрос, отключения — затем по кругу.
   */
  function nextEvents(target: MockSession): FeedEvent[] {
    const phase = target.step % 9;
    const messageId = `MSG${pad(target.step + 1, 4)}`;
    const now = new Date();

    switch (phase) {
      case 0: {
        target.conn += 1;
        target.second = target.conn + 1;
        target.connections += 2;
        return [
          openEvent(target, target.conn, 50 + target.conn),
          openEvent(target, target.second, 51 + target.conn),
        ];
      }
      case 1: {
        target.sample += 1;
        return [textData(target, target.conn, "a2l", resultFrame(messageId, now, target.sample), false)];
      }
      case 2:
        return [cp1251Data(target, target.conn, "a2l", resultFrameCp1251(messageId, now, target.sample))];
      case 3: {
        if (target.cfg.responder !== "hl7") return [];
        return [textData(target, target.conn, "l2a", ackFrame(messageId, now), true)];
      }
      case 4: {
        target.sample += 1;
        return [
          textData(target, target.second, "a2l", resultFrame(`${messageId}B`, now, target.sample), false),
        ];
      }
      case 5:
        return [textData(target, target.conn, "a2l", queryFrame(messageId, now), false)];
      case 6: {
        if (target.cfg.responder !== "hl7") return [];
        return [textData(target, target.second, "l2a", ackFrame(`${messageId}B`, now), true)];
      }
      case 7:
        return [
          {
            type: "info",
            ...at(target),
            conn: target.conn,
            message: "Автоответ: ACK отправлен",
          },
        ];
      default:
        return [closeEvent(target, target.conn), closeEvent(target, target.second)];
    }
  }

  /** Сколько фрагментов заливает сценарий `burst` и по сколько в пачке. */
  const BURST_EVENTS = 12_000;
  const BURST_BATCH = 500;
  const BURST_CONNECTIONS = 4;

  /** Пачка «длинной сессии»: 12 000 фрагментов за 60 с по четырём соединениям. */
  function burstBatch(target: MockSession, from: number): FeedEvent[] {
    const events: FeedEvent[] = [];
    for (let index = from; index < Math.min(from + BURST_BATCH, BURST_EVENTS); index += 1) {
      const monoUs = Math.round((index / BURST_EVENTS) * 60_000_000);
      // Последний фрагмент — «сейчас», первый — минуту назад: время в ленте не идёт назад.
      const when = new Date(Date.now() - (60_000_000 - monoUs) / 1000);
      const conn = (index % BURST_CONNECTIONS) + 1;
      const dir: Dir = index % 3 === 0 ? "l2a" : "a2l";
      const messageId = `BURST${pad(index, 6)}`;
      const cp1251 = index % 7 === 0;
      const text = cp1251
        ? resultFrameCp1251(messageId, when, index % 1000)
        : dir === "l2a"
          ? ackFrame(messageId, when)
          : resultFrame(messageId, when, index % 1000);
      const bytes = cp1251 ? cp1251Bytes(text) : Array.from(new TextEncoder().encode(text));
      if (dir === "a2l") target.bytesA2l += bytes.length;
      else target.bytesL2a += bytes.length;
      events.push({
        type: "data",
        t: when.toISOString(),
        monoUs,
        conn,
        dir,
        synthetic: dir === "l2a",
        b64: bytesToBase64(bytes),
      });
    }
    return events;
  }

  /** Заливка ленты для проверки виртуализации: пачки по 500 событий в отдельных задачах. */
  function startBurst(target: MockSession): void {
    // Заполненная минута уже «прошла»: следующие события продолжают её по mono_us.
    target.startedAtMono -= 60_000;
    const opens: FeedEvent[] = [];
    for (let conn = 1; conn <= BURST_CONNECTIONS; conn += 1) {
      opens.push(openEvent(target, conn, 60 + conn));
    }
    target.conn = BURST_CONNECTIONS;
    target.second = BURST_CONNECTIONS;
    target.connections += BURST_CONNECTIONS;
    emit(target, opens);

    let produced = 0;
    const push = (): void => {
      if (target.failed || session !== target) return;
      emit(target, burstBatch(target, produced));
      produced += BURST_BATCH;
      if (produced < BURST_EVENTS) setTimeout(push, 0);
    };
    setTimeout(push, 0);
  }

  function failJournal(target: MockSession): void {
    target.failed = true;
    if (target.timer) clearInterval(target.timer);
    target.timer = null;
    const { t, monoUs } = at(target);
    emit(target, [
      {
        type: "error",
        t,
        monoUs,
        message: "запись в журнал: на диске нет места",
      },
      {
        type: "stopped",
        reason: "journal_failure",
        code: "disk_full",
        message: "storage full (os error 112)",
      },
    ]);
  }

  return {
    async listInterfaces() {
      return [...MOCK_INTERFACES];
    },

    async defaultLogDir() {
      return MOCK_LOG_DIR;
    },

    async logDirWarning(dir) {
      const lower = dir.toLowerCase();
      const warning = (code: LogDirWarning["code"]): LogDirWarning => ({
        code,
        params: { dir },
      });
      if (lower.includes("onedrive")) return warning("onedrive");
      if (dir.startsWith("\\\\") || dir.startsWith("//")) return warning("network");
      return null;
    },

    async sessionStatus() {
      return {
        running: session !== null && !session.failed,
        startInfo: session?.startInfo ?? null,
      };
    },

    async startSession(cfg, onBatch) {
      if (scenario === "port_in_use") {
        throw new BridgeError("port_in_use", "порт занят", { port: String(cfg.listenPort) });
      }
      if (session && !session.failed) {
        throw new BridgeError("already_running", "сессия уже идёт");
      }

      const startedAt = new Date();
      const journalPath = joinPath(cfg.logDir || MOCK_LOG_DIR, journalName(startedAt));
      lastJournalPath = journalPath;
      journalDir = cfg.logDir || MOCK_LOG_DIR;
      const host =
        cfg.listenHost === "" || cfg.listenHost === "0.0.0.0"
          ? MOCK_INTERFACES[0].addr
          : cfg.listenHost;

      const target: MockSession = {
        cfg,
        startInfo: {
          journalPath,
          localAddr: `${host}:${cfg.listenPort}`,
          startedAt: startedAt.toISOString(),
        },
        startedAtMono: performance.now(),
        conn: 0,
        second: 0,
        step: -1,
        sample: 0,
        bytesA2l: 0,
        bytesL2a: 0,
        connections: 0,
        timer: null,
        onBatch,
        failed: false,
      };
      session = target;

      target.timer = setInterval(() => {
        target.step += 1;
        emit(target, nextEvents(target));
        if (scenario === "journal_failure" && target.step >= JOURNAL_FAILURE_AFTER_STEPS) {
          failJournal(target);
        }
      }, TICK_MS);

      if (scenario === "burst") startBurst(target);

      return target.startInfo;
    },

    async stopSession() {
      const target = session;
      if (!target) throw new BridgeError("io", "сессия не запущена");
      if (target.timer) clearInterval(target.timer);
      target.timer = null;
      session = null;

      if (target.failed) {
        throw new BridgeError("journal", "журнал прерван", {
          dir: target.cfg.logDir,
          path: target.startInfo.journalPath,
        });
      }

      emit(target, [{ type: "stopped", reason: "user" }]);
      return {
        stats: {
          connections: target.connections,
          bytesAnalyzerToLis: target.bytesA2l,
          bytesLisToAnalyzer: target.bytesL2a,
        },
        journalPath: target.startInfo.journalPath,
      };
    },

    async addSessionNote(text) {
      if (!session) throw new BridgeError("io", "сессия не запущена");
      emit(session, [{ type: "note", t: new Date().toISOString(), text }]);
    },

    async openJournal(path) {
      const density = Array.from({ length: 600 }, (_, index) =>
        Math.round(Math.abs(Math.sin(index / 17)) * 400),
      );
      return {
        path,
        header: {
          format: "bytetap-session/1",
          app_version: "0.1.0",
          started_at: "2026-10-19T10:15:00.000000+03:00",
          mode: "tcp_server",
          autoresponder: "hl7_mllp",
          settings: { listen: "0.0.0.0:5000" },
          analyzer: "ANALYZER, зав. № 1234",
          note: "3 пробы, 1 запрос",
          host: { os: "Windows 11 23H2" },
        },
        events: MOCK_JOURNAL_EVENTS + journalNotes.length,
        durationUs: MOCK_JOURNAL_DURATION_US,
        stats: { connections: 15, bytesAnalyzerToLis: 10_240, bytesLisToAnalyzer: 512 },
        density: { a2l: density, l2a: density.map((value) => Math.round(value / 3)) },
      } satisfies JournalSummary;
    },

    async readJournal(_path, from, count) {
      const events: FeedEvent[] = [];
      const total = MOCK_JOURNAL_EVENTS + journalNotes.length;
      for (let index = from; index < Math.min(from + count, total); index += 1) {
        const appended = journalNotes[index - MOCK_JOURNAL_EVENTS];
        if (appended !== undefined) events.push(appended);
        else if (index < MOCK_JOURNAL_EVENTS) events.push(mockJournalEvent(index));
      }
      return events;
    },

    async journalLengths(path) {
      void path;
      const total = MOCK_JOURNAL_EVENTS + journalNotes.length;
      return Array.from({ length: total }, (_, index) => {
        if (index >= MOCK_JOURNAL_EVENTS) return 0;
        const event = mockJournalEvent(index);
        return event.type === "data" ? base64ByteLength(event.b64) : 0;
      });
    },

    async appendNote(_path, text) {
      journalNotes.push({ type: "note", t: new Date().toISOString(), text });
    },

    async pickJournal() {
      return lastJournalPath ?? joinPath(MOCK_LOG_DIR, "bytetap-20261019-101500-tcp_server.jsonl");
    },

    async pickLogDir() {
      return joinPath(MOCK_LOG_DIR, "journals");
    },

    async openLogDir(dir) {
      console.info(`[mock] открыть папку журналов: ${dir || journalDir}`);
    },
  };
}
