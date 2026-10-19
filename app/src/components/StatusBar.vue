<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { ArrowLeft, ArrowRight, Folder, PenLine, Plug } from "@lucide/vue";
import type { SessionStats } from "../api/types";
import type { SessionStatus } from "../session/reducer";
import { formatBytes } from "../format";

const props = defineProps<{
  status: SessionStatus;
  stats: SessionStats;
  dropped: number;
  journalPath: string | null;
}>();

const emit = defineEmits<{ note: [] }>();

const { t } = useI18n();

const recording = computed(() => props.status === "recording");
</script>

<template>
  <footer class="bt-status">
    <span>
      <Plug :size="13" aria-hidden="true" />
      {{ t("status.connections") }} <b>{{ stats.connections }}</b>
    </span>
    <span :title="t('status.bytesA2l')">
      <ArrowRight :size="13" aria-hidden="true" />
      <b>{{ formatBytes(stats.bytesAnalyzerToLis) }}</b>
    </span>
    <span :title="t('status.bytesL2a')">
      <ArrowLeft :size="13" aria-hidden="true" />
      <b>{{ formatBytes(stats.bytesLisToAnalyzer) }}</b>
    </span>
    <span v-if="dropped > 0">{{ t("status.dropped", { count: dropped }) }}</span>
    <button v-if="recording" class="bt-btn ghost" type="button" @click="emit('note')">
      <PenLine :size="13" aria-hidden="true" />
      {{ t("actions.note") }}
    </button>
    <span class="bt-spacer" />
    <span class="bt-path" :title="journalPath ?? ''">
      <Folder :size="13" aria-hidden="true" />
      <template v-if="journalPath !== null">{{ journalPath }}</template>
      <template v-else>{{ t("status.noJournal") }}</template>
    </span>
  </footer>
</template>

<style scoped>
.bt-path {
  max-width: 60%;
}
</style>
