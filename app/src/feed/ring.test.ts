import { describe, expect, it } from "vitest";
import type { FeedEvent } from "../api/types";
import { connChips, filterIndices } from "./filter";
import { FEED_CAPACITY, EventRing, FeedBuffer } from "./ring";

function data(index: number, conn = 1): FeedEvent {
  return {
    type: "data",
    t: new Date(2026, 9, 19, 10, 15, 0).toISOString(),
    monoUs: index,
    conn,
    dir: "a2l",
    synthetic: false,
    b64: btoa("x"),
  };
}

describe("EventRing", () => {
  it("держит ёмкость и вытесняет самые старые события", () => {
    const ring = new EventRing<number>(3);
    for (let value = 0; value < 5; value += 1) ring.push(value);

    expect(ring.length).toBe(3);
    expect(ring.toArray()).toEqual([2, 3, 4]);
  });

  it("сохраняет порядок после многих витков", () => {
    const ring = new EventRing<number>(4);
    for (let value = 0; value < 1001; value += 1) ring.push(value);

    expect(ring.toArray()).toEqual([997, 998, 999, 1000]);
  });

  it("на пустом буфере отдаёт ноль событий и не даёт прочитать лишнее", () => {
    const ring = new EventRing<number>(4);
    expect(ring.length).toBe(0);
    expect(() => ring.at(0)).toThrow(RangeError);
  });

  it("очищается и принимает события заново", () => {
    const ring = new EventRing<number>(2);
    ring.pushMany([1, 2, 3]);
    ring.clear();
    expect(ring.toArray()).toEqual([]);
    ring.push(9);
    expect(ring.toArray()).toEqual([9]);
  });
});

describe("FeedBuffer", () => {
  it("ограничен 10 000 событиями (AC7)", () => {
    const feed = new FeedBuffer<FeedEvent>(FEED_CAPACITY);
    const batch = Array.from({ length: 500 }, (_, index) => data(index));
    for (let round = 0; round < 200; round += 1) feed.pushMany(batch);

    expect(feed.length).toBe(FEED_CAPACITY);
    expect(feed.at(0)).toBe(batch[0]);
    expect(feed.at(FEED_CAPACITY - 1)).toBe(batch[batch.length - 1]);
  });

  it("поднимает версию на каждую пачку и очистку", () => {
    const feed = new FeedBuffer<FeedEvent>(10);
    const version = feed.version;
    feed.pushMany([data(1)]);
    expect(feed.version).toBe(version + 1);
    feed.clear();
    expect(feed.version).toBe(version + 2);
    expect(feed.length).toBe(0);
  });
});

describe("фильтр по соединению", () => {
  it("показывает события выбранного соединения и контекст сессии", () => {
    const feed = new FeedBuffer<FeedEvent>(100);
    feed.pushMany([
      data(1, 1),
      data(2, 2),
      { type: "note", t: new Date(2026, 9, 19, 10, 15, 0).toISOString(), text: "проба" },
      data(3, 2),
    ]);

    expect(filterIndices(feed, null)).toEqual([0, 1, 2, 3]);
    expect(filterIndices(feed, 1)).toEqual([0, 2]);
    expect(filterIndices(feed, 2)).toEqual([1, 2, 3]);
  });

  it("отдаёт номера соединений по возрастанию и не растит ряд чипов", () => {
    const feed = new FeedBuffer<FeedEvent>(100);
    feed.pushMany(Array.from({ length: 12 }, (_, index) => data(index, index + 1)));

    expect(connChips(feed)).toEqual([5, 6, 7, 8, 9, 10, 11, 12]);
  });
});
