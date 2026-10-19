import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { shallowReactive } from "vue";
import FeedView from "./FeedView.vue";
import { i18n } from "../i18n";
import type { FeedEvent, JournalSummary } from "../api/types";
import { JournalFeed } from "../journal/feed";

/** Событие с номером в тексте: по нему видно, какую страницу вернул загрузчик. */
function note(index: number): FeedEvent {
  return { type: "note", t: "2026-10-19T10:15:03.000+03:00", text: `событие ${index}` };
}

const EVENTS = 3000;

/** Сводка журнала: таблица корзин считает события ровно по времени. */
const summary: JournalSummary = {
  path: "C:\\journals\\a.jsonl",
  header: {},
  events: EVENTS,
  durationUs: 3_000_000,
  stats: { connections: 1, bytesAnalyzerToLis: 10, bytesLisToAnalyzer: 0 },
  density: { a2l: [], l2a: [] },
  firstEvent: Array.from({ length: 601 }, (_, point) =>
    Math.min(EVENTS, Math.round((point / 600) * EVENTS)),
  ),
};

/** Загрузчик отвечает через `setTimeout`: страница журнала приходит не в том же такте. */
function slowLoader(calls: number[]) {
  return (from: number, count: number): Promise<FeedEvent[]> =>
    new Promise((resolve) => {
      calls.push(from);
      setTimeout(
        () => resolve(Array.from({ length: count }, (_, offset) => note(from + offset))),
        0,
      );
    });
}

/** Даёт странице доехать: таймер, затем микрозадачи Vue. */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await flushPromises();
}

function mountJournal(view: "text" | "hex" = "text") {
  const calls: number[] = [];
  const feed = shallowReactive(
    new JournalFeed(summary.path, summary.events, slowLoader(calls)),
  );
  const wrapper = mount(FeedView, {
    props: { feed, journal: summary, view, encoding: "cp1251" as const },
    global: { plugins: [i18n] },
  });
  return { wrapper, feed, calls };
}

describe("FeedView — просмотр журнала (AC5, AC10)", () => {
  it("перерисовывает ленту, когда приходит страница журнала", async () => {
    const { wrapper, feed } = mountJournal();
    const before = feed.version;

    // Первый кадр: строки ещё не приехали, но страница уже запрошена.
    expect(wrapper.findAll(".bt-feed-row")).toHaveLength(0);

    await settle();

    expect(feed.version).toBeGreaterThan(before);
    const rows = wrapper.findAll(".bt-feed-row");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows[0].text()).toContain("событие 0");
  });

  it("показывает строки вокруг места, куда перешла трасса", async () => {
    const { wrapper, feed } = mountJournal();
    await settle();

    // Доля времени 0.5 на трассе — середина журнала, а не середина загруженного.
    const middle = feed.length / 2;
    (wrapper.vm as unknown as { seekEvent(index: number): void }).seekEvent(middle);
    await settle();

    const rows = wrapper.findAll(".bt-feed-row");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows[0].text()).toContain(`событие ${middle - 8}`);

    const visible = wrapper.emitted("update:visible");
    const last = visible?.at(-1)?.[0] as { start: number; end: number } | undefined;
    expect(last).toBeDefined();
    expect(last!.start).toBeGreaterThanOrEqual(0);
    expect(last!.end).toBeLessThanOrEqual(1);
    expect(last!.end).toBeGreaterThan(last!.start);
  });

  it("в виде HEX рисует дамп пришедшей страницы", async () => {
    const { wrapper } = mountJournal("hex");
    await settle();

    expect(wrapper.findAll(".bt-feed-hex").length).toBeGreaterThan(0);
  });
});
