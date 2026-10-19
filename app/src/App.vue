<script setup lang="ts">
/**
 * Оболочка приложения UI v2.
 *
 * Один экран за раз: выбор сценария, шаг сценария, запись и просмотр журнала.
 * Шапка одна на все экраны и она же заголовок окна без системной рамки. Служебное
 * живёт в меню «…» и в «Дополнительно» формы, поэтому в записи одна строка шапки:
 * бренд, «Стоп» с таймером, сводка режима, счётчики и меню.
 */
import { computed, onMounted, reactive, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import {
  CircleAlert,
  FolderOpen,
  MoreHorizontal,
  Pen,
  TriangleAlert,
  X,
} from "@lucide/vue";
import { bridge } from "./api/bridge";
import type {
  InterfaceInfo,
  LogDirWarning,
  RecentJournal,
  SerialPort,
  SessionConfig,
  SessionMode,
} from "./api/types";
import BrandMark from "./components/BrandMark.vue";
import FeedView from "./components/FeedView.vue";
import MenuButton from "./components/MenuButton.vue";
import ScenariosView from "./components/ScenariosView.vue";
import ScenarioForm from "./components/ScenarioForm.vue";
import TextPrompt from "./components/TextPrompt.vue";
import ThemeToggle from "./components/ThemeToggle.vue";
import TitleBar from "./components/TitleBar.vue";
import TraceView from "./components/TraceView.vue";
import { base64ToBytes } from "./feed/bytes";
import { formatBytes, formatDuration, formatDurationSpan } from "./format";
import { eventAtFraction } from "./trace/model";
import { journalFileName, journalMeta, journalNotes } from "./journal/meta";
import { ERROR_KEYS, errorField, errorParams, FAILURE_KEYS } from "./session/errors";
import { resolvedTheme } from "./theme";
import {
  sessionConfigFor,
  serialFormat,
  type AppConfig,
  type LastRun,
} from "./settings/settings";
import { useSettings } from "./settings/store";
import { useSession } from "./session/store";
import { checkPort } from "./validation";

const { t } = useI18n();

const {
  state,
  feed,
  journalFeed,
  elapsedMs,
  start,
  stop,
  adopt,
  addNote,
  openJournal,
  appendJournalNote,
  closeJournal,
  dismissError,
  dismissFailure,
  reportError,
} = useSession();
const settingsStore = useSettings();
const settings = settingsStore.state;

/** Экран выбора: сценарии или шаг сценария. Запись и просмотр задаются статусом. */
const screen = ref<"scenarios" | "form">("scenarios");
const interfaces = ref<InterfaceInfo[]>([]);
const serialPorts = ref<SerialPort[]>([]);
const recents = ref<RecentJournal[] | null>(null);
const logDirWarning = ref<LogDirWarning | null>(null);
const selectedProfile = ref("");
const lastJournalPath = ref<string | null>(null);

/** Методы ленты для перехода из трассы при просмотре журнала (AC9). */
interface FeedViewHandle {
  seekEvent(index: number): void;
}
const feedView = ref<FeedViewHandle | null>(null);
const visibleWindow = ref<{ start: number; end: number } | null>(null);
const prompt = reactive<{ kind: "note" | "profile" | "journalNote" | null; value: string }>({
  kind: null,
  value: "",
});

const recording = computed(
  () => state.value.status === "recording" || state.value.status === "stopping",
);
const journalOpen = computed(() => state.value.status === "viewing");
/** Пока идёт запись, старт или стоп, открыть журнал нельзя: лента занята. */
const journalBlocked = computed(
  () =>
    state.value.status === "starting" ||
    state.value.status === "recording" ||
    state.value.status === "stopping",
);
const busy = computed(
  () => state.value.status === "starting" || state.value.status === "stopping",
);
const nowUs = computed(() => Math.round(elapsedMs.value * 1000));
const density = computed(() => state.value.journal?.density ?? null);
const duration = computed(() => formatDuration(elapsedMs.value));

/** Строки ленты: при просмотре журнала — страницы, иначе кольцевой буфер записи. */
const activeFeed = computed(() =>
  journalOpen.value && journalFeed.value !== null ? journalFeed.value : feed,
);

const journal = computed(() => state.value.journal);
const meta = computed(() =>
  journal.value === null ? null : journalMeta(journal.value.header),
);
const journalName = computed(() =>
  journal.value === null ? "" : journalFileName(journal.value.path),
);
/** «· ANALYZER · 42 мин» под именем файла: анализатор и длительность записи. */
const journalMetaLine = computed(() => {
  const current = meta.value;
  if (current === null || journal.value === null) return [];
  return [
    current.analyzer === "" ? t("journal.notSpecified") : current.analyzer,
    formatDurationSpan(journal.value.durationUs),
  ].filter((part) => part !== "");
});
const journalNotesText = computed(() => {
  const pages = journalFeed.value;
  void pages?.version;
  if (meta.value === null) return "";
  return journalNotes(meta.value.note, pages?.notes(3) ?? []);
});

/** Протокол, выбранный «Авто» по первому байту первого фрагмента анализатора. */
const detectedProtocol = computed<"hl7" | "astm" | null>(() => {
  void feed.version;
  for (let index = 0; index < feed.length; index += 1) {
    const event = feed.at(index);
    if (event === undefined || event.type !== "data" || event.dir !== "a2l") continue;
    const first = base64ToBytes(event.b64)[0];
    if (first === 0x0b) return "hl7";
    if (first === 0x05) return "astm";
    return null;
  }
  return null;
});

/** Сводка режима в шапке записи: «жду на :5000 · HL7 (авто)», «COM5 · 9600 8N1». */
const recordSummary = computed(() => {
  const cfg = state.value.startedConfig;
  if (cfg === null) {
    const addr = state.value.startInfo?.localAddr ?? "";
    return addr === "" ? "" : t("record.listening", { addr });
  }
  if (cfg.mode === "tcp_client") {
    return t("record.connectedTo", {
      addr: `${cfg.connectHost ?? ""}:${cfg.connectPort ?? ""}`,
      responder: responderName(cfg),
    });
  }
  if (cfg.mode === "serial" && cfg.serial !== undefined) {
    return `${cfg.serial.port} · ${cfg.serial.baud} ${serialFormat(cfg.serial)}`;
  }
  const port = `:${cfg.listenPort ?? ""}`;
  const key = state.value.stats.connections > 0 ? "record.onPort" : "record.waitingOn";
  return t(key, { port, responder: responderName(cfg) });
});

function responderName(cfg: SessionConfig): string {
  if (cfg.responder === "hl7") return t("record.responderHl7");
  if (cfg.responder === "astm") return t("record.responderAstm");
  if (cfg.responder === "none") return t("record.responderNone");
  if (detectedProtocol.value === "hl7") return t("record.responderAutoHl7");
  if (detectedProtocol.value === "astm") return t("record.responderAutoAstm");
  return t("record.responderAuto");
}

/** Ошибка старта: поле формы и текст по коду, а не по сообщению ядра (AC3). */
const errorInfo = computed<{ field: ReturnType<typeof errorField> | null; text: string | null }>(
  () => {
    const error = state.value.error;
    if (error === null) return { field: null, text: null };
    return {
      field: errorField(error.code, settings.config.mode),
      text: t(ERROR_KEYS[error.code], errorParams(error, settings.config)),
    };
  },
);

/** Отказ журнала: одна строка причины и два текстовых действия (M2-AC15). */
const failureText = computed(() => {
  const failure = state.value.journalFailure;
  if (failure === null) return "";
  return t("banner.journalFailureTitleWith", {
    reason: t(FAILURE_KEYS[failure.code]),
  });
});

/** Предупреждение о настройках: одной muted-строкой на стартовом экране. */
const settingsWarning = computed(() => {
  const warning = settingsStore.state.warning;
  if (warning === null) return "";
  if (warning.code === "corrupt") return t("settings.warningCorrupt", { path: warning.path });
  return t("settings.warningReset");
});

const lastRun = computed<LastRun | null>(() => settings.lastRun);
const journalPath = computed(
  () => state.value.startInfo?.journalPath ?? lastJournalPath.value,
);

const promptTitle = computed(() => {
  if (prompt.kind === "note") return t("actions.noteTitle");
  if (prompt.kind === "journalNote") return t("journal.noteTitle");
  return t("actions.profileName");
});
const promptLabel = computed(() => {
  if (prompt.kind === "note") return t("actions.note");
  if (prompt.kind === "journalNote") return t("journal.noteLabel");
  return t("actions.profileName");
});
const promptPlaceholder = computed(() => {
  if (prompt.kind === "note") return t("actions.notePlaceholder");
  if (prompt.kind === "journalNote") return t("journal.notePlaceholder");
  return t("actions.profileNamePlaceholder");
});
const promptConfirm = computed(() => {
  if (prompt.kind === "note") return t("actions.noteAdd");
  if (prompt.kind === "journalNote") return t("journal.noteAdd");
  return t("actions.save");
});

async function refreshLogDirWarning(): Promise<void> {
  const dir = settings.config.logDir;
  if (dir === "") {
    logDirWarning.value = null;
    return;
  }
  try {
    logDirWarning.value = await bridge.logDirWarning(dir);
  } catch {
    logDirWarning.value = null;
  }
}

/** Последние журналы папки; `null` — команды в мосте нет, блок скрыт. */
async function refreshRecents(): Promise<void> {
  try {
    recents.value = await bridge.listRecentJournals(settings.config.logDir, 3);
  } catch {
    recents.value = null;
  }
}

async function refreshSerialPorts(): Promise<void> {
  try {
    serialPorts.value = await bridge.listSerialPorts();
  } catch {
    serialPorts.value = [];
  }
}

async function loadEnvironment(): Promise<void> {
  // Ошибка настроек не обрывает окружение: интерфейс работает на значениях по умолчанию,
  // а предупреждение объясняет, что случилось (AC11).
  await settingsStore.loadSettings();
  try {
    interfaces.value = await bridge.listInterfaces();
  } catch {
    interfaces.value = [];
  }
  try {
    const dir = await bridge.defaultLogDir();
    if (dir !== "") settingsStore.ensureLogDir(dir);
  } catch {
    // папку по умолчанию не узнать — поле останется пустым и старт будет недоступен
  }
  await Promise.all([refreshLogDirWarning(), refreshRecents(), refreshSerialPorts()]);
}

/** Перезагрузка страницы не должна оставить сессию сиротой (AC1). */
async function restoreSession(): Promise<void> {
  try {
    await adopt(await bridge.sessionStatus());
  } catch {
    // Состояние не узнать — интерфейс начнёт с выбора сценария.
  }
}

onMounted(() => {
  void loadEnvironment().finally(() => void restoreSession());
});

watch(
  () => settings.config.logDir,
  () => {
    void refreshLogDirWarning();
    void refreshRecents();
  },
);

watch(
  () => state.value.startInfo,
  (info) => {
    if (info !== null) lastJournalPath.value = info.journalPath;
  },
);

// Запись пошла: после «Стоп» человек возвращается к выбору сценария, а не к форме.
watch(
  () => state.value.status,
  (status) => {
    if (status === "recording") screen.value = "scenarios";
  },
);

function onChoose(mode: SessionMode): void {
  dismissError();
  settingsStore.updateConfig({ mode });
  screen.value = "form";
}

function onBack(): void {
  dismissError();
  screen.value = "scenarios";
}

async function onStart(patch: Partial<AppConfig>): Promise<void> {
  settingsStore.updateConfig(patch);
  const cfg = settings.config;
  const portOk =
    cfg.mode !== "tcp_server" || checkPort(String(cfg.listenPort)).problem === null;
  if (!portOk) return;
  settingsStore.rememberLastRun(cfg);
  lastJournalPath.value = null;
  visibleWindow.value = null;
  await start(sessionConfigFor(cfg));
}

async function onStop(): Promise<void> {
  await stop();
  if (state.value.summary !== null) lastJournalPath.value = state.value.summary.journalPath;
}

/**
 * Переход к месту из трассы (просмотр журнала): доля времени переводится в событие
 * через таблицу корзин сводки, поэтому плотность и лента говорят об одном времени.
 */
function onSeek(fraction: number): void {
  const current = state.value.journal;
  if (current === null) return;
  feedView.value?.seekEvent(eventAtFraction(current.firstEvent, current.events, fraction));
}


/** Лента сообщила видимую область — рамка на карте сессии. */
function onVisible(window: { start: number; end: number }): void {
  visibleWindow.value = window;
}

/** Закрытие журнала сбрасывает рамку видимой области. */
function onCloseJournal(): void {
  visibleWindow.value = null;
  closeJournal();
}

const dark = computed(() => resolvedTheme(settings.theme) === "dark");

function toggleTheme(): void {
  settingsStore.setTheme(dark.value ? "light" : "dark");
}

async function pickLogDir(): Promise<string | null> {
  try {
    return await bridge.pickLogDir();
  } catch (error) {
    reportError("io", error instanceof Error ? error.message : String(error));
    return null;
  }
}

async function onPickLogDir(): Promise<void> {
  const dir = await pickLogDir();
  if (dir !== null) settingsStore.updateConfig({ logDir: dir });
}

async function onOpenJournal(): Promise<void> {
  try {
    const path = await bridge.pickJournal();
    if (path === null) return;
    await openJournal(path);
  } catch (error) {
    reportError("io", error instanceof Error ? error.message : String(error));
  }
}

async function onOpenRecent(path: string): Promise<void> {
  if (journalBlocked.value) return;
  await openJournal(path);
}

async function onOpenFolder(): Promise<void> {
  const dir = settings.config.logDir;
  if (dir === "") return;
  try {
    await bridge.openLogDir(dir);
  } catch (error) {
    reportError("io", error instanceof Error ? error.message : String(error));
  }
}

async function copyJournalPath(): Promise<void> {
  const path = journalPath.value;
  if (path === null) return;
  try {
    await navigator.clipboard?.writeText(path);
  } catch {
    // Буфер обмена недоступен: путь виден в меню «Открыть папку журналов».
  }
}

async function onChooseDirFromFailure(): Promise<void> {
  const dir = await pickLogDir();
  if (dir === null) return;
  settingsStore.updateConfig({ logDir: dir });
  dismissFailure();
}

async function onOpenRecorded(): Promise<void> {
  const path = lastJournalPath.value;
  if (path !== null) {
    await openJournal(path);
    return;
  }
  await onOpenFolder();
}

function onSelectProfile(name: string): void {
  selectedProfile.value = name;
  if (name === "") return;
  settingsStore.applyProfile(name);
}

function onSaveProfile(): void {
  prompt.kind = "profile";
  prompt.value =
    settings.config.analyzer.trim() !== ""
      ? settings.config.analyzer.trim()
      : `:${settings.config.listenPort}`;
}

function onDeleteProfile(): void {
  if (selectedProfile.value === "") return;
  settingsStore.deleteProfile(selectedProfile.value);
  selectedProfile.value = "";
}

function openNote(): void {
  prompt.kind = "note";
  prompt.value = "";
}

/** «Комментарий» в остановленный журнал (FR-23). */
function openJournalNote(): void {
  prompt.kind = "journalNote";
  prompt.value = "";
}

function onPromptConfirm(text: string): void {
  if (prompt.kind === "note") {
    void addNote(text);
  } else if (prompt.kind === "journalNote") {
    void appendJournalNote(text);
  } else if (prompt.kind === "profile") {
    settingsStore.saveProfile(text);
    selectedProfile.value = text;
  }
  prompt.kind = null;
}
</script>

<template>
  <div class="bt-app">
    <!-- Шапка выбора сценария и формы: бренд, «Журналы», тема, кнопки окна. -->
    <TitleBar v-if="!recording && !journalOpen">
      <template #left>
        <BrandMark />
        <MenuButton :label="t('header.journals')" :icon="FolderOpen">
          <template #default="{ close }">
            <button
              class="bt-menu-item"
              type="button"
              :disabled="journalBlocked"
              @click="onOpenJournal(); close()"
            >
              {{ t("header.openJournal") }}
            </button>
            <button class="bt-menu-item" type="button" @click="onOpenFolder(); close()">
              {{ t("header.openFolder") }}
            </button>
          </template>
        </MenuButton>
      </template>
      <template #right>
        <ThemeToggle />
      </template>
    </TitleBar>

    <!-- Шапка записи: одна строка, «Стоп», сводка, счётчики, меню «…». -->
    <TitleBar v-else-if="recording">
      <template #left>
        <BrandMark />
        <button class="bt-stop" type="button" @click="onStop">
          <span class="bt-dot live" aria-hidden="true" />
          {{ t("record.stop") }}
          <span class="bt-mono">{{ duration }}</span>
        </button>
        <span class="bt-muted summary" :title="recordSummary">{{ recordSummary }}</span>
      </template>
      <template #right>
        <span class="bt-counters bt-mono">
          <span><i class="a2l">→</i> {{ formatBytes(state.stats.bytesAnalyzerToLis) }}</span>
          <span><i class="l2a">←</i> {{ formatBytes(state.stats.bytesLisToAnalyzer) }}</span>
          <span class="bt-faint">{{ state.stats.connections }} {{ t("record.connShort") }}</span>
        </span>
        <MenuButton :icon="MoreHorizontal" :title="t('record.menu')">
          <template #default="{ close }">
            <button class="bt-menu-item" type="button" @click="openNote(); close()">
              {{ t("record.addNote") }}
            </button>
            <button class="bt-menu-item" type="button" @click="onOpenFolder(); close()">
              {{ t("header.openFolder") }}
            </button>
            <button class="bt-menu-item" type="button" @click="copyJournalPath(); close()">
              {{ t("record.copyPath") }}
            </button>
            <span class="bt-menu-sep" />
            <button class="bt-menu-item" type="button" @click="toggleTheme(); close()">
              {{ dark ? t("theme.toLight") : t("theme.toDark") }}
            </button>
          </template>
        </MenuButton>
      </template>
    </TitleBar>

    <!-- Шапка просмотра журнала: закрыть, файл, комментарий, тема. -->
    <TitleBar v-else>
      <template #left>
        <BrandMark />
        <button
          class="bt-icon"
          type="button"
          :title="t('journal.close')"
          :aria-label="t('journal.close')"
          @click="onCloseJournal"
        >
          <X :size="16" aria-hidden="true" />
        </button>
        <span class="bt-mono journal-name" :title="journal?.path">{{ journalName }}</span>
        <span v-if="journalMetaLine.length > 0" class="bt-muted journal-meta">
          · {{ journalMetaLine.join(" · ") }}
        </span>
      </template>
      <template #right>
        <button class="bt-link" type="button" @click="openJournalNote">
          <Pen :size="15" aria-hidden="true" />
          {{ t("journal.comment") }}
        </button>
        <ThemeToggle />
      </template>
    </TitleBar>

    <!-- Отказ журнала: строка причины и два текстовых действия, без баннера. -->
    <div v-if="state.journalFailure !== null" class="bt-notice err" role="alert">
      <CircleAlert :size="15" aria-hidden="true" />
      <span>{{ failureText }}</span>
      <span class="bt-spacer" />
      <span class="actions">
        <button class="bt-link danger" type="button" @click="onChooseDirFromFailure">
          {{ t("banner.chooseOtherDir") }}
        </button>
        <button class="bt-link danger" type="button" @click="onOpenRecorded">
          {{ t("banner.openRecorded") }}
        </button>
      </span>
    </div>

    <!-- Просмотр журнала: предупреждение о ПДн, карта сессии, лента. -->
    <template v-if="journalOpen && journal !== null">
      <div class="bt-notice warn" role="note">
        <TriangleAlert :size="15" aria-hidden="true" />
        <span>{{ t("journal.personalData") }}</span>
        <span class="bt-spacer" />
        <span v-if="journalNotesText !== ''" class="bt-faint notes">«{{ journalNotesText }}»</span>
      </div>
      <div class="bt-journal-map">
        <TraceView
          :feed="feed"
          :now-us="nowUs"
          :density="density"
          :window="visibleWindow"
          @seek="onSeek"
        />
        <FeedView
          ref="feedView"
          :feed="activeFeed"
          :journal="state.journal"
          :view="settings.feed.view"
          :encoding="settings.feed.encoding"
          :dropped="state.dropped"
          @update:view="settingsStore.setFeedView"
          @update:encoding="settingsStore.setFeedEncoding"
          @update:visible="onVisible"
        />
      </div>
    </template>

    <ScenariosView
      v-else-if="!recording && screen === 'scenarios'"
      :last-run="lastRun"
      :recents="recents"
      :warning="settingsWarning"
      :busy="journalBlocked"
      @select="onChoose"
      @open-recent="onOpenRecent"
    />

    <ScenarioForm
      v-else-if="!recording"
      :config="settings.config"
      :interfaces="interfaces"
      :serial-ports="serialPorts"
      :log-dir-warning="logDirWarning"
      :error-field="errorInfo.field"
      :error-text="errorInfo.text"
      :profiles="settings.profiles"
      :selected-profile="selectedProfile"
      :busy="busy"
      @back="onBack"
      @update="settingsStore.updateConfig"
      @update-serial="settingsStore.updateSerial"
      @start="onStart"
      @pick-log-dir="onPickLogDir"
      @refresh-ports="refreshSerialPorts"
      @save-profile="onSaveProfile"
      @delete-profile="onDeleteProfile"
      @apply-profile="onSelectProfile"
    />

    <template v-else>
      <TraceView
        :feed="feed"
        :now-us="nowUs"
        :density="density"
        :window="visibleWindow"
        @seek="onSeek"
      />
      <FeedView
        ref="feedView"
        :feed="activeFeed"
        :journal="null"
        :view="settings.feed.view"
        :encoding="settings.feed.encoding"
        :dropped="state.dropped"
        @update:view="settingsStore.setFeedView"
        @update:encoding="settingsStore.setFeedEncoding"
        @update:visible="onVisible"
      />
    </template>

    <TextPrompt
      :open="prompt.kind !== null"
      :title="promptTitle"
      :label="promptLabel"
      :placeholder="promptPlaceholder"
      :value="prompt.value"
      :multiline="prompt.kind === 'note' || prompt.kind === 'journalNote'"
      :confirm-label="promptConfirm"
      @confirm="onPromptConfirm"
      @cancel="prompt.kind = null"
    />
  </div>
</template>

<style scoped>
.summary {
  overflow: hidden;
  max-width: 42%;
  font-size: var(--bt-text-data);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.journal-name {
  overflow: hidden;
  max-width: 32%;
  font-size: var(--bt-text-xs);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.journal-meta {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.notes {
  overflow: hidden;
  max-width: 40%;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.bt-counters {
  display: inline-flex;
  gap: var(--bt-space-3);
  align-items: center;
  flex: none;
  color: var(--bt-text);
  font-size: var(--bt-text-xs);
  white-space: nowrap;
}

.bt-counters .a2l {
  color: var(--bt-dir-a2l);
  font-style: normal;
}

.bt-counters .l2a {
  color: var(--bt-dir-l2a);
  font-style: normal;
}
</style>
