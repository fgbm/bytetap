## 2026-10-19 20:28 — claude-opus-5-5

Рендер HTML-макета в чате → страница пустая: глобальная `const top` в скрипте конфликтует с неконфигурируемым `window.top`. Оборачивать скрипты макетов в IIFE и не называть глобальные переменные `top`, `status`, `name`, `parent`, `self`.

## 2026-10-19 21:05 — deepseek-v4.1-flash

Смоук vite: `pkill -f vite` убивает собственный shell агента (совпадение по командной строке) → убивать по PID из `$!` или `pkill -f 'node .*vite'`.

## 2026-10-19 22:52 — deepseek-v4.1-flash

Три агента правят один worktree → `cargo test -p bytetap-core -p bytetap-testkit` падает на недописанном файле чужой зоны (`testkit/src/server.rs`, `transport/serial.rs`), и свои тесты не запустить. Обходить копией дерева (`rsync --exclude target --exclude node_modules` в `/tmp/opencode` + свой `CARGO_TARGET_DIR`), запуская в ней только свои targets.

## 2026-10-19 22:52 — deepseek-v4.1-flash

Code Mode: `execute` не видит `shell` в каталоге, а прямой вызов инструмента работает → не искать `tools.shell` внутри `execute`, вызывать `shell` напрямую.

## 2026-10-21 14:40 — claude-opus-5-5

U1 шаг 1: `cargo test --workspace` и `gh run download` в `/tmp/opencode/...` падают с `Disk quota exceeded` (tmpfs `/tmp` занят чужими каталогами, квота пользователя исчерпана) → тесты ядра с tempfile ложно красные. Обход: `TMPDIR=<worktree>/target/tmp`, артефакты CI — в `target/`.

## 2026-10-21 18:40 — claude-opus-5-5

U1 шаг 3: `cargo test -p bytetap-app --test snapshots` изредка (≈1 из 6 прогонов) падает SIGSEGV в `libvulkan.so.1` — параллельные харнессы `egui_kittest` с wgpu на lavapipe одновременно создают и закрывают устройства. Обход: рендер-тесты берут общий `Mutex` (`gpu()` в `tests/snapshots.rs`); после этого 0 падений из 40. Ещё: тестовый бинарник снимков запускать из `crates/bytetap-app`, иначе эталоны ищутся не там (`Missing snapshot`).
