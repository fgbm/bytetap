import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { base64ToBytes } from "../feed/bytes";
import { defaultConfig, sessionConfigFor, type AppConfig } from "../settings/settings";
import { createMockBridge, type MockScenario } from "./mock-bridge";
import type { DataEvent, FeedBatch, FeedEvent, SessionConfig } from "./types";

/** Шаг заглушки: по нему же идут номера кадров ASTM. */
const TICK_MS = 500;

function config(mode: AppConfig["mode"], patch: Partial<AppConfig> = {}): SessionConfig {
  return sessionConfigFor({ ...defaultConfig("C:\\journals"), mode, ...patch });
}

/** События заглушки за `ticks` шагов: таймер двигается виртуальным временем. */
async function collect(
  scenario: MockScenario,
  cfg: SessionConfig,
  ticks: number,
): Promise<FeedEvent[]> {
  const bridge = createMockBridge(scenario);
  const events: FeedEvent[] = [];
  await bridge.startSession(cfg, (batch: FeedBatch) => {
    events.push(...batch.events);
  });
  vi.advanceTimersByTime(TICK_MS * ticks);
  return events;
}

function closedConns(events: readonly FeedEvent[]): number[] {
  const conns: number[] = [];
  for (const event of events) {
    if (event.type === "close") conns.push(event.conn);
  }
  return conns;
}

describe("заглушка моста: закрытие соединений", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("закрывает каждое соединение сервера один раз за цикл", async () => {
    const events = await collect("default", config("tcp_server"), 10);
    expect(closedConns(events)).toEqual([1, 2]);
  });

  it("у клиента с одним соединением второго «отключился» нет", async () => {
    const events = await collect("default", config("tcp_client"), 10);
    expect(closedConns(events)).toEqual([1]);
  });
});

describe("заглушка моста: отказ журнала", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("отдаёт путь журнала сессии: записанное открывается и после сбоя", async () => {
    const bridge = createMockBridge("journal_failure");
    const events: FeedEvent[] = [];
    const info = await bridge.startSession(config("tcp_server"), (batch: FeedBatch) => {
      events.push(...batch.events);
    });
    // Сбой наступает после нескольких шагов обмена: часть журнала уже записана.
    vi.advanceTimersByTime(TICK_MS * 15);

    expect(events.find((event) => event.type === "stopped")).toMatchObject({
      reason: "journal_failure",
      code: "disk_full",
    });
    expect(info.journalPath.endsWith("-tcp_server.jsonl")).toBe(true);

    // Путь сессии виден и `session_status`: страница после перезагрузки открывает по нему.
    const status = await bridge.sessionStatus();
    expect(status).toMatchObject({ running: false, startInfo: { journalPath: info.journalPath } });

    // Журнал, оборванный без `session_end`, просмотр открывает.
    const summary = await bridge.openJournal(info.journalPath);
    expect(summary.path).toBe(info.journalPath);
  });
});

describe("заглушка моста: кадры ASTM E1381", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("нумерует кадры ASCII-цифрами 1…7, 0 и считает сумму по кадру", async () => {
    const events = await collect("default", config("tcp_server", { responder: "astm" }), 10);
    const frames = events
      .filter((event): event is DataEvent => event.type === "data" && event.dir === "a2l")
      .map((event) => base64ToBytes(event.b64))
      .filter((bytes) => bytes[0] === 0x02);

    expect(frames).toHaveLength(3);
    // Первый кадр после ENQ — «1», дальше по порядку; не байты 0x01 и 0x02.
    expect(frames.map((bytes) => String.fromCharCode(bytes[1]))).toEqual(["1", "2", "3"]);

    const [first, second] = frames;
    const length = first.length;
    // Хвост кадра: … CR ETX C1 C2 CR LF.
    expect(first[length - 6]).toBe(0x0d);
    expect(first[length - 5]).toBe(0x03);
    expect(first[length - 2]).toBe(0x0d);
    expect(first[length - 1]).toBe(0x0a);
    // Промежуточный кадр закрыт ETB.
    expect(second[second.length - 5]).toBe(0x17);

    const sum = first.slice(1, length - 4).reduce((total, byte) => total + byte, 0) & 0xff;
    const sent = Number.parseInt(
      String.fromCharCode(first[length - 4], first[length - 3]),
      16,
    );
    expect(sent).toBe(sum);
  });
});
