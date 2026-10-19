# bytetap

Все заметные изменения проекта. Формат — [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/), версии — [Semantic Versioning](https://semver.org/lang/ru/).

## [Unreleased]

### Добавлено

- Cargo workspace с crates `bytetap-core` (ядро без Tauri), `bytetap-cli` (консольная версия с `--version`) и `bytetap-testkit` (тестовые помощники), версия Rust закреплена в `rust-toolchain.toml`.
- Каркас приложения `app/`: Tauri v2 + Vue 3 + TypeScript + Vite, vue-i18n с единственной локалью ru, окно-заглушка с заголовком «bytetap», vitest с одним тестом.
- Сайт документации на VitePress в `docs/` (`base: /bytetap/`, локальный поиск, разделы `guide/`, `reference/`, `dev/`), битые ссылки ломают сборку.
- npm workspaces `app` и `docs` со скриптами `dev`, `build`, `check`, `docs:dev`, `docs:build`.
- GitHub Actions: `ci.yml` (fmt, clippy, test на Ubuntu и Windows, фронтенд, сайт, Windows exe в артефакт, actionlint), `docs.yml` (публикация сайта на GitHub Pages), `release.yml` (zip с `bytetap.exe`, README, LICENSE, `SHA256SUMS` и черновик релиза по тегу `v*`).
- Иконки-заглушки приложения для Windows-сборки.
