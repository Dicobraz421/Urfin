# Урфин — разработка: запуск, стенд, сборки

Перенесено из `CLAUDE.md` (R-02, скилл pets-dev). Читать нужный раздел.

## Стенд

Работа нативно на **Windows 11** владельца (до первой задачи — `docs/HANDOFF-stand-windows.md`). Linux и macOS не собираются и не проверяются (S0-decisions №57).

Проверка лестницей: L0 у агента — в облачной сессии или на стенде (fmt, check/clippy своего крейта, свои новые тесты) → L1 CI (`gate`: fmt, deny, clippy и nextest всего workspace, doc-тесты на Windows; снимки сцен — задача `screens`) → L2 ручная GUI-приёмка владельцем пакетом по бете (`docs/manual-tests/<ID>.md`). Перф-бюджеты меряются на стенде в release.

## Сборки и проверки

- **CI не дублировать** (решение 2026-11-02): полный `cargo clippy --workspace --all-targets -D warnings`, `cargo nextest run --workspace` и doc-тесты гоняет CI на PR и в очереди.
- У агента: `cargo check`/clippy своего крейта по ходу; nextest только своих новых/изменённых тестов (`-E 'test(...)'`); в конце волны `cargo fmt --all --check`. Где работает агент и откуда брать снимки — раздел «Где что собирается».
- Красный CI — `gh run view <id> --log-failed | tail -40`, одна правка с причиной; `rdeps` локально не гонять.
- На стенде — не больше одного агента со сборкой (остальные — в облаке). `~/.cargo/config.toml`: `incremental = false`, `jobs = 6`, `sccache` ≤ 4 ГБ; у каждого worktree свой `CARGO_TARGET_DIR`.
- Release (fat LTO, ~6 мин) — только для замеров по заданию (перф или размер в карточке); в CI — только main и бета.
- Реестр команд `docs/COMMANDS.md` и `docs/keys.md` генерирует тест: `URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc` (полоса `docs-gen`).

## Где что собирается

Решение 2026-11-11 (`docs/decisions/2026-11-11-cloud-pipeline.md`): машину владельца сборками не грузим.

- **Облачная сессия** (Ubuntu, 4 ядра / 16 ГБ / 30 ГБ) — задачи без окна: полосы `core`, `vfs`, `ops`, `keymap`, `i18n`, `view`, `net`, `pkg`, `docs`, `ci`. Проверки там: `cargo fmt --all --check`; `cargo check -p <крейт> --target x86_64-pc-windows-msvc` и clippy так же (цель ставится `rustup target add x86_64-pc-windows-msvc`); nextest своих тестов — если крейт собирается под Linux. Тесты только для Windows и весь workspace гоняет CI.
- **Снимки сцен** — задача `screens` в CI на каждом PR (dev-сборка, WARP). Взять: `gh run list --branch <ветка> --workflow ci.yml --limit 1 --json databaseId`, затем `gh run download <id> -n screens -D <каталог>`. Задача вне `gate`; если она красная — смотреть `gh run view <id> --log-failed`, слияние это не держит.
- **Стенд владельца** — замеры в release, ручная проверка (exe — из артефакта `urfin-Windows` сборки main: `gh run download <id> -n urfin-Windows`), задачи с интерфейсом, которым нужно много кругов снимков подряд (`--screens` локально).
- **Уборка**: после слияния удалить свой `CARGO_TARGET_DIR` и рабочую копию. Размер каталога быстро: `robocopy <каталог> NULL /L /S /NJH /BYTES /NFL /NDL /NC /XJ`.

## Сверка подсветки с tree-sitter

Трекеры кода (ADR-0024 §8) сверяются с полным разбором tree-sitter; в PR и merge queue этого нет: пакетов грамматик в `test (windows)` нет, идут только тесты трекера.

- **Ночью** — `.github/workflows/hl-corpus.yml`, раз в сутки и вручную: собирает DLL грамматик, сверяет код репозитория и крейты cargo registry раннера с границей на каждом кандидате и случайными срезами окна. На PR и очередь не влияет; падение видно в Actions, сводка по языкам — в Summary прогона.
- **Запустить вручную**: `gh workflow run hl-corpus.yml --repo Dicobraz421/Urfin`, смотреть — `gh run list --repo Dicobraz421/Urfin --workflow hl-corpus.yml --limit 1`, причину — `gh run view <id> --repo Dicobraz421/Urfin --log-failed | tail -40`.
- **На стенде**: DLL — как в шаге workflow, в свою папку; затем `HL_PACK=<папка DLL> HL_CORPUS=<папки через ;> cargo test -p urfin-view --lib java_corpus -- --nocapture`. `HL_SHOW` — сколько расхождений показать (12), `HL_MAX` — предел файла в байтах (512 КиБ), `HL_EXT` — другие расширения; один файл — `HL_ONE=<файл> HL_LANG=<язык> cargo test -p urfin-view --lib code_one -- --nocapture`. Случайные тексты — `crates/view/src/syntax/lex/code/samples/gen.py`. HTML — лексер со своей сверкой: `HL_PACK=<папка с html.dll> HL_CORPUS=<папки> cargo test -p urfin-view --lib html_corpus -- --nocapture`, `HL_ERRS=1` — чем лексер отличается от восстановления tree-sitter на файлах с ошибкой.

## Запуск

- Переносной режим: `urfin.portable` рядом с `urfin.exe` — настройки и состояние в `data\` рядом. Тестовые копии — только так, в `%TEMP%`.
- Снимки без человека: `urfin.exe --screens <каталог>` с dev-сборки (`target/debug/urfin.exe`) — сцены демо-дерева в PNG; картинка та же, что у release. Синтетический ввод в окно (SendInput и т. п.) не используем.
- Свой ConPTY (№76): `python ci/conpty.py target/debug` кладёт `conpty.dll` и `OpenConsole.exe` рядом с exe; без них терминал работает на системном ConPTY (`[term] conpty = auto`). Дистрибутив CI собирает с ними.
- Замер: `urfin.exe --bench [каталог]` (датасет `%LOCALAPPDATA%\Pets\testdata\urfin\gen-100000-s1`); `--bench-sftp` — тот же сценарий на панели `sftp://` из памяти (10k строк); `--bench-editor <файл>`, `--bench-viewer <файл>` — кадры, память, открытие; файлы для них — генераторы датасетов (S7-00a): `cargo run -p urfin-testdata --example testdata --release -- v1|v2|v3|v4|all|v5|v6 [--seed N] [--size 10g]`, V1–V4 — в `%LOCALAPPDATA%\Pets\testdata\urfin`, путь печатается — его и передавать (команды — в шапке `crates/testdata/examples/testdata.rs`); `--bench-search [строк]` — кадр экрана поиска на фикстуре (1M строк). Поиск F1–F5 — `cargo run -p urfin-search --example search_bench --release -- gen|run <каталог>` (шапка примера), итоги — `docs/perf/s8.md`. Переменные и профиль фазы (`URFIN_PROF`, `URFIN_PROF_PHASE`) — в шапках `view/edit_bench.rs` и `view/view_bench.rs`, итоги — `docs/perf/s7.md`, `docs/perf/s2.md`.

## Бюджеты (S0-decisions 2026-10-16, №80)

Первый кадр < 250 мс (своё время старта < 20 мс), кадр p50 ≤ 3 мс, p99 ≤ 6 мс при 100k строк (смена всего экрана — p99 ≤ 8 мс; нажатие в файле в одну строку — p50 ≤ 3,5, p99 ≤ 7 мс; ресайз панелей — p50 ≤ 9, p99 ≤ 12 мс; первый экран вьювера ≤ 50 мс, на файлах от 50 МБ — известное отступление, решение 2026-11-06), 0 % CPU в простое, своя память < 50 МБ (процесс ≤ 200 МБ private), бинарь ≤ 50 МиБ (гейт CI; растёт вместе с функциями, библиотеки ради размера не патчим).

## Контракты и решения

Межкрейтовые контракты (публичные типы core, Command id, Focus, ListingUpdate и т. п.) сам не меняй: предложение — `docs/adr/NNNN-*.md`, PR класса B, вопрос владельцу.
