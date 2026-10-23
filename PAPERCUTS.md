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

## 2026-10-22 00:20 — deepseek-v4.1-flash

CI `native-windows`: `Write-Host "$name: снимок: $_"` в PowerShell — это не строка, а ссылка на переменную области видимости: `ParserError: Variable reference is not valid. ':' was not followed by a valid variable name character`, причём номер строки указывает на закрывающую скобку блока, а не на саму строку. Писать `"${name}: …"`.

## 2026-10-22 00:20 — deepseek-v4.1-flash

`egui_kittest`: у `Node` два прямоугольника — `Node::rect()` в точках и `AccessKitNode::bounding_box()` в физических пикселях AccessKit. На 125 % сравнение `bounding_box()` с размером окна в точках даёт «виджет вне окна». Брать `Node::rect()`.

## 2026-10-22 00:21 — deepseek-v4.1-flash

egui: `ui.spacing_mut().item_spacing` учитывается при размещении **предыдущего** виджета, а не следующего (`allocate_space` берёт интервал сразу после себя). Правка интервала после добавления виджета сдвигает не его, а уже следующую группу — в шапке окна кнопки уехали на 8 px за край.

## 2026-10-22 00:21 — deepseek-v4.1-flash

egui_kittest `check_accessibility` ловит только роли `is_input`: строка ленты (`Role::Label` с `actions: [Click, Focus]` и текстом в `value`, а не в `label`) проходит проверку без подписи. Своя проверка дерева должна смотреть `label().or(value())`.

## 2026-10-22 07:20 — deepseek-v4.1-flash

`gh run view <id> --json artifacts` отвечает `Unknown JSON field: "artifacts"` (artifacts не входят в набор `--json`), а `gh run download` имена артефактов не подсказывает. Список — `gh api repos/<owner>/<repo>/actions/runs/<id>/artifacts --jq '.artifacts[] | .name'`.

## 2026-10-22 10:48 — deepseek-v4.1-flash

`git rm -r app` кладёт удаления в индекс, и следующий `git commit` без `-a` забирает их вместе с единственным добавленным файлом: коммит «правка одной строки в ci.yml» уехал на 99 файлов и сломал CI (package.json ещё звал workspace `app`). Перед частичным коммитом смотреть `git status --short` целиком: `git rm` — это уже staging.

## 2026-10-23 14:05 — deepseek-v4.1-flash

egui_kittest: `Event::PointerButton { modifiers }` не обновляет `input.modifiers` — Shift+клик и Ctrl+клик в тестах приходили как обычный клик, и разница была видна только по счётчику выделения. Модификаторы надо слать отдельно: `Event::ModifiersChanged(m)` до нажатия и `ModifiersChanged(default)` после; `Harness::event_modifiers` делает это само.

## 2026-10-23 14:05 — deepseek-v4.1-flash

Хелпер теста, который создаёт `tempfile::tempdir()` и возвращает только харнесс, удаляет папку с журналом до первого кадра: страницы журнала не читаются никогда, а тест висит на «странице, которой нет». Временную папку держать у теста (`journal_harness(dir.path())`), как в `tests/journal.rs`.
