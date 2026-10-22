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
            { text: "Первый запуск", link: "/guide/first-run" },
            { text: "Режимы работы", link: "/guide/modes" },
            { text: "Запуск из командной строки", link: "/guide/cli" },
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
                {
                  text: "M1 — ядро: TCP-сервер, журнал, автоответчик HL7/MLLP",
                  link: "/dev/stages/M1",
                },
                {
                  text: "M2 — интерфейс: настройки, старт/стоп, живая лента",
                  link: "/dev/stages/M2",
                },
                {
                  text: "M3 — TCP-клиент, ASTM, «Авто», COM-порт",
                  link: "/dev/stages/M3",
                },
                {
                  text: "U1 — нативный интерфейс (egui)",
                  link: "/dev/stages/U1",
                },
                {
                  text: "M4 — прокси TCP",
                  link: "/dev/stages/M4",
                },
              ],
            },
            { text: "Бэклог", link: "/dev/backlog" },
            { text: "Архитектура", link: "/dev/architecture" },
            { text: "Дизайн-система", link: "/dev/design" },
            { text: "Тестирование", link: "/dev/testing" },
            { text: "Релизы", link: "/dev/releases" },
            { text: "ADR", link: "/dev/adr/" },
          ],
        },
      ],
    },
    search: {
      provider: "local",
      options: {
        translations: {
          button: {
            buttonText: "Поиск",
            buttonAriaLabel: "Поиск по документации",
          },
          modal: {
            displayDetails: "Показать подробный список",
            resetButtonTitle: "Сбросить поиск",
            backButtonTitle: "Закрыть поиск",
            noResultsText: "Ничего не найдено по запросу",
            footer: {
              selectText: "выбрать",
              selectKeyAriaLabel: "Enter",
              navigateText: "перейти",
              navigateUpKeyAriaLabel: "Стрелка вверх",
              navigateDownKeyAriaLabel: "Стрелка вниз",
              closeText: "закрыть",
              closeKeyAriaLabel: "Esc",
            },
          },
        },
      },
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
    notFound: {
      title: "Страница не найдена",
      quote: "Такой страницы нет или она переехала. Проверьте адрес или вернитесь на главную.",
      linkLabel: "перейти на главную",
      linkText: "главная",
    },
    returnToTopLabel: "Наверх",
    skipToContentLabel: "Перейти к содержимому",
    sidebarMenuLabel: "Меню",
    darkModeSwitchLabel: "Тема",
    lightModeSwitchTitle: "Включить светлую тему",
    darkModeSwitchTitle: "Включить тёмную тему",
    langMenuLabel: "Сменить язык",
    footer: {
      message: "Лицензия MIT",
      copyright: "bytetap",
    },
  },
});
