import { fileURLToPath, URL } from "node:url";
import vueI18n from "@intlify/unplugin-vue-i18n/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    vue(),
    // Сообщения предкомпилируются на сборке: CSP без `unsafe-eval`, vue-i18n — runtime-сборка.
    vueI18n({
      include: fileURLToPath(new URL("./src/locales/**/*.json", import.meta.url)),
      runtimeOnly: true,
    }),
  ],
  resolve: {
    alias: {
      "vue-i18n": "vue-i18n/dist/vue-i18n.runtime.esm-bundler.js",
    },
  },
  clearScreen: false,
  build: {
    // Самый старый WebView2, который пропускает проверка `MIN_VERSION` в `src-tauri/src/webview2.rs`:
    // менять вместе. Совпадает с Baseline Widely Available Vite 8.
    target: ["chrome111", "edge111"],
    // CSP `font-src 'self'` без `data:`: шрифты идут отдельными файлами, не data-URL.
    assetsInlineLimit: (filePath: string) => (filePath.endsWith(".woff") || filePath.endsWith(".woff2") ? false : undefined),
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  test: {
    environment: "jsdom",
  },
});
