<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { CircleAlert, FileText, FolderOpen } from "@lucide/vue";
import type { JournalFailureCode } from "../api/types";
import { FAILURE_KEYS } from "../session/errors";

const props = defineProps<{
  code: JournalFailureCode;
  message: string;
}>();

const emit = defineEmits<{
  chooseDir: [];
  openRecorded: [];
}>();

const { t } = useI18n();

/** Причина по коду: «на диске нет места» вместо английского текста ядра. */
const title = computed(() =>
  t("banner.journalFailureTitleWith", { reason: t(FAILURE_KEYS[props.code]) }),
);
</script>

<template>
  <div class="bt-ban err" role="alert">
    <CircleAlert :size="16" aria-hidden="true" />
    <div>
      <b>{{ title }}</b><br />
      {{ t("banner.journalFailureText") }}
      <div v-if="message !== ''" class="bt-muted detail" :title="message">
        {{ t("banner.journalFailureDetail", { message }) }}
      </div>
      <div class="acts">
        <button class="bt-btn" type="button" @click="emit('chooseDir')">
          <FolderOpen :size="15" aria-hidden="true" />
          {{ t("banner.chooseOtherDir") }}
        </button>
        <button class="bt-btn" type="button" @click="emit('openRecorded')">
          <FileText :size="15" aria-hidden="true" />
          {{ t("banner.openRecorded") }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.detail {
  font-size: var(--bt-text-xs);
}
</style>
