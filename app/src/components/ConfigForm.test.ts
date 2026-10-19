import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import ConfigForm from "./ConfigForm.vue";
import { i18n } from "../i18n";
import type { InterfaceInfo, LogDirWarning, SessionConfig } from "../api/types";
import { defaultConfig } from "../settings/settings";
import { MOCK_INTERFACES } from "../api/mock-bridge";

interface FormProps {
  config: SessionConfig;
  portText: string;
  interfaces: InterfaceInfo[];
  portError: string | null;
  logDirError: string | null;
  logDirWarning: LogDirWarning | null;
}

function mountForm(overrides: Partial<FormProps> = {}) {
  const config = defaultConfig("C:\\journals");
  return mount(ConfigForm, {
    props: {
      config,
      portText: "5000",
      interfaces: MOCK_INTERFACES,
      portError: null,
      logDirError: null,
      logDirWarning: null,
      ...overrides,
    },
    global: { plugins: [i18n] },
  });
}

function buttonByText(wrapper: ReturnType<typeof mountForm>, text: string) {
  return wrapper.findAll("button").find((button) => button.text().includes(text));
}

describe("ConfigForm", () => {
  it("оставляет активным только TCP-сервер, остальные режимы — с пометкой версии", () => {
    const wrapper = mountForm();
    const t = i18n.global.t;

    const server = buttonByText(wrapper, t("config.modeServer"));
    expect(server?.attributes("disabled")).toBeUndefined();
    expect(server?.classes()).toContain("on");

    const client = buttonByText(wrapper, t("config.modeClient"));
    expect(client?.attributes("disabled")).toBeDefined();
    expect(client?.text()).toContain(t("config.tagM3"));

    const com = buttonByText(wrapper, t("config.modeCom"));
    expect(com?.attributes("disabled")).toBeDefined();
    expect(com?.text()).toContain(t("config.tagM3"));

    expect(buttonByText(wrapper, t("config.modeProxy"))?.text()).toContain(t("config.tagV02"));
    expect(buttonByText(wrapper, t("config.modeComSniff"))?.text()).toContain(t("config.tagV03"));
  });

  it("включает HL7/MLLP и «Без ответов», а «Авто» и ASTM держит выключенными", () => {
    const wrapper = mountForm();
    const t = i18n.global.t;

    expect(buttonByText(wrapper, t("config.responderHl7"))?.attributes("disabled")).toBeUndefined();
    expect(buttonByText(wrapper, t("config.responderNone"))?.attributes("disabled")).toBeUndefined();
    expect(buttonByText(wrapper, t("config.responderAuto"))?.attributes("disabled")).toBeDefined();
    expect(buttonByText(wrapper, t("config.responderAstm"))?.attributes("disabled")).toBeDefined();

    expect(buttonByText(wrapper, t("config.responderHl7"))?.classes()).toContain("on");
  });

  it("переключает автоответ и адрес прослушивания", async () => {
    const wrapper = mountForm();

    await buttonByText(wrapper, i18n.global.t("config.responderNone"))?.trigger("click");
    expect(wrapper.emitted("update:config")?.[0]).toEqual([{ responder: "none" }]);

    await wrapper.get("select").setValue("192.168.1.10");
    expect(wrapper.emitted("update:config")?.[1]).toEqual([{ listenHost: "192.168.1.10" }]);
  });

  it("показывает ошибку у порта и у папки журналов", async () => {
    const wrapper = mountForm({ portText: "70000", portError: "Порт — от 1 до 65535" });
    expect(wrapper.get(".bt-ferr").text()).toContain("Порт — от 1 до 65535");
    expect(wrapper.get("input.port").classes()).toContain("err");

    const withLogDirError = mountForm({ logDirError: "Не удалось писать в папку" });
    expect(withLogDirError.text()).toContain("Не удалось писать в папку");
  });

  it("отдаёт ввод порта и выбор папки наружу", async () => {
    const wrapper = mountForm();
    await wrapper.get("input.port").setValue("5001");
    expect(wrapper.emitted("update:portText")?.[0]).toEqual(["5001"]);

    const pick = wrapper.findAll("button").find((button) => button.text().includes(i18n.global.t("config.logDirChoose")));
    await pick?.trigger("click");
    expect(wrapper.emitted("pick-log-dir")).toHaveLength(1);
  });

  it("переводит предупреждение о папке журналов по коду моста (FR-19)", () => {
    const wrapper = mountForm({
      logDirWarning: { code: "onedrive", params: { dir: "C:\\Users\\op\\OneDrive\\bytetap" } },
    });
    expect(wrapper.text()).toContain(i18n.global.t("config.logDirWarningOnedrive"));
    expect(wrapper.text()).not.toContain("onedrive");
  });

  it("показывает подсказку с адресами ноутбука и брандмауэр (AC2)", () => {
    const wrapper = mountForm();
    expect(wrapper.text()).toContain("192.168.1.10");
    expect(wrapper.text()).toContain("10.0.0.5");
    expect(wrapper.text()).toContain(i18n.global.t("config.firewall"));
    expect(wrapper.findAll(".bt-addr")).toHaveLength(2);
  });
});
