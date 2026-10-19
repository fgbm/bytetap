/// <reference types="vite/client" />

/** `VITE_BRIDGE` — `mock` или `tauri`; без переменной мост выбирается по окружению. */
interface ImportMetaEnv {
  readonly VITE_BRIDGE?: "mock" | "tauri";
}
