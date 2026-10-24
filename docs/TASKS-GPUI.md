# Переезд UI на GPUI (ADR-0018)

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Порядок строгий. Каждая задача — ветка `g/NN-кратко`, один PR, заголовок Conventional Commits. Перф-гейт ADR-0018 — для G-04…G-07.

**Заморозка (№55):** задачи M1 по `ui`/`app` на egui — стоп. Продолжаются core/vfs/ops/term и логика keymap. Перед G-01 — тег `egui-final` на текущем main (базовая линия бенчей).

**Статус (2026-10-20):** G-01 — #85, слит. G-02…G-07 — одной веткой `g/02-app-shell`, PR #88 (CI зелёный, ждёт ручной приёмки §32). Перф-гейт: простой 0 %, первый кадр 189–249 мс, бинарь 23,5 МБ, UI-поток при листании ~2,0–2,4 мс (бюджет 2) — `docs/perf/gpui-vs-egui.md`.

## G-01 Фундамент
- Тег `egui-final`. Зависимости `gpui`, `gpui-kit` (фиксированные версии) в `[workspace.dependencies]`; новый крейт не нужен — `ui` переписывается на месте, egui-код переносится в `crates/ui/legacy` до G-06.
- cargo-deny: лицензии GPUI/gpui-kit и транзитивных; дубли в графе.
- CI: бюджет бинаря 25 МБ (№54).
- Приёмка: `cargo build -p urfin-app` собирает пустое окно GPUI за флагом `--ui=gpui` (egui остаётся по умолчанию); CI зелёный.

## G-02 Каркас приложения
- `Entity<CoreHost>`: владеет `Core`; `pump` — по сигналу канала из `MsgSink`/`Waker` (→ `cx.notify()`), без таймеров.
- Окно: восстановление геометрии из сессии, single-instance (S1-15), tracing, crash-hook — без изменений логики.
- Приёмка: окно GPUI с отладочным выводом двух табов (как `DebugUi`), навигация FakeListing; CPU в простое ≤ 0,5 % (30 с, Process Explorer); `win-manual`.

## G-03 Ввод
- Отображение keymap S3 → actions/key contexts GPUI; контексты из `Focus`; аккорды; автоповтор только `repeatable`.
- Раскладка-независимость (v4 №5): матрица en-US, ru, uk, el, he, ja (IME вкл/выкл), Dvorak, AZERTY — автотест на синтетических keystroke + ручной чек-лист.
- Печать при фокусе на панели → `cmdline.insert`; Alt+буква — quick search.
- Приёмка: все команды из `docs/keys.md` достижимы; тест матрицы зелёный; `win-manual` (ru-раскладка: Ctrl+C/V/A/Q/U/O/R, Alt+F7, Ctrl+[ ]).

## G-04 Панели
- Две панели на `uniform_list`/виртуальной таблице gpui-kit; кэш строк по `(tab, gen, idx)`; курсор/выделение по №31, раскраска по №32; режимы Full/Brief/Medium/Wide/Tree/Info/QuickView (Custom — заглушка).
- Breadcrumb, футер панели, `..`. Метрики строк, курсора, отметки и раскраски — `docs/design/DESIGN.md`.
- Приёмка: 100k строк — p99 листания ≤ 9,5 мс на Intel; CPU в пределах гейта против `egui-final`; `win-manual`.

## G-05 Каркас интерфейса и тема
- Тема Fluent поверх gpui-kit (№56): акцент и light/dark из ОС, строки 24 px, Segoe UI Variable/Fluent Icons с диска ОС; **без Mica**, сплошной фон каркаса (№60). Темы — `docs/THEMES.md` / `docs/TASKS-THEMES.md` (№59), правило «одна информация — одно место» (№61).
- Табы в заголовке окна (по набору на панель), cmdline, keybar F1–F10 с модификаторами, тосты (№51), inline-подтверждения и конфликты (PromptDetail), job-бар, меню F9, quick search.
- Дизайн — `docs/design/DESIGN.md` (метрики, правила, снимки `docs/design/screens/`).
- Приёмка: экраны соответствуют `docs/design/screens/` (поправки №31–№32 сохраняются); ресайз p99 ≤ 13 мс; `win-manual` со скриншотами.

## G-06 Удаление egui и рендер
- Удалить egui/eframe/wgpu и `crates/ui/legacy`; `--ui=gpui` — единственный путь.
- `ui.renderer = auto | warp`: проверить поддержку WARP в GPUI DX11; нет — issue/патч, опцию скрыть, записать в ADR-0018.
- CI: проверка размера бинаря снова блокирующая на Windows (до G-06 — предупреждение: egui и GPUI в одном бинаре, ~30 МБ; владелец 2026-10-20).
- Приёмка: `cargo tree` без egui/wgpu; бинарь ≤ 25 МБ; CI зелёный.

## G-07 Перф-гейт и старт
- Прогон бенча (тот же сценарий, что в ADR) GPUI против `egui-final` на Intel и NVIDIA; JSON в `docs/perf/runs/`.
- Разбор первого кадра (цель базиса: тёплый < 250 мс): профиль старта (WPR/Tracy), список узких мест, задачи на исправление.
- Приёмка: все пункты перф-гейта ADR-0018 зелёные, отчёт в `docs/perf/`.

## Дальше (после G-07, отдельными пакетами)
- S6: отрисовка терминала на GPUI (`alacritty_terminal` + GPUI, по образцу Zed).
- S7: вьювер и редактор на редакторе gpui-kit, tree-sitter (№53); дизайн — `docs/design/screens/screen-viewer.png`, `screen-editor.png`.
- Ревизия спек S2/S3/S7 под GPUI (S0).
