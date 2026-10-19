import { createI18n } from "vue-i18n";
import ru from "./locales/ru";

/** Единственная локаль каркаса; другие локали добавляются в `messages` без правок компонентов. */
export const i18n = createI18n({
  legacy: false,
  locale: "ru",
  fallbackLocale: "ru",
  messages: { ru },
});
