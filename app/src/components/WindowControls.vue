<script setup lang="ts">
/**
 * Кнопки окна в стиле Windows: свернуть, развернуть/восстановить, закрыть.
 *
 * Окно без системной рамки (`decorations: false`), поэтому кнопки живут в шапке:
 * 46px шириной во всю её высоту. В браузере и на mock-мосте кнопки видны, но ничего
 * не делают — окна там нет. Закрытие идёт штатным `close()`, а остановку сессии
 * выполняет обработчик `CloseRequested` в `app/src-tauri` (NFR-5).
 */
import { onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { getCurrentWindow, type Window as TauriWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "@lucide/vue";
import { isTauri } from "../api/bridge";

const { t } = useI18n();

const maximized = ref(false);
let current: TauriWindow | null = null;
let unlisten: (() => void) | null = null;

/** Окно Tauri или `null` в браузере: кнопки остаются на месте, но без эффекта. */
function windowOrNull(): TauriWindow | null {
  if (!isTauri()) return null;
  try {
    return getCurrentWindow();
  } catch {
    return null;
  }
}

async function minimize(): Promise<void> {
  await current?.minimize();
}

async function toggleMaximize(): Promise<void> {
  await current?.toggleMaximize();
}

async function close(): Promise<void> {
  await current?.close();
}

onMounted(async () => {
  current = windowOrNull();
  if (current === null) return;
  maximized.value = await current.isMaximized();
  unlisten = await current.onResized(async () => {
    maximized.value = current !== null && (await current.isMaximized());
  });
});

onBeforeUnmount(() => {
  unlisten?.();
  unlisten = null;
  current = null;
});
</script>

<template>
  <div class="bt-window-buttons">
    <button type="button" :title="t('window.minimize')" :aria-label="t('window.minimize')" @click="minimize">
      <Minus :size="16" aria-hidden="true" />
    </button>
    <button
      type="button"
      :title="maximized ? t('window.restore') : t('window.maximize')"
      :aria-label="maximized ? t('window.restore') : t('window.maximize')"
      @click="toggleMaximize"
    >
      <Copy v-if="maximized" :size="14" aria-hidden="true" />
      <Square v-else :size="13" aria-hidden="true" />
    </button>
    <button class="close" type="button" :title="t('window.close')" :aria-label="t('window.close')" @click="close">
      <X :size="16" aria-hidden="true" />
    </button>
  </div>
</template>
