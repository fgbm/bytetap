import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import App from "./App.vue";
import { i18n } from "./i18n";
import { resetSession } from "./session/store";
import { resetSettings } from "./settings/store";

describe("App", () => {
  it("показывает главный экран: бренд, кнопки шапки и форму настроек", async () => {
    resetSession();
    resetSettings();

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();

    expect(wrapper.text()).toContain("bytetap");
    expect(wrapper.text()).toContain(i18n.global.t("header.openJournal"));
    expect(wrapper.text()).toContain(i18n.global.t("config.listenOn"));
    expect(wrapper.text()).toContain(i18n.global.t("feed.emptyTitle"));
    expect(i18n.global.locale.value).toBe("ru");
  });
});
