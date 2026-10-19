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
- Журнал сессии `bytetap-session/1` (FR-19…FR-24): модель событий `session`, `open`, `data`, `close`, `error`, `note`, `session_end`; обратимое экранирование `text` по FR-22 (печатные ASCII как есть, `<` как `<x3C>`, именованные управляющие как `<STX>`, остальные байты как `<xHH>`) с обратной функцией и proptest; писатель отдельной задачей `tokio` с неограниченным каналом и групповой фиксацией (`write` → `flush` → `sync_data` на пачку); дописывание `note` в конец файла; читатель с номером строки и допуском обрезанной последней строки. Формат описан в `docs/reference/session-format.md` и JSON Schema `docs/public/schema/bytetap-session-1.schema.json`; решение о писателе — ADR 0001.
- `bytetap-cli verify <файл>` — проверка журнала по вшитой JSON Schema и статистика (события по типам, байты по направлениям, соединения), код выхода ≠ 0 при ошибке; `bytetap-cli note <файл> <текст>` дописывает комментарий инженера (FR-23).
- Синтетический журнал `tests/fixtures/journal/minimal.jsonl` (HL7/MLLP, `ANALYZER`/`LIS`/`PATIENT^TEST`) и тест устойчивости T-4: журнал остаётся валидным после аварийного убийства процесса (M1-AC16, M1-AC7).
