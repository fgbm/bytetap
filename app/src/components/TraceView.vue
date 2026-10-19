<script setup lang="ts">
/**
 * Трасса обмена: две дорожки на canvas (AC9).
 *
 * Во время записи — бегущее окно ~60 с по `mono_us`: импульс на каждый фрагмент,
 * высота по логарифму длины. При просмотре журнала — плотность всей сессии на
 * 600 корзин с рамкой видимой области ленты; клик и перетаскивание переходят
 * к месту. Отрисовка идёт не чаще кадра и только когда данные или время изменились.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { ArrowLeft, ArrowRight } from "@lucide/vue";
import type { Dir, FeedEvent } from "../api/types";
import { base64ByteLength } from "../feed/bytes";
import type { FeedBuffer } from "../feed/ring";
import type { SessionStatus } from "../session/reducer";
import {
  densityBars,
  fractionFromX,
  frameRect,
  impulses,
  windowRange,
  type Point,
} from "../trace/model";

const props = defineProps<{
  status: SessionStatus;
  feed: FeedBuffer<FeedEvent>;
  /** Микросекунды от начала сессии: по ним движется окно во время записи. */
  nowUs: number;
  /** Плотность всей сессии из журнала; `null` — идёт запись. */
  density: { a2l: number[]; l2a: number[] } | null;
  /** Видимая область ленты в долях ширины — рамка на карте сессии. */
  window: { start: number; end: number } | null;
}>();

const emit = defineEmits<{ seek: [fraction: number] }>();

const { t } = useI18n();

const a2lCanvas = ref<HTMLCanvasElement | null>(null);
const l2aCanvas = ref<HTMLCanvasElement | null>(null);
const area = ref<HTMLElement | null>(null);
const brush = ref(frameRect(props.window));

/** Токены темы не читаются в jsdom — на этот случай значения светлой темы. */
const FALLBACK = {
  a2l: "#0a7d4f",
  l2a: "#b4530a",
} as const;

const hasSignals = computed(() => {
  void props.feed.version;
  return props.density !== null || props.feed.length > 0;
});

function cssColor(element: HTMLElement, name: string, fallback: string): string {
  if (typeof getComputedStyle !== "function") return fallback;
  const value = getComputedStyle(element).getPropertyValue(name).trim();
  return value === "" ? fallback : value;
}

/** Последний известный `mono_us`: окно не отстаёт от данных при сжатой истории. */
function latestMonoUs(): number {
  for (let index = props.feed.length - 1; index >= 0; index -= 1) {
    const event = props.feed.at(index);
    if ("monoUs" in event && typeof event.monoUs === "number") return event.monoUs;
  }
  return 0;
}

/** Последние фрагменты направления в окне `[fromUs, toUs]`. */
function pointsFor(dir: Dir, fromUs: number, toUs: number): Point[] {
  const points: Point[] = [];
  for (let index = props.feed.length - 1; index >= 0; index -= 1) {
    const event = props.feed.at(index);
    if (event.type !== "data" || event.dir !== dir) continue;
    // `mono_us` растёт по буферу: первое старое событие завершает выборку.
    if (event.monoUs < fromUs || event.monoUs > toUs) break;
    points.push({ monoUs: event.monoUs, len: base64ByteLength(event.b64) });
  }
  return points.reverse();
}

function prepare(canvas: HTMLCanvasElement): CanvasRenderingContext2D | null {
  const context = canvas.getContext("2d");
  if (context === null) return null;
  const ratio = typeof window === "undefined" ? 1 : window.devicePixelRatio || 1;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  if (width <= 0 || height <= 0) return null;
  canvas.width = Math.round(width * ratio);
  canvas.height = Math.round(height * ratio);
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);
  return context;
}

/** Импульсы бегущего окна: короткая линия от базовой линии, высота по log(len). */
function drawWindow(
  canvas: HTMLCanvasElement,
  color: string,
  dir: Dir,
  fromUs: number,
  toUs: number,
): void {
  const context = prepare(canvas);
  if (context === null) return;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  const bottom = height - 1.5;

  context.strokeStyle = color;
  context.globalAlpha = 0.35;
  context.beginPath();
  context.moveTo(0, bottom);
  context.lineTo(width, bottom);
  context.stroke();

  context.globalAlpha = 0.9;
  context.beginPath();
  for (const impulse of impulses(pointsFor(dir, fromUs, toUs), fromUs, toUs)) {
    const x = Math.round(impulse.x * width) + 0.5;
    context.moveTo(x, bottom);
    context.lineTo(x, bottom - Math.max(1, impulse.height * (height - 2)));
  }
  context.stroke();
  context.globalAlpha = 1;
}

/** Плотность всей сессии: столбик на корзину. */
function drawDensity(canvas: HTMLCanvasElement, color: string, values: readonly number[]): void {
  const context = prepare(canvas);
  if (context === null) return;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  const bars = densityBars(values);
  const slotWidth = width / bars.length;

  context.fillStyle = color;
  context.globalAlpha = 0.75;
  context.beginPath();
  for (let index = 0; index < bars.length; index += 1) {
    const value = bars[index];
    if (value <= 0) continue;
    const x = index * slotWidth;
    const barHeight = Math.max(1, value * (height - 1));
    context.rect(x, height - barHeight, Math.max(1, slotWidth - 0.5), barHeight);
  }
  context.fill();
  context.globalAlpha = 1;
}

function draw(): void {
  const a2l = a2lCanvas.value;
  const l2a = l2aCanvas.value;
  if (a2l === null || l2a === null) return;

  const a2lColor = cssColor(a2l, "--bt-dir-a2l", FALLBACK.a2l);
  const l2aColor = cssColor(l2a, "--bt-dir-l2a", FALLBACK.l2a);

  if (props.density !== null) {
    drawDensity(a2l, a2lColor, props.density.a2l);
    drawDensity(l2a, l2aColor, props.density.l2a);
  } else {
    const { fromUs, toUs } = windowRange(Math.max(props.nowUs, latestMonoUs()));
    drawWindow(a2l, a2lColor, "a2l", fromUs, toUs);
    drawWindow(l2a, l2aColor, "l2a", fromUs, toUs);
  }
  brush.value = frameRect(props.window);
}

let frame = 0;
let dirty = false;

/** Отрисовка не чаще кадра: пачка событий даёт один перерисованный кадр. */
function requestDraw(): void {
  dirty = true;
  if (frame !== 0) return;
  if (typeof requestAnimationFrame !== "function") {
    dirty = false;
    draw();
    return;
  }
  frame = requestAnimationFrame(() => {
    frame = 0;
    if (!dirty) return;
    dirty = false;
    draw();
  });
}

/** `prefers-reduced-motion`: окно не двигается по таймеру, только по новым данным. */
function reducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

watch(() => props.feed.version, requestDraw);
watch(() => props.nowUs, () => {
  if (reducedMotion()) return;
  requestDraw();
});
watch(() => props.density, requestDraw);
watch(
  () => props.window,
  (value) => {
    brush.value = frameRect(value);
  },
);

let observer: MutationObserver | null = null;
let media: MediaQueryList | null = null;

function onThemeChange(): void {
  requestDraw();
}

onMounted(() => {
  if (typeof MutationObserver !== "undefined") {
    observer = new MutationObserver(onThemeChange);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "class"] });
  }
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", onThemeChange);
  }
  requestDraw();
});

onBeforeUnmount(() => {
  observer?.disconnect();
  observer = null;
  media?.removeEventListener("change", onThemeChange);
  media = null;
  if (frame !== 0 && typeof cancelAnimationFrame === "function") cancelAnimationFrame(frame);
  frame = 0;
});

let dragging = false;

function seekAt(event: PointerEvent): void {
  const element = area.value;
  if (element === null) return;
  emit("seek", fractionFromX(event.clientX, element.getBoundingClientRect()));
}

function onPointerDown(event: PointerEvent): void {
  dragging = true;
  (event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId);
  seekAt(event);
}

function onPointerMove(event: PointerEvent): void {
  if (dragging) seekAt(event);
}

function onPointerUp(): void {
  dragging = false;
}
</script>

<template>
  <div class="bt-trace">
    <div class="bt-lane">
      <label>
        <ArrowRight :size="12" aria-hidden="true" />
        {{ t("trace.a2l") }}
      </label>
      <canvas ref="a2lCanvas" class="wave" aria-hidden="true" />
    </div>
    <div class="bt-lane">
      <label>
        <ArrowLeft :size="12" aria-hidden="true" />
        {{ t("trace.l2a") }}
      </label>
      <canvas ref="l2aCanvas" class="wave" aria-hidden="true" />
    </div>

    <div
      v-if="density !== null"
      ref="area"
      class="brush-area"
      :title="t('trace.seekHint')"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
    >
      <span
        class="brush"
        :style="{ left: `${brush.left * 100}%`, width: `${brush.width * 100}%` }"
      />
    </div>

    <span v-if="status === 'viewing'" class="idle">{{ t("trace.viewing") }}</span>
    <span v-else-if="!hasSignals" class="idle">{{ t("trace.idle") }}</span>
  </div>
</template>

<style scoped>
.wave {
  display: block;
  flex: 1;
  height: 18px;
  width: 100%;
}

/* Область карты сессии: клик и перетаскивание переходят к месту (AC9). */
.brush-area {
  position: absolute;
  top: 9px;
  bottom: 7px;
  left: calc(var(--bt-space-3) + 118px + var(--bt-space-2));
  right: var(--bt-space-3);
  cursor: pointer;
  touch-action: none;
}

.brush {
  position: absolute;
  top: 0;
  bottom: 0;
  min-width: 2px;
  border: 1.5px solid var(--bt-ctrl);
  border-radius: var(--bt-radius-chip);
  background: color-mix(in srgb, var(--bt-ctrl) 8%, transparent);
  pointer-events: none;
}
</style>
