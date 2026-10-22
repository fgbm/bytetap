/**
 * Тест `shared/control-chips.ts` — общего модуля приложения и сайта (FR-22).
 *
 * Запуск: `npm test` (`node --test scripts/*.test.mjs`). Node 22.18 и новее снимает типы с `.ts`
 * сам, поэтому тест импортирует модуль как есть, без сборки и без TypeScript в
 * зависимостях.
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import { segments } from "../shared/control-chips.ts";

test("управляющие символы становятся плашками", () => {
  assert.deepEqual(segments("MSH|<CR>PID"), [
    { chip: false, value: "MSH|" },
    { chip: true, value: "<CR>" },
    { chip: false, value: "PID" },
  ]);
});

test("байт данных с шестнадцатеричным кодом — тоже плашка", () => {
  assert.deepEqual(segments("<x0B><STX>"), [
    { chip: true, value: "<x0B>" },
    { chip: true, value: "<STX>" },
  ]);
});

test("похожий на глиф текст остаётся текстом", () => {
  assert.deepEqual(segments("a <foo> b <CR>"), [
    { chip: false, value: "a <foo> b " },
    { chip: true, value: "<CR>" },
  ]);
});

test("пустой текст не даёт сегментов", () => {
  assert.deepEqual(segments(""), []);
});
