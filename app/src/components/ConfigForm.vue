<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  CircleAlert,
  Copy,
  FolderOpen,
  Info,
  Network,
  Server,
  Usb,
} from "@lucide/vue";
import type { InterfaceInfo, LogDirWarning, Responder, SessionConfig } from "../api/types";
import { ALL_INTERFACES } from "../settings/settings";

const props = defineProps<{
  config: SessionConfig;
  portText: string;
  interfaces: InterfaceInfo[];
  portError: string | null;
  logDirError: string | null;
  logDirWarning: LogDirWarning | null;
}>();

const emit = defineEmits<{
  "update:config": [patch: Partial<SessionConfig>];
  "update:portText": [value: string];
  "pick-log-dir": [];
}>();

const { t } = useI18n();

const copiedAddr = ref("");

function update(patch: Partial<SessionConfig>): void {
  emit("update:config", patch);
}

function setResponder(responder: Responder): void {
  update({ responder });
}

function onPortInput(event: Event): void {
  emit("update:portText", (event.target as HTMLInputElement).value);
}

function onListenHost(event: Event): void {
  update({ listenHost: (event.target as HTMLSelectElement).value });
}

async function copyAddress(addr: string): Promise<void> {
  try {
    await navigator.clipboard?.writeText(addr);
    copiedAddr.value = addr;
    setTimeout(() => {
      if (copiedAddr.value === addr) copiedAddr.value = "";
    }, 1500);
  } catch {
    copiedAddr.value = "";
  }
}

/** Предупреждение переводится по коду из моста, а путь подставляется параметром (FR-19). */
const logDirWarningText = computed(() => {
  const warning = props.logDirWarning;
  if (warning === null) return null;
  const keys = {
    onedrive: "config.logDirWarningOnedrive",
    network: "config.logDirWarningNetwork",
  } as const;
  return t(keys[warning.code], warning.params);
});
</script>

<template>
  <section class="bt-grid">
    <div class="bt-field wide">
      <label>{{ t("config.mode") }}</label>
      <div class="bt-seg">
        <button type="button" class="on" :aria-pressed="true">
          <Server :size="13" aria-hidden="true" />
          {{ t("config.modeServer") }}
        </button>
        <button type="button" disabled>
          <Network :size="13" aria-hidden="true" />
          {{ t("config.modeClient") }}
          <small>{{ t("config.tagM3") }}</small>
        </button>
        <button type="button" disabled>
          <Usb :size="13" aria-hidden="true" />
          {{ t("config.modeCom") }}
          <small>{{ t("config.tagM3") }}</small>
        </button>
        <button type="button" disabled>
          {{ t("config.modeProxy") }}
          <small>{{ t("config.tagV02") }}</small>
        </button>
        <button type="button" disabled>
          {{ t("config.modeComSniff") }}
          <small>{{ t("config.tagV03") }}</small>
        </button>
      </div>
    </div>

    <div class="bt-field">
      <label for="bt-listen">{{ t("config.listenOn") }}</label>
      <div class="bt-row">
        <select id="bt-listen" class="bt-input" :value="config.listenHost" @change="onListenHost">
          <option :value="ALL_INTERFACES">{{ t("config.allInterfaces") }}</option>
          <option v-for="iface in interfaces" :key="iface.addr" :value="iface.addr">
            {{ iface.addr }} — {{ iface.name }}
          </option>
        </select>
        <input
          class="bt-input bt-mono port"
          :class="{ err: portError !== null }"
          inputmode="numeric"
          autocomplete="off"
          :value="portText"
          :aria-label="t('config.port')"
          :aria-invalid="portError !== null"
          @input="onPortInput"
        />
      </div>
      <span v-if="portError" class="bt-ferr">
        <CircleAlert :size="13" aria-hidden="true" />
        {{ portError }}
      </span>
    </div>

    <div class="bt-field">
      <label>{{ t("config.responder") }}</label>
      <div class="bt-seg">
        <button type="button" disabled>
          {{ t("config.responderAuto") }}
          <small>{{ t("config.tagM3") }}</small>
        </button>
        <button
          type="button"
          :class="{ on: config.responder === 'hl7' }"
          :aria-pressed="config.responder === 'hl7'"
          @click="setResponder('hl7')"
        >
          {{ t("config.responderHl7") }}
        </button>
        <button type="button" disabled>
          {{ t("config.responderAstm") }}
          <small>{{ t("config.tagM3") }}</small>
        </button>
        <button
          type="button"
          :class="{ on: config.responder === 'none' }"
          :aria-pressed="config.responder === 'none'"
          @click="setResponder('none')"
        >
          {{ t("config.responderNone") }}
        </button>
      </div>
    </div>

    <div class="bt-field">
      <label for="bt-analyzer">{{ t("config.analyzer") }}</label>
      <input
        id="bt-analyzer"
        class="bt-input"
        :value="config.analyzer"
        :placeholder="t('config.analyzerPlaceholder')"
        @input="update({ analyzer: ($event.target as HTMLInputElement).value })"
      />
    </div>

    <div class="bt-field">
      <label for="bt-note">{{ t("config.note") }}</label>
      <input
        id="bt-note"
        class="bt-input"
        :value="config.note"
        :placeholder="t('config.notePlaceholder')"
        @input="update({ note: ($event.target as HTMLInputElement).value })"
      />
    </div>

    <div class="bt-field wide">
      <label for="bt-log-dir">{{ t("config.logDir") }}</label>
      <div class="bt-row">
        <input
          id="bt-log-dir"
          class="bt-input bt-mono"
          readonly
          :value="config.logDir"
          :aria-invalid="logDirError !== null"
          :class="{ err: logDirError !== null }"
        />
        <button class="bt-btn" type="button" @click="emit('pick-log-dir')">
          <FolderOpen :size="15" aria-hidden="true" />
          {{ t("config.logDirChoose") }}
        </button>
      </div>
      <span v-if="logDirError" class="bt-ferr">
        <CircleAlert :size="13" aria-hidden="true" />
        {{ logDirError }}
      </span>
      <span v-else-if="logDirWarningText" class="log-warning">
        <CircleAlert :size="13" aria-hidden="true" />
        {{ logDirWarningText }}
      </span>
    </div>

    <div class="bt-field wide">
      <div class="bt-hint">
        <Info :size="15" aria-hidden="true" />
        <div>
          {{ t("config.hint", { port: portText }) }}
          <br />
          <template v-if="interfaces.length > 0">
            <span v-for="iface in interfaces" :key="iface.addr" class="addr-pair">
              <span class="bt-addr">
                {{ iface.addr }}
                <button
                  type="button"
                  :title="t('config.copyAddress', { addr: iface.addr })"
                  :aria-label="t('config.copyAddress', { addr: iface.addr })"
                  @click="copyAddress(iface.addr)"
                >
                  <Copy :size="12" aria-hidden="true" />
                </button>
              </span>
              <span class="bt-muted">{{ copiedAddr === iface.addr ? t("config.copied") : iface.name }}</span>
            </span>
          </template>
          <span v-else class="bt-muted">{{ t("config.noInterfaces") }}</span>
          <br />
          <span class="bt-muted">{{ t("config.firewall") }}</span>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.port {
  width: 76px;
  flex: none;
}

.addr-pair {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-right: 10px;
}

.log-warning {
  display: flex;
  gap: 5px;
  align-items: flex-start;
  color: var(--bt-warn);
  font-size: var(--bt-text-xs);
}

.log-warning > svg {
  flex: none;
  margin-top: 2px;
}
</style>
