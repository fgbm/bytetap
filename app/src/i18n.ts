import { createI18n } from "vue-i18n";
import ru from "./locales/ru";

/**
 * Единственная локаль каркаса; другие локали добавляются в `messages` без правок компонентов.
 *
 * Форматы чисел и дат тоже живут в локали: интерфейс не собирает их вручную, а просит
 * `n()` и `d()`, чтобы запятая, пробелы разрядов и порядок даты менялись вместе с языком.
 */
export const i18n = createI18n({
  legacy: false,
  locale: "ru",
  fallbackLocale: "ru",
  messages: { ru },
  numberFormats: {
    ru: {
      integer: { style: "decimal", maximumFractionDigits: 0, useGrouping: true },
      oneDecimal: {
        style: "decimal",
        minimumFractionDigits: 1,
        maximumFractionDigits: 1,
        useGrouping: true,
      },
    },
  },
  datetimeFormats: {
    ru: {
      journal: {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      },
    },
  },
});
