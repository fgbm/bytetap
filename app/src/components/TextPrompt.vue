<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { useI18n } from "vue-i18n";

const props = defineProps<{
  open: boolean;
  title: string;
  label: string;
  placeholder: string;
  value?: string;
  multiline?: boolean;
  confirmLabel: string;
}>();

const emit = defineEmits<{
  confirm: [text: string];
  cancel: [];
}>();

const { t } = useI18n();

const text = ref("");
const field = ref<HTMLInputElement | HTMLTextAreaElement | null>(null);

watch(
  () => props.open,
  async (open) => {
    if (!open) return;
    text.value = props.value ?? "";
    await nextTick();
    field.value?.focus();
    field.value?.select();
  },
);

function confirm(): void {
  const value = text.value.trim();
  if (value === "") return;
  emit("confirm", value);
}
</script>

<template>
  <div v-if="open" class="bt-modal-backdrop" @keydown.esc="emit('cancel')">
    <div class="bt-modal" role="dialog" aria-modal="true" :aria-label="title">
      <h2>{{ title }}</h2>
      <label class="bt-muted" for="bt-prompt">{{ label }}</label>
      <textarea
        v-if="multiline"
        id="bt-prompt"
        ref="field"
        v-model="text"
        class="bt-input"
        :placeholder="placeholder"
        @keydown.ctrl.enter="confirm"
      />
      <input
        v-else
        id="bt-prompt"
        ref="field"
        v-model="text"
        class="bt-input"
        :placeholder="placeholder"
        @keydown.enter="confirm"
      />
      <div class="acts">
        <button class="bt-btn ghost" type="button" @click="emit('cancel')">
          {{ t("actions.cancel") }}
        </button>
        <button class="bt-btn" type="button" :disabled="text.trim() === ''" @click="confirm">
          {{ confirmLabel }}
        </button>
      </div>
    </div>
  </div>
</template>
