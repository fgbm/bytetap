<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { Bot, Server } from "@lucide/vue";
import type { SessionConfig } from "../api/types";

defineProps<{
  config: SessionConfig;
}>();

const { t } = useI18n();
</script>

<template>
  <span class="bt-chip">
    <Server :size="13" aria-hidden="true" />
    {{ t("chips.server") }} <b>:{{ config.listenPort }}</b>
  </span>
  <span class="bt-chip">
    <Bot :size="13" aria-hidden="true" />
    <b v-if="config.responder === 'hl7'">{{ t("chips.responderHl7") }}</b>
    <b v-else>{{ t("chips.responderNone") }}</b>
    <template v-if="config.responder === 'hl7'">{{ t("chips.auto") }}</template>
  </span>
</template>
