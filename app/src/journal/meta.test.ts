import { describe, expect, it } from "vitest";
import { journalDuration, journalFileName, journalMeta, journalNotes } from "./meta";

const header = {
  mode: "tcp_server",
  autoresponder: "hl7_mllp",
  settings: { listen: "0.0.0.0:5000" },
  started_at: "2026-10-19T10:15:00.000000+03:00",
  analyzer: "ANALYZER, зав. № 1234",
  note: "3 пробы, 1 запрос",
};

describe("journalMeta", () => {
  it("разбирает заголовок: анализатор, режим, адрес, порт, начало и комментарий", () => {
    expect(journalMeta(header)).toEqual({
      analyzer: "ANALYZER, зав. № 1234",
      mode: "tcp_server",
      responder: "hl7_mllp",
      listen: "0.0.0.0:5000",
      port: "5000",
      startedAt: "2026-10-19T10:15:00.000000+03:00",
      note: "3 пробы, 1 запрос",
    });
  });

  it("журнал без необязательных полей даёт пустые строки, а не падает", () => {
    const meta = journalMeta({ mode: "tcp_client" });
    expect(meta.analyzer).toBe("");
    expect(meta.responder).toBe("");
    expect(meta.listen).toBe("");
    expect(meta.port).toBe("");
    expect(meta.note).toBe("");
    expect(meta.mode).toBe("tcp_client");
  });

  it("понимает IPv6-адрес в settings.listen", () => {
    expect(journalMeta({ settings: { listen: "[::1]:5000" } }).port).toBe("5000");
  });
});

describe("journalFileName", () => {
  it("отбрасывает папку в путях Windows и Unix", () => {
    expect(journalFileName("C:\\Users\\operator\\Documents\\bytetap\\a.jsonl")).toBe("a.jsonl");
    expect(journalFileName("/home/operator/bytetap/a.jsonl")).toBe("a.jsonl");
    expect(journalFileName("a.jsonl")).toBe("a.jsonl");
  });
});

describe("journalDuration", () => {
  it("всегда показывает часы, минуты и секунды", () => {
    expect(journalDuration(2_538_000_000)).toBe("00:42:18");
    expect(journalDuration(3_661_000_000)).toBe("01:01:01");
    expect(journalDuration(0)).toBe("00:00:00");
  });
});

describe("journalNotes", () => {
  it("ставит комментарий заголовка первым, затем дописанные по порядку", () => {
    // `notes()` отдаёт новые первыми; в строке они идут от старых к новым.
    expect(journalNotes("старт", ["четвёртый", "третий"])).toBe("старт · третий · четвёртый");
  });

  it("пропускает пустые комментарии", () => {
    expect(journalNotes("", [])).toBe("");
    expect(journalNotes("", ["один"])).toBe("один");
  });
});
