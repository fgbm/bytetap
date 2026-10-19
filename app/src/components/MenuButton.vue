<script setup lang="ts">
/**
 * Выпадающее меню: одна кнопка и всплывающий список.
 *
 * Служебное в UI v2 живёт в меню, поэтому он один на шапку, панель ленты и форму:
 * пункты приходят слотом, закрытие — клик вне меню или Escape.
 */
import { computed, onBeforeUnmount, ref, watch, type Component } from "vue";
import { ChevronDown } from "@lucide/vue";

const props = defineProps<{
  /** Подпись кнопки; без неё кнопка — иконка. */
  label?: string;
  /** Иконка Lucide слева от подписи. */
  icon?: Component;
  /** Всплывающий список прижат к левому краю кнопки. */
  align?: "left" | "right";
  /** Подсказка и доступное имя, если подписи нет. */
  title?: string;
  /** Показывать шеврон у подписи (выпадающие «Текст ▾»). */
  chevron?: boolean;
  disabled?: boolean;
}>();

const open = ref(false);
const root = ref<HTMLElement | null>(null);

const buttonClass = computed(() => (props.label === undefined ? "bt-icon" : "bt-link"));

function close(): void {
  open.value = false;
}

function toggle(): void {
  if (props.disabled === true) return;
  open.value = !open.value;
}

function onPointerDown(event: Event): void {
  const element = root.value;
  if (!open.value || element === null) return;
  if (event.target instanceof Node && element.contains(event.target)) return;
  close();
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key === "Escape" && open.value) {
    close();
    event.stopPropagation();
  }
}

watch(open, (value) => {
  if (typeof document === "undefined") return;
  if (value) {
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeydown);
  } else {
    document.removeEventListener("pointerdown", onPointerDown, true);
    document.removeEventListener("keydown", onKeydown);
  }
});

onBeforeUnmount(() => {
  if (typeof document === "undefined") return;
  document.removeEventListener("pointerdown", onPointerDown, true);
  document.removeEventListener("keydown", onKeydown);
});

defineExpose({ close });
</script>

<template>
  <div ref="root" class="bt-menu">
    <button
      :class="buttonClass"
      type="button"
      :disabled="disabled"
      :title="title"
      :aria-label="title"
      :aria-expanded="open"
      @click="toggle"
    >
      <component :is="icon" v-if="icon !== undefined" :size="16" aria-hidden="true" />
      <span v-if="label !== undefined">{{ label }}</span>
      <ChevronDown v-if="chevron === true" :size="12" aria-hidden="true" />
    </button>
    <div v-if="open" class="bt-menu-pop" :class="{ left: align === 'left' }" role="menu">
      <slot :close="close" />
    </div>
  </div>
</template>
