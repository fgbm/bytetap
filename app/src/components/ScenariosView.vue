<script setup lang="ts">
/**
 * Стартовый экран — выбор сценария (UI v2).
 *
 * Три карточки отвечают на один вопрос: как анализатор связан с ноутбуком. Карточка
 * последнего запуска обведена и показывает его порт. Клавиши ↑/↓ ходят по карточкам,
 * Enter открывает выбранную. Ниже — до трёх последних записей папки журналов; если
 * команды `list_recent_journals` в мосте нет (`null`), блока нет вовсе.
 */
import { computed, onBeforeUnmount, onMounted, ref, type Component } from "vue";
import { useI18n } from "vue-i18n";
import { ChevronRight, MonitorDown, Network, Usb } from "@lucide/vue";
import type { RecentJournal, SessionMode } from "../api/types";
import type { LastRun } from "../settings/settings";
import { formatDurationSpan, formatRecentTime } from "../format";

const props = defineProps<{
  lastRun: LastRun | null;
  /** `null` — команды в мосте нет: блок «Последние записи» скрыт. */
  recents: RecentJournal[] | null;
  /** Предупреждение о настройках одной строкой; пусто — ничего не показываем. */
  warning: string;
  /** Идёт запись или старт/стоп: журнал не открывается, лента занята. */
  busy: boolean;
}>();

const emit = defineEmits<{
  select: [mode: SessionMode];
  openRecent: [path: string];
}>();

const { t } = useI18n();

interface Card {
  mode: SessionMode;
  title: string;
  description: string;
  icon: Component;
}

const CARDS: Card[] = [
  {
    mode: "tcp_server",
    title: "scenarios.serverTitle",
    description: "scenarios.serverDescription",
    icon: MonitorDown,
  },
  {
    mode: "tcp_client",
    title: "scenarios.clientTitle",
    description: "scenarios.clientDescription",
    icon: Network,
  },
  {
    mode: "serial",
    title: "scenarios.serialTitle",
    description: "scenarios.serialDescription",
    icon: Usb,
  },
];

const selected = ref(0);

/** «последний раз: порт 5000» — только у карточки последнего сценария. */
function lastDetail(mode: SessionMode): string {
  const run = props.lastRun;
  if (run === null || run.mode !== mode) return "";
  if (run.mode === "tcp_client") return `${run.host}:${run.port}`;
  if (run.mode === "serial") return `${run.port} · ${run.baud}`;
  return t("scenarios.lastPort", { port: run.port });
}

function isLast(mode: SessionMode): boolean {
  return props.lastRun !== null && props.lastRun.mode === mode;
}

function open(mode: SessionMode): void {
  emit("select", mode);
}

function move(delta: number): void {
  const total = CARDS.length;
  selected.value = (selected.value + delta + total) % total;
}

/** Клавиатура экрана: глобально, пока экран смонтирован, но не в полях ввода. */
function onKeydown(event: KeyboardEvent): void {
  const target = event.target as HTMLElement | null;
  const tag = target?.tagName ?? "";
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
  if (event.key === "ArrowDown") {
    event.preventDefault();
    move(1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    move(-1);
  } else if (event.key === "Enter") {
    event.preventDefault();
    open(CARDS[selected.value].mode);
  }
}

const recentItems = computed(() => props.recents ?? []);

function recentMeta(journal: RecentJournal): string {
  const parts = [formatRecentTime(journal.startedAt)];
  const span = formatDurationSpan(journal.durationUs);
  if (span !== "") parts.push(span);
  return parts.join(" · ");
}

function preselectLast(): void {
  const index = CARDS.findIndex((card) => card.mode === props.lastRun?.mode);
  if (index >= 0) selected.value = index;
}

onMounted(() => {
  preselectLast();
  window.addEventListener("keydown", onKeydown);
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <div class="bt-screen">
    <section class="bt-scenarios">
      <h1 class="bt-question">{{ t("scenarios.question") }}</h1>

      <div class="bt-cards" role="listbox" :aria-label="t('scenarios.question')">
        <button
          v-for="(card, index) in CARDS"
          :key="card.mode"
          class="bt-scenario"
          :class="{ last: isLast(card.mode), active: index === selected }"
          type="button"
          role="option"
          :aria-selected="index === selected"
          @mouseenter="selected = index"
          @click="open(card.mode)"
        >
          <span class="bt-tile"><component :is="card.icon" :size="24" aria-hidden="true" /></span>
          <span class="bt-scenario-body">
            <span class="bt-scenario-title">{{ t(card.title) }}</span>
            <span class="bt-scenario-desc">{{ t(card.description) }}<template v-if="lastDetail(card.mode) !== ''"> · {{ t("scenarios.lastTime", { detail: lastDetail(card.mode) }) }}</template></span>
          </span>
          <ChevronRight class="bt-scenario-chevron" :size="16" aria-hidden="true" />
        </button>
      </div>

      <section v-if="recentItems.length > 0" class="bt-recents">
        <h2>{{ t("scenarios.recent") }}</h2>
        <button
          v-for="journal in recentItems"
          :key="journal.path"
          class="bt-recent"
          type="button"
          :disabled="busy"
          :title="journal.path"
          @click="emit('openRecent', journal.path)"
        >
          <span class="name">{{ journal.path.split(/[\\/]/).pop() }}</span>
          <span class="meta"><template v-if="journal.analyzer !== ''">{{ journal.analyzer }} · </template>{{ recentMeta(journal) }}</span>
        </button>
      </section>

      <p v-if="warning !== ''" class="bt-hintline warn">{{ warning }}</p>
    </section>
  </div>
</template>

<style scoped>
/* Карточка под курсором и под клавиатурой: подсветка одна и та же. */
.bt-scenario.active {
  background: var(--bt-hover);
}

.bt-recent {
  display: flex;
  flex-direction: column;
}

.warn {
  margin-top: var(--bt-space-5);
  color: var(--bt-warn);
}
</style>
