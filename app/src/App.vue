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

<style>
body {
  margin: 0;
  background: var(--bt-surface-0);
  color: var(--bt-text);
  font-family: var(--bt-font-ui);
  font-size: var(--bt-text-md);
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
}
</style>

<style scoped>
.app {
  margin: 0 auto;
  max-width: 42rem;
  padding: var(--bt-space-6) var(--bt-space-5);
}

.title {
  margin: 0 0 var(--bt-space-2);
  font-size: var(--bt-text-xl);
  font-weight: 600;
  letter-spacing: -0.02em;
}

.tagline {
  margin: 0 0 var(--bt-space-5);
  font-size: var(--bt-text-md);
  color: var(--bt-text-muted);
}

.status {
  margin: 0 0 var(--bt-space-4);
  color: var(--bt-text-muted);
}

.warning {
  margin: 0;
  padding: var(--bt-space-3) var(--bt-space-4);
  border-left: var(--bt-dir-width) solid var(--bt-danger);
  border-radius: 0 var(--bt-radius-control) var(--bt-radius-control) 0;
  background: var(--bt-surface-1);
  font-size: var(--bt-text-sm);
}
</style>
