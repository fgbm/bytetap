<script setup lang="ts">
/**
 * Шапка приложения = заголовок окна.
 *
 * Окно без системной рамки, поэтому перетаскивание и двойной клик живут здесь:
 * пустые области помечены `data-tauri-drag-region`, справа — кнопки окна.
 * Содержимое строки у каждого экрана своё и приходит слотами в одну строку.
 */
import WindowControls from "./WindowControls.vue";
</script>

<template>
  <header class="bt-titlebar" data-tauri-drag-region>
    <slot name="left" />
    <slot />
    <span class="bt-drag" data-tauri-drag-region />
    <slot name="right" />
    <WindowControls />
  </header>
</template>
