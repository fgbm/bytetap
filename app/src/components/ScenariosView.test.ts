import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import ScenariosView from "./ScenariosView.vue";
import { i18n } from "../i18n";
import type { RecentJournal } from "../api/types";
import type { LastRun } from "../settings/settings";

const LAST: LastRun = { mode: "tcp_server", port: "5000", host: "", baud: 0 };

const RECENTS: RecentJournal[] = [
  {
    path: "C:\\journals\\bytetap-20261019-101500-tcp_server.jsonl",
    startedAt: new Date().toISOString(),
    analyzer: "ANALYZER",
    durationUs: 2_520_000_000,
    mode: "tcp_server",
  },
];

function mountView(overrides: Partial<{ lastRun: LastRun | null; recents: RecentJournal[] | null; warning: string; busy: boolean }> = {}) {
  return mount(ScenariosView, {
    props: {
      lastRun: LAST,
      recents: RECENTS,
      warning: "",
      busy: false,
      ...overrides,
    },
    global: { plugins: [i18n] },
  });
}

describe("ScenariosView — выбор сценария (UI v2)", () => {
  it("выделяет последний сценарий и показывает его порт", () => {
    const wrapper = mountView();
    const cards = wrapper.findAll(".bt-scenario");

    expect(cards).toHaveLength(3);
    expect(cards[0].classes()).toContain("last");
    expect(cards[1].classes()).not.toContain("last");
    expect(cards[0].text()).toContain(
      i18n.global.t("scenarios.lastTime", { detail: i18n.global.t("scenarios.lastPort", { port: "5000" }) }),
    );
    expect(cards[1].text()).not.toContain("последний раз");
  });

  it("ставит курсор на последний сценарий и ходит по карточкам стрелками", async () => {
    const wrapper = mountView();
    expect(wrapper.findAll(".bt-scenario")[0].attributes("aria-selected")).toBe("true");

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" }));
    await wrapper.vm.$nextTick();
    expect(wrapper.findAll(".bt-scenario")[1].attributes("aria-selected")).toBe("true");

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp" }));
    await wrapper.vm.$nextTick();
    expect(wrapper.findAll(".bt-scenario")[0].attributes("aria-selected")).toBe("true");

    // Вверх с первой карточки — на последнюю: список замкнут.
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp" }));
    await wrapper.vm.$nextTick();
    expect(wrapper.findAll(".bt-scenario")[2].attributes("aria-selected")).toBe("true");

    wrapper.unmount();
  });

  it("Enter открывает выбранный сценарий, клик — свой", async () => {
    const wrapper = mountView();

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    await wrapper.vm.$nextTick();
    expect(wrapper.emitted("select")).toEqual([["tcp_client"]]);

    await wrapper.findAll(".bt-scenario")[2].trigger("click");
    expect(wrapper.emitted("select")?.[1]).toEqual(["serial"]);

    wrapper.unmount();
  });

  it("без команды списка журналов блок «Последние записи» скрыт", () => {
    expect(mountView({ recents: null }).find(".bt-recents").exists()).toBe(false);
    expect(mountView({ recents: [] }).find(".bt-recents").exists()).toBe(false);
    expect(mountView().find(".bt-recents").exists()).toBe(true);
  });

  it("открывает последнюю запись по клику", async () => {
    const wrapper = mountView();
    await wrapper.get(".bt-recent").trigger("click");
    expect(wrapper.emitted("openRecent")).toEqual([[RECENTS[0].path]]);
  });
});
