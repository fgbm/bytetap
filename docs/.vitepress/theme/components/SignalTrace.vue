<script setup lang="ts">
import { computed } from "vue";

/**
 * Фирменная трасса на главной: две дорожки со столбиками (UI v2).
 *
 * Как в приложении (`TraceView` и `trace/model.ts`), высота столбика растёт по
 * логарифму длины фрагмента: короткий служебный кадр остаётся видимым, длинное
 * сообщение занимает дорожку целиком. Меандр здесь не рисуется — трасса это график,
 * а не иконка.
 */
interface Props {
  /** Длины фрагментов «анализатор → ЛИС» в байтах, по одному на слот; 0 — тишина. */
  analyzer?: number[];
  /** Длины фрагментов «ЛИС → анализатор» в байтах, по одному на слот; 0 — тишина. */
  lis?: number[];
}

const props = withDefaults(defineProps<Props>(), {
  analyzer: () => [0, 128, 512, 0, 96, 2048, 320, 0, 1536, 224, 0, 768, 0, 384, 64, 1024],
  lis: () => [0, 0, 14, 0, 0, 14, 0, 14, 14, 0, 14, 0, 14, 0, 14, 0],
});

const WIDTH = 640;
const HEIGHT = 108;
/** Высота дорожки: столбик длинного фрагмента занимает её целиком. */
const LANE_HEIGHT = 26;
/** Базовая линия дорожки «анализатор → ЛИС». */
const LANE_A2L = 38;
/** Базовая линия дорожки «ЛИС → анализатор». */
const LANE_L2A = 88;
/** Ширина столбика: тоньше не видно, толще — уже плашка. */
const BAR_WIDTH = 2;
/** Нижняя граница высоты: короткий ACK не должен сливаться с базовой линией. */
const BAR_MIN = 4;
/** Длина фрагмента, на которой столбик занимает всю дорожку. */
const FULL_LEN = 4096;
/** Какую часть дорожки занимает самый короткий фрагмент. */
const MIN_SHARE = 0.3;

interface Bar {
  x: number;
  height: number;
}

/** Высота столбика по длине фрагмента — от 30% до 100% дорожки, как в приложении. */
function barHeight(len: number): number {
  const share = Math.min(1, Math.log10(Math.max(1, len)) / Math.log10(FULL_LEN));
  return Math.max(BAR_MIN, (MIN_SHARE + (1 - MIN_SHARE) * share) * LANE_HEIGHT);
}

/** Столбики дорожки: слот на фрагмент, нулевая длина — тишина. */
function bars(values: number[]): Bar[] {
  const slot = WIDTH / values.length;
  const out: Bar[] = [];
  values.forEach((value, index) => {
    if (value <= 0) return;
    out.push({ x: Math.round(index * slot + slot / 2), height: barHeight(value) });
  });
  return out;
}

const analyzerBars = computed(() => bars(props.analyzer));
const lisBars = computed(() => bars(props.lis));
</script>

<template>
  <svg class="trace" :viewBox="`0 0 ${WIDTH} ${HEIGHT}`" preserveAspectRatio="none" aria-hidden="true">
    <line class="baseline" :x1="0" :y1="LANE_A2L" :x2="WIDTH" :y2="LANE_A2L" />
    <line class="baseline" :x1="0" :y1="LANE_L2A" :x2="WIDTH" :y2="LANE_L2A" />
    <g class="lane lane-a2l">
      <rect
        v-for="(bar, index) in analyzerBars"
        :key="`a2l-${index}`"
        :x="bar.x"
        :y="LANE_A2L - bar.height"
        :width="BAR_WIDTH"
        :height="bar.height"
      />
    </g>
    <g class="lane lane-l2a">
      <rect
        v-for="(bar, index) in lisBars"
        :key="`l2a-${index}`"
        :x="bar.x"
        :y="LANE_L2A - bar.height"
        :width="BAR_WIDTH"
        :height="bar.height"
      />
    </g>
  </svg>
</template>

<style scoped>
.trace {
  display: block;
  width: 100%;
  height: auto;
  border: 1px solid var(--bt-border);
  border-radius: var(--bt-radius-panel);
  background: var(--bt-trace-bg);
}

.baseline {
  stroke: var(--bt-border-faint);
  stroke-width: 1;
}

.lane {
  opacity: 0.9;
}

.lane-a2l {
  fill: var(--bt-dir-a2l);
}

.lane-l2a {
  fill: var(--bt-dir-l2a);
}
</style>
