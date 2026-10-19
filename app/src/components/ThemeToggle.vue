<script setup lang="ts">
/**
 * Переключатель темы в шапке: Sun/Moon без третьего состояния в глифе.
 *
 * Показывается тема, в которую переключит: солнце на тёмной теме, луна — на светлой.
 * Системная тема разворачивается по `prefers-color-scheme` (`resolvedTheme`).
 */
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { Moon, Sun } from "@lucide/vue";
import { useSettings } from "../settings/store";
import { resolvedTheme } from "../theme";

const { t } = useI18n();
const settingsStore = useSettings();
const settings = settingsStore.state;

const dark = computed(() => resolvedTheme(settings.theme) === "dark");

function toggle(): void {
  settingsStore.setTheme(dark.value ? "light" : "dark");
}
</script>

<template>
  <button
    class="bt-icon"
    type="button"
    :title="dark ? t('theme.toLight') : t('theme.toDark')"
    :aria-label="dark ? t('theme.toLight') : t('theme.toDark')"
    @click="toggle"
  >
    <Sun v-if="dark" :size="16" aria-hidden="true" />
    <Moon v-else :size="16" aria-hidden="true" />
  </button>
</template>
