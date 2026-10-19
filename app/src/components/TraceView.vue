<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ArrowLeft, ArrowRight } from "@lucide/vue";
import type { SessionStatus } from "../session/reducer";

const props = defineProps<{
  status: SessionStatus;
}>();

const { t } = useI18n();
</script>

<template>
  <div class="bt-trace">
    <div class="bt-lane">
      <label>
        <ArrowRight :size="12" aria-hidden="true" />
        {{ t("trace.a2l") }}
      </label>
      <svg class="wave" viewBox="0 0 600 18" preserveAspectRatio="none" aria-hidden="true">
        <path
          d="M0 15 L600 15"
          fill="none"
          stroke="var(--bt-dir-a2l)"
          stroke-width="1.5"
          vector-effect="non-scaling-stroke"
        />
      </svg>
    </div>
    <div class="bt-lane">
      <label>
        <ArrowLeft :size="12" aria-hidden="true" />
        {{ t("trace.l2a") }}
      </label>
      <svg class="wave" viewBox="0 0 600 18" preserveAspectRatio="none" aria-hidden="true">
        <path
          d="M0 15 L600 15"
          fill="none"
          stroke="var(--bt-dir-l2a)"
          stroke-width="1.5"
          vector-effect="non-scaling-stroke"
        />
      </svg>
    </div>
    <span v-if="props.status === 'viewing'" class="idle">{{ t("trace.viewing") }}</span>
    <span v-else-if="props.status !== 'recording'" class="idle">{{ t("trace.idle") }}</span>
  </div>
</template>

<style scoped>
.wave {
  flex: 1;
  height: 18px;
  overflow: visible;
}
</style>
