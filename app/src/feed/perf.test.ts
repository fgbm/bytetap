import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { shallowReactive } from "vue";
import FeedView from "../components/FeedView.vue";
import type { Dir, FeedEvent } from "../api/types";
import { FeedBuffer } from "./ring";
import { i18n } from "../i18n";
import { resetSession, useSession } from "../session/store";

const STAMP = new Date(2026, 9, 19, 10, 15, 3).toISOString();

function b64(text: string): string {
  return btoa(text);
}

function data(
  index: number,
  options: { conn?: number; dir?: Dir; synthetic?: boolean; text?: string } = {},
): FeedEvent {
  return {
    type: "data",
    t: STAMP,
    monoUs: index * 1000,
    conn: options.conn ?? 1,
    dir: options.dir ?? "a2l",
    synthetic: options.synthetic ?? false,
    b64: b64(options.text ?? `MSG${index}\r`),
  };
}

function mountFeed(feed: FeedBuffer<FeedEvent>, view: "text" | "hex" = "text") {
  return mount(FeedView, {
    props: {
      feed,
      journal: null,
      view,
      encoding: "cp1251" as const,
      dropped: 0,
    },
    global: { plugins: [i18n] },
  });
}

describe("FeedView — текстовый вид", () => {
  it("рисует фрагмент: время, глиф направления и `‹VT›` без плашки", () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(100));
    feed.pushMany([data(1, { text: "\u000bMSH|\r" })]);
    const wrapper = mountFeed(feed);

    const row = wrapper.find(".bt-feed-row");
    expect(row.exists()).toBe(true);
    expect(row.find(".ts").text()).toMatch(/^\d\d:\d\d:\d\d\.\d{3}$/);
    // Одно соединение — колонка номера не нужна.
    expect(row.find(".cn").exists()).toBe(false);
    expect(row.find(".ar").text()).toBe("→");
    expect(row.find(".bt-cc").text()).toBe("‹VT›");
    expect(row.text()).toContain("MSH|");
    expect(row.classes()).toContain("a2l");
  });

  it("не даёт управляющим символам фона: в стилях остаются только цвет и кегль", () => {
    const css = readFileSync(resolve(process.cwd(), "src/styles/app.css"), "utf8");
    const rule = css.slice(css.indexOf(".bt-cc {"), css.indexOf("}", css.indexOf(".bt-cc {")));
    expect(rule).toContain("color:");
    expect(rule).not.toContain("background");
    expect(rule).not.toContain("border");
  });

  it("показывает автоответ словом в конце строки, без штриховки", () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(100));
    feed.pushMany([data(1, { dir: "l2a", synthetic: true, text: "\u000bACK\r" })]);
    const wrapper = mountFeed(feed);

    const row = wrapper.find(".bt-feed-row");
    expect(row.classes()).toContain("l2a");
    expect(row.classes()).not.toContain("syn");
    expect(row.find(".bt-syn").text()).toBe(i18n.global.t("feed.synthetic"));
  });

  it("показывает служебные строки open, close и error", () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(100));
    feed.pushMany([
      { type: "open", t: STAMP, monoUs: 1, conn: 1, peer: "192.168.1.50:49152", local: ":5000" },
      { type: "close", t: STAMP, monoUs: 2, conn: 1, by: "analyzer", reason: "eof" },
      { type: "error", t: STAMP, monoUs: 3, conn: 1, message: "сброс соединения" },
      { type: "note", t: STAMP, text: "проба 1" },
    ]);
    const wrapper = mountFeed(feed);

    const rows = wrapper.findAll(".bt-feed-row");
    expect(rows).toHaveLength(4);
    expect(rows[0].classes()).toContain("svc");
    expect(rows[0].text()).toContain("#1 подключился 192.168.1.50:49152");
    expect(rows[0].find(".svc-line").exists()).toBe(true);
    expect(rows[1].classes()).toContain("svc");
    expect(rows[1].text()).toContain("#1 отключился · анализатор");
    expect(rows[2].classes()).toContain("err");
    expect(rows[3].text()).toContain("проба 1");
  });

  it("показывает номер соединения, когда соединений было больше одного", () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(100));
    feed.pushMany([data(1, { conn: 1 }), data(2, { conn: 2 })]);
    const wrapper = mountFeed(feed);

    const rows = wrapper.findAll(".bt-feed-row");
    expect(rows[0].find(".cn").text()).toBe("#1");
    expect(rows[1].find(".cn").text()).toBe("#2");
  });
});

describe("FeedView — HEX и фильтр", () => {
  it("разворачивает фрагмент в дамп по 16 байт", () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(100));
    feed.pushMany([data(1, { text: "\u000bMSH|^~\\&|ANALYZER\r\u001c\r" })]);
    const wrapper = mountFeed(feed, "hex");

    const rows = wrapper.findAll(".bt-feed-hex");
    expect(rows).toHaveLength(2);
    expect(rows[0].findAll(".o")[0].text()).toMatch(/^\d\d:\d\d:\d\d\.\d{3}$/);
    expect(rows[0].text()).toContain("0000");
    expect(rows[1].text()).toContain("0010");
    expect(rows[0].findAll("i")).toHaveLength(1);
    expect(rows[0].find(".a").text()).toBe(".MSH|^~\\&|ANALYZ");
  });

  it("фильтр соединений — только при нескольких соединениях", () => {
    const single = shallowReactive(new FeedBuffer<FeedEvent>(100));
    single.pushMany([data(1, { conn: 1 }), data(2, { conn: 1 })]);
    expect(mountFeed(single).find(".bt-conns").exists()).toBe(false);

    const two = shallowReactive(new FeedBuffer<FeedEvent>(100));
    two.pushMany([data(1, { conn: 1 }), data(2, { conn: 2 })]);
    expect(mountFeed(two).find(".bt-conns").exists()).toBe(true);
  });

  it("фильтрует строки по номеру соединения", async () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(100));
    feed.pushMany([
      data(1, { conn: 1, text: "first" }),
      data(2, { conn: 2, text: "second" }),
    ]);
    const wrapper = mountFeed(feed);

    const chips = wrapper.findAll(".bt-conns button");
    expect(chips.map((chip) => chip.text())).toEqual(["все", "#1", "#2"]);

    await chips[2].trigger("click");
    const rows = wrapper.findAll(".bt-feed-row");
    expect(rows).toHaveLength(1);
    expect(rows[0].text()).toContain("second");
  });

  it("показывает плавающую таблетку «N новых» вместо кнопки автопрокрутки", async () => {
    const feed = shallowReactive(new FeedBuffer<FeedEvent>(1000));
    feed.pushMany(Array.from({ length: 40 }, (_, index) => data(index)));
    const wrapper = mountFeed(feed);
    const scroller = wrapper.get(".bt-feed");

    // Кнопки автопрокрутки больше нет.
    expect(wrapper.find(".autoscroll").exists()).toBe(false);

    // Прокрутка вверх: догон выключается, новые строки считаются.
    Object.defineProperty(scroller.element, "scrollHeight", { value: 4000, configurable: true });
    Object.defineProperty(scroller.element, "clientHeight", { value: 200, configurable: true });
    (scroller.element as HTMLElement).scrollTop = 100;
    await scroller.trigger("scroll");

    feed.pushMany(Array.from({ length: 3 }, (_, index) => data(100 + index)));
    await flushPromises();

    const pill = wrapper.find(".bt-newpill");
    expect(pill.exists()).toBe(true);
    expect(pill.text()).toContain("3");

    await pill.trigger("click");
    expect(wrapper.find(".bt-newpill").exists()).toBe(false);
  });
});

describe("FeedView — виртуализация и буфер (AC7, FR-28)", () => {
  it("100 000 событий через store оставляют 10 000 в буфере", () => {
    resetSession();
    const { feed } = useSession();
    const batch = Array.from({ length: 500 }, (_, index) => data(index, { conn: index % 4 }));

    for (let round = 0; round < 200; round += 1) feed.pushMany(batch);

    expect(feed.length).toBe(10_000);
    expect(feed.at(0)).toBe(batch[0]);
  });

  it("в DOM только видимые строки и запас, а не все 10 000", async () => {
    resetSession();
    const { feed } = useSession();
    const batch = Array.from({ length: 500 }, (_, index) => data(index, { conn: index % 4 }));
    for (let round = 0; round < 200; round += 1) feed.pushMany(batch);

    const wrapper = mountFeed(feed);
    await flushPromises();

    const rows = wrapper.findAll(".bt-feed-row");
    // 400 / 28 ≈ 15 видимых строк, по 8 в запасе сверху и снизу.
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThanOrEqual(32);
    expect(wrapper.find(".bt-feed-list").attributes("style")).toContain("280000px");
  });

  it("пачка из 5000 событий добавляется быстрее 50 мс (мягкий порог)", () => {
    const feed = new FeedBuffer<FeedEvent>(10_000);
    const batch = Array.from({ length: 5000 }, (_, index) => data(index));

    const started = performance.now();
    feed.pushMany(batch);
    const elapsed = performance.now() - started;

    // Порог мягкий: он ловит появление копирования массива, а не разброс CI.
    expect(elapsed).toBeLessThan(50);
    expect(feed.length).toBe(5000);
  });
});
