# TASKS-M2 — пакет задач вехи M2 «Операции» (сессия S5)

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Текст задач — `specs/S5.md` §8, здесь не дублируется. «Дельты» — правки из S0-contracts-v2…v5, S0-decisions и ADR; они главнее текста задачи (CLAUDE.md «Где правда»). «Следствие» — вывод сборщика пакета; расходится с ADR — прав ADR, вопрос владельцу.

Ветки `s5/NN-кратко`, заголовок PR — Conventional Commits. Видимое в окне — с меткой `win-manual` и файлом `docs/manual-tests/<ID>.md`.

**Платформа:** только Windows (S0-decisions №57). S5-17 (macOS) и S5-18 (Linux) не делаем; unix-ветки в задачах (`unlinkat`, `getdents`, `statx`, mode) — не писать, на них хватает `cfg`-заглушки `std`.

**Интерфейс:** GPUI (ADR-0018). Черновики F5–F8, переименование на месте, вопросы, job-бар и тосты уже есть в `crates/ui/src/gpui_shell` (G-05) — M2 подключает к ним настоящие команды.

## Порядок

- **M2a, MVP** (copy/move/delete/mkdir/rename на std-движке): S5-01 → (S5-03 ∥ S5-04) → S5-10 → S5-05 → S5-06 → S5-07 → S5-08 → S5-09 → S5-11 → S5-12.
  S5-10 (mkdir, rename) поднят раньше S5-05: зависит только от 04 и сразу делает рабочими F7 и Shift+F6.
- **M2b, Windows-скорость:** S5-13 (корзина — по S0 №11 F8 = корзина, без неё F8 удаляет насовсем только с Shift) → S5-14 → S5-16 → S5-15 → S5-19 → S5-20 → S5-25.
- **M2c, остальное:** S5-21, S5-22, S5-23, S5-24; S5-26/27 — по решению владельца.
- S5-02 (`VolumeInfo`) уже закрыт S4-10 (`urfin_vfs::volume`, контракт v2 №12) — остаток S5-02 (Dev Drive/CLONE, кэш по корню, <1 мс из кэша) доделать внутри S5-15/S5-19, когда он понадобится.

## S5 — Файловые операции

- **S5-01 Каркас ops и API** — дельты: v2 №7 (долгие воркеры — `spawn_dedicated`, потоки ≤ `ops.max_threads`), №8 (`JobSpec{kind, title, lane, priority, owner_ns:"ops"}`, `job.retry`/`job.priority` роутятся в ops), №9 (`Progress.phase/total_final/parked`), №10 (состояние ops в `AppState.ext`), №11 (`Value::List`, `Args::locs` — `src=[...]`), №13 (`Handler::on_quit` → вето с вопросом), №15 (`Banner.actions`), №16 (`PromptDetail::Conflict`); v3 №15 (`ops.delete/copy/move` без `confirmed=true` → `UiEvent::Ext{"ops.draft"}`, с `confirmed` — job), v3 №17 (`ops.copy/move/delete{current?}`, имя в `ops.mkdir` опционально, `file.clip.*`); v5 №10 (спан `ops.job` — контракт); v5 №16 (`Engine::Vfs` для не-локального источника, `ops.move` из `READ_ONLY` → черновик с копией). ADR-0013 (`ui.draft command=…`). Следствие: команды уже вызывает UI (`gpui_shell/field.rs`, keymap-пресеты) — имена и аргументы сверить с ними, `docs/COMMANDS.md` перегенерировать.
- **S5-03 Пути и имена** — дельты: S0 №57 (только Windows: `\\?\`, `CON`, `a.`, `a `); NFC/NFD-часть — только детект коллизий регистра (NTFS case-insensitive). Пересечение с `urfin_vfs::name`/`path` (S4-02) — переиспользовать, не дублировать (следствие).
- **S5-04 Job engine** — дельты: v2 №7, №8, №9, №14 (refresh ≤ 1/с на таб, core дедуплицирует); v5 №10 (`ops.job`); S1 §7.1 (`retire`, `KEEP_FINISHED = 64`).
- **S5-05 Walker** — дельты: v5 №14 (общий `urfin_vfs::walk` — владелец S4; ops берёт его для не-локальных источников, свой стриминговый обход с `DirNode` — для локальных, следствие: не дублировать fast path S4-07, а переиспользовать `vfs::sys`).
- **S5-06 Copy MVP** — дельты: S0 №12 (проверка копий выкл; межтомный move — size+fsync; hash по флагу), №13 (метаданные как Explorer, ACL — опция), v2 №17 (датасеты B1–B6 — `urfin-testdata`/`xtask gen-dataset`, офлайн). Unix mode — не делаем (№57).
- **S5-07 Немодальные конфликты** — дельты: v2 №16 (`PromptDetail::Conflict`), v2 №9 (`parked` → «ещё N»); UI уже рисует вопрос строкой (`overlays::prompt_bar`) — добавить вид конфликта (src/dst, размеры, даты) в `gpui_shell`, `win-manual`.
- **S5-08 Move** — дельты: S0 №12 (межтомный — size+fsync), v5 №16 (`READ_ONLY` → черновик с копией).
- **S5-09 Delete** — дельты: S0 №11 (F8/Del — корзина, Shift — насовсем; до S5-13 F8 без Shift — тост «корзина — позже» и черновик с «насовсем», следствие); junction-тест обязателен.
- **S5-10 Mkdir и Rename** — дельты: v3 №17 (имя в `ops.mkdir` опционально → черновик), ADR-0013; поле переименования на месте уже шлёт `ops.rename {to}` (`gpui_shell/field.rs`), Shift+F6 → `ui.rename` (PR #88); фокус курсора на новом/переименованном (`pending_focus`, S4-15).
- **S5-11 Ошибки и ретраи** — дельты: v2 №8 (`job.retry` роутится в ops).
- **S5-12 Журнал и восстановление** — дельты: S0 №14 (баннер [Продолжить] [Очистить]), v2 №13 (`on_quit` вето), v2 №15 (`Banner.actions` ≤ 3); single-instance (S1-15) — чужие живые журналы не трогать.
- **S5-13 Корзина** — дельты: S0 №11, №15 (`$Recycle.Bin` напрямую — за флагом, выкл).
- **S5-14…S5-16, S5-19…S5-25** — дельты: S0 №57 (только Windows), v5 «Пороги регрессий» и `perf-evidence` (JSON прогона в `docs/perf/runs/`), бюджеты 10-16.
