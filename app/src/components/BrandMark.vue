<script setup lang="ts">
/** Бренд: две точки направлений обмена и моноширинное имя. */
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <span class="bt-brand">
    <span class="bt-brand-dots" aria-hidden="true"><i></i><i></i></span>
    <span>{{ t("app.title") }}</span>
  </span>
</template>
