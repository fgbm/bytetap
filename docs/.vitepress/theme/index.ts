import type { Theme } from "vitepress";
import DefaultTheme from "vitepress/theme";
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
import "../../../design/tokens.css";
import "./style.css";
import Layout from "./Layout.vue";
import ExchangeSample from "./components/ExchangeSample.vue";
import Screenshot from "./components/Screenshot.vue";
import SignalTrace from "./components/SignalTrace.vue";

/**
 * Тема сайта: DefaultTheme с токенами bytetap, компонентами примеров обмена и
 * снимками приложения в обеих темах.
 * Шрифты IBM Plex подключены локально из @fontsource — внешних запросов нет.
 */
export default {
  extends: DefaultTheme,
  Layout,
  enhanceApp({ app }) {
    app.component("ExchangeSample", ExchangeSample);
    app.component("Screenshot", Screenshot);
    app.component("SignalTrace", SignalTrace);
  },
} satisfies Theme;
