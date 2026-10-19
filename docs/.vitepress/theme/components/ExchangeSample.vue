<script setup lang="ts">
import { ArrowLeft, ArrowRight, Bot } from "@lucide/vue";

/** Строка примера обмена: время, направление, признак автоответа и текст фрагмента. */
export interface ExchangeRow {
  t: string;
  dir: "a2l" | "l2a";
  synthetic?: boolean;
  text: string;
}

defineProps<{ rows: ExchangeRow[] }>();

/** Управляющие символы, записанные как `<STX>`, `<CR>`, `<x0B>` и т.п. (FR-22). */
const CONTROL = /(<(?:STX|ETX|ETB|EOT|ENQ|ACK|NAK|VT|FS|CR|LF|x[0-9A-Fa-f]{2})>)/g;

interface Segment {
  chip: boolean;
  value: string;
}

/** Режет текст на обычные куски и плашки управляющих символов; всё выводится интерполяцией. */
function segments(text: string): Segment[] {
  return text
    .split(CONTROL)
    .filter((value) => value !== "")
    .map((value) => ({ chip: /^<[^<>]+>$/.test(value), value }));
}
</script>

<template>
  <ol class="exchange">
    <li
      v-for="(row, index) in rows"
      :key="index"
      class="row"
      :class="[`dir-${row.dir}`, { 'is-synthetic': row.synthetic }]"
    >
      <time class="time">{{ row.t }}</time>
      <component :is="row.dir === 'a2l' ? ArrowRight : ArrowLeft" class="arrow" :size="14" aria-hidden="true" />
      <span class="sr-only">{{ row.dir === "a2l" ? "анализатор → ЛИС" : "ЛИС → анализатор" }}</span>
      <span class="bar" aria-hidden="true" />
      <span class="body">
        <span v-if="row.synthetic" class="synthetic-label">
          <Bot :size="12" aria-hidden="true" />
          автоответ
        </span>
        <span class="text">
          <template v-for="(part, partIndex) in segments(row.text)" :key="partIndex">
            <span v-if="part.chip" class="chip">{{ part.value }}</span>
            <template v-else>{{ part.value }}</template>
          </template>
        </span>
      </span>
    </li>
  </ol>
</template>

<style scoped>
.exchange {
  margin: var(--bt-space-4) 0;
  padding: var(--bt-space-2) 0;
  list-style: none;
  border: 1px solid var(--bt-border);
  border-radius: var(--bt-radius-panel);
  background: var(--bt-trace-bg);
  overflow-x: auto;
}

.row {
  display: flex;
  align-items: flex-start;
  gap: var(--bt-space-2);
  min-height: var(--bt-lane-height);
  padding: 2px var(--bt-space-3);
  border-bottom: 1px solid var(--bt-border-faint);
  font-family: var(--bt-font-mono);
  font-size: var(--bt-text-data);
  line-height: 1.5;
}

.row:last-child {
  border-bottom: 0;
}

.time {
  flex: none;
  padding-top: 1px;
  color: var(--bt-text-muted);
  font-variant-numeric: tabular-nums;
}

.arrow {
  flex: none;
  margin-top: 2px;
}

.dir-a2l .arrow {
  color: var(--bt-dir-a2l);
}

.dir-l2a .arrow {
  color: var(--bt-dir-l2a);
}

.bar {
  flex: none;
  align-self: stretch;
  width: var(--bt-dir-width);
  border-radius: 2px;
}

.dir-a2l .bar {
  background: var(--bt-dir-a2l);
}

.dir-l2a .bar {
  background: var(--bt-dir-l2a);
}

.body {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
}

.text {
  overflow-wrap: anywhere;
  color: var(--bt-text);
}

.is-synthetic .body {
  background-image: repeating-linear-gradient(
    135deg,
    var(--bt-hatch) 0,
    var(--bt-hatch) 3px,
    transparent 3px,
    transparent 7px
  );
  border-radius: var(--bt-radius-chip);
}

.synthetic-label {
  display: inline-flex;
  align-items: center;
  gap: var(--bt-space-1);
  color: var(--bt-text-muted);
  font-family: var(--bt-font-ui);
  font-size: var(--bt-text-2xs);
  letter-spacing: 0.02em;
  text-transform: uppercase;
}

.chip {
  display: inline-block;
  margin: 0 1px;
  padding: 0 3px;
  border: 1px solid var(--bt-border);
  border-radius: var(--bt-radius-chip);
  background: var(--bt-ctrl-bg);
  color: var(--bt-ctrl);
  font-size: var(--bt-text-2xs);
  line-height: 1.5;
  white-space: nowrap;
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
