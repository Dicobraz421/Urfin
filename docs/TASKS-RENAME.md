# R-01 — переименование FileNav → Urfin

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Основание: S0-decisions №45 (издатель Aahz), №49 (имя Urfin). Приоритет: **выполнить следующей задачей**, до продолжения M1 — дальше конфликты только растут.

## Цель
Одним PR (ветка `chore/rename-urfin`, заголовок `chore: rename FileNav to Urfin`) заменить имя продукта во всём репозитории. Поведение кода не меняется.

## Объём
1. **Крейты:** `filenav-*` → `urfin-*` в `[package] name`, `[workspace.dependencies]`, всех `use filenav_*` → `urfin_*`. Папки `crates/*` не переименовывать (имена папок без префикса).
2. **Бинарь:** `[[bin]] name = "urfin"`; CI (`ci.yml`, `ci/size.py`, `ci/release_features.py`), скрипты, `--smoke` и пр. — на `urfin`.
3. **Строковые идентификаторы в коде:** каталоги профиля (`FileNav` → `Urfin`, `filenav` → `urfin` для XDG), `filenav.portable` → `urfin.portable`, имя лога, `FILENAV_LOG` → `URFIN_LOG`, имя pipe/socket/mutex single-instance, имена потоков, если содержат имя, Fluent-ключи и тексты (`ui-app-name` и т.п.), заголовок окна, ресурсы exe (`winresource`: ProductName `Urfin`, CompanyName `Aahz`, FileDescription `Urfin — files and settings`).
4. **About/сплэш:** отображаемое имя `Úrfionn`; строка «a.k.a. Oorfene Deuce» в About (Fluent-ключ, en+ru). Если экранов ещё нет — завести ключи и пометить TODO в задаче соответствующей сессии.
5. **Документы:** README, CLAUDE.md, docs/** (специ, ADR, HANDOFF, TASKS, MANUAL-TESTS, perf) — `FileNav`→`Urfin`, `filenav`→`urfin`, `FILENAV`→`URFIN`. Исключения: исторические цитаты в ADR (оставить, добавить примечание «до 2026-10-17 — FileNav»), имя папки на диске `2026 FileNav` не трогать.
6. **Издатель:** все упоминания `Anton Antropov`/`AntonAntropov` как издателя → `Aahz` / winget ID `Aahz.Urfin`. Копирайт в LICENSE-* не трогать.
7. `filenav-bootstrap.zip` в корне — удалить из репо (устаревший артефакт), добавить `*.zip` в `.gitignore`, если не нужен.

## Приёмка
- `git grep -i filenav` возвращает только: исторические примечания в ADR и путь папки стенда (если встречается в HANDOFF). Список исключений — в описании PR.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace` — зелёные; CI windows/ubuntu/macos — зелёные.
- `target\release\urfin.exe --smoke` — exit 0; свойства exe показывают ProductName Urfin, CompanyName Aahz.
- Старый профиль `%APPDATA%\FileNav` не мигрируется (релизов не было) — указать в PR.
- Метка `win-manual`: владелец запускает `urfin.exe`, видит заголовок окна «Urfin».

## После мержа
Переименовать GitHub-репозиторий в `urfin` (если уже создан) — делает владелец; Code обновляет remote и ссылки в README/CI.
