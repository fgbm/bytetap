import { defineConfig } from "vitepress";

/**
 * Сайт документации bytetap (DOC-1…DOC-4).
 *
 * Битые внутренние ссылки ломают сборку: `ignoreDeadLinks` намеренно не включён.
 */
export default defineConfig({
  lang: "ru-RU",
  title: "bytetap",
  description:
    "Запись «сырого» обмена между лабораторным анализатором и ЛИС: HL7/MLLP, ASTM и собственные протоколы производителей",
  base: "/bytetap/",
  cleanUrls: true,
  lastUpdated: true,
  themeConfig: {
    nav: [
      { text: "Руководство", link: "/guide/", activeMatch: "/guide/" },
      { text: "Справочник", link: "/reference/", activeMatch: "/reference/" },
      { text: "Разработка", link: "/dev/", activeMatch: "/dev/" },
    ],
    sidebar: {
      "/guide/": [
        {
          text: "Руководство",
          items: [
            { text: "Введение", link: "/guide/" },
            { text: "Установка", link: "/guide/install" },
            { text: "SmartScreen и антивирусы", link: "/guide/smartscreen" },
            { text: "WebView2", link: "/guide/webview2" },
            { text: "Первый запуск", link: "/guide/first-run" },
            { text: "Режимы работы", link: "/guide/modes" },
            { text: "Брандмауэр", link: "/guide/firewall" },
            { text: "Персональные данные", link: "/guide/personal-data" },
            { text: "Проблемы", link: "/guide/troubleshooting" },
          ],
        },
      ],
      "/reference/": [
        {
          text: "Справочник",
          items: [
            { text: "Введение", link: "/reference/" },
            { text: "Формат журнала сессии", link: "/reference/session-format" },
            { text: "Автоответчик", link: "/reference/autoresponder" },
          ],
        },
      ],
      "/dev/": [
        {
          text: "Разработка",
          items: [
            { text: "Введение", link: "/dev/" },
            { text: "Требования", link: "/dev/requirements" },
            { text: "Дорожная карта", link: "/dev/roadmap" },
            {
              text: "Этапы",
              items: [
                { text: "Список этапов", link: "/dev/stages/" },
                { text: "M0 — каркас", link: "/dev/stages/M0" },
              ],
            },
            { text: "Архитектура", link: "/dev/architecture" },
            { text: "Тестирование", link: "/dev/testing" },
            { text: "Релизы", link: "/dev/releases" },
            { text: "ADR", link: "/dev/adr/" },
          ],
        },
      ],
    },
    search: {
      provider: "local",
    },
    outline: {
      level: [2, 3],
      label: "На этой странице",
    },
    docFooter: {
      prev: "Назад",
      next: "Вперёд",
    },
    lastUpdated: {
      text: "Обновлено",
    },
    returnToTopLabel: "Наверх",
    sidebarMenuLabel: "Меню",
    darkModeSwitchLabel: "Тема",
    footer: {
      message: "Лицензия MIT",
      copyright: "bytetap",
    },
  },
});
