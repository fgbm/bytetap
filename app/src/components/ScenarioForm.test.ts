import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import ScenarioForm from "./ScenarioForm.vue";
import { i18n } from "../i18n";
import type { InterfaceInfo, SessionMode } from "../api/types";
import { defaultConfig, type AppConfig } from "../settings/settings";
import { MOCK_INTERFACES, MOCK_SERIAL_PORTS } from "../api/mock-bridge";

interface FormProps {
  config: AppConfig;
  interfaces: InterfaceInfo[];
  serialPorts: typeof MOCK_SERIAL_PORTS;
  logDirWarning: { code: "onedrive" | "network"; params: Record<string, string> } | null;
  errorField: "port" | "address" | "serial" | "logDir" | "start" | null;
  errorText: string | null;
  profiles: { name: string; config: AppConfig }[];
  selectedProfile: string;
  busy: boolean;
}

function mountForm(mode: SessionMode, overrides: Partial<FormProps> = {}) {
  const config = { ...defaultConfig("C:\\journals"), mode };
  return mount(ScenarioForm, {
    props: {
      config,
      interfaces: MOCK_INTERFACES,
      serialPorts: MOCK_SERIAL_PORTS,
      logDirWarning: null,
      errorField: null,
      errorText: null,
      profiles: [],
      selectedProfile: "",
      busy: false,
      ...overrides,
    },
    global: { plugins: [i18n] },
  });
}

function expand(wrapper: ReturnType<typeof mountForm>): Promise<void> {
  return wrapper.get(".bt-more-toggle").trigger("click");
}

describe("ScenarioForm — сценарий «Анализатор подключается ко мне»", () => {
  it("показывает порт, автоответ и крупный адрес для анализатора", () => {
    const wrapper = mountForm("tcp_server");

    expect(wrapper.find("#bt-port").exists()).toBe(true);
    expect(wrapper.get(".bt-address").text()).toContain("192.168.1.10:5000");
    expect(wrapper.text()).toContain(i18n.global.t("form.copy"));
    expect(wrapper.text()).toContain("другая сеть: 10.0.0.5 (Wi-Fi)");
    expect(wrapper.text()).toContain(i18n.global.t("form.firewall"));
    // Подсказка — иконка с title: текст ссылки «подробнее» из интерфейса убран.
    const hint = wrapper.get(".bt-hintline .bt-info");
    expect(hint.attributes("title")).toBe(i18n.global.t("form.firewallHint"));

    const options = wrapper.findAll("#bt-responder option").map((option) => option.text());
    expect(options).toEqual([
      i18n.global.t("form.responderAuto"),
      i18n.global.t("form.responderHl7"),
      i18n.global.t("form.responderAstm"),
      i18n.global.t("form.responderNone"),
    ]);
    expect((wrapper.get("#bt-responder").element as HTMLSelectElement).value).toBe("auto");
  });

  it("интерфейс прослушивания живёт в «Дополнительно»", async () => {
    const wrapper = mountForm("tcp_server");
    expect(wrapper.find("#bt-iface").exists()).toBe(false);

    await expand(wrapper);
    expect(wrapper.find("#bt-iface").exists()).toBe(true);
  });

  it("проверяет порт и не стартует с пустым полем", async () => {
    const wrapper = mountForm("tcp_server", {
      config: { ...defaultConfig("C:\\journals"), listenPort: 5000 },
    });

    await wrapper.get("#bt-port").setValue("70000");
    expect(wrapper.get(".bt-ferr").text()).toBe(i18n.global.t("errors.port_range"));

    await wrapper.get(".bt-start").trigger("click");
    expect(wrapper.emitted("start")).toBeUndefined();
  });

  it("отдаёт корректный порт и стартует", async () => {
    const wrapper = mountForm("tcp_server");

    await wrapper.get("#bt-port").setValue("5001");
    await wrapper.get(".bt-start").trigger("click");

    const emitted = wrapper.emitted("start");
    expect(emitted?.[0]?.[0]).toMatchObject({ listenPort: 5001 });
  });
});

describe("ScenarioForm — сценарий «Я подключаюсь к анализатору»", () => {
  it("показывает адрес анализатора, порт и автоответ", () => {
    const wrapper = mountForm("tcp_client");
    expect(wrapper.get(".bt-form-title").text()).toBe(i18n.global.t("scenarios.clientTitle"));
    expect(wrapper.text()).toContain(i18n.global.t("form.connectAddress"));
    expect((wrapper.get("#bt-host").element as HTMLInputElement).value).toBe("192.168.1.50");
    expect((wrapper.get("#bt-cport").element as HTMLInputElement).value).toBe("5000");
    expect(wrapper.find("#bt-responder-client").exists()).toBe(true);
  });

  it("переподключение живёт в «Дополнительно»", async () => {
    const wrapper = mountForm("tcp_client");
    expect(wrapper.find("#bt-reconnect").exists()).toBe(false);

    await expand(wrapper);
    expect((wrapper.get("#bt-reconnect").element as HTMLInputElement).value).toBe("5");
  });

  it("не стартует без адреса анализатора", async () => {
    const wrapper = mountForm("tcp_client");
    await wrapper.get("#bt-host").setValue("");

    expect(wrapper.get(".bt-ferr").text()).toBe(i18n.global.t("form.addressRequired"));

    await wrapper.get(".bt-start").trigger("click");
    expect(wrapper.emitted("start")).toBeUndefined();
  });
});

describe("ScenarioForm — сценарий «Кабель COM-порта»", () => {
  it("показывает порты с описанием, скорость 9600 и формат 8N1", () => {
    const wrapper = mountForm("serial");
    expect(wrapper.get(".bt-form-title").text()).toBe(i18n.global.t("scenarios.serialTitle"));

    const options = wrapper.findAll("#bt-serial option").map((option) => option.text());
    expect(options).toContain("USB-SERIAL CH340 (COM5)");
    expect(options).toContain("Bluetooth link (COM3)");

    expect((wrapper.get("#bt-baud").element as HTMLSelectElement).value).toBe("9600");
    expect((wrapper.get("#bt-format").element as HTMLSelectElement).value).toBe("8N1");
    expect(wrapper.find("#bt-responder-serial").exists()).toBe(true);
  });

  it("поток, DTR и RTS живут в «Дополнительно»", async () => {
    const wrapper = mountForm("serial");
    expect(wrapper.find("#bt-flow").exists()).toBe(false);

    await expand(wrapper);
    expect(wrapper.find("#bt-flow").exists()).toBe(true);
    expect(wrapper.text()).toContain(i18n.global.t("form.dtr"));
    expect(wrapper.text()).toContain(i18n.global.t("form.rts"));
  });

  it("просит выбрать порт и шлёт выбранный порт наружу", async () => {
    const wrapper = mountForm("serial");
    // Пока поле не тронуто, красного текста нет: экран не кричит заранее.
    expect(wrapper.text()).not.toContain(i18n.global.t("form.serialRequired"));

    await wrapper.get(".bt-start").trigger("click");
    expect(wrapper.text()).toContain(i18n.global.t("form.serialRequired"));
    expect(wrapper.emitted("start")).toBeUndefined();

    await wrapper.get("#bt-serial").setValue("COM5");
    expect(wrapper.emitted("update-serial")?.[0]).toEqual([{ port: "COM5" }]);
  });

  it("просит обновить список портов и выбрать папку", async () => {
    const wrapper = mountForm("serial", { errorField: "logDir", errorText: "нет прав" });
    // Ошибка папки открывает «Дополнительно»: поле не должно быть скрыто.
    expect(wrapper.find("#bt-log-dir").exists()).toBe(true);

    const refresh = wrapper.findAll("button").find((button) => button.attributes("title") === i18n.global.t("form.refreshPorts"));
    await refresh?.trigger("click");
    expect(wrapper.emitted("refresh-ports")).toHaveLength(1);

    const choose = wrapper.findAll("button").find((button) => button.text() === i18n.global.t("form.choose"));
    await choose?.trigger("click");
    expect(wrapper.emitted("pick-log-dir")).toHaveLength(1);
  });

  it("NAK на неверную сумму — только для «Авто» и ASTM", async () => {
    const auto = mountForm("serial");
    await expand(auto);
    expect(auto.text()).toContain(i18n.global.t("form.nak"));

    const hl7 = { ...defaultConfig("C:\\journals"), mode: "serial" as const };
    hl7.responder = "hl7";
    const wrapper = mountForm("serial", { config: hl7 });
    await expand(wrapper);
    expect(wrapper.text()).not.toContain(i18n.global.t("form.nak"));
  });
});

describe("ScenarioForm — тишина интерфейса (UI v2)", () => {
  it("не показывает несуществующие режимы и переключатель режимов", () => {
    const wrapper = mountForm("tcp_server");
    expect(wrapper.text()).not.toContain("Прокси");
    expect(wrapper.text()).not.toContain("Прослушка");
    expect(wrapper.findAll(".bt-seg")).toHaveLength(0);
  });

  it("рисует свою стрелку у каждого списка: системную в WebView2 не показываем", async () => {
    const wrapper = mountForm("serial");
    await expand(wrapper);

    const selects = wrapper.findAll("select");
    expect(selects.length).toBeGreaterThan(0);
    for (const select of selects) {
      const box = select.element.parentElement as HTMLElement;
      expect(box.classList.contains("bt-select")).toBe(true);
      expect(box.querySelector("svg")).not.toBeNull();
    }
    expect(wrapper.findAll(".bt-select")).toHaveLength(selects.length);
  });

  it("показывает ошибку старта текстом у поля, без баннера", () => {
    const wrapper = mountForm("tcp_server", { errorField: "port", errorText: "Порт 5000 занят" });
    expect(wrapper.get(".bt-ferr").text()).toBe("Порт 5000 занят");
    expect(wrapper.find(".bt-ban").exists()).toBe(false);
  });
});
