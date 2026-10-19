/**
 * Заглушка моста для `npm run dev`, браузера и тестов.
 *
 * Отдаёт те же типы, что ядро, и синтетический HL7-обмен ANALYZER ↔ LIS ↔ PATIENT^TEST
 * по таймеру: подключение анализатора, ORU^R01, синтетический ACK автоответчика, QRY.
 * Ошибки воспроизводятся сценарием из адреса страницы: `?scenario=port_in_use`
 * или `?scenario=journal_failure`.
 */

import type { Bridge } from "./bridge";
import {
  BridgeError,
  type Dir,
  type FeedBatch,
  type FeedEvent,
  type InterfaceInfo,
  type JournalSummary,
  type SessionConfig,
  type StartInfo,
} from "./types";

export const MOCK_INTERFACES: InterfaceInfo[] = [
  { name: "Ethernet", addr: "192.168.1.10" },
  { name: "Wi-Fi", addr: "10.0.0.5" },
];

const MOCK_LOG_DIR = "C:\\Users\\operator\\Documents\\bytetap";
const TICK_MS = 500;
const JOURNAL_FAILURE_AFTER_STEPS = 14;

export type MockScenario = "default" | "port_in_use" | "journal_failure";

function scenarioFromLocation(): MockScenario {
  if (typeof window === "undefined") return "default";
  const value = new URLSearchParams(window.location.search).get("scenario");
  return value === "port_in_use" || value === "journal_failure" ? value : "default";
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

function toBase64(text: string): string {
  return btoa(String.fromCharCode(...new TextEncoder().encode(text)));
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

interface MockSession {
  cfg: SessionConfig;
  startInfo: StartInfo;
  startedAtMono: number;
  conn: number;
  step: number;
  sample: number;
  bytesA2l: number;
  bytesL2a: number;
  connections: number;
  timer: ReturnType<typeof setInterval> | null;
  onBatch: (batch: FeedBatch) => void;
  failed: boolean;
}

export function createMockBridge(scenario: MockScenario = scenarioFromLocation()): Bridge {
  let session: MockSession | null = null;
  let lastJournalPath: string | null = null;
  let journalDir = MOCK_LOG_DIR;

  function emit(target: MockSession, events: FeedEvent[], dropped = 0): void {
    if (events.length === 0) return;
    target.onBatch({ events, dropped });
  }

  function dataEvent(target: MockSession, dir: Dir, text: string, synthetic: boolean): FeedEvent {
    const bytes = new TextEncoder().encode(text).length;
    if (dir === "a2l") target.bytesA2l += bytes;
    else target.bytesL2a += bytes;
    return {
      type: "data",
      t: new Date().toISOString(),
      monoUs: Math.round((performance.now() - target.startedAtMono) * 1000),
      conn: target.conn,
      dir,
      synthetic,
      b64: toBase64(text),
    };
  }

  function at(target: MockSession): { t: string; monoUs: number } {
    return {
      t: new Date().toISOString(),
      monoUs: Math.round((performance.now() - target.startedAtMono) * 1000),
    };
  }

  /**
   * Один шаг обмена: подключение анализатора, результаты, синтетический автоответ,
   * запрос, отключение — затем всё по кругу с новым номером соединения.
   */
  function nextEvents(target: MockSession): FeedEvent[] {
    const phase = target.step % 6;
    const messageId = `MSG${pad(target.step + 1, 4)}`;
    const now = new Date();

    switch (phase) {
      case 0: {
        target.conn += 1;
        target.connections += 1;
        return [
          {
            type: "open",
            ...at(target),
            conn: target.conn,
            peer: `192.168.1.${50 + target.conn}:${49152 + target.step}`,
            local: target.startInfo.localAddr,
          },
        ];
      }
      case 1: {
        target.sample += 1;
        return [dataEvent(target, "a2l", resultFrame(messageId, now, target.sample), false)];
      }
      case 2: {
        if (target.cfg.responder !== "hl7") return [];
        return [dataEvent(target, "l2a", ackFrame(messageId, now), true)];
      }
      case 3:
        return [dataEvent(target, "a2l", queryFrame(messageId, now), false)];
      case 4: {
        if (target.cfg.responder !== "hl7") return [];
        return [
          {
            type: "info",
            ...at(target),
            conn: target.conn,
            message: "Автоответ: ACK отправлен",
          },
        ];
      }
      default:
        return [
          {
            type: "close",
            ...at(target),
            conn: target.conn,
            by: "analyzer",
            reason: "соединение закрыто анализатором",
          },
        ];
    }
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
        message: "на диске нет места",
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
      if (lower.includes("onedrive")) return "onedrive";
      if (dir.startsWith("\\\\") || dir.startsWith("//")) return "network";
      return null;
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
          analyzer: "ANALYZER, зав. № 1234",
          mode: "tcp_server",
          responder: "hl7",
          note: "3 пробы, 1 запрос",
        },
        events: 128,
        durationUs: 2_538_000_000,
        stats: { connections: 3, bytesAnalyzerToLis: 10_240, bytesLisToAnalyzer: 512 },
        density: { a2l: density, l2a: density.map((value) => Math.round(value / 3)) },
      } satisfies JournalSummary;
    },

    async readJournal() {
      return [];
    },

    async appendNote() {
      // В заглушке дописывать некуда: журнала на диске нет.
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
