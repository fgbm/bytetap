import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import ActionBar from "./ActionBar.vue";
import { i18n } from "../i18n";
import type { SessionSummary } from "../api/types";
import { defaultConfig } from "../settings/settings";
import type { SessionStatus } from "../session/reducer";

const summary: SessionSummary | null = null;

function mountBar(status: SessionStatus, profiles = [{ name: "A", config: defaultConfig() }]) {
  return mount(ActionBar, {
    props: {
      status,
      elapsedMs: 0,
      profiles,
      selectedProfile: "A",
      canStart: true,
      logDir: "C:\\journals",
      summary,
    },
    global: { plugins: [i18n] },
  });
}

describe("ActionBar", () => {
  it("во время записи не даёт выбирать, сохранять и удалять профили (AC3)", () => {
    const wrapper = mountBar("recording");
    expect(wrapper.get("select").attributes("disabled")).toBeDefined();

    const buttons = wrapper.findAll("button");
    const save = buttons.find((button) => button.text().includes(i18n.global.t("actions.profileSave")));
    const remove = buttons.find((button) => button.attributes("aria-label") === i18n.global.t("actions.profileDelete"));
    expect(save?.attributes("disabled")).toBeDefined();
    expect(remove?.attributes("disabled")).toBeDefined();

    // Кнопка «Стоп» при этом активна: запись можно завершить.
    const stop = buttons.find((button) => button.text().includes(i18n.global.t("actions.stop")));
    expect(stop?.attributes("disabled")).toBeUndefined();
  });

  it("после остановки профили снова доступны (AC11)", () => {
    const wrapper = mountBar("idle");
    expect(wrapper.get("select").attributes("disabled")).toBeUndefined();
    const save = wrapper
      .findAll("button")
      .find((button) => button.text().includes(i18n.global.t("actions.profileSave")));
    expect(save?.attributes("disabled")).toBeUndefined();
  });

  it("пока сессия запускается, профили тоже заблокированы", () => {
    const wrapper = mountBar("starting");
    expect(wrapper.get("select").attributes("disabled")).toBeDefined();
  });
});
