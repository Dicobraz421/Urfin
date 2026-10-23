# Urfin — правила для Claude Code
<!-- ≤3 КБ. Только стабильные правила. Текущая работа — в карточках docs/tasks/. -->

Двухпанельный файловый менеджер (DN/Far, Windows Fluent): Rust 2024, GPUI (gpui-kit), только Windows. Базис — скилл `pets-architecture`, процесс — скилл `pets-dev`.

## Где правда
1. `docs/decisions/` — решения владельца (архив до 2026-10-24 — `docs/specs/S0-decisions.md`, «№NN»).
2. `docs/CONTRACTS.md` — межкрейтовые контракты (менять только PR класса B).
3. Карточка `docs/tasks/<ID>.md` и разделы из её `read:`; спеки `docs/specs/Sx.md` — только нужный раздел.
4. Запуск, снимки, стенд, бюджеты — `docs/DEV.md` (читать нужный раздел).

## Правила
- Полосы — `lanes.toml`; одна волна = одна полоса = один PR; `gh pr merge --auto --rebase`.
- Проверки в конце волны: fmt, clippy и nextest затронутых (`rdeps`), снимки затронутых сцен dev-сборкой.
- `unsafe` — только в модулях `sys`. Ошибки: thiserror в либах, anyhow в app; логи — tracing.
- UI-поток не делает I/O и не блокируется: тяжёлое — через Runtime (Interactive/Bulk, rayon).
- Офлайн: ни сети в рантайме, ни телеметрии; ресурсы вшиты. Строки UI — Fluent (urfin-i18n), EN+RU.
- Хоткеи с буквой — на любой раскладке. Ручные проверки — `docs/manual-tests/<ID>.md`.
- Не трогать: чужие полосы, Cargo.toml/lock (→ полоса deps), генерируемые доки (`COMMANDS.md`, `keys.md`), `.github/`.
