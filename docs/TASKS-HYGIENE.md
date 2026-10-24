# H-01 — гигиена диска (Urfin, Postman, pets)

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Решение владельца 2026-10-19: сборки и тестовые данные съедают диск. Применить во **всех трёх** репозиториях (`2026 FileNav`, `Postman`, `pets`), одной задачей на репо, ветка `chore/disk-hygiene`.

## 1. Профиль dev (Cargo.toml workspace)
```toml
[profile.dev]
debug = "line-tables-only"   # стектрейсы с номерами строк, PDB в разы меньше

[profile.dev.package."*"]
debug = false                # без отладочных символов у зависимостей
```
Если в профиле уже есть `opt-level` для зависимостей — сохранить. Профиль `profiling` (если есть) не трогать.

## 2. Общий кэш сборки
- Стенд: `CARGO_TARGET_DIR=D:\cargo-target` (Dev Drive) — общий для трёх репо, общие зависимости (gpui и др.) собираются один раз. Прописать в `docs/HANDOFF-stand-windows.md` и `scripts/stand/setup-windows.ps1` (через `setx`, с проверкой наличия Dev Drive; если нет — не трогать).
- Код и скрипты не должны хардкодить `target/` — только `cargo metadata --format-version 1 | target_directory` или `env!("CARGO_TARGET_DIR")`-эквиваленты в xtask.
- Не собирать под чужие таргеты на стенде (`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`) — это CI; если нужна проверка cfg, `cargo check --target …`, не `build`.

## 3. Временные файлы (%TEMP%)
- Тесты: только `tempfile::TempDir`/`NamedTempFile` (автоудаление при drop); запрещены «ручные» каталоги в `%TEMP%` без очистки. Найти и исправить существующие (grep `temp_dir()`, `env::temp_dir`, `%TEMP%`).
- Все временные каталоги — с префиксом `urfin-` / `postman-` / `pets-`, чтобы их было видно и можно было чистить.
- Генератор датасетов (testdata/xtask): писать в `%LOCALAPPDATA%\Pets\testdata\<app>\<dataset>`, не в `%TEMP%`; переиспользовать при совпадении сида; команда `xtask testdata clean`; бенчи после прогона удаляют свои рабочие копии (флаг `--keep` для отладки).
- Разовая очистка: удалить в `%TEMP%` каталоги с префиксами выше и брошенные `.tmp*` от tempfile старше 1 дня (скрипт `scripts/stand/clean-temp.ps1`, сначала показывает список и объём, удаляет по `-Apply`).

## 4. Периодическая чистка
- `scripts/stand/clean.ps1`: показывает объём `target\debug\incremental`, `criterion`, чужих таргетов, testdata, %TEMP%-префиксов; удаляет по `-Apply`. Упомянуть в HANDOFF.
- Опционально `cargo sweep --time 14` (если владелец согласен поставить `cargo-sweep`).

## Приёмка
- После `cargo clean` и одной полной сборки + `nextest`: размер `target\debug` (или общего каталога) — в отчёте PR «было/стало».
- `cargo nextest run` не оставляет новых каталогов в `%TEMP%` (проверка до/после в PR).
- Стектрейсы паник по-прежнему содержат файл и строку.
