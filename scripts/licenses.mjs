#!/usr/bin/env node
/**
 * Собирает `THIRD-PARTY-LICENSES.md`: сторонние crate, которые попадают в `bytetap.exe`,
 * и лицензии встроенных ресурсов (шрифты IBM Plex, иконки Lucide).
 *
 * Список берётся из `cargo metadata` — того же графа зависимостей, что и в `Cargo.lock`,
 * поэтому файл воспроизводим на любой машине. В CI (`--check`) скрипт сравнивает
 * собранный текст с файлом в репозитории: устаревший файл роняет сборку.
 *
 *   node scripts/licenses.mjs           # перезаписать THIRD-PARTY-LICENSES.md
 *   node scripts/licenses.mjs --check   # проверить, что файл актуален
 */
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outPath = join(root, "THIRD-PARTY-LICENSES.md");
const check = process.argv.includes("--check");

/** Пакеты графа зависимостей bytetap-app: имя, версия, лицензия. */
function crates() {
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    }),
  );
  const byId = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const app = metadata.packages.find((pkg) => pkg.name === "bytetap-app");
  if (!app) throw new Error("в графе нет пакета bytetap-app");

  // Обход от bytetap-app по обычным и build-зависимостям: dev-зависимости (egui_kittest,
  // proptest) в exe не попадают.
  const seen = new Set();
  const queue = [app.id];
  while (queue.length > 0) {
    const id = queue.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    const node = metadata.resolve.nodes.find((item) => item.id === id);
    if (!node) continue;
    for (const dep of node.deps ?? []) {
      if (dep.dep_kinds.some((kind) => kind.kind !== "dev")) queue.push(dep.pkg);
    }
  }

  return [...seen]
    .map((id) => byId.get(id))
    .filter((pkg) => pkg && !metadata.workspace_members.includes(pkg.id))
    .map((pkg) => ({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license ?? (pkg.license_file ? `см. ${pkg.license_file}` : "не указана"),
    }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

/** Лицензия встроенного ресурса целиком: заголовок и текст файла. */
function asset(title, file) {
  const text = readFileSync(join(root, file), "utf8").trim();
  return `### ${title}\n\n\`${file}\`\n\n\`\`\`text\n${text}\n\`\`\`\n`;
}

const lines = [];
lines.push("# Сторонние лицензии");
lines.push("");
lines.push(
  "Граф зависимостей `bytetap-app` — того, из чего собирается `bytetap.exe` на Windows и на Linux. В exe встроены ещё два набора ресурсов: шрифты IBM Plex и иконки Lucide. Текст лицензий crate лежит в их пакетах в реестре crates.io; здесь — имена, версии и идентификаторы SPDX. Файл собирается скриптом `scripts/licenses.mjs` из `cargo metadata`, CI проверяет, что он актуален.",
);
lines.push("");
lines.push("## Crate");
lines.push("");
lines.push("| Crate | Версия | Лицензия |");
lines.push("| --- | --- | --- |");
for (const pkg of crates()) {
  lines.push(`| ${pkg.name} | ${pkg.version} | ${pkg.license} |`);
}
lines.push("");
lines.push("## Ресурсы в exe");
lines.push("");
lines.push(asset("IBM Plex Sans и Plex Mono — SIL Open Font License 1.1", "crates/bytetap-app/assets/fonts/OFL.txt"));
lines.push(asset("Иконки Lucide — ISC", "crates/bytetap-app/assets/icons/LICENSE"));
lines.push("");
lines.push("## Само приложение");
lines.push("");
lines.push("bytetap — MIT, текст в `LICENSE`.");
lines.push("");

const text = lines.join("\n");
if (check) {
  const current = existsSync(outPath) ? readFileSync(outPath, "utf8") : "";
  if (current !== text) {
    console.error("THIRD-PARTY-LICENSES.md устарел: запустите `node scripts/licenses.mjs`");
    process.exit(1);
  }
  console.log("THIRD-PARTY-LICENSES.md актуален");
} else {
  writeFileSync(outPath, text);
  console.log(`THIRD-PARTY-LICENSES.md: ${text.split("\n").length} строк`);
}
