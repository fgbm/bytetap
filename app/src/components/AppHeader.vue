<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { FileText, FolderOpen, Monitor, Moon, Sun } from "@lucide/vue";
import type { ThemeMode } from "../settings/settings";

const props = defineProps<{
  theme: ThemeMode;
  journalOpen: boolean;
}>();

const emit = defineEmits<{
  openJournal: [];
  openFolder: [];
  cycleTheme: [];
}>();

const { t } = useI18n();

const themeLabel = computed(() => {
  if (props.theme === "light") return t("header.themeLight");
  if (props.theme === "dark") return t("header.themeDark");
  return t("header.themeSystem");
});
</script>

<template>
  <header class="bt-header">
    <span class="bt-brand"><i class="bt-brand-mark"></i><span>{{ t("app.title") }}</span></span>
    <slot />
    <span class="bt-spacer"></span>
    <button
      class="bt-btn ghost"
      type="button"
      :disabled="journalOpen"
      @click="emit('openJournal')"
    >
      <FileText :size="15" aria-hidden="true" />
      {{ t("header.openJournal") }}
    </button>
    <button class="bt-btn ghost" type="button" @click="emit('openFolder')">
      <FolderOpen :size="15" aria-hidden="true" />
      {{ t("header.journalFolder") }}
    </button>
    <button
      class="bt-icon-btn"
      type="button"
      :title="themeLabel"
      :aria-label="t('header.themeToggle')"
      @click="emit('cycleTheme')"
    >
      <Sun v-if="theme === 'light'" :size="15" aria-hidden="true" />
      <Moon v-else-if="theme === 'dark'" :size="15" aria-hidden="true" />
      <Monitor v-else :size="15" aria-hidden="true" />
    </button>
  </header>
</template>
