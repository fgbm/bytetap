<script setup lang="ts">
import { withBase } from "vitepress";

/**
 * Снимок экрана приложения в светлой и тёмной теме (U1-AC21).
 *
 * Оба файла лежат в `docs/public/screenshots/`: `<name>.png` и `<name>-dark.png`.
 * Показывается тот, который совпадает с темой сайта, поэтому переключатель темы в
 * шапке документации переключает и снимки.
 */
interface Props {
  /** Имя файла без темы и расширения: `scenarios`, `recording`, `journal`. */
  name: string;
  /** Подпись: что видно на снимке. */
  alt: string;
}

defineProps<Props>();
</script>

<template>
  <p class="bt-shot">
    <img class="shot-light" :src="withBase(`/screenshots/${name}.png`)" :alt="alt" />
    <img class="shot-dark" :src="withBase(`/screenshots/${name}-dark.png`)" :alt="alt" />
  </p>
</template>
