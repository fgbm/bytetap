## 2026-10-19 20:28 — claude-opus-5-5

Рендер HTML-макета в чате → страница пустая: глобальная `const top` в скрипте конфликтует с неконфигурируемым `window.top`. Оборачивать скрипты макетов в IIFE и не называть глобальные переменные `top`, `status`, `name`, `parent`, `self`.

## 2026-10-19 21:05 — deepseek-v4.1-flash

Смоук vite: `pkill -f vite` убивает собственный shell агента (совпадение по командной строке) → убивать по PID из `$!` или `pkill -f 'node .*vite'`.

## 2026-10-19 22:52 — deepseek-v4.1-flash

Три агента правят один worktree → `cargo test -p bytetap-core -p bytetap-testkit` падает на недописанном файле чужой зоны (`testkit/src/server.rs`, `transport/serial.rs`), и свои тесты не запустить. Обходить копией дерева (`rsync --exclude target --exclude node_modules` в `/tmp/opencode` + свой `CARGO_TARGET_DIR`), запуская в ней только свои targets.

## 2026-10-19 22:52 — deepseek-v4.1-flash

Code Mode: `execute` не видит `shell` в каталоге, а прямой вызов инструмента работает → не искать `tools.shell` внутри `execute`, вызывать `shell` напрямую.
