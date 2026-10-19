<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import {
  ArrowLeft,
  ArrowRight,
  CircleAlert,
  Info,
  PenLine,
  Plug,
  Square,
  Unplug,
} from "@lucide/vue";
import type { FeedEvent, JournalSummary } from "../api/types";
import type { SessionStatus } from "../session/reducer";
import { eventText } from "../feed/preview";
import { formatTime } from "../format";

const props = defineProps<{
  events: FeedEvent[];
  status: SessionStatus;
  journal: JournalSummary | null;
}>();

const { t } = useI18n();

type RowIcon = "a2l" | "l2a" | "open" | "close" | "error" | "info" | "note" | "stopped";

interface Row {
  key: number;
  cls: string;
  ts: string;
  conn: string;
  icon: RowIcon;
  text: string;
}

/**
 * Простой список до шага 3: шаг 3 заменит его виртуализированной лентой
 * с HEX, кодировками, фильтром соединений и прокруткой.
 */
const rows = computed<Row[]>(() =>
  props.events.map((event, index) => {
    const base: Row = { key: index, cls: "", ts: "", conn: "", icon: "info", text: "" };
    switch (event.type) {
      case "data":
        return {
          ...base,
          cls: event.dir + (event.synthetic ? " syn" : ""),
          ts: formatTime(event.t),
          conn: `#${event.conn}`,
          icon: event.dir,
          text: eventText(event),
        };
      case "open":
        return {
          ...base,
          cls: "ev",
          ts: formatTime(event.t),
          conn: `#${event.conn}`,
          icon: "open",
          text: t("feed.opened", { peer: event.peer, local: event.local }),
        };
      case "close":
        return {
          ...base,
          cls: "ev",
          ts: formatTime(event.t),
          conn: `#${event.conn}`,
          icon: "close",
          text: event.reason === "" ? t("feed.closed") : event.reason,
        };
      case "error":
        return {
          ...base,
          cls: "wn",
          ts: formatTime(event.t),
          conn: event.conn === undefined ? "" : `#${event.conn}`,
          icon: "error",
          text: event.message,
        };
      case "info":
        return {
          ...base,
          cls: "ev",
          ts: formatTime(event.t),
          conn: event.conn === undefined ? "" : `#${event.conn}`,
          icon: "info",
          text: event.message,
        };
      case "note":
        return {
          ...base,
          cls: "ev",
          ts: formatTime(event.t),
          icon: "note",
          text: event.text,
        };
      case "stopped":
        return {
          ...base,
          cls: "ev",
          icon: "stopped",
          text: t("feed.stopped"),
        };
      default:
        return base;
    }
  }),
);
</script>

<template>
  <div class="bt-feed">
    <div v-if="journal !== null" class="bt-empty">
      <b>{{ t("feed.viewingTitle") }}</b>
      <span>{{ t("feed.viewingText", { path: journal.path }) }}</span>
    </div>
    <div v-else-if="events.length === 0" class="bt-empty">
      <b>{{ t("feed.emptyTitle") }}</b>
      <span>{{ t("feed.emptyText") }}</span>
    </div>
    <template v-else>
      <div v-for="row in rows" :key="row.key" class="bt-feed-row" :class="row.cls">
        <span class="ts">{{ row.ts }}</span>
        <span class="ar">
          <ArrowRight v-if="row.icon === 'a2l'" :size="14" aria-hidden="true" />
          <ArrowLeft v-else-if="row.icon === 'l2a'" :size="14" aria-hidden="true" />
          <Plug v-else-if="row.icon === 'open'" :size="14" aria-hidden="true" />
          <Unplug v-else-if="row.icon === 'close'" :size="14" aria-hidden="true" />
          <CircleAlert v-else-if="row.icon === 'error'" :size="14" aria-hidden="true" />
          <Square v-else-if="row.icon === 'stopped'" :size="14" aria-hidden="true" />
          <PenLine v-else-if="row.icon === 'note'" :size="14" aria-hidden="true" />
          <Info v-else :size="14" aria-hidden="true" />
        </span>
        <span class="cn">{{ row.conn }}</span>
        <span class="pl">{{ row.text }}</span>
      </div>
    </template>
  </div>
</template>
