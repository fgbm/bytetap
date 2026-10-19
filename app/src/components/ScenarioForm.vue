<script setup lang="ts">
/**
 * Шаг сценария: форма одного из трёх режимов (UI v2).
 *
 * Поля минимальные: подпись 12px над полем, само поле 34px с тонкой рамкой,
 * служебное (интерфейс, переподключение, поток, DTR/RTS, анализатор, комментарий,
 * папка журналов, NAK, профили) — под свёрнутым «Дополнительно». Ошибка старта
 * показывается красным текстом 12px под своим полем, без баннеров.
 */
import { computed, reactive, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { ArrowLeft, ChevronDown, ChevronRight, Copy, Info, RefreshCw } from "@lucide/vue";
import type {
  InterfaceInfo,
  LogDirWarning,
  Responder,
  SerialPort,
  SessionMode,
} from "../api/types";
import {
  ALL_INTERFACES,
  BAUD_MAX,
  BAUD_MIN,
  BAUD_RATES,
  serialFormat,
  type AppConfig,
  type Profile,
} from "../settings/settings";
import type { ErrorField } from "../session/errors";
import { checkPort, PORT_PROBLEM_KEYS } from "../validation";

const props = defineProps<{
  config: AppConfig;
  interfaces: InterfaceInfo[];
  serialPorts: SerialPort[];
  logDirWarning: LogDirWarning | null;
  /** Поле, под которым мост просит показать ошибку старта. */
  errorField: ErrorField | null;
  errorText: string | null;
  profiles: Profile[];
  selectedProfile: string;
  /** Идёт старт или запись: кнопка занята. */
  busy: boolean;
}>();

const emit = defineEmits<{
  back: [];
  update: [patch: Partial<AppConfig>];
  "update-serial": [patch: Partial<AppConfig["serial"]>];
  start: [patch: Partial<AppConfig>];
  "pick-log-dir": [];
  "refresh-ports": [];
  "save-profile": [];
  "delete-profile": [];
  "apply-profile": [name: string];
}>();

const { t } = useI18n();

const mode = computed<SessionMode>(() => props.config.mode);

/** Заголовок шага — тот же, что у карточки сценария на стартовом экране. */
const titleKey = computed(() => {
  if (props.config.mode === "tcp_client") return "scenarios.clientTitle";
  if (props.config.mode === "serial") return "scenarios.serialTitle";
  return "scenarios.serverTitle";
});

const portText = ref(String(props.config.listenPort));
const connectHost = ref(props.config.connectHost);
const connectPortText = ref(String(props.config.connectPort));
const baudOther = ref(!BAUD_RATES.includes(props.config.serial.baud));
const baudText = ref(String(props.config.serial.baud));
const more = ref(false);
const copied = ref(false);
/** Нажали «Старт»: только после этого пустые поля краснеют. */
const attempted = ref(false);
/** Тронутые поля: своя проверка не кричит раньше времени (UI v2 — тишина). */
const touched = reactive({ port: false, address: false, serial: false });

// Профиль или последние настройки меняют конфигурацию снаружи: поля портов следуют за ней.
watch(
  () => props.config.listenPort,
  (value) => {
    if (checkPort(portText.value).value !== value) portText.value = String(value);
  },
);
watch(
  () => props.config.connectHost,
  (value) => {
    if (connectHost.value !== value) connectHost.value = value;
  },
);
watch(
  () => props.config.connectPort,
  (value) => {
    if (checkPort(connectPortText.value).value !== value) connectPortText.value = String(value);
  },
);
// Ошибка папки журналов живёт в «Дополнительно»: поле не должно быть скрыто от человека.
watch(
  () => props.errorField,
  (field) => {
    if (field === "logDir") more.value = true;
  },
  { immediate: true },
);
watch(
  () => props.config.serial.baud,
  (value) => {
    if (Number(baudText.value) !== value) baudText.value = String(value);
    baudOther.value = !BAUD_RATES.includes(value);
  },
);

const RESPONDERS: { value: Responder; key: string }[] = [
  { value: "auto", key: "form.responderAuto" },
  { value: "hl7", key: "form.responderHl7" },
  { value: "astm", key: "form.responderAstm" },
  { value: "none", key: "form.responderNone" },
];

const FORMATS = ["8N1", "8E1", "8O1", "8M1", "8S1", "8N2", "7E1", "7O1", "7E2"] as const;

const LETTER_PARITY: Record<string, AppConfig["serial"]["parity"]> = {
  N: "none",
  E: "even",
  O: "odd",
  M: "mark",
  S: "space",
};

const format = computed(() => serialFormat(props.config.serial));

const listenHost = computed(
  () => props.config.listenHost || props.interfaces[0]?.addr || "0.0.0.0",
);

/** Адрес, который вписывается в анализатор: адрес ноутбука и порт. */
const addressText = computed(() => `${listenHost.value}:${checkPort(portText.value).value ?? portText.value}`);

const otherInterface = computed(
  () => props.interfaces.find((iface) => iface.addr !== listenHost.value) ?? null,
);

const listenPortCheck = computed(() => checkPort(portText.value));
const connectPortCheck = computed(() => checkPort(connectPortText.value));
const baudValue = computed(() => {
  const value = Number(baudText.value);
  return Number.isInteger(value) && value >= BAUD_MIN && value <= BAUD_MAX ? value : null;
});

const addressMissing = computed(() => connectHost.value.trim() === "");
const serialPortMissing = computed(() => props.config.serial.port === "");

/** Порты из моста плюс сохранённый: пропавший порт не исчезает из списка сам. */
const serialOptions = computed<SerialPort[]>(() => {
  const list = [...props.serialPorts];
  const saved = props.config.serial.port;
  if (saved !== "" && !list.some((port) => port.name === saved)) {
    list.push({ name: saved, description: saved });
  }
  return list;
});

function localError(field: ErrorField): string | null {
  const shown = attempted.value || (field !== "logDir" && field !== "start" && touched[field]);
  if (!shown) return null;
  if (field === "port") {
    if (props.config.mode === "tcp_client") {
      return connectPortCheck.value.problem === null
        ? null
        : t(PORT_PROBLEM_KEYS[connectPortCheck.value.problem]);
    }
    return listenPortCheck.value.problem === null
      ? null
      : t(PORT_PROBLEM_KEYS[listenPortCheck.value.problem]);
  }
  if (field === "address") {
    return addressMissing.value ? t("form.addressRequired") : null;
  }
  if (field === "serial") {
    if (serialPortMissing.value) return t("form.serialRequired");
    if (baudOther.value && baudValue.value === null) return t("form.baudRange");
    return null;
  }
  return null;
}

/** Ошибка у поля: сначала своя проверка, затем ошибка моста по коду. */
function fieldError(field: ErrorField): string | null {
  const own = localError(field);
  if (own !== null) return own;
  return props.errorField === field ? props.errorText : null;
}

const startError = computed(() =>
  props.errorField === "start" || props.errorField === null ? props.errorText : null,
);

const valid = computed(() => {
  if (props.config.mode === "tcp_client") {
    return (
      listenPortCheck.value.problem === null &&
      connectPortCheck.value.problem === null &&
      !addressMissing.value
    );
  }
  if (props.config.mode === "serial") {
    // Порт TCP-сервера в этом режиме не нужен: его поле даже не показано.
    return !serialPortMissing.value && baudValue.value !== null;
  }
  return listenPortCheck.value.problem === null;
});

function onPortInput(event: Event): void {
  touched.port = true;
  portText.value = (event.target as HTMLInputElement).value;
  const port = checkPort(portText.value).value;
  if (port !== null && port !== props.config.listenPort) emit("update", { listenPort: port });
}

function onConnectPortInput(event: Event): void {
  touched.port = true;
  connectPortText.value = (event.target as HTMLInputElement).value;
  const port = checkPort(connectPortText.value).value;
  if (port !== null && port !== props.config.connectPort) emit("update", { connectPort: port });
}

function onResponder(event: Event): void {
  emit("update", { responder: (event.target as HTMLSelectElement).value as Responder });
}

function onFormat(event: Event): void {
  const value = (event.target as HTMLSelectElement).value;
  const match = /^([78])([NEOMS])([12])$/i.exec(value);
  if (match === null) return;
  emit("update-serial", {
    dataBits: match[1] === "7" ? 7 : 8,
    parity: LETTER_PARITY[match[2].toUpperCase()] ?? "none",
    stopBits: match[3] === "2" ? 2 : 1,
  });
}

function onBaud(event: Event): void {
  touched.serial = true;
  const value = (event.target as HTMLSelectElement).value;
  if (value === "other") {
    baudOther.value = true;
    return;
  }
  baudOther.value = false;
  baudText.value = value;
  emit("update-serial", { baud: Number(value) });
}

function onBaudInput(event: Event): void {
  touched.serial = true;
  baudText.value = (event.target as HTMLInputElement).value;
  if (baudValue.value !== null) emit("update-serial", { baud: baudValue.value });
}

async function copyAddress(): Promise<void> {
  try {
    await navigator.clipboard?.writeText(addressText.value);
    copied.value = true;
    setTimeout(() => {
      copied.value = false;
    }, 1500);
  } catch {
    copied.value = false;
  }
}

/** Старт: поля портов уходят в настройки, дальше — конфигурация выбранного режима. */
function onStart(): void {
  attempted.value = true;
  if (!valid.value || props.busy) return;
  const patch: Partial<AppConfig> = { connectHost: connectHost.value.trim() };
  const port = listenPortCheck.value.value;
  const connectPort = connectPortCheck.value.value;
  if (port !== null) patch.listenPort = port;
  if (connectPort !== null) patch.connectPort = connectPort;
  if (baudValue.value !== null) patch.serial = { ...props.config.serial, baud: baudValue.value };
  emit("start", patch);
}

function onProfile(event: Event): void {
  emit("apply-profile", (event.target as HTMLSelectElement).value);
}
</script>

<template>
  <div class="bt-screen">
    <section class="bt-form-screen">
      <button class="bt-link bt-back" type="button" @click="emit('back')">
        <ArrowLeft :size="15" aria-hidden="true" />
        {{ t("form.back") }}
      </button>
      <h1 class="bt-form-title">{{ t(titleKey) }}</h1>

      <div class="bt-fields">
        <!-- (а) Анализатор подключается ко мне: TCP-сервер -->
        <template v-if="mode === 'tcp_server'">
          <div class="bt-field">
            <label for="bt-port">{{ t("form.port") }}</label>
            <input
              id="bt-port"
              class="bt-input bt-mono"
              :class="{ err: fieldError('port') !== null }"
              inputmode="numeric"
              autocomplete="off"
              :value="portText"
              :aria-invalid="fieldError('port') !== null"
              @input="onPortInput"
            />
            <span v-if="fieldError('port')" class="bt-ferr">{{ fieldError("port") }}</span>
          </div>
          <div class="bt-field wide">
            <label for="bt-responder">{{ t("form.responder") }}</label>
            <span class="bt-select">
              <select id="bt-responder" class="bt-input" :value="config.responder" @change="onResponder">
                <option v-for="item in RESPONDERS" :key="item.value" :value="item.value">
                  {{ t(item.key) }}
                </option>
              </select>
              <ChevronDown :size="14" aria-hidden="true" />
            </span>
          </div>

          <div class="bt-field wide">
            <div class="bt-address">
              <b>{{ addressText }}</b>
              <button class="bt-chip-btn" type="button" @click="copyAddress">
                <Copy :size="13" aria-hidden="true" />
                {{ copied ? t("form.copied") : t("form.copy") }}
              </button>
            </div>
            <span v-if="otherInterface !== null" class="bt-hintline">
              {{ t("form.otherNetwork", { addr: otherInterface.addr, name: otherInterface.name }) }}
            </span>
            <span v-else-if="interfaces.length === 0" class="bt-hintline">{{ t("form.noInterfaces") }}</span>
            <span class="bt-hintline">
              {{ t("form.firewall") }}
              <Info
                class="bt-info"
                :size="13"
                role="img"
                :title="t('form.firewallHint')"
                :aria-label="t('form.firewallHint')"
              />
            </span>
          </div>
        </template>

        <!-- (б) Я подключаюсь к анализатору: TCP-клиент -->
        <template v-else-if="mode === 'tcp_client'">
          <div class="bt-field wide">
            <label for="bt-host">{{ t("form.connectAddress") }}</label>
            <input
              id="bt-host"
              class="bt-input bt-mono"
              :class="{ err: fieldError('address') !== null }"
              autocomplete="off"
              :value="connectHost"
              :placeholder="t('form.connectPlaceholder')"
              :aria-invalid="fieldError('address') !== null"
              @input="touched.address = true; connectHost = ($event.target as HTMLInputElement).value"
            />
            <span v-if="fieldError('address')" class="bt-ferr">{{ fieldError("address") }}</span>
          </div>
          <div class="bt-field">
            <label for="bt-cport">{{ t("form.port") }}</label>
            <input
              id="bt-cport"
              class="bt-input bt-mono"
              :class="{ err: fieldError('port') !== null }"
              inputmode="numeric"
              autocomplete="off"
              :value="connectPortText"
              :aria-invalid="fieldError('port') !== null"
              @input="onConnectPortInput"
            />
            <span v-if="fieldError('port')" class="bt-ferr">{{ fieldError("port") }}</span>
          </div>
          <div class="bt-field wide">
            <label for="bt-responder-client">{{ t("form.responder") }}</label>
            <span class="bt-select">
              <select
                id="bt-responder-client"
                class="bt-input"
                :value="config.responder"
                @change="onResponder"
              >
                <option v-for="item in RESPONDERS" :key="item.value" :value="item.value">
                  {{ t(item.key) }}
                </option>
              </select>
              <ChevronDown :size="14" aria-hidden="true" />
            </span>
          </div>
        </template>

        <!-- (в) Кабель COM-порта -->
        <template v-else>
          <div class="bt-field wide">
            <label for="bt-serial">{{ t("form.serialPort") }}</label>
            <div class="bt-row">
              <span class="bt-select">
                <select
                  id="bt-serial"
                  class="bt-input"
                  :class="{ err: fieldError('serial') !== null }"
                  :value="config.serial.port"
                  :aria-invalid="fieldError('serial') !== null"
                  @change="touched.serial = true; emit('update-serial', { port: ($event.target as HTMLSelectElement).value })"
                >
                  <option value="">{{ t("form.serialChoose") }}</option>
                  <option v-for="port in serialOptions" :key="port.name" :value="port.name">
                    {{ port.description }} ({{ port.name }})
                  </option>
                </select>
                <ChevronDown :size="14" aria-hidden="true" />
              </span>
              <button
                class="bt-icon"
                type="button"
                :title="t('form.refreshPorts')"
                :aria-label="t('form.refreshPorts')"
                @click="emit('refresh-ports')"
              >
                <RefreshCw :size="15" aria-hidden="true" />
              </button>
            </div>
            <span v-if="fieldError('serial')" class="bt-ferr">{{ fieldError("serial") }}</span>
          </div>
          <div class="bt-field">
            <label for="bt-baud">{{ t("form.baud") }}</label>
            <span v-if="!baudOther" class="bt-select">
              <select id="bt-baud" class="bt-input bt-mono" :value="String(config.serial.baud)" @change="onBaud">
                <option v-for="rate in BAUD_RATES" :key="rate" :value="String(rate)">{{ rate }}</option>
                <option value="other">{{ t("form.baudOther") }}</option>
              </select>
              <ChevronDown :size="14" aria-hidden="true" />
            </span>
            <input
              v-else
              id="bt-baud"
              class="bt-input bt-mono"
              inputmode="numeric"
              :value="baudText"
              :aria-invalid="fieldError('serial') !== null"
              @input="onBaudInput"
            />
          </div>
          <div class="bt-field">
            <label for="bt-format">{{ t("form.format") }}</label>
            <span class="bt-select">
              <select id="bt-format" class="bt-input bt-mono" :value="format" @change="onFormat">
                <option v-for="value in FORMATS" :key="value" :value="value">{{ value }}</option>
              </select>
              <ChevronDown :size="14" aria-hidden="true" />
            </span>
          </div>
          <div class="bt-field wide">
            <label for="bt-responder-serial">{{ t("form.responder") }}</label>
            <span class="bt-select">
              <select
                id="bt-responder-serial"
                class="bt-input"
                :value="config.responder"
                @change="onResponder"
              >
                <option v-for="item in RESPONDERS" :key="item.value" :value="item.value">
                  {{ t(item.key) }}
                </option>
              </select>
              <ChevronDown :size="14" aria-hidden="true" />
            </span>
          </div>
        </template>
      </div>

      <div class="bt-more">
        <button class="bt-more-toggle" type="button" :aria-expanded="more" @click="more = !more">
          <ChevronDown v-if="more" :size="14" aria-hidden="true" />
          <ChevronRight v-else :size="14" aria-hidden="true" />
          {{ t("form.more") }}
        </button>

        <div v-if="more" class="bt-more-body">
          <template v-if="mode === 'tcp_server'">
            <div class="bt-field wide">
              <label for="bt-iface">{{ t("form.listenOn") }}</label>
              <span class="bt-select">
                <select
                  id="bt-iface"
                  class="bt-input"
                  :value="config.listenHost"
                  @change="emit('update', { listenHost: ($event.target as HTMLSelectElement).value })"
                >
                  <option :value="ALL_INTERFACES">{{ t("form.allInterfaces") }}</option>
                  <option v-for="iface in interfaces" :key="iface.addr" :value="iface.addr">
                    {{ iface.addr }} — {{ iface.name }}
                  </option>
                </select>
                <ChevronDown :size="14" aria-hidden="true" />
              </span>
            </div>
          </template>

          <template v-if="mode === 'tcp_client'">
            <div class="bt-field">
              <label for="bt-reconnect">{{ t("form.reconnect") }}</label>
              <input
                id="bt-reconnect"
                class="bt-input bt-mono"
                inputmode="numeric"
                :value="config.reconnectSec"
                @input="emit('update', { reconnectSec: Number(($event.target as HTMLInputElement).value) || 0 })"
              />
            </div>
          </template>

          <template v-if="mode === 'serial'">
            <div class="bt-field">
              <label for="bt-flow">{{ t("form.flow") }}</label>
              <span class="bt-select">
                <select
                  id="bt-flow"
                  class="bt-input"
                  :value="config.serial.flow"
                  @change="emit('update-serial', { flow: ($event.target as HTMLSelectElement).value as AppConfig['serial']['flow'] })"
                >
                  <option value="none">{{ t("form.flowNone") }}</option>
                  <option value="rtscts">{{ t("form.flowRtscts") }}</option>
                  <option value="xonxoff">{{ t("form.flowXonxoff") }}</option>
                </select>
                <ChevronDown :size="14" aria-hidden="true" />
              </span>
            </div>
            <div class="bt-field">
              <label class="bt-check">
                <input
                  type="checkbox"
                  :checked="config.serial.dtr"
                  @change="emit('update-serial', { dtr: ($event.target as HTMLInputElement).checked })"
                />
                {{ t("form.dtr") }}
              </label>
              <label class="bt-check">
                <input
                  type="checkbox"
                  :checked="config.serial.rts"
                  @change="emit('update-serial', { rts: ($event.target as HTMLInputElement).checked })"
                />
                {{ t("form.rts") }}
              </label>
            </div>
          </template>

          <div class="bt-field">
            <label for="bt-analyzer">{{ t("form.analyzer") }}</label>
            <input
              id="bt-analyzer"
              class="bt-input"
              :value="config.analyzer"
              :placeholder="t('form.analyzerPlaceholder')"
              @input="emit('update', { analyzer: ($event.target as HTMLInputElement).value })"
            />
          </div>
          <div class="bt-field">
            <label for="bt-note">{{ t("form.note") }}</label>
            <input
              id="bt-note"
              class="bt-input"
              :value="config.note"
              :placeholder="t('form.notePlaceholder')"
              @input="emit('update', { note: ($event.target as HTMLInputElement).value })"
            />
          </div>

          <div class="bt-field wide">
            <label for="bt-log-dir">{{ t("form.logDir") }}</label>
            <div class="bt-row">
              <input
                id="bt-log-dir"
                class="bt-input bt-mono"
                readonly
                :value="config.logDir"
                :class="{ err: fieldError('logDir') !== null }"
                :aria-invalid="fieldError('logDir') !== null"
              />
              <button class="bt-chip-btn" type="button" @click="emit('pick-log-dir')">
                {{ t("form.choose") }}
              </button>
            </div>
            <span v-if="fieldError('logDir')" class="bt-ferr">{{ fieldError("logDir") }}</span>
            <span v-else-if="logDirWarning !== null" class="bt-ferr soft">
              {{ t(`form.logDirWarning${logDirWarning.code === "onedrive" ? "Onedrive" : "Network"}`) }}
            </span>
          </div>

          <div v-if="config.responder === 'auto' || config.responder === 'astm'" class="bt-field wide">
            <label class="bt-check">
              <input
                type="checkbox"
                :checked="config.astmNakOnBadChecksum"
                @change="emit('update', { astmNakOnBadChecksum: ($event.target as HTMLInputElement).checked })"
              />
              {{ t("form.nak") }}
            </label>
          </div>

          <div class="bt-field wide">
            <label for="bt-profile">{{ t("form.profile") }}</label>
            <div class="bt-row">
              <span class="bt-select">
                <select id="bt-profile" class="bt-input" :value="selectedProfile" @change="onProfile">
                  <option value="">{{ t("form.profileLast") }}</option>
                  <option v-for="profile in profiles" :key="profile.name" :value="profile.name">
                    {{ profile.name }}
                  </option>
                </select>
                <ChevronDown :size="14" aria-hidden="true" />
              </span>
              <button class="bt-chip-btn" type="button" @click="emit('save-profile')">
                {{ t("form.profileSave") }}
              </button>
              <button
                class="bt-chip-btn"
                type="button"
                :disabled="selectedProfile === ''"
                @click="emit('delete-profile')"
              >
                {{ t("form.profileDelete") }}
              </button>
            </div>
          </div>
        </div>
      </div>

      <button class="bt-start" type="button" :disabled="busy" @click="onStart">
        <span class="bt-dot" aria-hidden="true" />
        {{ busy ? t("form.starting") : t("form.start") }}
      </button>
      <span v-if="startError" class="bt-ferr start-error">{{ startError }}</span>
    </section>
  </div>
</template>

<style scoped>
.bt-row {
  display: flex;
  gap: var(--bt-space-2);
  align-items: center;
}

.bt-row > .bt-input {
  flex: 1;
  min-width: 0;
}

.bt-check + .bt-check {
  margin-top: 6px;
}

.soft {
  color: var(--bt-warn);
}

.start-error {
  display: block;
  margin-top: var(--bt-space-2);
  text-align: center;
}
</style>
