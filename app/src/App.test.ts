import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App.vue";
import { bridge } from "./api/bridge";
import { BridgeError, type FeedBatch, type JournalSummary, type StartInfo } from "./api/types";
import { i18n } from "./i18n";
import { resetSession, useSession } from "./session/store";
import { defaultConfig, sessionConfigFor } from "./settings/settings";
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

/** Путь журнала сессии: его должно открывать «Открыть записанное» (FR-29). */
const sessionPath = "C:\\journals\\bytetap-20261019-101500-tcp_server.jsonl";

const startInfo: StartInfo = {
  generation: 7,
  journalPath: sessionPath,
  localAddr: "0.0.0.0:5000",
  startedAt: new Date(2026, 9, 19, 10, 15, 0).toISOString(),
};

const journalSummary: JournalSummary = {
  path: sessionPath,
  header: { format: "bytetap-session/1", analyzer: "ANALYZER" },
  events: 3,
  durationUs: 1_000_000,
  stats: { connections: 1, bytesAnalyzerToLis: 10, bytesLisToAnalyzer: 4 },
  density: { a2l: [10], l2a: [4] },
  firstEvent: [0],
};

/** Отказ журнала в ленте: ядро останавливает сессию и присылает `stopped`. */
const failureBatch: FeedBatch = {
  events: [
    { type: "stopped", reason: "journal_failure", code: "disk_full", message: "storage full" },
  ],
  dropped: 0,
};

const config = sessionConfigFor(defaultConfig("C:\\journals"));

/** Мост без файлов: журнал, страницы и папка — заглушки, старт отдаёт путь сессии. */
function stubBridge(running = false) {
  let emit: (batch: FeedBatch) => void = () => undefined;
  const open = vi.spyOn(bridge, "openJournal").mockResolvedValue(journalSummary);
  const openDir = vi.spyOn(bridge, "openLogDir").mockResolvedValue();
  const status = vi
    .spyOn(bridge, "sessionStatus")
    .mockResolvedValue(running ? { running: true, startInfo } : { running: false, startInfo: null });
  vi.spyOn(bridge, "journalLengths").mockResolvedValue([]);
  vi.spyOn(bridge, "readJournal").mockResolvedValue([]);
  vi.spyOn(bridge, "startSession").mockImplementation(async (_cfg, onBatch) => {
    emit = onBatch;
    return startInfo;
  });
  vi.spyOn(bridge, "stopSession").mockResolvedValue({
    stats: { connections: 1, bytesAnalyzerToLis: 10, bytesLisToAnalyzer: 4 },
    journalPath: sessionPath,
  });
  return { open, openDir, status, emit: () => emit };
}

/** Кнопка по подписи: баннер отказа — единственная строка с этими действиями. */
function buttonByText(wrapper: VueWrapper, label: string) {
  const found = wrapper.findAll("button").find((button) => button.text() === label);
  expect(found, `кнопка «${label}»`).toBeDefined();
  return found!;
}

describe("App: «Открыть записанное» после отказа журнала (FR-29, NFR-5)", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("открывает файл сессии, а не папку журналов", async () => {
    resetSession();
    resetSettings();
    const spies = stubBridge();

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();
    await useSession().start(config);
    await flushPromises();
    spies.emit()(failureBatch);
    await flushPromises();

    const openBtn = buttonByText(wrapper, i18n.global.t("banner.openRecorded"));
    expect(openBtn.attributes("disabled")).toBeUndefined();
    await openBtn.trigger("click");
    await flushPromises();

    expect(spies.open).toHaveBeenCalledWith(sessionPath);
    expect(spies.openDir, "папка вместо файла сессии").not.toHaveBeenCalled();
  });

  it("после штатного «Стоп» оставляет тот же путь сессии", async () => {
    resetSession();
    resetSettings();
    stubBridge();

    mount(App, { global: { plugins: [i18n] } });
    await flushPromises();
    await useSession().start(config);
    await flushPromises();
    await useSession().stop();
    await flushPromises();

    const session = useSession();
    expect(session.state.value.startInfo).toBeNull();
    expect(session.state.value.recordedPath).toBe(sessionPath);
    expect(session.state.value.summary?.journalPath).toBe(sessionPath);
  });

  it("берёт путь из session_status после перезагрузки страницы", async () => {
    resetSession();
    resetSettings();
    const spies = stubBridge(true);
    // Отказ журнала на финальной фиксации: и тогда записанное надо открыть.
    vi.spyOn(bridge, "stopSession").mockRejectedValue(
      new BridgeError("journal", "journal interrupted", {
        failure: "disk_full",
        path: sessionPath,
      }),
    );

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();

    expect(spies.status).toHaveBeenCalled();
    expect(useSession().state.value.status).toBe("recording");
    expect(useSession().state.value.recordedPath).toBe(sessionPath);

    await wrapper.find(".bt-stop").trigger("click");
    await flushPromises();

    const openBtn = buttonByText(wrapper, i18n.global.t("banner.openRecorded"));
    await openBtn.trigger("click");
    await flushPromises();

    expect(spies.open).toHaveBeenCalledWith(sessionPath);
    expect(spies.openDir).not.toHaveBeenCalled();
  });

  it("после ошибки до создания журнала открывать нечего", async () => {
    resetSession();
    resetSettings();
    const spies = stubBridge();
    vi.spyOn(bridge, "startSession").mockRejectedValue(
      new BridgeError("log_dir", "cannot create the journal", { dir: "C:\\journals" }),
    );

    const wrapper = mount(App, { global: { plugins: [i18n] } });
    await flushPromises();
    await useSession().start(config);
    await flushPromises();

    expect(useSession().state.value.recordedPath).toBeNull();
    const openBtn = wrapper
      .findAll("button")
      .find((button) => button.text() === i18n.global.t("banner.openRecorded"));
    // Баннера отказа журнала нет: журнал не создавался, это ошибка старта у поля.
    expect(openBtn).toBeUndefined();
    expect(spies.open).not.toHaveBeenCalled();
    expect(spies.openDir).not.toHaveBeenCalled();
  });
});
