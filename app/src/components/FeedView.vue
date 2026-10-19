<script setup lang="ts">
/**
 * Лента обмена: виртуализированный список фрагментов и служебных событий (FR-26,
 * FR-28). В DOM живут только видимые строки; кольцевой буфер держит 10 000 событий.
 *
 * Вид «Текст» — время, направление, соединение, текст плашками; вид «HEX» —
 * дамп по 16 байт, каждая строка дампа — отдельный элемент виртуализации.
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type Component } from "vue";
import { useI18n } from "vue-i18n";
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  Bot,
  Info,
  PenLine,
  Plug,
  Square,
  TriangleAlert,
  Unplug,
} from "@lucide/vue";
import type { FeedEvent, JournalSummary } from "../api/types";
import {
  base64ByteLength,
  base64ToBytes,
  ENCODINGS,
  ENCODING_KEYS,
  type Encoding,
} from "../feed/bytes";
import { connChips, filterIndices } from "../feed/filter";
import { hexLine, hexLineCount, type HexByte } from "../feed/hex";
import { eventRow, type FeedRow, type RowIcon, type Translate } from "../feed/rows";
import type { FeedSource } from "../feed/source";
import {
  applyScroll,
  DEFAULT_VIEWPORT_HEIGHT,
  firstVisibleRow,
  HEX_ROW_HEIGHT,
  isAtBottom,
  isScaled,
  listHeight,
  rangeFromRow,
  scrollTopForRow,
  TEXT_ROW_HEIGHT,
  type ScrollAnchor,
} from "../feed/virtual";
import { fractionAtEvent } from "../trace/model";
import type { FeedViewMode } from "../settings/settings";

const props = defineProps<{
  feed: FeedSource<FeedEvent>;
  journal: JournalSummary | null;
  view: FeedViewMode;
  encoding: Encoding;
}>();

const emit = defineEmits<{
  "update:view": [view: FeedViewMode];
  "update:encoding": [encoding: Encoding];
  /** Видимая область ленты в долях — рамка на карте сессии (AC9). */
  "update:visible": [window: { start: number; end: number }];
}>();

const { t } = useI18n();
const translate: Translate = (key, params) => t(key, params ?? {});

/** Иконка строки по типу события. */
const ICONS: Record<RowIcon, Component> = {
  a2l: ArrowRight,
  l2a: ArrowLeft,
  open: Plug,
  close: Unplug,
  error: TriangleAlert,
  info: Info,
  note: PenLine,
  stopped: Square,
};

interface HexRow {
  key: string;
  lineIndex: number;
  cls: string;
  time: string;
  icon: RowIcon | null;
  conn: string;
  offset: string;
  bytes: HexByte[];
  ascii: string;
}

const scroller = ref<HTMLElement | null>(null);
const scrollTop = ref(0);
/**
 * Якорь точной прокрутки: со сжатой распоркой колесо двигает строки по одной,
 * а далёкие прыжки считаются пропорцией по всей высоте (FR-28).
 */
const anchor = ref<ScrollAnchor | null>(null);
const viewportHeight = ref(DEFAULT_VIEWPORT_HEIGHT);
/** Догонять ли новые строки; ручная прокрутка вверх выключает (AC8). */
const follow = ref(true);
const connFilter = ref<number | null>(null);

/** Просмотр журнала: строки читаются страницами, высоты HEX берутся из индекса (FR-29). */
const viewing = computed(() => props.journal !== null);
const view = computed<FeedViewMode>(() => props.view);

/**
 * Индексы событий буфера: все или выбранного соединения.
 *
 * В журнале события читаются страницами, поэтому список всех индексов не строится:
 * порядок строки совпадает с номером события, а фильтра соединений там нет.
 * Высоты HEX-строк приходят из индекса журнала, а не из прочитанных страниц.
 */
const indices = computed<number[] | null>(() => {
  void props.feed.version;
  if (viewing.value) return null;
  return filterIndices(props.feed, connFilter.value);
});

/** Номер события по порядку строки: в журнале строка и есть событие. */
function eventIndex(order: number): number {
  const list = indices.value;
  if (list === null) return order;
  return list[order] ?? -1;
}

/** Сколько строк в ленте: в журнале — общее число событий из сводки. */
const rowCount = computed(() => indices.value?.length ?? props.feed.length);

/** Длина события в байтах: из индекса журнала, иначе из загруженного события. */
function byteLengthAt(index: number, event: FeedEvent | undefined): number | null {
  const known = props.feed.byteLength?.(index);
  if (known !== undefined && known !== null) return known;
  if (event === undefined) return null;
  return event.type === "data" ? base64ByteLength(event.b64) : 0;
}

/** Номера соединений для чипов фильтра. */
const conns = computed<number[]>(() => {
  void props.feed.version;
  if (viewing.value) return [];
  return connChips(props.feed);
});

/** Выбранное соединение исчезло из буфера — фильтр снимается. */
watch(conns, (list) => {
  if (connFilter.value !== null && !list.includes(connFilter.value)) connFilter.value = null;
});

/** Число строк дампа на событие — только для вида HEX. */
const lineCounts = computed<number[]>(() => {
  if (view.value !== "hex") return [];
  const counts: number[] = [];
  const list = indices.value;
  if (list === null) {
    // Журнал: высоты известны из индекса, страницы не читаются.
    for (let index = 0; index < props.feed.length; index += 1) {
      counts.push(hexLineCount(props.feed.byteLength?.(index) ?? 0));
    }
    return counts;
  }
  for (const index of list) {
    const event = props.feed.at(index);
    counts.push(
      event !== undefined && event.type === "data"
        ? hexLineCount(base64ByteLength(event.b64))
        : 1,
    );
  }
  return counts;
});

/** Префиксные суммы строк дампа: номер строки → событие. */
const linePrefix = computed<number[]>(() => {
  const prefix = [0];
  let total = 0;
  for (const count of lineCounts.value) {
    total += count;
    prefix.push(total);
  }
  return prefix;
});

const totalRows = computed(() =>
  view.value === "hex" ? (linePrefix.value[linePrefix.value.length - 1] ?? 0) : rowCount.value,
);

const rowHeight = computed(() => (view.value === "hex" ? HEX_ROW_HEIGHT : TEXT_ROW_HEIGHT));

/** Первая видимая строка: при сжатой распорке — смещением от якоря или пропорцией. */
const firstRow = computed(() =>
  firstVisibleRow(
    scrollTop.value,
    totalRows.value,
    viewportHeight.value,
    rowHeight.value,
    anchor.value,
  ),
);

const range = computed(() =>
  rangeFromRow(
    totalRows.value,
    firstRow.value,
    viewportHeight.value,
    rowHeight.value,
  ),
);

/** Просит у журнала страницы видимого окна; буферу записи это не нужно. */
function requestPages(from: number, to: number): void {
  props.feed.ensure?.(from, to);
}

watch(range, (value) => requestPages(value.start, value.end), { immediate: true });

/** Строки текстового вида: только видимые, с текстом по кодировке. */
const textRows = computed<FeedRow[]>(() => {
  // Пришедшая страница журнала меняет содержимое строк, а не их число: без этой
  // зависимости лента не перерисовалась бы, пока не сдвинется прокрутка (AC5, AC10).
  void props.feed.version;
  if (view.value !== "text") return [];
  const rows: FeedRow[] = [];
  const { start, end } = range.value;
  for (let order = start; order < end; order += 1) {
    const index = eventIndex(order);
    if (index < 0) break;
    const event = props.feed.at(index);
    // Страница журнала ещё едет: строка появится, когда она придёт.
    if (event === undefined) continue;
    rows.push(eventRow(event, order, translate, props.encoding));
  }
  return rows;
});

/** Ближайшее событие, начинающееся не позже строки `line`. */
function eventAtLine(line: number): number {
  const prefix = linePrefix.value;
  let low = 0;
  let high = rowCount.value - 1;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if ((prefix[middle + 1] ?? 0) <= line) low = middle;
    else high = middle - 1;
  }
  return low;
}

/** Строки дампа для видимого окна. */
const hexRows = computed<HexRow[]>(() => {
  // Та же зависимость, что у текстовых строк: высоты HEX известны заранее, а байты
  // приходят вместе со страницей журнала.
  void props.feed.version;
  if (view.value !== "hex") return [];
  const rows: HexRow[] = [];
  const { start, end } = range.value;
  if (end <= start) return rows;
  const prefix = linePrefix.value;
  let eventOrder = eventAtLine(start);
  let line = start;

  while (line < end && eventOrder < rowCount.value) {
    const index = eventIndex(eventOrder);
    if (index < 0) break;
    const event = props.feed.at(index);
    if (event === undefined) break;
    const bytes = event.type === "data" ? base64ToBytes(event.b64) : new Uint8Array(0);
    const length = byteLengthAt(index, event) ?? bytes.length;
    const count = hexLineCount(length);
    const row = eventRow(event, eventOrder, translate, props.encoding);
    const firstLine = prefix[eventOrder] ?? 0;

    for (let local = line - firstLine; local < count && line < end; local += 1) {
      const dump = hexLine(bytes, local);
      rows.push({
        key: `${index}:${local}`,
        lineIndex: line,
        cls: row.kind === "data" ? row.cls : row.cls === "wn" ? "ev wn" : "ev",
        time: local === 0 ? row.time : "",
        icon: local === 0 ? row.icon : null,
        conn: local === 0 ? row.conn : "",
        offset: dump.offset,
        bytes: dump.bytes,
        ascii: dump.ascii,
      });
      line += 1;
    }
    eventOrder += 1;
  }
  return rows;
});

const listHeightPx = computed(() =>
  listHeight(totalRows.value, rowHeight.value),
);

const empty = computed(() => totalRows.value === 0);
const journalEmpty = computed(() => props.journal !== null && empty.value);

/**
 * Верх строки в координатах содержимого списка.
 *
 * Без сжатия строка стоит на своём номере. Со сжатой распоркой браузер не держит
 * содержимое выше предела, поэтому строки привязаны к окну: первая отрисованная
 * строка встаёт на пиксель начала окна, остальные — через высоту строки.
 */
function rowTop(order: number, height: number): number {
  if (!isScaled(totalRows.value, rowHeight.value)) return order * height;
  const start = range.value.start;
  const top = scrollTop.value - (firstRow.value - start) * height;
  return Math.max(0, top) + (order - start) * height;
}

function rowStyle(order: number): Record<string, string> {
  return {
    top: `${rowTop(order, TEXT_ROW_HEIGHT)}px`,
    height: `${TEXT_ROW_HEIGHT}px`,
  };
}

function hexStyle(line: number): Record<string, string> {
  return {
    top: `${rowTop(line, HEX_ROW_HEIGHT)}px`,
    height: `${HEX_ROW_HEIGHT}px`,
  };
}

function measure(): void {
  const element = scroller.value;
  if (element === null) return;
  viewportHeight.value = element.clientHeight || DEFAULT_VIEWPORT_HEIGHT;
  scrollTop.value = element.scrollTop;
}

function onScroll(): void {
  const element = scroller.value;
  if (element === null) return;
  scrollTop.value = element.scrollTop;
  viewportHeight.value = element.clientHeight || DEFAULT_VIEWPORT_HEIGHT;
  // Точка отсчёта для колеса: от неё следующий шаг считается по одной строке.
  anchor.value = { top: element.scrollTop, row: firstRow.value };
  // В просмотре журнала автопрокрутки нет: страница не должна уводить из середины.
  follow.value =
    !viewing.value &&
    applyScroll(element.scrollTop, element.clientHeight, element.scrollHeight).follow;
  emitVisible();
}

/**
 * Видимая область в долях: при просмотре журнала — по времени (AC9).
 *
 * Плотность на трассе нарисована по `mono_us`, поэтому рамка берёт время первой и
 * последней видимой строки из таблицы корзин журнала. Без таблицы (запись) доли
 * считаются по прокрутке: лента идёт по времени и так.
 */
function emitVisible(): void {
  const table = props.journal?.firstEvent ?? null;
  const { start, end } = range.value;
  if (table !== null && table.length > 0 && end > start) {
    const first = view.value === "hex" ? eventAtLine(start) : eventIndex(start);
    const last = view.value === "hex" ? eventAtLine(end - 1) : eventIndex(end - 1);
    emit("update:visible", {
      start: fractionAtEvent(table, Math.max(0, first)),
      end: fractionAtEvent(table, Math.max(0, last) + 1),
    });
    return;
  }

  const element = scroller.value;
  if (element === null) return;
  const total = element.scrollHeight;
  if (total <= 0) {
    emit("update:visible", { start: 0, end: 1 });
    return;
  }
  const top = element.scrollTop / total;
  const bottom = (element.scrollTop + element.clientHeight) / total;
  emit("update:visible", { start: Math.min(1, Math.max(0, top)), end: Math.min(1, Math.max(0, bottom)) });
}

async function scrollToBottom(): Promise<void> {
  await nextTick();
  const element = scroller.value;
  if (element === null) return;
  element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
  scrollTop.value = element.scrollTop;
  // Догон — прыжок: якорь сбрасывается, нижний край считается пропорцией.
  anchor.value = null;
}

/** Кнопка «Автопрокрутка»: включает догон и прыгает вниз (AC8). */
function enableFollow(): void {
  follow.value = true;
  void scrollToBottom();
}

/**
 * Переход к событию журнала — из трассы при просмотре (AC9).
 *
 * Трасса щёлкает по времени и переводит долю в номер события через таблицу корзин;
 * строка события в журнале и есть событие. Со сжатой распоркой пиксель считается
 * обратным отображением строки.
 */
function seekEvent(index: number): void {
  const total = totalRows.value;
  if (total <= 0) return;
  const event = Math.min(rowCount.value - 1, Math.max(0, Math.trunc(index)));
  const row = view.value === "hex" ? linePrefix.value[event] ?? 0 : event;
  const top = scrollTopForRow(row, total, viewportHeight.value, rowHeight.value);
  const element = scroller.value;
  if (element !== null) element.scrollTop = top;
  scrollTop.value = element === null ? top : element.scrollTop;
  anchor.value = null;
  follow.value =
    !viewing.value &&
    element !== null &&
    isAtBottom(element.scrollTop, element.clientHeight, element.scrollHeight);
  const { start, end } = range.value;
  requestPages(start, end);
  emitVisible();
}

defineExpose({ seekEvent });

/** Новая пачка в буфере: догоняем, если автопрокрутка включена. */
watch(
  () => props.feed.version,
  () => {
    if (!follow.value || viewing.value) return;
    void scrollToBottom();
  },
);

watch(
  () => [props.view, props.encoding],
  () => {
    if (!follow.value) return;
    void scrollToBottom();
  },
);

/** Открыли или закрыли журнал: лента начинается с начала, догон только у записи. */
watch(
  () => props.journal?.path ?? null,
  async () => {
    follow.value = !viewing.value;
    await nextTick();
    const element = scroller.value;
    if (element !== null) element.scrollTop = 0;
    scrollTop.value = 0;
    anchor.value = null;
    measure();
    emitVisible();
  },
);

let resize: ResizeObserver | null = null;

onMounted(() => {
  measure();
  emitVisible();
  if (typeof ResizeObserver !== "undefined" && scroller.value !== null) {
    resize = new ResizeObserver(() => measure());
    resize.observe(scroller.value);
  }
});

onBeforeUnmount(() => {
  resize?.disconnect();
  resize = null;
});
</script>

<template>
  <div class="bt-feed-view">
    <div class="bt-feed-bar">
      <div class="bt-seg" role="group" :aria-label="t('feed.viewGroup')">
        <button
          type="button"
          :class="{ on: view === 'text' }"
          @click="emit('update:view', 'text')"
        >
          {{ t("feed.viewText") }}
        </button>
        <button
          type="button"
          :class="{ on: view === 'hex' }"
          @click="emit('update:view', 'hex')"
        >
          {{ t("feed.viewHex") }}
        </button>
      </div>
      <div class="bt-seg" role="group" :aria-label="t('feed.encoding')">
        <button
          v-for="encodingName in ENCODINGS"
          :key="encodingName"
          type="button"
          :class="{ on: encoding === encodingName }"
          @click="emit('update:encoding', encodingName)"
        >
          {{ t(ENCODING_KEYS[encodingName]) }}
        </button>
      </div>
      <span class="bt-spacer" />
      <div v-if="!viewing" class="bt-conns" role="group" :aria-label="t('feed.connFilter')">
        <button
          type="button"
          :class="{ on: connFilter === null }"
          @click="connFilter = null"
        >
          {{ t("feed.connAll") }}
        </button>
        <button
          v-for="conn in conns"
          :key="conn"
          type="button"
          :class="{ on: connFilter === conn }"
          @click="connFilter = conn"
        >
          #{{ conn }}
        </button>
      </div>
      <button
        v-if="!viewing"
        class="bt-btn ghost autoscroll"
        :class="{ on: !follow }"
        type="button"
        :title="follow ? t('feed.autoscrollOn') : t('feed.autoscrollOff')"
        @click="enableFollow"
      >
        <ArrowDown :size="15" aria-hidden="true" />
        {{ t("feed.autoscroll") }}
      </button>
    </div>

    <div v-if="journalEmpty" class="bt-empty">
      <b>{{ t("journal.emptyTitle") }}</b>
      <span>{{ t("journal.emptyText") }}</span>
    </div>
    <div v-else-if="empty" class="bt-empty">
      <b>{{ t("feed.emptyTitle") }}</b>
      <span>{{ t("feed.emptyText") }}</span>
    </div>
    <div v-else ref="scroller" class="bt-feed" @scroll.passive="onScroll">
      <div class="bt-feed-list" :style="{ height: `${listHeightPx}px` }">
        <template v-if="view === 'text'">
          <div
            v-for="row in textRows"
            :key="row.index"
            class="bt-feed-row"
            :class="row.cls"
            :style="rowStyle(row.index)"
          >
            <span class="ts">{{ row.time }}</span>
            <span class="ar">
              <component :is="ICONS[row.icon]" :size="14" aria-hidden="true" />
            </span>
            <span class="cn">{{ row.conn }}</span>
            <span class="pl">
              <template v-if="row.kind === 'data'">
                <template v-for="(segment, position) in row.segments" :key="position">
                  <span v-if="segment.chip" class="bt-cc">{{ segment.value }}</span>
                  <template v-else>{{ segment.value }}</template>
                </template>
              </template>
              <template v-else>{{ row.text }}</template>
              <span v-if="row.synthetic" class="bt-tag">
                <Bot :size="11" aria-hidden="true" />
                {{ t("feed.synthetic") }}
              </span>
            </span>
          </div>
        </template>
        <template v-else>
          <div
            v-for="row in hexRows"
            :key="row.key"
            class="bt-feed-hex"
            :class="row.cls"
            :style="hexStyle(row.lineIndex)"
          >
            <span class="o">{{ row.time }}</span>
            <span class="ar">
              <component
                :is="ICONS[row.icon ?? 'info']"
                v-if="row.icon !== null"
                :size="14"
                aria-hidden="true"
              />
            </span>
            <span class="o">{{ row.conn }}</span>
            <span class="o">{{ row.offset }}</span>
            <span class="b">
              <template v-for="(byte, position) in row.bytes" :key="position">
                <i v-if="byte.control">{{ byte.value }}</i>
                <template v-else>{{ byte.value }} </template>
              </template>
            </span>
            <span class="a">{{ row.ascii }}</span>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.bt-feed-view {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
}

.bt-feed-list {
  position: relative;
}

.autoscroll.on {
  color: var(--bt-ctrl);
  border-color: var(--bt-ctrl);
}

/* Размер строки задаётся inline из констант виртуализации, не из CSS. */
.bt-feed-row {
  position: absolute;
  left: 0;
  right: 0;
}

.bt-feed-hex {
  position: absolute;
  left: 0;
  right: 0;
}
</style>
