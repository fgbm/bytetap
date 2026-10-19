import { beforeEach, describe, expect, it } from "vitest";
import {
  defaultConfig,
  defaultSettings,
  sessionConfigFor,
  findProfile,
  normalizeConfig,
  normalizeSettings,
  removeProfile,
  upsertProfile,
  type Profile,
} from "./settings";
import {
  CorruptSettingsError,
  CORRUPT_KEY,
  localSettingsStorage,
  preserveCorruptSettings,
  SETTINGS_KEY,
} from "./storage";

const config = defaultConfig("C:\\journals");

describe("normalizeSettings", () => {
  it("на пустом и испорченном сохранении отдаёт значения по умолчанию", () => {
    expect(normalizeSettings(null, "D:\\logs")).toEqual(defaultSettings("D:\\logs"));
    expect(normalizeSettings("мусор", "D:\\logs").config.listenPort).toBe(5000);
    expect(normalizeSettings({ config: { listenPort: 70000 } }).config.listenPort).toBe(5000);
    expect(normalizeSettings({ theme: "neon" }).theme).toBe("system");
  });

  it("сохраняет профили, отбрасывая записи без имени", () => {
    const loaded = normalizeSettings({
      config,
      theme: "dark",
      profiles: [{ name: "Анализатор X", config }, { name: "  ", config }, { config }],
    });
    expect(loaded.theme).toBe("dark");
    expect(loaded.profiles).toHaveLength(1);
    expect(loaded.profiles[0].name).toBe("Анализатор X");
    expect(loaded.profiles[0].config.listenPort).toBe(5000);
  });

  it("принимает три режима M3 и отвергает неизвестный", () => {
    const raw = normalizeConfig(
      { mode: "tcp_client", responder: "astm", listenHost: "192.168.1.10" },
      config,
    );
    expect(raw.mode).toBe("tcp_client");
    expect(raw.responder).toBe("astm");
    expect(raw.listenHost).toBe("192.168.1.10");
    expect(normalizeConfig({ mode: "proxy" }, config).mode).toBe("tcp_server");
  });

  it("по умолчанию «Авто» и три режима M3 в настройках порта", () => {
    const settings = defaultSettings();
    expect(settings.config.responder).toBe("auto");
    expect(settings.config.serial.dataBits).toBe(8);
    expect(settings.config.serial.baud).toBe(9600);
    expect(settings.config.astmNakOnBadChecksum).toBe(false);
    expect(normalizeConfig({ responder: "astm" }, config).responder).toBe("astm");
  });

  it("собирает конфигурацию только выбранного режима", () => {
    const serial = sessionConfigFor({ ...config, mode: "serial" });
    expect(serial.serial?.baud).toBe(9600);
    expect(serial.listenPort).toBeUndefined();
    expect(serial.connectHost).toBeUndefined();

    const client = sessionConfigFor({ ...config, mode: "tcp_client" });
    expect(client.connectHost).toBe("192.168.1.50");
    expect(client.listenPort).toBeUndefined();
    expect(client.serial).toBeUndefined();

    const server = sessionConfigFor(config);
    expect(server.listenPort).toBe(5000);
    expect(server.connectPort).toBeUndefined();
  });

  it("запоминает вид и кодировку ленты, отвергая неизвестные значения", () => {
    expect(normalizeSettings({ feed: { view: "hex", encoding: "utf8" } }).feed).toEqual({
      view: "hex",
      encoding: "utf8",
    });
    expect(normalizeSettings({ feed: { view: "dump", encoding: "koi8-r" } }).feed).toEqual({
      view: "text",
      encoding: "cp1251",
    });
    expect(normalizeSettings({}).feed).toEqual({ view: "text", encoding: "cp1251" });
  });
});

describe("профили", () => {
  const profiles: Profile[] = [];

  it("сохраняет новый профиль", () => {
    const next = upsertProfile(profiles, "Анализатор X, :5000", config);
    expect(next).toHaveLength(1);
    expect(findProfile(next, "Анализатор X, :5000")?.config.listenPort).toBe(5000);
  });

  it("заменяет профиль с тем же именем, а не плодит копии", () => {
    const first = upsertProfile(profiles, "A", config);
    const changed = { ...config, listenPort: 5001 };
    const second = upsertProfile(first, "A", changed);
    expect(second).toHaveLength(1);
    expect(second[0].config.listenPort).toBe(5001);
  });

  it("не сохраняет профиль без имени", () => {
    expect(upsertProfile(profiles, "   ", config)).toEqual(profiles);
  });

  it("удаляет профиль по имени", () => {
    const two = upsertProfile(upsertProfile(profiles, "A", config), "B", config);
    expect(removeProfile(two, "A").map((profile) => profile.name)).toEqual(["B"]);
  });
});

describe("localSettingsStorage", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("сохраняет и читает настройки из localStorage", async () => {
    const storage = localSettingsStorage();
    const settings = { ...defaultSettings("C:\\journals"), theme: "light" as const };
    await storage.save(settings);

    expect(localStorage.getItem(SETTINGS_KEY)).not.toBeNull();
    expect(await storage.load()).toEqual(settings);
  });

  it("на испорченной записи сообщает об испорченном файле, а не об отсутствии", async () => {
    localStorage.setItem(SETTINGS_KEY, "{не json");
    await expect(localSettingsStorage().load()).rejects.toBeInstanceOf(CorruptSettingsError);
  });

  it("сохраняет копию испорченной записи перед перезаписью", async () => {
    localStorage.setItem(SETTINGS_KEY, "{не json");
    const copy = await preserveCorruptSettings("mock");
    expect(copy).not.toBeNull();
    expect(localStorage.getItem(copy ?? ""), "копия рядом").toBe("{не json");
    expect((copy ?? "").startsWith(CORRUPT_KEY)).toBe(true);
  });

  it("без сохранения возвращает null", async () => {
    expect(await localSettingsStorage().load()).toBeNull();
  });
});
