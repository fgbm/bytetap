<script setup lang="ts">
import { computed } from "vue";

interface Props {
  /** Импульсы дорожки «анализатор → ЛИС», по одному на слот, 0…1. */
  analyzer?: number[];
  /** Импульсы дорожки «ЛИС → анализатор», по одному на слот, 0…1. */
  lis?: number[];
}

const props = withDefaults(defineProps<Props>(), {
  analyzer: () => [0, 0.8, 1, 0, 0.6, 1, 0.3, 0, 1, 0.5, 0, 0.9, 0, 0.7, 0, 0],
  lis: () => [0, 0, 1, 0.4, 0, 1, 0, 0.8, 1, 0, 0.6, 0, 1, 0, 0.2, 0],
});

const WIDTH = 640;
const HEIGHT = 108;
const AMPLITUDE = 18;
const LANE_A2L = 30;
const LANE_L2A = 78;

/** Детерминированная полилиния: каждый импульс — фронт, плато и спад в своём слоте. */
function lane(values: number[], center: number): string {
  const slot = WIDTH / values.length;
  const points = [`0,${center}`];

  values.forEach((value, index) => {
    const amplitude = Math.min(1, Math.max(0, value)) * AMPLITUDE;
    const rise = (index + 0.18) * slot;
    const fall = (index + 0.62) * slot;

    points.push(`${rise.toFixed(1)},${center}`);
    points.push(`${rise.toFixed(1)},${(center - amplitude).toFixed(1)}`);
    points.push(`${fall.toFixed(1)},${(center - amplitude).toFixed(1)}`);
    points.push(`${fall.toFixed(1)},${center}`);
  });

  points.push(`${WIDTH},${center}`);
  return points.join(" ");
}

const analyzerPoints = computed(() => lane(props.analyzer, LANE_A2L));
const lisPoints = computed(() => lane(props.lis, LANE_L2A));
</script>

<template>
  <svg class="trace" :viewBox="`0 0 ${WIDTH} ${HEIGHT}`" preserveAspectRatio="none" aria-hidden="true">
    <line class="baseline" :x1="0" :y1="LANE_A2L" :x2="WIDTH" :y2="LANE_A2L" />
    <line class="baseline" :x1="0" :y1="LANE_L2A" :x2="WIDTH" :y2="LANE_L2A" />
    <polyline class="lane lane-a2l" :points="analyzerPoints" />
    <polyline class="lane lane-l2a" :points="lisPoints" />
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
  fill: none;
  stroke-width: 2;
  stroke-linejoin: round;
}

.lane-a2l {
  stroke: var(--bt-dir-a2l);
}

.lane-l2a {
  stroke: var(--bt-dir-l2a);
}
</style>
