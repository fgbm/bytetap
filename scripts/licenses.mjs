#!/usr/bin/env node
/**
 * Собирает `THIRD-PARTY-LICENSES.md`: сторонние crate, которые попадают в `bytetap.exe`,
 * полные тексты их лицензий и лицензии встроенных ресурсов (шрифты IBM Plex, иконки
 * Lucide).
 *
 * Список берётся из `cargo metadata` — того же графа зависимостей, что и в `Cargo.lock`,
 * с фильтром по платформе поставки (`x86_64-pc-windows-msvc`): в exe едут только
 * Windows-зависимости графа, и Linux-пакеты в файле лицензий не нужны. Поэтому файл
 * воспроизводим на любой машине.
 *
 * Тексты лицензий читаются из каталогов пакетов (`manifest_path`): `LICENSE*`,
 * `LICENCE*`, `COPYING*`, `NOTICE*`. Одинаковые тексты (MIT у сотни crate — один и тот
 * же) собраны в одну группу со списком пакетов. Если в пакете файла лицензии нет, берётся
 * стандартный текст SPDX из `scripts/license-texts/<идентификатор>.txt` с пометкой.
 * Для MPL-2.0 указан адрес исходников пакета (`repository`).
 *
 *   node scripts/licenses.mjs           # перезаписать THIRD-PARTY-LICENSES.md
 *   node scripts/licenses.mjs --check   # проверить, что файл актуален
 */
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outPath = join(root, "THIRD-PARTY-LICENSES.md");
const check = process.argv.includes("--check");

/** Платформа поставки: релизный `bytetap.exe` — Windows x64. */
const PLATFORM = "x86_64-pc-windows-msvc";

/** Файлы лицензий в каталоге пакета: `LICENSE`, `LICENSE-MIT`, `COPYING`, `NOTICE`. */
const LICENSE_FILE = /^(LICEN[CS]E|COPYING|NOTICE)/i;

/** Пакеты графа зависимостей bytetap-app для платформы поставки. */
function crates() {
  const metadata = JSON.parse(
    execFileSync(
      "cargo",
      ["metadata", "--format-version", "1", "--locked", "--filter-platform", PLATFORM],
      {
        cwd: root,
        encoding: "utf8",
        maxBuffer: 64 * 1024 * 1024,
      },
    ),
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
      dir: dirname(pkg.manifest_path),
      repository: pkg.repository ?? "",
    }))
    .sort(
      // Порядок без локали: `localeCompare` на Windows и Linux сортирует по-разному,
      // и проверка `--check` падала бы на релизной сборке.
      (a, b) =>
        a.name < b.name ? -1 : a.name > b.name ? 1 : a.version < b.version ? -1 : a.version > b.version ? 1 : 0,
    );
}

/**
 * Текст файла: окончания строк к `\n`, хвостовые пробелы сняты.
 *
 * Файлы в реестре crates.io лежат с разными окончаниями (OFL.txt с CRLF, LICENSE с LF),
 * а checkout на Windows с `core.autocrlf` переписывает их на CRLF — иначе `--check` на
 * windows-latest расходился бы с файлом из ubuntu.
 */
function readText(path) {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n").trim();
}

/** Тексты лицензий из каталога пакета: имя файла и текст. */
function licenseFiles(dir) {
  if (!existsSync(dir)) {
    throw new Error(
      `нет каталога пакета ${dir}: запустите \`cargo fetch --locked\`, чтобы тексты лицензий были на диске`,
    );
  }
  return readdirSync(dir)
    .filter((file) => LICENSE_FILE.test(file) && statSync(join(dir, file)).isFile())
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0))
    .map((file) => ({ source: file, text: readText(join(dir, file)) }));
}

/** Стандартный текст SPDX из `scripts/license-texts/`: `null`, если шаблона нет. */
function spdxText(id) {
  const path = join(root, "scripts", "license-texts", `${id}.txt`);
  return existsSync(path) ? readText(path) : null;
}

/** Идентификаторы SPDX выражения лицензии: `MIT OR Apache-2.0` → `MIT`, `Apache-2.0`. */
function spdxIds(expression) {
  return expression
    .replace(/[()]/g, " ")
    .split(/\s+AND\s+|\s+OR\s+|\s+WITH\s+/i)
    .map((id) => id.trim())
    .filter(Boolean);
}

/** Тексты лицензий одного пакета: из его каталога или стандартные SPDX с пометкой. */
function texts(pkg) {
  const files = licenseFiles(pkg.dir);
  if (files.length > 0) {
    return files.map((file) => ({ ...file, spdx: false }));
  }
  const out = spdxIds(pkg.license)
    .map((id) => ({ text: spdxText(id), id }))
    .filter((item) => item.text !== null)
    .map((item) => ({ source: `SPDX ${item.id}`, text: item.text, spdx: true }));
  if (out.length === 0) {
    throw new Error(
      `у пакета ${pkg.name} ${pkg.version} нет файла лицензии и нет шаблона в scripts/license-texts для «${pkg.license}»`,
    );
  }
  return out;
}

/** Группы одинаковых текстов: текст → пакеты с ним и признак стандартного SPDX. */
function groups(packages) {
  const byText = new Map();
  const perPackage = new Map();
  for (const pkg of packages) {
    const numbers = [];
    for (const item of texts(pkg)) {
      let group = byText.get(item.text);
      if (!group) {
        group = { text: item.text, spdx: item.spdx, packages: [], repositories: new Set() };
        byText.set(item.text, group);
      }
      if (!group.packages.some((other) => other.name === pkg.name && other.version === pkg.version)) {
        group.packages.push({ name: pkg.name, version: pkg.version });
      }
      if (pkg.license.includes("MPL") && pkg.repository) group.repositories.add(pkg.repository);
      numbers.push(group);
    }
    perPackage.set(`${pkg.name} ${pkg.version}`, numbers);
  }
  const list = [...byText.values()];
  for (const group of list) {
    group.packages.sort((a, b) =>
      a.name < b.name ? -1 : a.name > b.name ? 1 : a.version < b.version ? -1 : 1,
    );
  }
  // Порядок групп — по первому пакету: он не зависит от порядка обхода графа.
  list.sort((a, b) => {
    const [first, second] = [a.packages[0], b.packages[0]];
    return first.name < second.name ? -1 : first.name > second.name ? 1 : 0;
  });
  return { list, perPackage };
}

const packages = crates();
const { list, perPackage } = groups(packages);

const lines = [];
const push = (...items) => lines.push(...items);
push("# Сторонние лицензии");
push("");
push(
  `Граф зависимостей \`bytetap-app\` для платформы поставки \`${PLATFORM}\` — того, из чего собирается \`bytetap.exe\`. В exe встроены ещё два набора ресурсов: шрифты IBM Plex и иконки Lucide; шрифты egui (\`default_fonts\`) в exe не входят. Ниже — пакеты с версиями и идентификаторами SPDX, а затем полные тексты лицензий. Одинаковые тексты собраны в одну группу со списком пакетов — MIT у сотни crate это один и тот же текст. Где в пакете файла лицензии не было, приведён стандартный текст SPDX из \`scripts/license-texts/\` с пометкой. Файл собирается скриптом \`scripts/licenses.mjs\` из \`cargo metadata\`; CI проверяет, что он актуален.`,
);
push("");
push("## Crate");
push("");
push("| Crate | Версия | Лицензия | Текст |");
push("| --- | --- | --- | --- |");
for (const pkg of packages) {
  const numbers = (perPackage.get(`${pkg.name} ${pkg.version}`) ?? []).map(
    (group) => list.indexOf(group) + 1,
  );
  const references = [...new Set(numbers)].sort((a, b) => a - b).join(", ");
  push(`| ${pkg.name} | ${pkg.version} | ${pkg.license} | ${references} |`);
}
push("");
push("## Тексты лицензий");
push("");
push(
  "Номер группы — из столбца «Текст». Пометка «стандартный текст SPDX» значит, что в пакете не было файла лицензии: приведён стандартный текст лицензии, а правообладатели и год — в метаданных пакета (`authors` на crates.io).",
);
push("");
list.forEach((group, index) => {
  const expressions = [
    ...new Set(
      group.packages.map(
        (pkg) => packages.find((item) => item.name === pkg.name && item.version === pkg.version).license,
      ),
    ),
  ].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  const count = group.packages.length;
  push(`### ${index + 1}. ${expressions.join(", ")} — ${count} ${plural(count)}`);
  push("");
  push(
    `Пакеты: ${group.packages.map((pkg) => `${pkg.name} ${pkg.version}`).join(", ")}.`,
  );
  push("");
  if (group.spdx) {
    push(
      "Стандартный текст SPDX: файла лицензии в пакете нет, текст взят из `scripts/license-texts/`.",
    );
    push("");
  }
  if (group.repositories.size > 0) {
    push(
      `MPL-2.0: исходники — ${[...group.repositories].sort((a, b) => (a < b ? -1 : 1)).join(", ")}.`,
    );
    push("");
  }
  push("```text");
  push(group.text);
  push("```");
  push("");
});
push("## Ресурсы в exe");
push("");
push(
  "### Шрифты IBM Plex Sans и Plex Mono — SIL Open Font License 1.1\n\n`crates/bytetap-app/assets/fonts/OFL.txt`\n\n```text\n" +
    readText(join(root, "crates/bytetap-app/assets/fonts/OFL.txt")) +
    "\n```\n",
);
push(
  "### Иконки Lucide — ISC\n\n`crates/bytetap-app/assets/icons/LICENSE`\n\n```text\n" +
    readText(join(root, "crates/bytetap-app/assets/icons/LICENSE")) +
    "\n```\n",
);
push("## Само приложение");
push("");
push("bytetap — MIT, текст в `LICENSE`.");
push("");

/** «1 пакет», «2 пакета», «5 пакетов». */
function plural(count) {
  const tail = count % 100;
  if (tail >= 11 && tail <= 14) return "пакетов";
  if (count % 10 === 1) return "пакет";
  if (count % 10 >= 2 && count % 10 <= 4) return "пакета";
  return "пакетов";
}

const text = lines.join("\n");
if (check) {
  const current = existsSync(outPath) ? readFileSync(outPath, "utf8").replace(/\r\n/g, "\n") : "";
  if (current !== text) {
    console.error("THIRD-PARTY-LICENSES.md устарел: запустите `node scripts/licenses.mjs`");
    const fresh = text.split("\n");
    const have = current.split("\n");
    for (let i = 0; i < Math.max(fresh.length, have.length); i++) {
      if (fresh[i] !== have[i]) {
        console.error(`первое расхождение, строка ${i + 1}:`);
        console.error(`  в файле:     ${JSON.stringify(have[i])}`);
        console.error(`  по графу:    ${JSON.stringify(fresh[i])}`);
        break;
      }
    }
    process.exit(1);
  }
  console.log("THIRD-PARTY-LICENSES.md актуален");
} else {
  writeFileSync(outPath, text);
  console.log(`THIRD-PARTY-LICENSES.md: ${text.split("\n").length} строк`);
}
