#!/usr/bin/env node
/**
 * Проверяет, что внутренние ссылки nav и sidebar VitePress указывают на существующие страницы.
 *
 * Markdown-ссылки VitePress проверяет сам при сборке, а пункты навигации — нет: опечатка в sidebar
 * даёт живую ссылку на 404. Скрипт читает `docs/.vitepress/config.mts` (Node снимает типы сам)
 * и сверяет каждый внутренний `link` с файлом в `docs/`.
 */
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const docsDir = join(root, "docs");

const { default: config } = await import("../docs/.vitepress/config.mts");

/** @type {{ text: string, link: string }[]} */
const links = [];

function collect(items) {
  for (const item of items ?? []) {
    if (typeof item.link === "string") links.push({ text: item.text ?? item.link, link: item.link });
    if (Array.isArray(item.items)) collect(item.items);
  }
}

collect(config.themeConfig?.nav);
for (const group of Object.values(config.themeConfig?.sidebar ?? {})) collect(group);

function isExternal(link) {
  return /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i.test(link);
}

function candidates(link) {
  const path = link.split("#")[0].split("?")[0];
  const rel = path.replace(/^\//, "").replace(/\/$/, "");
  const base = join(docsDir, rel);
  return rel === ""
    ? [join(docsDir, "index.md")]
    : [`${base}.md`, join(base, "index.md")];
}

const missing = [];
for (const { text, link } of links) {
  if (isExternal(link)) continue;
  if (!link.startsWith("/")) {
    missing.push({ text, link, reason: "внутренняя ссылка должна начинаться с «/»" });
    continue;
  }
  if (!candidates(link).some((file) => existsSync(file))) {
    missing.push({ text, link, reason: "нет файла в docs/" });
  }
}

if (missing.length > 0) {
  console.error("Битые ссылки навигации VitePress:");
  for (const { text, link, reason } of missing) {
    console.error(`  «${text}» → ${link}: ${reason}`);
  }
  process.exit(1);
}

console.log(`Ссылки навигации VitePress в порядке: ${links.length}.`);
