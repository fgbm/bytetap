import { describe, expect, it } from "vitest";
import { i18n } from "../i18n";
import type { FeedEvent } from "../api/types";
import { eventRow } from "./rows";

const t = i18n.global.t as (key: string, params?: Record<string, unknown>) => string;

function info(code?: string): FeedEvent {
  return {
    type: "info",
    t: "2026-10-19T10:15:03.000+03:00",
    monoUs: 3_000_000,
    conn: 1,
    message: "protocol is not detected: the first significant byte 0x4D",
    ...(code === undefined ? {} : { code }),
  };
}

describe("строки ленты — тексты событий по коду (AC6)", () => {
  it("известный код переводится, английский message остаётся в журнале", () => {
    const row = eventRow(info("protocol_undetected"), 0, t, "ascii");
    expect(row.text).toBe(t("events.protocol_undetected"));
    expect(row.text).not.toContain("first significant byte");
  });

  it("неизвестный код и событие без кода показывают message как есть", () => {
    expect(eventRow(info("new_code"), 0, t, "ascii").text).toContain("first significant byte");
    expect(eventRow(info(), 0, t, "ascii").text).toContain("first significant byte");
  });
});
