<script setup lang="ts">
import { segments } from "../../../../shared/control-chips";

/**
 * Пример обмена на главной: та же тихая лента, что в приложении (UI v2).
 *
 * Время — цветом text-faint, направление — цветным глифом стрелки, управляющие
 * символы — приглушёнными обозначениями `‹CR›` без фона и рамки, автоответ — словом
 * в конце строки цветом dir-l2a. Общий модуль разбора `shared/control-chips.ts`
 * отдаёт сегменты с `<CR>`; угловые скобки-глифы ставит этот компонент, как `rows.ts`
 * в приложении, поэтому тест общего модуля не меняется.
 */
export interface ExchangeRow {
  t: string;
  dir: "a2l" | "l2a";
  synthetic?: boolean;
  text: string;
}

defineProps<{ rows: ExchangeRow[] }>();

/** `<CR>` → `‹CR›`: глиф вместо плашки, фоном управляющие символы не выделяются. */
function glyph(value: string): string {
  return `‹${value.slice(1, -1)}›`;
}
</script>

<template>
  <ol class="exchange">
    <li
      v-for="(row, index) in rows"
      :key="index"
      class="row"
      :class="`dir-${row.dir}`"
    >
      <time class="time">{{ row.t }}</time>
      <span class="arrow" aria-hidden="true">{{ row.dir === "a2l" ? "→" : "←" }}</span>
      <span class="sr-only">{{ row.dir === "a2l" ? "анализатор → ЛИС" : "ЛИС → анализатор" }}</span>
      <span class="text">
        <template v-for="(part, partIndex) in segments(row.text)" :key="partIndex">
          <span v-if="part.chip" class="ctrl">{{ glyph(part.value) }}</span>
          <template v-else>{{ part.value }}</template>
        </template>
        <span v-if="row.synthetic" class="synthetic">автоответ</span>
      </span>
    </li>
  </ol>
</template>

<style scoped>
.exchange {
  margin: var(--bt-space-4) 0;
  padding: var(--bt-space-2) 0;
  list-style: none;
  border-radius: var(--bt-radius-panel);
  background: var(--bt-trace-bg);
  overflow-x: auto;
}

.row {
  display: flex;
  align-items: baseline;
  gap: var(--bt-space-2);
  min-height: var(--bt-lane-height);
  padding: 0 var(--bt-space-3);
  font-family: var(--bt-font-mono);
  font-size: var(--bt-text-data);
  line-height: 1.5;
}

.time {
  flex: none;
  color: var(--bt-text-faint);
  font-variant-numeric: tabular-nums;
}

.arrow {
  flex: none;
  width: 12px;
  font-weight: 600;
}

.dir-a2l .arrow {
  color: var(--bt-dir-a2l);
}

.dir-l2a .arrow {
  color: var(--bt-dir-l2a);
}

.text {
  min-width: 0;
  color: var(--bt-text);
  overflow-wrap: anywhere;
}

/* Управляющие символы: только цвет и кегль, без плашек, фона и рамки (UI v2). */

.ctrl {
  color: var(--bt-ctrl);
  font-size: var(--bt-text-2xs);
  opacity: 0.55;
}

/* Автоответ: слово в конце строки цветом ЛИС → анализатор, без штриховки и полосы. */

.synthetic {
  margin-left: 6px;
  color: var(--bt-dir-l2a);
  font-family: var(--bt-font-ui);
  font-size: var(--bt-text-2xs);
}

.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  margin: -1px;
  padding: 0;
  overflow: hidden;
  clip: rect(0 0 0 0);
  white-space: nowrap;
  border: 0;
}
</style>
