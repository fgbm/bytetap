<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { Save, Square, Trash2 } from "@lucide/vue";
import type { SessionSummary } from "../api/types";
import type { SessionStatus } from "../session/reducer";
import type { Profile } from "../settings/settings";
import { formatBytes, formatDuration } from "../format";

const props = defineProps<{
  status: SessionStatus;
  elapsedMs: number;
  profiles: Profile[];
  selectedProfile: string;
  canStart: boolean;
  logDir: string;
  summary: SessionSummary | null;
}>();

const emit = defineEmits<{
  start: [];
  stop: [];
  selectProfile: [name: string];
  saveProfile: [];
  deleteProfile: [];
}>();

const { t } = useI18n();

const recording = computed(() => props.status === "recording");
const busy = computed(() => props.status === "starting" || props.status === "stopping");
const duration = computed(() => formatDuration(props.elapsedMs));
const startLabel = computed(() => {
  if (props.status === "starting") return t("actions.starting");
  if (props.status === "stopping") return t("actions.stopping");
  return t("actions.start");
});

const summaryText = computed(() => {
  if (props.summary === null) return null;
  return t("status.summary", {
    connections: props.summary.stats.connections,
    a2l: formatBytes(props.summary.stats.bytesAnalyzerToLis),
    l2a: formatBytes(props.summary.stats.bytesLisToAnalyzer),
  });
});

function onProfileChange(event: Event): void {
  emit("selectProfile", (event.target as HTMLSelectElement).value);
}
</script>

<template>
  <div class="bt-actions">
    <button v-if="!recording" class="bt-rec big" type="button" :disabled="!canStart || busy" @click="emit('start')">
      <span class="bt-dot" aria-hidden="true" />
      {{ startLabel }}
    </button>
    <button v-else class="bt-rec big" type="button" @click="emit('stop')">
      <span class="bt-dot live" aria-hidden="true" />
      <Square :size="13" aria-hidden="true" />
      {{ t("actions.stop") }}
      <span class="bt-mono">{{ duration }}</span>
    </button>

    <span class="bt-muted journal-hint">{{ t("config.journalPath", { dir: logDir }) }}</span>
    <span class="bt-spacer" />

    <label class="bt-muted profile-label" for="bt-profile">{{ t("actions.profile") }}</label>
    <select
      id="bt-profile"
      class="bt-input profile"
      :value="selectedProfile"
      :aria-label="t('actions.profileChoose')"
      @change="onProfileChange"
    >
      <option value="">{{ t("actions.profileLast") }}</option>
      <option v-for="profile in profiles" :key="profile.name" :value="profile.name">
        {{ profile.name }}
      </option>
    </select>
    <button class="bt-btn" type="button" @click="emit('saveProfile')">
      <Save :size="15" aria-hidden="true" />
      {{ t("actions.profileSave") }}
    </button>
    <button
      class="bt-btn ghost"
      type="button"
      :disabled="selectedProfile === ''"
      :aria-label="t('actions.profileDelete')"
      @click="emit('deleteProfile')"
    >
      <Trash2 :size="15" aria-hidden="true" />
    </button>

    <span v-if="summaryText" class="bt-muted summary bt-mono">{{ summaryText }}</span>
  </div>
</template>

<style scoped>
.profile {
  width: auto;
  min-width: 180px;
  max-width: 260px;
  flex: none;
}

.profile-label {
  white-space: nowrap;
}

.summary {
  flex-basis: 100%;
}
</style>
