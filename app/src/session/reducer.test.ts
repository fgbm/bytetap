import { describe, expect, it } from "vitest";
import type { FeedBatch, SessionSummary } from "../api/types";
import { initialSessionState, reduceSession, type SessionState } from "./reducer";

const startedAt = new Date(2026, 9, 19, 10, 15, 0).toISOString();

function recording(): SessionState {
  let state = reduceSession(initialSessionState(), { type: "start_requested" });
  state = reduceSession(state, {
    type: "started",
    info: { journalPath: "C:\\journals\\a.jsonl", localAddr: "0.0.0.0:5000", startedAt },
  });
  return state;
}

const summary: SessionSummary = {
  stats: { connections: 2, bytesAnalyzerToLis: 300, bytesLisToAnalyzer: 40 },
  journalPath: "C:\\journals\\a.jsonl",
};

describe("reduceSession", () => {
  it("идёт по цепочке idle → starting → recording → stopping → idle", () => {
    const starting = reduceSession(initialSessionState(), { type: "start_requested" });
    expect(starting.status).toBe("starting");

    const recordingState = reduceSession(starting, {
      type: "started",
      info: { journalPath: "j", localAddr: "0.0.0.0:5000", startedAt },
    });
    expect(recordingState.status).toBe("recording");
    expect(recordingState.startInfo?.journalPath).toBe("j");

    const stopping = reduceSession(recordingState, { type: "stop_requested" });
    expect(stopping.status).toBe("stopping");

    const idle = reduceSession(stopping, { type: "stopped", summary });
    expect(idle.status).toBe("idle");
    expect(idle.startInfo).toBeNull();
    expect(idle.summary).toEqual(summary);
    expect(idle.stats).toEqual(summary.stats);
  });

  it("не стартует повторно и не останавливает то, что не идёт", () => {
    const recordingState = recording();
    expect(reduceSession(recordingState, { type: "start_requested" })).toBe(recordingState);
    expect(reduceSession(initialSessionState(), { type: "stop_requested" }).status).toBe("idle");
  });

  it("накапливает байты по направлениям, соединения и dropped во время записи", () => {
    const batch: FeedBatch = {
      events: [
        { type: "open", t: startedAt, monoUs: 0, conn: 1, peer: "1.2.3.4:5", local: ":5000" },
        {
          type: "data",
          t: startedAt,
          monoUs: 1,
          conn: 1,
          dir: "a2l",
          synthetic: false,
          b64: btoa("1234"),
        },
        {
          type: "data",
          t: startedAt,
          monoUs: 2,
          conn: 1,
          dir: "l2a",
          synthetic: true,
          b64: btoa("12"),
        },
      ],
      dropped: 3,
    };

    const state = reduceSession(recording(), { type: "batch", batch });
    expect(state.stats).toEqual({ connections: 1, bytesAnalyzerToLis: 4, bytesLisToAnalyzer: 2 });
    expect(state.dropped).toBe(3);
  });

  it("ошибка старта возвращает в idle и оставляет код для поля", () => {
    const starting = reduceSession(initialSessionState(), { type: "start_requested" });
    const state = reduceSession(starting, {
      type: "start_failed",
      error: { code: "port_in_use", message: "bind failed", params: { port: "5000" } },
    });
    expect(state.status).toBe("idle");
    expect(state.error?.code).toBe("port_in_use");
    expect(state.startInfo).toBeNull();
  });

  it("отказ журнала переводит в failed и сохраняет сообщение (AC15)", () => {
    const state = reduceSession(recording(), {
      type: "journal_failed",
      message: "на диске нет места",
    });
    expect(state.status).toBe("failed");
    expect(state.journalFailure?.message).toBe("на диске нет места");
    expect(state.startInfo).toBeNull();

    const dismissed = reduceSession(state, { type: "failure_dismissed" });
    expect(dismissed.status).toBe("idle");
    expect(dismissed.journalFailure).toBeNull();
  });

  it("после отказа журнала события продолжают считаться, стоп подводит итог", () => {
    const failed: SessionState = { ...recording(), status: "failed", journalFailure: { message: "x" } };
    const withBatch = reduceSession(failed, {
      type: "batch",
      batch: {
        events: [
          {
            type: "data",
            t: startedAt,
            monoUs: 1,
            conn: 1,
            dir: "a2l",
            synthetic: false,
            b64: btoa("123"),
          },
        ],
        dropped: 0,
      },
    });
    expect(withBatch.stats.bytesAnalyzerToLis).toBe(3);

    const idle = reduceSession(withBatch, { type: "stopped", summary });
    expect(idle.status).toBe("idle");
    expect(idle.journalFailure).toBeNull();
  });

  it("просмотр журнала — отдельное состояние, во время записи не открывается", () => {
    const journal = {
      path: "a.jsonl",
      header: {},
      events: 1,
      durationUs: 1,
      stats: { connections: 4, bytesAnalyzerToLis: 300, bytesLisToAnalyzer: 40 },
      density: { a2l: [], l2a: [] },
    };
    const viewing = reduceSession(initialSessionState(), { type: "journal_opened", journal });
    expect(viewing.status).toBe("viewing");
    expect(viewing.journal?.path).toBe("a.jsonl");
    expect(viewing.stats, "строка состояния показывает итоги журнала").toEqual(journal.stats);

    const closed = reduceSession(viewing, { type: "journal_closed" });
    expect(closed.status).toBe("idle");
    expect(closed.journal).toBeNull();

    const recordingState = recording();
    expect(reduceSession(recordingState, { type: "journal_opened", journal })).toBe(recordingState);
  });

  it("старт из просмотра закрывает журнал и начинает заново", () => {
    const journal = {
      path: "a.jsonl",
      header: {},
      events: 0,
      durationUs: 0,
      stats: { connections: 0, bytesAnalyzerToLis: 0, bytesLisToAnalyzer: 0 },
      density: { a2l: [], l2a: [] },
    };
    const viewing = reduceSession(initialSessionState(), { type: "journal_opened", journal });
    const starting = reduceSession(viewing, { type: "start_requested" });
    expect(starting.status).toBe("starting");
    expect(starting.journal).toBeNull();
    expect(starting.summary).toBeNull();
  });
});
