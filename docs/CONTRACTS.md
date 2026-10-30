# Урфин — межкрейтовые контракты (действующее состояние)

Один текущий документ (скилл pets-dev, R-05): сведены изменения S0-contracts v2–v6 поверх `specs/S1.md` §2 с учётом
отменённого позже (egui → GPUI, только Windows). Менять — только PR класса B (CODEOWNERS) с записью в «Изменения».
История версий — `docs/archive/S0-contracts-v2…v6.md`; ссылки вида «v3 №5» в коде и документах ведут туда.

## Core: состояние и команды

- **Листинг.** `ListingUpdate::Chunk { entries, visible }` (core дописывает `visible` в `order` через `Arc::make_mut`); `ListingUpdate::Patch(Patch { base_len, seq, update, remove /*desc, swap_remove*/, append, order })` — применяется при совпадении gen/len/seq, правит selection, stats, держит курсор и `pending_focus`, иначе отбрасывается. `seq: u32` в `LoadReq`, `ArrangeReq`, `Order { order, for_len, seq }`; `Listing.arrange_seq` растёт на sort/filter/hidden. `ListingService::tab_event(TabEvent { Shown, Hidden, Closed, Left { tab, loc, entries } })`. Core дедуплицирует `panel.refresh` по табу в пределах pump. (v2 №1–4, №14)
- **Entry и пути.** `Attrs::ESCAPED_NAME = 1<<10`, `SIZE_KNOWN = 1<<11`, `SIZE_PARTIAL` (размер досчитывается, «≥ X»). Пути для ОС — только `vfs::name::{to_os, join_os}`. Нормализация `Location::local`: абсолютный; `\\?\C:\`→`C:\`, `\\?\UNC\s\sh`→`\\s\sh`; буква диска — заглавная; `/`→`\`; `.`/`..` лексически; без хвостового разделителя, кроме корня; регистр и symlink не трогать. `Location.outer` — глубина ≤ 8; `>>` в компонентах экранируется в `to_uri`. (v2 №5–6, v5 №8, v6 №7)
- **Расширения.** `AppState.ext: Extensions` — typemap `get/get_mut/insert`; мутации только в Handler/Msg; ключи — типы своих крейтов. Ext-состояние вкладок виртуальных схем (`DuState`, `PkgState`). (v2 №10, v6 №3)
- **Значения команд.** `Value::List(Arc<[Value]>)`, `Args::list/locs`, текст `src=["a","b"]`; `Args::side(k) -> Option<SideRef>` (`left|right|active|passive`). `Value::Text` не вводим. (v2 №11, v3 №5, v5 №6)
- **Фокус.** `Focus { Panel, Cmdline, Prompt(PromptId), Viewer, Editor, Terminal, Overlay(OverlayKind) }`, `OverlayKind { Menu, QuickSearch, InlineEdit, Draft, Palette, Find }`. Keymap поглощает в `Overlay`/`Cmdline` привязки своего контекста и `global`; в `Terminal` и `Prompt` — только своего контекста ([ADR-0015](adr/0015-global-context.md)); в `Editor` — `editor` + `global`, остальное — виджету. IME: Cmdline, Terminal, Editor и оверлеи с вводом; Viewer — без IME. (v3 №1–2, v4 №2, v5 №1–2)
- **Панель.** `panel.viewport { side, rows, cols, top }` (`top` — прокрутка, в Cursor, меняет `rev`); `panel.cursor.left/right/match {mask, dir}`, `panel.root`, `panel.mode {files|info|qview|tree}` (`Tab.mode`, в сессии), `panel.bookmark.set/go {n}`; `sel.move {motion}`, `sel.invert {dirs?}`, `sel.same {by, on}`, маска в `sel.add/remove` — опциональна. `ViewMode::Custom(u8)` — наборы колонок. (v3 №3–4, №6)
- **Задачи.** `JobSpec { kind, title, lane, priority: u8, owner_ns }`; `JobHandle::set_state(Queued|Running)`; `job.retry/priority {id}` — в handler `owner_ns`; `Waiting(PromptId)` — открыт вопрос, переходы повторяемы. `Progress`: `phase: AtomicU8` (`Scan, Transfer, Verify, Finalize, Recover`), `total_final`, `parked`; те же поля в `ProgressSnap`. (v2 №8–9)
- **Вопросы и баннеры.** `PromptSpec.detail: Option<PromptDetail>` (`Conflict { src, dst, index, total }`), `PromptSpec.secret` (маска, не логируется, не в историю/сессию). В конфликте «Прервать задачу» = `Choice { id: "abort", key: 'q' }`, Esc — «позже». `Banner.actions: SmallVec<[(CompactString, Command); 3]>`. (v2 №15–16, v3 №16, v5 №7)
- **Выход.** `Handler::on_quit(&mut self, cx) -> QuitDecision { Allow, Veto(PromptSpec) }`; `app.quit` опрашивает все handlers до `shutdown`. (v2 №13)
- **Runtime.** `Lane::Bulk` — короткие задачи; долгие воркеры (ops, watcher, PTY) — `spawn_dedicated`; ops ограничивает потоки `ops.max_threads`. `MsgSink::poke()` и `Runtime::waker() -> Arc<dyn Waker>` через общий `WakeFlag` — разбудить UI без Msg. (v2 №7, v3 №7, v5 №6)
- **Очередь устройств.** `urfin_core::DeviceGate` — одна на процесс (`Runtime::devices`, `Core::devices`): копирование, удаление и скан du ждут слот физического устройства по `[ops.device_jobs]` (`DeviceJobs`); ключ и лимит — `urfin_vfs::volume::{device_key, jobs_for}`. (решение 2026-10-30)
- **Открытие файла.** `panel.enter` → `file.open { alt?, internal?, elevated? }`; резолв — `trait OpenResolver` (`Services.open_resolvers`): S8 → `[file.assoc]` → `term.open`. (v3 №9, v5 №9)

## VFS и операции

- `VolumeInfo` (носитель SSD/HDD/NVMe/USB/Net, ФС, caps: reflink, case_sensitive, trash, free) — `urfin_vfs::volume`. Watcher: debounce 50 мс / max 250 мс; ops шлёт refresh ≤ 1/с на таб. (v2 №12, №14)
- Trait `Vfs` (владелец — S4): `resolve(base, item)`, `extract_plan` + `ExtractStream`, `Caps::{READ_ONLY, SEQUENTIAL, VIRTUAL}`; фаза 3 — `writer() -> VfsWriteTxn`. Колонки схемы: `Vfs::columns() -> &[ColumnSpec]` (`Text/Size/Date/Bar`), данные — `Entry.ext`; и для не-виртуальных схем (`sftp`: права, владелец). Обходчик `urfin_vfs::walk` и `walk_with(&dyn DirLister)`. (v5 №14–15, v6 №1, №8, №21)
- Схемы: `search:`, `cmp:`, `du:` (`/<snap_id>/<rel>`, `f=`), `pkg:` (`/installed|/updates|/search?…`) — в сессии не сохраняются; `sftp://<профиль>/путь` — таб сохраняется без подключения (`user@host:port` — только ввод). (v5 №8, v6 №5–6, №20)
- Ops: `ops.delete/copy/move` без `confirmed=true` → `UiEvent::Ext { id: "ops.draft" }`; `ops.copy/move/delete { current? }`, имя в `ops.mkdir` опционально, `file.clip.copy/cut/paste`; `Engine::Vfs` для не-локального источника; `ops.move` из `READ_ONLY` → черновик копирования; `ops.compare { recursive? }`. (v3 №15, №17, v5 №16, №19)
- Форматы на диске (фиксированы, смена — с `ver`): кэш `du` v1 (`%LOCALAPPDATA%\Urfin\state\du\*.du`); `connections.toml` v1, `state/ssh/…`, записи `*.cast` (asciicast v2); `~/.ssh/known_hosts` — только дописывание. (v6 №9, №26)

## Терминал (urfin-term)

- Слои окна снизу вверх: терминал → панели → cmdline → вьювер/редактор → оверлеи; видимость панелей не меняет размер PTY. (v3 №11, v5 №3)
- Ввод при фокусе на панели: печатный текст → `cmdline.insert`; в `Focus::Terminal` не поглощённое keymap → `term.key`/`term.input` (имена клавиш — gpui-kit `Keystroke`). (v3 №13, ADR-0018)
- Публичные безопасные API (`unsafe` — только `term::sys`): `term::proc::spawn_piped(argv, cwd, env, flags) -> PipedChild` (job object, `kill_tree`, `wait_timeout`), `session_has_process(name, since)`; `TermApi::feed_session(title) -> FeedSink`; `SessionKind::External { argv, env, title }`; `TermApi::broadcast(ids, bytes)`, `TermApi::tap(id) -> TapRx`; флаг «цвет по умолчанию» у ячейки снапшота. (v6 №15, №22)

## Ввод и клавиатура

- **Хоткеи с буквой — на любой раскладке и языке (обязательно).** Раскладка даёт латиницу на клавише — сравнение по символу (Dvorak, AZERTY, QWERTZ); иначе (кириллица, греческий, иврит, арабский, CJK, IME) — по физической позиции US-QWERTY; модификаторы не влияют; для всех контекстов и аккордов. Быстрый поиск и ввод в cmdline — по набранному тексту; при совпадении с привязкой — привязка. `keymap.match = auto|physical|logical`. (v4 №5)
- Подписи keybar — Fluent `keybar-<id>`, иначе `cmd-<id>`; поля в `CommandMeta` нет. (v3 №14)
- Пространства команд: `viewer.*`, `editor.*`, `search.*`, `arc.*` (v5 №19), `du.*` (`du.elevate` выключен до ADR-0021), `pkg.*`, `ui.utilities` — Ctrl+F9, `ssh.*`, `sftp.*`, `term.highlight.toggle`, `term.record.toggle`, `viewer.cast.*`. Флаги/контексты keymap: `in_search`, `in_archive`, `in_du`, `pkg`, `in_sftp`, `in_sshpair`, `ssh_term`, `ssh_group`, `cast_view`. (v6 №10–13, №25)
- Ввод редактора — `ViewInbox` (очередь UI → handler, разбирается в начале pump), не через Command bus. (v5 №4)

## Крейты, платформа, ресурсы

- Крейты: core, vfs, ops, term, keymap, i18n (листовой, Fluent en/ru; тосты и промпты — только ключами), ui, view, search, arc, testdata (dev); фаза 3 — `urfin-net` (без `unsafe`), `urfin-pkg` (без `unsafe`). (v3 №8, v5 №13, v6 №14, №23)
- UI — GPUI (gpui-kit, ADR-0018); только Windows (решение №57). `unsafe` — только в `sys` крейтов vfs, ops, term, ui, view, app. (v4 №1, v5 №5)
- Шрифты: Segoe UI Variable, Segoe Fluent Icons, моно — системный Cascadia Mono (→ Consolas), с диска ОС; рамки и Powerline терминал рисует сам. (владелец 2026-10-17; отменяет v3 №12, v4 №3–4)
- Манифест exe: `longPathAware`, `PerMonitorV2`, `activeCodePage=UTF-8` (`winresource`). Флаги app: `--bench-startup`, `--bench [каталог]`, `--screens <каталог>`, `--smoke`; ранний выход до GPUI — `--pkg-check`, `--askpass` (без окна и IPC единственного экземпляра). Features `profile`, `dhat-heap`, `count-alloc` — запрещены в release (CI). (v5 №10–12, v6 №17, №24)
- **Стабильные имена tracing-спанов — контракт:** `startup.*`, `frame`, `list.*`, `ops.job`, `term.build`, `view.*`, `du.*`, `pkg.*`, `net.*`, `ssh.term.spawn`, `term.record`, `term.highlight`, `viewer.cast.seek`; секретов в полях нет (тест). (v5 №10, v6 №19, №27)

## Политика и бюджеты

- Бинарь `urfin.exe` ≤ 30 МБ (растёт с функциями, №80; DLL ConPTY не входят). Пороги регрессий: CI — 3 % по инструкциям и аллокациям, 20 % A/B по времени; стенд — 10 % при CV ≤ 5 %; PR в горячие пути vfs/ops/term/ui-render — JSON прогона в `docs/perf/runs/`. Заголовки PR — Conventional Commits. (v5)
- Зависимости одобрены списками v3, v5, v6 (`alacritty_terminal`, `base64`, `memmap2`, `zip`, `sevenz-rust2`, `russh`, `tokio`, `keyring`, `windows-registry` и др.); прочие — без согласования при чистом `cargo deny` (решение №58).

## Изменения

| Дата | Версия | Что |
|---|---|---|
| 2026-10-24 | — | Сведено в один документ (R-05); старые версии — `docs/archive/`. |
| 2026-10-23 | v6 | F-01/F-02/F-03: колонки схем, `du:`, `pkg:`, `sftp`, API терминала, `urfin-net`/`urfin-pkg`, флаги `--pkg-check`/`--askpass`. |
| 2026-10-14 | v5 | S7–S9: `Find`, `ViewInbox`, `OpenResolver`, крейты search/arc/testdata, спаны, бюджеты и пороги. |
| 2026-10-14 | v4 | Fluent-оформление, `unsafe` в `ui::sys`, хоткеи на любой раскладке. |
| 2026-10-14 | v3 | Focus/Overlay, ввод через keymap, viewport, команды панели, i18n, слои окна. |
| 2026-10-14 | v2 | Листинг Chunk/Patch/seq, Entry, `Location::local`, задачи, Extensions, `on_quit`. |
