<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { FileText, PenLine, TriangleAlert, X } from "@lucide/vue";
import { bridge } from "./api/bridge";
import type { InterfaceInfo, LogDirWarning } from "./api/types";
import ActionBar from "./components/ActionBar.vue";
import AppHeader from "./components/AppHeader.vue";
import ConfigForm from "./components/ConfigForm.vue";
import ErrorBanner from "./components/ErrorBanner.vue";
import FeedView from "./components/FeedView.vue";
import JournalFailureBanner from "./components/JournalFailureBanner.vue";
import SessionChips from "./components/SessionChips.vue";
import StatusBar from "./components/StatusBar.vue";
import TextPrompt from "./components/TextPrompt.vue";
import TraceView from "./components/TraceView.vue";
import { formatDateTime } from "./format";
import {
  journalDuration,
  journalFileName,
  journalMeta,
  journalNotes,
  MODE_KEYS,
  RESPONDER_KEYS,
} from "./journal/meta";
import { checkPort, PORT_PROBLEM_KEYS } from "./validation";
import { nextTheme } from "./theme";
import { useSettings } from "./settings/store";
import { useSession } from "./session/store";
import { ERROR_KEYS, errorParams, LOG_DIR_ERROR_CODES, PORT_ERROR_CODES } from "./session/errors";

const { t } = useI18n();

const {
  state,
  feed,
  journalFeed,
  elapsedMs,
  start,
  stop,
  stopAny,
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

const interfaces = ref<InterfaceInfo[]>([]);
const logDirWarning = ref<LogDirWarning | null>(null);
const portText = ref(String(settings.config.listenPort));
const selectedProfile = ref("");
const lastJournalPath = ref<string | null>(null);
/** Методы ленты для перехода из трассы при просмотре журнала (AC9). */
interface FeedViewHandle {
  seekFraction(fraction: number): void;
}
const feedView = ref<FeedViewHandle | null>(null);
const visibleWindow = ref<{ start: number; end: number } | null>(null);
const prompt = reactive<{ kind: "note" | "profile" | "journalNote" | null; value: string }>({
  kind: null,
  value: "",
});

const recording = computed(() => state.value.status === "recording");
const journalOpen = computed(() => state.value.status === "viewing");
/** Пока идёт запись или настройка стартует, открыть журнал нельзя: лента занята. */
const journalBlocked = computed(
  () =>
    state.value.status === "starting" ||
    state.value.status === "recording" ||
    state.value.status === "stopping",
);
const nowUs = computed(() => Math.round(elapsedMs.value * 1000));
const density = computed(() => state.value.journal?.density ?? null);
const canStart = computed(
  () =>
    checkPort(portText.value).problem === null &&
    settings.config.logDir.trim() !== "" &&
    state.value.status !== "recording" &&
    state.value.status !== "starting" &&
    state.value.status !== "stopping",
);

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
const journalMode = computed(() => {
  const current = meta.value;
  if (current === null) return "";
  const mode = t(MODE_KEYS[current.mode] ?? "journal.modeUnknown");
  const responder =
    current.responder === ""
      ? ""
      : `, ${t(RESPONDER_KEYS[current.responder] ?? "journal.modeUnknown")}`;
  return `${mode}${current.port === "" ? "" : ` :${current.port}`}${responder}`;
});
const journalStarted = computed(() =>
  meta.value === null ? "" : formatDateTime(meta.value.startedAt),
);
const journalDurationText = computed(() =>
  journal.value === null ? "" : journalDuration(journal.value.durationUs),
);
/** Комментарий из заголовка и последние дописанные `note` (FR-23). */
const journalNotesText = computed(() => {
  const pages = journalFeed.value;
  void pages?.version;
  if (meta.value === null) return "";
  return journalNotes(meta.value.note, pages?.notes(3) ?? []);
});

const portError = computed(() => {
  const error = state.value.error;
  if (error !== null && PORT_ERROR_CODES.includes(error.code)) {
    return t(ERROR_KEYS[error.code], errorParams(error, settings.config));
  }
  const problem = checkPort(portText.value).problem;
  return problem === null ? null : t(PORT_PROBLEM_KEYS[problem]);
});

const logDirError = computed(() => {
  const error = state.value.error;
  if (error === null || !LOG_DIR_ERROR_CODES.includes(error.code)) return null;
  return t(ERROR_KEYS[error.code], errorParams(error, settings.config));
});

const journalPath = computed(
  () => state.value.startInfo?.journalPath ?? lastJournalPath.value,
);

/** Запись идёт, а настроек старта у страницы нет: сессия пережила перезагрузку (AC1). */
const orphanSession = computed(
  () => state.value.status === "recording" && state.value.startedConfig === null,
);

/** Баннер о настройках: сброс или испорченный файл с копией (AC11). */
const settingsWarning = computed(() => {
  const warning = settingsStore.state.warning;
  if (warning === null) return "";
  if (warning.code === "corrupt") {
    return t("settings.warningCorrupt", { path: warning.path });
  }
  return t("settings.warningReset");
});

const promptTitle = computed(() => {
  if (prompt.kind === "note") return t("actions.noteTitle");
  if (prompt.kind === "journalNote") return t("journal.noteTitle");
  return t("actions.profileName");
});
const promptLabel = computed(() => {
  if (prompt.kind === "note") return t("config.note");
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

async function loadEnvironment(): Promise<void> {
  // Ошибка настроек не обрывает окружение: интерфейс работает на значениях по умолчанию,
  // а баннер settings.warning* объясняет, что случилось (AC11).
  await settingsStore.loadSettings();
  portText.value = String(settings.config.listenPort);
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
  await refreshLogDirWarning();
}

/** Перезагрузка страницы не должна оставить сессию сиротой (AC1). */
async function restoreSession(): Promise<void> {
  try {
    await adopt(await bridge.sessionStatus());
  } catch {
    // Состояние не узнать — интерфейс просто начнёт с «Готов к старту».
  }
}

onMounted(() => {
  void loadEnvironment().finally(() => void restoreSession());
});

watch(
  () => settings.config.logDir,
  () => {
    void refreshLogDirWarning();
  },
);

watch(portText, (value) => {
  const port = checkPort(value).value;
  if (port !== null && port !== settings.config.listenPort) {
    settingsStore.updateConfig({ listenPort: port });
  }
});

watch(
  () => state.value.startInfo,
  (info) => {
    if (info !== null) lastJournalPath.value = info.journalPath;
  },
);

async function onStart(): Promise<void> {
  const port = checkPort(portText.value).value;
  if (port === null) return;
  settingsStore.updateConfig({ listenPort: port });
  lastJournalPath.value = null;
  visibleWindow.value = null;
  await start({ ...settings.config, listenPort: port });
}

async function onStop(): Promise<void> {
  await stop();
  if (state.value.summary !== null) lastJournalPath.value = state.value.summary.journalPath;
}

/** «Остановить» из баннера «сессия уже идёт»: лента могла остаться от прошлой страницы. */
async function onStopFromBanner(): Promise<void> {
  await stopAny();
  if (state.value.summary !== null) lastJournalPath.value = state.value.summary.journalPath;
}

/** Переход к доле списка из трассы (просмотр журнала). */
function onSeek(fraction: number): void {
  feedView.value?.seekFraction(fraction);
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

function cycleTheme(): void {
  settingsStore.setTheme(nextTheme(settings.theme));
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
    const journal = await openJournal(path);
    if (journal !== null) lastJournalPath.value = journal.path;
  } catch (error) {
    reportError("io", error instanceof Error ? error.message : String(error));
  }
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
  if (settingsStore.applyProfile(name)) {
    portText.value = String(settings.config.listenPort);
  }
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

/** «Дописать комментарий» в остановленный журнал (FR-23). */
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
    <AppHeader
      :theme="settings.theme"
      :journal-open="journalOpen"
      :busy="journalBlocked"
      @open-journal="onOpenJournal"
      @open-folder="onOpenFolder"
      @cycle-theme="cycleTheme"
    >
      <SessionChips v-if="state.startedConfig !== null" :config="state.startedConfig" />
    </AppHeader>

    <ErrorBanner
      v-if="state.error !== null"
      :error="state.error"
      :config="settings.config"
      @dismiss="dismissError"
      @stop="onStopFromBanner"
    />

    <div v-if="settingsStore.state.warning !== null" class="bt-ban warn" role="note">
      <TriangleAlert :size="16" aria-hidden="true" />
      <div>{{ settingsWarning }}</div>
    </div>

    <!-- Сессия пережила перезагрузку страницы: ленты нет, но «Стоп» завершит запись (AC1). -->
    <div v-if="orphanSession" class="bt-ban warn" role="note">
      <TriangleAlert :size="16" aria-hidden="true" />
      <div>{{ t("status.stopOrphan") }}</div>
    </div>

    <JournalFailureBanner
      v-if="state.journalFailure !== null"
      :code="state.journalFailure.code"
      :message="state.journalFailure.message"
      @choose-dir="onChooseDirFromFailure"
      @open-recorded="onOpenRecorded"
    />

    <!-- Просмотр журнала: полоса файла, предупреждение о ПДн и метаданные заголовка (FR-29, FR-34). -->
    <template v-if="journalOpen && journal !== null">
      <div class="journal-bar">
        <FileText :size="15" aria-hidden="true" />
        <span class="bt-mono name" :title="journal.path">{{ journalName }}</span>
        <span class="read-only">{{ t("journal.readOnly") }}</span>
        <span class="bt-spacer" />
        <button class="bt-btn" type="button" @click="openJournalNote">
          <PenLine :size="15" aria-hidden="true" />
          {{ t("journal.appendNote") }}
        </button>
        <button class="bt-btn ghost" type="button" :aria-label="t('journal.close')" @click="onCloseJournal">
          <X :size="15" aria-hidden="true" />
          {{ t("journal.close") }}
        </button>
      </div>

      <div class="bt-ban pd" role="note">
        <TriangleAlert :size="16" aria-hidden="true" />
        <div>{{ t("journal.personalData") }}</div>
      </div>

      <div class="meta">
        <span>
          {{ t("journal.metaAnalyzer") }}
          <b>{{ meta?.analyzer === "" ? t("journal.notSpecified") : meta?.analyzer }}</b>
        </span>
        <span>{{ t("journal.metaMode") }} <b>{{ journalMode }}</b></span>
        <span>
          {{ t("journal.metaStart") }}
          <b class="bt-mono">{{ journalStarted }}</b>
        </span>
        <span>
          {{ t("journal.metaDuration") }}
          <b class="bt-mono">{{ journalDurationText }}</b>
        </span>
        <span v-if="journalNotesText !== ''" class="notes">
          {{ t("journal.metaNote") }} <b>{{ journalNotesText }}</b>
        </span>
      </div>
    </template>

    <div v-if="!recording && !journalOpen" class="config-region">
      <ConfigForm
        :config="settings.config"
        :port-text="portText"
        :interfaces="interfaces"
        :port-error="portError"
        :log-dir-error="logDirError"
        :log-dir-warning="logDirWarning"
        @update:config="settingsStore.updateConfig"
        @update:port-text="portText = $event"
        @pick-log-dir="onPickLogDir"
      />
    </div>

    <ActionBar
      v-if="!journalOpen"
      :status="state.status"
      :elapsed-ms="elapsedMs"
      :profiles="settings.profiles"
      :selected-profile="selectedProfile"
      :can-start="canStart"
      :log-dir="settings.config.logDir"
      :summary="state.summary"
      @start="onStart"
      @stop="onStop"
      @select-profile="onSelectProfile"
      @save-profile="onSaveProfile"
      @delete-profile="onDeleteProfile"
    />

    <TraceView
      :status="state.status"
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
      @update:view="settingsStore.setFeedView"
      @update:encoding="settingsStore.setFeedEncoding"
      @update:visible="onVisible"
    />
    <StatusBar
      :status="state.status"
      :stats="state.stats"
      :dropped="state.dropped"
      :journal-path="journalPath"
      @note="openNote"
    />

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
/* Настройки не выталкивают ленту и строку состояния за пределы окна: при коротком окне они прокручиваются. */
.config-region {
  flex: 0 1 auto;
  min-height: 0;
  overflow-y: auto;
}

.journal-bar {
  display: flex;
  align-items: center;
  gap: var(--bt-space-2);
  flex-wrap: wrap;
  padding: 9px var(--bt-space-3);
  border-bottom: 1px solid var(--bt-border);
}

.name {
  font-weight: 500;
}

.read-only {
  padding: 1px 6px;
  border-radius: var(--bt-radius-chip);
  background: var(--bt-border-faint);
  color: var(--bt-text-muted);
  font-size: 10.5px;
}

/* Предупреждение о персональных данных (FR-34): всегда на экране журнала. */
.bt-ban.pd {
  background: var(--bt-warn-bg);
  color: var(--bt-text);
}

.bt-ban.pd > svg {
  color: var(--bt-warn);
}

/* Метаданные заголовка журнала: анализатор, режим, начало, длительность, комментарий. */
.meta {
  display: flex;
  flex-wrap: wrap;
  gap: 4px var(--bt-space-3);
  padding: 8px var(--bt-space-3);
  border-bottom: 1px solid var(--bt-border);
  color: var(--bt-text-muted);
  font-size: var(--bt-text-xs);
}

.meta b {
  color: var(--bt-text);
  font-weight: 500;
}

.meta .notes b {
  white-space: pre-wrap;
}
</style>
