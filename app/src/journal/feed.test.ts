import { describe, expect, it } from "vitest";
import type { FeedEvent } from "../api/types";
import { JOURNAL_PAGE, JOURNAL_PAGES, JournalFeed } from "./feed";

/** Событие с номером в тексте: по нему видно, какую страницу вернул загрузчик. */
function note(index: number): FeedEvent {
  return { type: "note", t: "2026-10-19T10:15:00+03:00", text: `событие ${index}` };
}

/** Загрузчик, который считает вызовы и отдаёт события по номеру. */
function loader() {
  const calls: { from: number; count: number }[] = [];
  const load = async (from: number, count: number): Promise<FeedEvent[]> => {
    calls.push({ from, count });
    return Array.from({ length: count }, (_, offset) => note(from + offset));
  };
  return { calls, load: load as (from: number, count: number) => Promise<FeedEvent[]> };
}

function deferred() {
  let resolve: (events: FeedEvent[]) => void = () => undefined;
  const promise = new Promise<FeedEvent[]>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("JournalFeed", () => {
  it("читает страницу по требованию и отдаёт undefined, пока она едет", async () => {
    const pending = deferred();
    const feed = new JournalFeed("a.jsonl", 128, () => pending.promise);

    expect(feed.length, "длина берётся из сводки, без чтения файла").toBe(128);
    expect(feed.at(0)).toBeUndefined();

    pending.resolve(Array.from({ length: 128 }, (_, index) => note(index)));
    await pending.promise;
    await Promise.resolve();

    expect(feed.at(0)).toEqual(note(0));
    expect(feed.at(127)).toEqual(note(127));
    expect(feed.at(128)).toBeUndefined();
    expect(feed.version).toBeGreaterThan(0);
  });

  it("просит страницы видимого окна, а не всего журнала", async () => {
    const total = JOURNAL_PAGE * 10;
    const { calls, load } = loader();
    const feed = new JournalFeed("a.jsonl", total, load);

    feed.ensure(0, 20);
    expect(calls).toEqual([{ from: 0, count: JOURNAL_PAGE }]);
    await Promise.resolve();
    await Promise.resolve();

    feed.ensure(JOURNAL_PAGE + 5, JOURNAL_PAGE + 30);
    expect(calls).toHaveLength(2);
    expect(calls[1]).toEqual({ from: JOURNAL_PAGE, count: JOURNAL_PAGE });
  });

  it("не держит в памяти больше страниц, чем разрешено (LRU)", async () => {
    const total = JOURNAL_PAGE * 40;
    const { load } = loader();
    const feed = new JournalFeed("a.jsonl", total, load);

    for (let page = 0; page < 40; page += 1) {
      feed.ensure(page * JOURNAL_PAGE, page * JOURNAL_PAGE + 1);
      await Promise.resolve();
      await Promise.resolve();
    }

    expect(feed.pagesInMemory).toBe(JOURNAL_PAGES);
    expect(feed.at(39 * JOURNAL_PAGE), "последняя страница на месте").toBeDefined();
    expect(feed.at(0), "самая старая вытеснена").toBeUndefined();
  });

  it("дописанный комментарий продлевает журнал и перечитывает последнюю страницу", async () => {
    const { calls, load } = loader();
    const feed = new JournalFeed("a.jsonl", 3, load);

    feed.ensure(0, 3);
    await Promise.resolve();
    await Promise.resolve();
    expect(feed.at(2)).toEqual(note(2));

    feed.extend(4);
    expect(feed.length).toBe(4);
    expect(feed.at(3), "новое событие ещё не прочитано").toBeUndefined();
    expect(calls.at(-1)).toEqual({ from: 0, count: 4 });
  });

  it("отказ страницы не повторяется и не зацикливает прокрутку", async () => {
    let attempts = 0;
    const feed = new JournalFeed("a.jsonl", 10, async () => {
      attempts += 1;
      throw new Error("журнал не читается");
    });

    feed.ensure(0, 10);
    await Promise.resolve();
    await Promise.resolve();
    feed.ensure(0, 10);
    await Promise.resolve();
    await Promise.resolve();

    expect(attempts).toBe(1);
    expect(feed.at(0)).toBeUndefined();
  });

  it("отдаёт последние комментарии журнала, новые первыми", async () => {
    const total = JOURNAL_PAGE + 100;
    const feed = new JournalFeed("a.jsonl", total, async (from, count) =>
      from === 0
        ? [note(0), { type: "note", t: "t", text: "старый" }]
        : Array.from({ length: count }, (_, offset) => note(from + offset)),
    );

    feed.ensure(0, total);
    await Promise.resolve();
    await Promise.resolve();

    expect(feed.notes(2)).toEqual(["событие 599", "событие 598"]);
  });
});
