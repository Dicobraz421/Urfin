# Урфин — разработка: запуск, стенд, сборки

Перенесено из `CLAUDE.md` (R-02, скилл pets-dev). Читать нужный раздел.

## Стенд

Работа нативно на **Windows 11** владельца (до первой задачи — `docs/HANDOFF-stand-windows.md`). Linux и macOS не собираются и не проверяются (S0-decisions №57).

Проверка лестницей: L0 на стенде (fmt, clippy, nextest затронутых) → L1 CI (`gate`: fmt, deny, clippy, nextest на Windows) → L2 ручная GUI-приёмка владельцем пакетом по бете (`docs/manual-tests/<ID>.md`). Перф-бюджеты меряются на стенде в release.

## Сборки и проверки

- `cargo check -p <крейт>` по ходу; в конце волны один раз: `cargo fmt --all` · `cargo clippy -p <затронутые> --all-targets -- -D warnings` · `cargo nextest run -E 'rdeps(<затронутые>)'`.
- Общий кэш компиляции — `sccache` (`~/.cargo/config.toml`); у каждого worktree свой `CARGO_TARGET_DIR`.
- Release (fat LTO, ~6 мин) — только когда в карточке есть перф или размер; в CI — только main и бета.
- Реестр команд `docs/COMMANDS.md` и `docs/keys.md` генерирует тест: `URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc` (полоса `docs-gen`).

## Запуск

- Переносной режим: `urfin.portable` рядом с `urfin.exe` — настройки и состояние в `data\` рядом. Тестовые копии — только так, в `%TEMP%`.
- Снимки без человека: `urfin.exe --screens <каталог>` с dev-сборки (`target/debug/urfin.exe`) — сцены демо-дерева в PNG; картинка та же, что у release. Синтетический ввод в окно (SendInput и т. п.) не используем.
- Замер: `urfin.exe --bench [каталог]` (датасет `%LOCALAPPDATA%\Pets\testdata\urfin\gen-100000-s1`).

## Бюджеты (S0-decisions 2026-10-16, №80)

Первый кадр < 250 мс (своё время старта < 20 мс), кадр ≤ 2 мс при 100k строк, 0 % CPU в простое, своя память < 50 МБ (процесс ≤ 200 МБ private), бинарь ≤ 30 МБ (растёт вместе с функциями, библиотеки ради размера не патчим).

## Контракты и решения

Межкрейтовые контракты (публичные типы core, Command id, Focus, ListingUpdate и т. п.) сам не меняй: предложение — `docs/adr/NNNN-*.md`, PR класса B, вопрос владельцу.
