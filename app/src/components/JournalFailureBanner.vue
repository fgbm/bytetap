<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { CircleAlert, FileText, FolderOpen } from "@lucide/vue";

const props = defineProps<{
  message: string;
}>();

const emit = defineEmits<{
  chooseDir: [];
  openRecorded: [];
}>();

const { t } = useI18n();

const title = computed(() =>
  props.message === ""
    ? t("banner.journalFailureTitle")
    : t("banner.journalFailureTitleWith", { message: props.message }),
);
</script>

<template>
  <div class="bt-ban err" role="alert">
    <CircleAlert :size="16" aria-hidden="true" />
    <div>
      <b>{{ title }}</b><br />
      {{ t("banner.journalFailureText") }}
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
