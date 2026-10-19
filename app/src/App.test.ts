import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import App from "./App.vue";
import { i18n } from "./i18n";
import { resetSession } from "./session/store";
import { resetSettings } from "./settings/store";

describe("App", () => {
  it("начинает с выбора сценария: бренд, «Журналы», три карточки и последние записи", async () => {
    resetSession();
    resetSettings();

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();

    expect(wrapper.text()).toContain("bytetap");
    expect(wrapper.text()).toContain(i18n.global.t("header.journals"));
    expect(wrapper.text()).toContain(i18n.global.t("scenarios.question"));
    expect(wrapper.findAll(".bt-scenario")).toHaveLength(3);
    expect(wrapper.text()).toContain(i18n.global.t("scenarios.serverTitle"));
    expect(wrapper.text()).toContain(i18n.global.t("scenarios.clientTitle"));
    expect(wrapper.text()).toContain(i18n.global.t("scenarios.serialTitle"));
    // Стартовый экран — не форма: полей и баннеров на нём нет.
    expect(wrapper.find("form").exists()).toBe(false);
    expect(wrapper.find(".bt-start").exists()).toBe(false);
    expect(i18n.global.locale.value).toBe("ru");
  });

  it("скрывает будущие режимы: прокси и прослушки COM в интерфейсе нет", async () => {
    resetSession();
    resetSettings();

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();

    expect(wrapper.text()).not.toContain("Прокси");
    expect(wrapper.text()).not.toContain("Прослушка");
  });

  it("открывает шаг сценария по клику на карточку", async () => {
    resetSession();
    resetSettings();

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();

    await wrapper.findAll(".bt-scenario")[1].trigger("click");

    expect(wrapper.find(".bt-form-title").text()).toBe(i18n.global.t("scenarios.clientTitle"));
    expect(wrapper.find(".bt-start").exists()).toBe(true);
    expect(wrapper.text()).toContain(i18n.global.t("form.connectAddress"));
  });
});
