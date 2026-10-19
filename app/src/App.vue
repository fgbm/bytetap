<script setup lang="ts">
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <main class="app">
    <h1 class="title">{{ t("app.title") }}</h1>
    <p class="tagline">{{ t("app.tagline") }}</p>
    <p class="status">{{ t("app.status") }}</p>
    <p class="warning">{{ t("app.warning") }}</p>
  </main>
</template>

<style scoped>
.app {
  margin: 0 auto;
  max-width: 42rem;
  padding: 3rem 1.5rem;
  font-family: system-ui, sans-serif;
  line-height: 1.5;
}

.title {
  margin: 0 0 0.5rem;
  font-size: 2rem;
  letter-spacing: -0.02em;
}

.tagline {
  margin: 0 0 1.5rem;
  font-size: 1.125rem;
}

.status {
  margin: 0 0 1rem;
  opacity: 0.75;
}

.warning {
  margin: 0;
  padding: 0.75rem 1rem;
  border-left: 3px solid currentColor;
  background: rgba(127, 127, 127, 0.12);
  font-size: 0.875rem;
}
</style>
