# Урфин — разработка: запуск, стенд, сборки

Перенесено из `CLAUDE.md` (R-02, скилл pets-dev). Читать нужный раздел.

## Стенд

Работа нативно на **Windows 11** владельца (до первой задачи — `docs/HANDOFF-stand-windows.md`). Linux и macOS не собираются и не проверяются (S0-decisions №57).

Проверка лестницей: L0 на стенде (fmt, check/clippy своего крейта, свои новые тесты, снимки) → L1 CI (`gate`: fmt, deny, clippy и nextest всего workspace, doc-тесты на Windows) → L2 ручная GUI-приёмка владельцем пакетом по бете (`docs/manual-tests/<ID>.md`). Перф-бюджеты меряются на стенде в release.

## Сборки и проверки

- **CI не дублировать** (решение 2026-11-02): полный `cargo clippy --workspace --all-targets -D warnings`, `cargo nextest run --workspace` и doc-тесты гоняет CI на PR и в очереди.
- Локально: `cargo check`/clippy своего крейта по ходу; nextest только своих новых/изменённых тестов (`-E 'test(...)'`); в конце волны `cargo fmt --all --check` и снимки `--screens` затронутых сцен dev-сборкой (CI их не делает).
- Красный CI — `gh run view <id> --log-failed | tail -40`, одна правка с причиной; `rdeps` локально не гонять.
- Агентов параллельно — не больше 2–3. `~/.cargo/config.toml`: `incremental = false`, `jobs = 6`, `sccache` ≤ 4 ГБ; у каждого worktree свой `CARGO_TARGET_DIR`.
- Release (fat LTO, ~6 мин) — только для замеров по заданию (перф или размер в карточке); в CI — только main и бета.
- Реестр команд `docs/COMMANDS.md` и `docs/keys.md` генерирует тест: `URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc` (полоса `docs-gen`).

## Запуск

- Переносной режим: `urfin.portable` рядом с `urfin.exe` — настройки и состояние в `data\` рядом. Тестовые копии — только так, в `%TEMP%`.
- Снимки без человека: `urfin.exe --screens <каталог>` с dev-сборки (`target/debug/urfin.exe`) — сцены демо-дерева в PNG; картинка та же, что у release. Синтетический ввод в окно (SendInput и т. п.) не используем.
- Свой ConPTY (№76): `python ci/conpty.py target/debug` кладёт `conpty.dll` и `OpenConsole.exe` рядом с exe; без них терминал работает на системном ConPTY (`[term] conpty = auto`). Дистрибутив CI собирает с ними.
- Замер: `urfin.exe --bench [каталог]` (датасет `%LOCALAPPDATA%\Pets\testdata\urfin\gen-100000-s1`); `--bench-sftp` — тот же сценарий на панели `sftp://` из памяти (10k строк); `--bench-editor <файл>`, `--bench-viewer <файл>` — кадры, память, открытие. Переменные и профиль фазы (`URFIN_PROF`, `URFIN_PROF_PHASE`) — в шапках `view/edit_bench.rs` и `view/view_bench.rs`, итоги — `docs/perf/s7.md`, `docs/perf/s2.md`.

## Бюджеты (S0-decisions 2026-10-16, №80)

Первый кадр < 250 мс (своё время старта < 20 мс), кадр p50 ≤ 3 мс, p99 ≤ 6 мс при 100k строк (смена всего экрана — p99 ≤ 8 мс), 0 % CPU в простое, своя память < 50 МБ (процесс ≤ 200 МБ private), бинарь ≤ 30 МБ (растёт вместе с функциями, библиотеки ради размера не патчим).

## Контракты и решения

Межкрейтовые контракты (публичные типы core, Command id, Focus, ListingUpdate и т. п.) сам не меняй: предложение — `docs/adr/NNNN-*.md`, PR класса B, вопрос владельцу.
