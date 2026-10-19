import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import App from "./App.vue";
import { i18n } from "./i18n";

describe("App", () => {
  it("показывает строки окна-заглушки из локали ru", () => {
    const wrapper = mount(App, { global: { plugins: [i18n] } });

    expect(wrapper.get("h1").text()).toBe("bytetap");
    expect(wrapper.text()).toContain(i18n.global.t("app.tagline"));
    expect(i18n.global.locale.value).toBe("ru");
  });
});
