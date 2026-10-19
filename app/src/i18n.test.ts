import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { ENCODING_KEYS } from "./feed/bytes";
import { ERROR_KEYS, FAILURE_KEYS } from "./session/errors";
import { PORT_PROBLEM_KEYS } from "./validation";

/** Корень исходников: vitest запускается из каталога `app`. */
const SRC = resolve(process.cwd(), "src");

/** JSON читается с диска: unplugin-vue-i18n компилирует импорт в AST сообщений. */
const ru = JSON.parse(readFileSync(resolve(SRC, "locales/ru.json"), "utf8")) as Record<string, unknown>;

function collectFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      if (entry === "locales") continue;
      collectFiles(path, out);
    } else if (/\.(ts|vue)$/.test(entry) && !entry.endsWith(".test.ts")) {
      out.push(path);
    }
  }
  return out;
}

/** Все ключи `t("…")` из шаблонов и кода: интерфейс не хранит строки вне локали (AC12). */
function usedKeys(): string[] {
  const keys = new Set<string>();
  for (const file of collectFiles(SRC)) {
    const text = readFileSync(file, "utf8");
    for (const match of text.matchAll(/\bt\(\s*["']([^"']+)["']/g)) {
      keys.add(match[1]);
    }
  }
  return [...keys].sort();
}

function hasKey(key: string): boolean {
  let node: unknown = ru;
  for (const part of key.split(".")) {
    if (typeof node !== "object" || node === null) return false;
    node = (node as Record<string, unknown>)[part];
  }
  return typeof node === "string" && node !== "";
}

describe("i18n", () => {
  it("все ключи, используемые в шаблонах и коде, есть в ru.json", () => {
    const missing = usedKeys().filter((key) => !hasKey(key));
    expect(missing).toEqual([]);
  });

  it("ключи ошибок и причин отказа журнала заведены в локали", () => {
    const keys = [
      ...Object.values(ERROR_KEYS),
      ...Object.values(FAILURE_KEYS),
      ...Object.values(PORT_PROBLEM_KEYS),
    ];
    expect(keys.filter((key) => !hasKey(key))).toEqual([]);
  });

  it("подписи кодировок ленты заведены в локали", () => {
    expect(Object.values(ENCODING_KEYS).filter((key) => !hasKey(key))).toEqual([]);
  });

  it("покрывает все коды ошибок контракта", () => {
    expect(Object.keys(ERROR_KEYS).sort()).toEqual([
      "address_invalid",
      "address_unavailable",
      "already_running",
      "io",
      "journal",
      "journal_open",
      "log_dir",
      "permission_denied",
      "port_busy",
      "port_in_use",
      "port_not_found",
      "port_settings",
      "serial",
    ]);
  });

  it("ключи в ru.json не пустые", () => {
    const empty: string[] = [];
    const walk = (node: Record<string, unknown>, prefix: string) => {
      for (const [key, value] of Object.entries(node)) {
        const full = prefix === "" ? key : `${prefix}.${key}`;
        if (typeof value === "string") {
          if (value.trim() === "") empty.push(full);
        } else if (typeof value === "object" && value !== null) {
          walk(value as Record<string, unknown>, full);
        } else {
          empty.push(full);
        }
      }
    };
    walk(ru, "");
    expect(empty).toEqual([]);
  });
});
