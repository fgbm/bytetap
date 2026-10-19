<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { FileText, X } from "@lucide/vue";
import { bridge } from "./api/bridge";
import type { InterfaceInfo } from "./api/types";
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
import { checkPort, PORT_PROBLEM_KEYS } from "./validation";
import { nextTheme } from "./theme";
import { useSettings } from "./settings/store";
import { useSession } from "./session/store";
import { ERROR_KEYS, errorParams, LOG_DIR_ERROR_CODES, PORT_ERROR_CODES } from "./session/errors";

const { t } = useI18n();

const {
  state,
  events,
  elapsedMs,
  start,
  stop,
  addNote,
  openJournal,
  closeJournal,
  dismissError,
  dismissFailure,
  reportError,
} = useSession();
const settingsStore = useSettings();
const settings = settingsStore.state;

const interfaces = ref<InterfaceInfo[]>([]);
const logDirWarning = ref<string | null>(null);
const portText = ref(String(settings.config.listenPort));
const selectedProfile = ref("");
const lastJournalPath = ref<string | null>(null);
const prompt = reactive<{ kind: "note" | "profile" | null; value: string }>({
  kind: null,
  value: "",
});

const recording = computed(() => state.value.status === "recording");
const journalOpen = computed(
  () => state.value.status === "viewing" || state.value.status === "starting",
);
const canStart = computed(
  () =>
    checkPort(portText.value).problem === null &&
    settings.config.logDir.trim() !== "" &&
    state.value.status !== "recording" &&
    state.value.status !== "starting" &&
    state.value.status !== "stopping",
);

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

const promptTitle = computed(() =>
  prompt.kind === "note" ? t("actions.noteTitle") : t("actions.profileName"),
);
const promptLabel = computed(() =>
  prompt.kind === "note" ? t("config.note") : t("actions.profileName"),
);
const promptPlaceholder = computed(() =>
  prompt.kind === "note" ? t("actions.notePlaceholder") : t("actions.profileNamePlaceholder"),
);
const promptConfirm = computed(() =>
  prompt.kind === "note" ? t("actions.noteAdd") : t("actions.save"),
);

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

onMounted(() => {
  void loadEnvironment();
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
  await start({ ...settings.config, listenPort: port });
}

async function onStop(): Promise<void> {
  await stop();
  if (state.value.summary !== null) lastJournalPath.value = state.value.summary.journalPath;
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

function onPromptConfirm(text: string): void {
  if (prompt.kind === "note") {
    void addNote(text);
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
      @open-journal="onOpenJournal"
      @open-folder="onOpenFolder"
      @cycle-theme="cycleTheme"
    >
      <SessionChips v-if="state.startInfo !== null" :config="settings.config" />
    </AppHeader>

    <ErrorBanner
      v-if="state.error !== null"
      :error="state.error"
      :config="settings.config"
      @dismiss="dismissError"
    />

    <JournalFailureBanner
      v-if="state.journalFailure !== null"
      :message="state.journalFailure.message"
      @choose-dir="onChooseDirFromFailure"
      @open-recorded="onOpenRecorded"
    />

    <div v-if="!recording" class="config-region">
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

    <div v-if="state.status === 'viewing' && state.journal !== null" class="journal-bar">
      <FileText :size="15" aria-hidden="true" />
      <span class="bt-mono name">{{ state.journal.path }}</span>
      <span class="read-only">{{ t("journal.readOnly") }}</span>
      <span class="bt-spacer" />
      <button class="bt-btn" type="button" @click="closeJournal">
        <X :size="15" aria-hidden="true" />
        {{ t("journal.close") }}
      </button>
    </div>

    <ActionBar
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

    <TraceView :status="state.status" />
    <FeedView :events="events" :status="state.status" :journal="state.journal" />
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
      :multiline="prompt.kind === 'note'"
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
</style>
