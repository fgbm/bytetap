import "@fontsource/ibm-plex-sans/cyrillic-400.css";
import "@fontsource/ibm-plex-sans/cyrillic-500.css";
import "@fontsource/ibm-plex-sans/cyrillic-600.css";
import "@fontsource/ibm-plex-sans/latin-400.css";
import "@fontsource/ibm-plex-sans/latin-500.css";
import "@fontsource/ibm-plex-sans/latin-600.css";
import "@fontsource/ibm-plex-mono/cyrillic-400.css";
import "@fontsource/ibm-plex-mono/cyrillic-500.css";
import "@fontsource/ibm-plex-mono/cyrillic-600.css";
import "@fontsource/ibm-plex-mono/latin-400.css";
import "@fontsource/ibm-plex-mono/latin-500.css";
import "@fontsource/ibm-plex-mono/latin-600.css";
import "../../design/tokens.css";
import "../../design/tokens-system.css";
import { createApp } from "vue";
import App from "./App.vue";
import { i18n } from "./i18n";
import { flushSettings } from "./settings/store";
import { applyTheme, cachedTheme } from "./theme";
import "./styles/app.css";

// Тема из кеша применяется синхронно до монтирования: окно не вспыхивает чужой темой,
// пока из `settings.json` читается настоящее значение. Встроенный скрипт в index.html
// запрещён CSP, поэтому кеш читается здесь.
applyTheme(cachedTheme() ?? "system");

// Перезагрузка страницы оставила бы сессию в ядре без интерфейса (AC1): в собранном
// приложении F5 и Ctrl+R отключены, а «Старт» и «Стоп» продолжают работать.
if (import.meta.env.PROD) {
  window.addEventListener("keydown", (event) => {
    const key = event.key.toLowerCase();
    const reload = key === "f5" || ((event.ctrlKey || event.metaKey) && key === "r");
    if (reload) event.preventDefault();
  });
}

// Отложенная запись настроек не должна потерять последние правки при закрытии окна.
window.addEventListener("beforeunload", () => {
  void flushSettings();
});

createApp(App).use(i18n).mount("#app");
