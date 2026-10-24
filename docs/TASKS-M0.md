# TASKS-M0 — первый пакет задач (веха M0, сессия S1)

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Пакет: S1-01 → S1-02 → S1-03 → (S1-04 ∥ S1-05). Порядок и зависимости — specs/S1.md §5.
Текст задач перенесён из specs/S1.md §5 без изменений. Блок «Дельты контрактов» перечисляет правки из S0-contracts-v2/v3/v4/v5 и S0-decisions, относящиеся к задаче. Эти правки имеют приоритет над текстом задачи и над S1.md (см. CLAUDE.md «Где правда»). Пометка «следствие» означает вывод сборщика пакета из дельты, а не текст ADR. Если следствие расходится с ADR, прав ADR, а вопрос уходит владельцу.

Ветки: `s1/01-workspace`, `s1/02-ci`, `s1/03-base-types`, `s1/04-commands`, `s1/05-runtime`. Заголовок PR — Conventional Commits (v5, «Зависимости и политика»), например `feat(core): base types`, `ci: workflow`.

---

## S1-01 Workspace-каркас

- Цель: собираемый workspace по §3.
- Крейт: все (заглушки).
- API: в каждом lib `pub fn version() -> &'static str`; в app `main` печатает версию.
- Приёмка:
  - `cargo build --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` проходят локально на Windows.
  - Есть файлы лицензий, `docs/specs/S1.md` (копия этого документа) и ADR 0001–0003 (по разделу 0).
- Тест: smoke-тест `version()` в каждом крейте.
- Зависит: —

**Дельты контрактов**
- **v3 №8.** Добавить крейт-заглушку `crates/i18n` (пакет `urfin-i18n`, lib). Как у остальных заглушек: `pub fn version()` и smoke-тест. Крейт листовой, то есть не зависит от других крейтов `urfin-*`. Позже от него будут зависеть core, vfs, ops, ui и term. *Следствие:* в S1-01 крейт только входит в workspace, а рёбра `→ urfin-i18n` заводятся, когда крейт начинает использоваться (S2-01/S2-02). Fluent-зависимости (`fluent-bundle`, `unic-langid`, одобрены в v3) в заглушку не тянуть.
- **v3 №8, уточнение владельца:** в лицензиях и README ничего не менять.
- **v5 №13.** Ещё три крейта-заглушки, оформленных так же (`pub fn version()` + smoke-тест):
  - `crates/search` → пакет `urfin-search` (lib, владелец S8);
  - `crates/arc` → пакет `urfin-arc` (lib, владелец S8);
  - `crates/testdata` → пакет `urfin-testdata` (lib, владелец S9) с `publish = false`. Крейт только для разработки: подключается исключительно как `[dev-dependencies]` и не попадает в граф `urfin-app`.

  Крейт `urfin-net` не создавать: это фаза 3. *Следствие:* `members = ["crates/*"]` подхватит новые каталоги автоматически, и рёбра к ним в S1-01 не заводятся. Зависимости из одобренного v5-списка (zip, sevenz-rust2, grep-*, ChaCha8 и т.п.) в заглушки не тянуть.
- **v4 №1 + v5 №5.** `unsafe` разрешён в модулях `sys` крейтов vfs, ops, term, ui, view и app. В `[workspace.lints]` остаётся `rust.unsafe_code = "deny"`, а `#[allow(unsafe_code)]` получают только эти модули `sys`. В S1.md §3 перечислены лишь vfs/ops/term. В core по-прежнему `#![forbid(unsafe_code)]`.
- **Нумерация ADR (следствие).** ADR-0004…0008 уже заняты документами S0 (`S0-contracts-v2` = 0004, `S0-decisions` = 0005, `S0-contracts-v3` = 0006, `S0-contracts-v4` = 0007, `S0-contracts-v5` = 0008). Задача создаёт `docs/adr/0001…0003`. Новые ADR (см. CLAUDE.md) начинаются с 0009.
- **Документы.** `docs/specs/S1.md` и остальные спеки уже лежат в репо из стартового пакета. Их не перезаписывать.
- *Вопрос закрыт (v5 №5):* `unsafe` в `app::sys` (хук `KeyNotes` на Windows, меню macOS) разрешён. В S1-01 самих модулей `app::sys` ещё нет, они появятся в S1-13/S3-07/S3-08.

---

## S1-02 CI

- Цель: `ci.yml` по §3, `deny.toml`, `ci/size.py`, `.config/nextest.toml`.
- Крейт: — (инфра).
- API: —
- Приёмка: на PR зелёные `fmt`, `deny` и `test` × 3 ОС; артефакт загружен; провал при бинаре >15 МБ (проверено временным порогом 1 МБ).
- Тест: сам прогон CI.
- Зависит: 01.

**Дельты контрактов**
- **v3, «Зависимости (одобрено)».** В `deny.toml` (`[bans]`) нужен `skip` для дубля `windows-sys` 0.59/0.61: `alacritty_terminal` тянет 0.59, а eframe и наши `sys`-модули — 0.61. *Следствие:* пока `alacritty_terminal` не подключён (S6), дубля нет. Если cargo-deny ругается на неиспользуемый skip, его добавляют вместе с подключением term-зависимостей.
- **v5, «Зависимости и политика».** В `[licenses] allow` (`deny.toml`) добавить `BSD-3-Clause`. Её требует `encoding_rs`: `(MIT OR Apache-2.0) AND BSD-3-Clause`. Остальной allow-лист S1.md §3 не меняется. Исключение `LicenseRef-UnRAR` для `unrar` **не** добавлять: это ждёт решения владельца №37. *Следствие:* вшитые шрифты (JetBrains Mono — OFL, Fluent System Icons — MIT, v4 №3) — это ассеты, а не крейты, и cargo-deny их не проверяет. Их лицензии оформляет S2-04.
- **v5 №11.** CI запрещает features `profile`, `dhat-heap`, `count-alloc` в release-сборке. В job `test` перед release-сборкой нужен шаг, который падает, если хотя бы одна из этих features включена в разрешённом графе `urfin-app` для release-профиля. *Следствие:* сами features заведут S9-08 и S9-10, а до тех пор шаг проходит. Проверка должна искать features по именам, не опираясь на их наличие в `Cargo.toml`. Доп. приёмка: временное включение feature с одним из этих имён в `default` у заглушки валит шаг.
- **v5, «Зависимости и политика» (бюджет бинаря).** Порог 15 МБ (`ci/size.py`) проверяет только `urfin.exe` (на mac/Linux — `urfin`). Файлы `conpty.dll` и `OpenConsole.exe` (S6), а также дистрибутивы (zip/MSI) в бюджет не входят и проверяются отчётом без гейта (S9 §8 №8).
- **v5, «Зависимости и политика» (Conventional Commits).** В `ci.yml` добавить проверку заголовка PR на формат Conventional Commits (`type(scope)?: описание`, например `feat(core): …`). Невалидный заголовок валит CI. *Следствие:* проверка запускается только на событии `pull_request`, включая `edited`, чтобы правка заголовка перезапускала её. Сторонние actions — только с фиксированной версией (тег или sha). Генерация changelog (git-cliff) не входит в S1-02, это S9-18.
- **v3 №10 (следствие).** Версия `winit` должна быть строго той, что тянет eframe 0.36, без дубля. Правило `multiple-versions` в `deny.toml` не должно глушиться skip'ом для winit.
- *Не в S1-02:* пороги регрессий v5 (CI-instr 3 %, CI-ab 20 %, стенд 10 %) и правило `perf-evidence` — это `bench.yml` и S9-07, а не `ci.yml` этой задачи.

---

## S1-03 Базовые типы

- Цель: `ids`, `Location` (+uri), `Entry`/`EntryKind`/`Attrs`, `Mask`, `CancelToken`.
- Крейт: core.
- API: §2.1.
- Приёмка:
  - `size_of::<Entry>() <= 48` (assert в тесте).
  - `Location` проходит roundtrip uri для `C:\`, UNC `\\srv\sh\x`, `/`, цепочки `>>`.
  - `Mask`: `*.rs,*.toml|*.bak`, `?`, регистр по ОС.
  - Отмена родителя отменяет детей.
- Тест: unit + proptest (uri roundtrip, mask против эталонной наивной реализации).
- Зависит: 01.

**Дельты контрактов**
- **v2 №5.** Новые биты `Attrs`:
  - `pub const ESCAPED_NAME: u32 = 1 << 10;` — имя содержит обратимо экранированные не-UTF-8 юниты (PUA U+10F700…U+10FFFF, S4 §0). Реальное имя для ОС даёт `vfs::name::to_os`.
  - `pub const SIZE_KNOWN: u32 = 1 << 11;` — у каталога поле `size` валидно (посчитано).

  Пути для ОС строятся только через `vfs::name::{to_os, join_os}`. Это реализация S4-02, в core декодирования нет. `size_of::<Entry>() <= 48` сохраняется, потому что `Attrs` остаётся `u32`.
- **v2 №6.** Нормализация в `Location::local` (правила фиксируются в core):
  - путь абсолютный;
  - Windows: `\\?\C:\` → `C:\`, `\\?\UNC\s\sh` → `\\s\sh`; буква диска в верхнем регистре; `/` → `\`;
  - `.` и `..` разрешаются лексически;
  - без хвостового разделителя, кроме корня;
  - регистр имён не трогается, symlink не раскрываются.

  Эталонная `normalize_local` для сверочных тестов появится в S4-02 (S4 §9 №6).
  *Доп. приёмка (следствие):* `Location::local("c:/x/") == Location::local("C:\\x")` на Windows, `\\?\`-формы нормализуются до обычной, `a\.\b\..\c` → `a\c`, корни `C:\` и `/` сохраняют разделитель. Roundtrip uri проверять уже на нормализованных значениях.
- **v5 №8.** Требования к `Location`:
  - Глубина цепочки `outer` не больше 8. `parse_uri` отвергает более глубокую цепочку ошибкой `LocError`. Имя варианта `TooDeep` взято из запроса S8 §9 №6.
  - Последовательность `>>` внутри компонентов пути (имя файла или каталога) экранируется в `to_uri` и восстанавливается в `parse_uri`. Разделитель контейнеров `>>` остаётся однозначным.
  - *Следствие:* формат экранирования в v5 не задан, а uri — сериализованная форма в `session.toml` и в аргументах команд. Выбранный обратимый формат описать в rustdoc `to_uri`/`parse_uri` и предложить ADR (номер ≥0009) в том же PR.
  - *Доп. приёмка (следствие):* roundtrip uri для имён с `>>`, `>`, `%` и `\` в компонентах, для цепочки глубины 8 (успех) и глубины 9 (ошибка). proptest uri-roundtrip генерирует имена с `>>`.
  - Схемы `search:` и `cmp:` в сессии не сохраняются. Это поведение `Session::capture` (S1-11), а не S1-03.

Других дельт к S1-03 в S0-contracts-v2…v5 и S0-decisions нет. Решение №8 (скрытые на Windows) и №10 (natural sort) реализует S4 поверх существующих битов `HIDDEN`/`DOTFILE`.

---

## S1-04 Модель команд

- Цель: `CommandId`, `Value`, `Args`, `Command` (+parse и serde), `CommandMeta`, `ArgSpec`, `CommandRegistry`, `CmdError`.
- Крейт: core.
- API: §2.2.
- Приёмка:
  - Невалидные id отвергаются.
  - `Command::parse("panel.go path=\"C:\\x y\"")` работает.
  - TOML-формы `"id"` и `{cmd=…}` проходят roundtrip.
  - `validate` ловит отсутствие required-аргумента и неверный тип.
- Тест: unit + insta-снапшоты TOML.
- Зависит: 03.

**Дельты контрактов**
- **v2 №11.** `Value::List(Arc<[Value]>)`, аксессоры `Args::list(k)` и `Args::locs(k)` (список `Location`: элементы `Loc` или `Str`, разобранные как в `Args::loc`). Текстовая форма для `Command::parse`: `src=["a","b"]`.
  *Следствие:* `ValueKind` пополняется вариантом `List` (для `ArgSpec`/`validate`). В TOML список — массив (`{ cmd = "ops.copy", src = ["a","b"] }`), insta-снапшот должен покрывать эту форму.
  *Доп. приёмка (следствие):* `Command::parse("ops.copy src=[\"C:\\a\",\"C:\\b y\"] dest=\"D:\\\"")` даёт `List` из двух элементов, и roundtrip TOML сохраняет его.
- **v5 №4.** `Value::Text` не вводится: ввод редактора идёт через `ViewInbox` (S7), а не через `Args`. Набор вариантов `Value` — `Bool`, `Int`, `Str`, `Loc`, `List`.
- **v3 №5 + v5 №6.** Аргумент `side` принимает `left | right | active | passive` и передаётся как `Value::Str`.
  - Публичный API core: `pub enum SideRef { Left, Right, Active, Passive }` и `Args::side(&self, k: &str) -> Option<SideRef>`. `None` при отсутствии ключа или неизвестном значении.
  - `validate` принимает все четыре значения для аргументов `side`.
  - Перевод `Active`/`Passive` в `Side` делает вызывающий код относительно `AppState.active` (handlers в S1-08). Сам `Args::side` от `AppState` не зависит.
  - Доп. приёмка: `Args::side` разбирает все четыре значения без учёта регистра, неизвестное значение даёт `None`, а `validate` — `CmdError::Arg`.
- **v3 №14 + S0-decisions №4 (Fluent).** Поле для подписи keybar в `CommandMeta` не добавляется. Подписи живут в Fluent: `keybar-<id>`, фолбэк `cmd-<id>`.
  *Следствие:* комментарий к `CommandMeta::title` в S1.md («ключ перевода = `cmd.<id>`») устарел. Ключи — `cmd-<id>`, где точки id заменены дефисами (пример S2 §7: `cmd-ops-copy` для `ops.copy`): идентификаторы сообщений Fluent не допускают `.`. В S1-04 менять только doc-комментарий, функцию маппинга id→ключ делает i18n (S2-01/S2-02).

Команды, утверждённые в v3 №4 и v5 №19 (`panel.cursor.match`, `panel.mode`, `viewer.*`, `editor.*`, `search.*`, `arc.*`, `ops.compare {recursive?}` и т.д.), регистрируют их крейты: core — в S1-08, остальные — S3, S5, S7, S8. Команды `job.retry/priority` (v2 №8) — в S1-09. Сама модель команд от них не меняется.

---

## S1-05 Runtime и сообщения

- Цель: полосы-пулы, `spawn_cpu` (rayon global), `spawn_dedicated`, `MsgSink` (bounded, send с cancel), `WakeFlag`, `Waker`, `msg()`, `catch_unwind`, shutdown.
- Крейт: core.
- API: §2.5 (Runtime, Msg, MsgSink, TaskCx, Lane, RuntimeConfig, Waker).
- Приёмка:
  - Паника в задаче не уменьшает число живых потоков.
  - 10 000 `send` дают ≤ число pump-циклов вызовов `wake()` (коалесцирование).
  - Send на полный канал разблокируется при cancel ≤60 мс и при shutdown.
  - Shutdown с зависшей задачей укладывается в таймаут.
  - Потоки имеют имена.
- Тест: unit с `TestWaker` (счётчик), loom не нужен.
- Зависит: 03.

**Дельты контрактов**
- **v3 №7 + v5 №6.** Будить UI без `Msg`:
  - `MsgSink::poke(&self)` и `Runtime::waker(&self) -> Arc<dyn Waker>`, сигнатуры утверждены. `waker()` возвращает тот же waker, что передан в `Core::new`.
  - `poke()` идёт через тот же `WakeFlag`, что и `send`: `Waker::wake()` вызывается только при переходе false→true. Без аллокации и без занятия канала.
  - Применение: терминал S6 читает `TermShared` напрямую и будит кадр до 60 раз/с (S6 §1.1, §11 №1).
  - Доп. приёмка: 10 000 `poke()` между двумя pump дают ≤1 `wake()` (`TestWaker`). `poke()` после закрытия на shutdown не паникует.
- **v2 №7 (документация).** `Lane::Bulk` — только короткие задачи. Долгие воркеры (ops, watcher, PTY) запускаются через `spawn_dedicated`, а ops сам ограничивает свои потоки `ops.max_threads`. В S1-05 зафиксировать это в rustdoc у `Lane::Bulk`, `Runtime::spawn` и `Runtime::spawn_dedicated`. Поведение кода не меняется.

Других дельт к S1-05 нет. Поля `Progress` (v2 №9) и `JobSpec` (v2 №8) относятся к S1-09. Стабильные имена tracing-спанов (v5 №10) к Runtime не относятся: это S1-12/S1-13 и S9.

---

## Следующие пакеты

- **S1-06 Модель состояния** — см. specs/S1.md, дельты: v2 №10 (`AppState.ext`), №15 (`Banner.actions`); v3 №1 (`Focus`, `OverlayKind`), №3 (`Cursor.top` меняет `rev`), №4 (поле `Tab.mode`), №6 (`ViewMode::Custom(u8)`); v5 №1 (`OverlayKind::Find`).
- **S1-07 Шина Core** — см. specs/S1.md, дельты: v2 №13 (`Handler::on_quit`, `QuitDecision`), №14 (дедуп `panel.refresh` по tab в пределах pump); v5 №4 (`ViewInbox` разбирается в начале pump; механизм хука согласовать с S7).
- **S1-08 Core-команды и навигация** — см. specs/S1.md, дельты: v2 №1 (`Chunk{entries,visible}`), №2 (`Patch`), №3 (`seq`, `arrange_seq`), №4 (`tab_event`, `TabEvent`); v3 №3 (`panel.viewport{side,rows,cols,top}`, `panel.cursor.left/right`), №4 (новые команды panel/sel), №5 (резолв `side`), №9 (`panel.enter` → `file.open`); v5 №6 (резолв `SideRef`), №9 (`OpenResolver`, `Services.open_resolvers`).
- **S1-09 Jobs и Prompts** — см. specs/S1.md, дельты: v2 №8 (`JobSpec.priority/owner_ns`, `set_state`, `job.retry/priority`, семантика `Waiting`), №9 (`Progress.phase/total_final/parked`), №16 (`PromptSpec.detail`, `PromptDetail::Conflict`); v5 №7 (`PromptSpec.secret`).
- **S1-10 Config и Paths** — см. specs/S1.md, дельты: S0-decisions «Изменения задач» (`General.language` = BCP-47), №1 (профиль ОС, portable по `urfin.portable`).
- **S1-11 Session** — см. specs/S1.md, дельты: v3 №4 (`Tab.mode` и закладки `panel.bookmark` хранятся в сессии); S0-decisions №2 (восстановление сессии — да); v5 №8 (`search:`/`cmp:` не сохраняются, таб восстанавливается к реальному каталогу).
- **S1-12 Tracing и паники** — см. specs/S1.md, дельты: v2 №15 (crash-баннер через `Banner.actions`); v5 №10 (стабильные имена спанов `startup.*`).
- **S1-13 Окно eframe и цикл кадра** — см. specs/S1.md, дельты: v3 №2 (`raw_input_hook` отдан keymap), №10 (`event_loop_builder`, winit без дубля), №13 (`zoom_with_keyboard = false`, ввод при фокусе на панели); v4 №2 (IME в Cmdline/Terminal/оверлеях); v5 №2 (IME: `Editor` вкл, `Viewer` выкл), №3 (слои с view S7), №5 (`app::sys`), №10 (`--bench-startup`, `--bench`, спаны `frame`).
- **S1-14 Перф-база и ADR-сверка** — см. specs/S1.md, дельты: сверка кода с v2–v5 и S0-decisions; пороги регрессий v5 («Зависимости и политика»).
- **S1-15 Single-instance** — см. specs/S0-decisions.md «Изменения задач» (задачи нет в S1.md), дельты: S0-decisions №3 (single-instance, `--new-instance`).
