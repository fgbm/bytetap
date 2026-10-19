<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { CircleAlert, X } from "@lucide/vue";
import type { SessionConfig } from "../api/types";
import type { SessionError } from "../session/reducer";
import { ERROR_KEYS, errorParams } from "../session/errors";

const props = defineProps<{
  error: SessionError;
  config: SessionConfig;
}>();

const emit = defineEmits<{ dismiss: [] }>();

const { t } = useI18n();

const text = computed(() => t(ERROR_KEYS[props.error.code], errorParams(props.error, props.config)));
</script>

<template>
  <div class="bt-ban err" role="alert">
    <CircleAlert :size="16" aria-hidden="true" />
    <div class="body">
      <div>{{ text }}</div>
    </div>
    <button class="bt-icon-btn" type="button" :aria-label="t('banner.dismiss')" @click="emit('dismiss')">
      <X :size="14" aria-hidden="true" />
    </button>
  </div>
</template>

<style scoped>
.body {
  flex: 1;
}

.bt-icon-btn {
  border-color: transparent;
}
</style>
