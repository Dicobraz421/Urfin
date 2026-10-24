# TASKS-M1 — пакет задач вехи M1 «Навигация» (сессии S4, S2, S3)

> **Архив** (2026-10-24, pets-dev R-07): открытые задачи — карточки [docs/tasks/](tasks/README.md); этот файл — история и порядок, не правится.

Пакет: S4 (VFS и листинг) → S2 (UI) → S3 (хоткеи), по решению владельца 2026-10-17 первым идёт S4. Текст задач — в specs/S4.md §8, specs/S2.md §9, specs/S3.md §6, здесь не дублируется. Блок «дельты» перечисляет правки из S0-contracts-v2…v5, S0-decisions и ADR, относящиеся к задаче; они имеют приоритет над текстом задачи (CLAUDE.md «Где правда»). Пометка «следствие» — вывод сборщика пакета, а не текст дельты; если следствие расходится с ADR, прав ADR, вопрос — владельцу.

Ветки: `s4/NN-кратко`, `s2/NN-кратко`, `s3/NN-кратко`. Заголовок PR — Conventional Commits. Окно и ввод — с меткой `win-manual` и чек-листом.

Сокращения: «Платформы» — S0-decisions «Платформы (обновлено)»; «Принцип» — «Принцип дизайна»; «бюджеты 10-16» — S0-decisions «Бюджеты старта и памяти» (2026-10-16).

## Порядок

**Заморозка снята 2026-10-21:** GPUI слит (#88, G-02…G-07), задачи ui/app идут на GPUI. Остаток M1: S2-20, S2-22, S3-11.

**Заморозка (ADR-0018, S0-decisions №55, 2026-10-19).** UI переезжает на GPUI — `docs/TASKS-GPUI.md`. Задачи M1 по `ui`/`app` на egui остановлены, кроме багфиксов, блокирующих работу:
- M1f: S2-20 (перф кадра; незаконченный стенд кадра и бенч — в `git stash` «S2-20 WIP»), S2-22 (запись настроек), S3-11 (матрица раскладок — переходит в G-03);
- вне M1: S2-19; R-02b (WARP на wgpu, issue #83) — снимается, рендер решает G-06.
Продолжаются core/vfs/ops/term и логика keymap: бэклог S4-08 + адаптер `notify`, issue #78.

Подпакеты (роадмап S1 → (S2 ∥ S4) → S3; macOS — в конец):
- **M1a, реальный листинг:** S4-01 → S4-02 → S4-04 → S4-18 → S4-03 → S4-05 → S4-06 → S4-07.
- **M1b, каркас UI:** S2-01 → S2-02 → S2-03 → S2-05 → S2-04 → S2-08 → S2-06 → S2-07 → S2-09 → S2-10 → S2-11 → S2-12 → S2-13 → S2-14.
- **M1c, хоткеи:** S3-01 → S3-02 → S3-03 → S3-04 → S3-05 → S3-09 → S3-06 → S3-07 → S3-10.
- **M1d, оверлеи UI:** S2-15 → S2-16 → S2-17 → S2-18 (после S4-10).
- **M1e, watch и кэш:** S4-08 → S4-10 → S4-11 → S4-12 → S4-13 (polling + inotify) → S4-14 → S4-15 → S4-16 → S4-17 → S4-19. Решение владельца 2026-10-18: S4-08 (Linux fast path) и адаптер `notify` (inotify) — в бэклог после M1f; на Linux пока `std::fs` и опрос.
- **M1f, финал:** S2-21 → S2-20 → S2-22 → S3-11 (Windows).
- **Пассив macOS:** S4-09, FSEvents-часть S4-13, S3-08, mac-части S2-03/S2-06/S3-11.
- **Вне M1:** S2-19 (M2+).

Зависимости по спекам: S4 §8 — 01 → (02 ∥ 04 ∥ 18) → 03 → 05 → 06 → (07 ∥ 08 ∥ 09) → (10 ∥ 11) → (12 ∥ 13) → 14 → 15 → 16 → 17 → 19; S2 §9 — 01 → 02 → 03 → (04 ∥ 05 ∥ 08) → 06 → 07 → 09 → 10 → (11 ∥ 12 ∥ 13 ∥ 14) → 15 → 16 → 17 → 18 → 21 → 20 → 22; S3 §6 — 01 → 02 → 03 → 04 → 05 → (06 ∥ 07 ∥ 08 ∥ 09 ∥ 10) → 11. Межсессионные: S2-12 (меню chevron) ← S4-03; S2-14 (`KeyHints`) ← S3-04; S2-18 ← S4-10; S3-06 (отрисовка) ← S2-16; S3-10 (запись пресета) ← S2-22.

---

## S4 — VFS и листинг

- **S4-01 Каркас vfs и StdFs** — см. specs/S4.md, дельты:
  - v5 №15: владелец trait `Vfs` — S4; позже — `resolve`, `extract_plan` + `ExtractStream`, `Caps::{READ_ONLY, SEQUENTIAL, VIRTUAL}` (следствие: методы — с default `Unsupported` до S8).
  - v4 №1 + v5 №5: `unsafe` только в `vfs::sys`.
  - S0-decisions №8: на Windows скрытость — атрибут Hidden; `DOTFILE` ставится, но не скрывает.
  - «Платформы»: macOS-ветки — cfg-заглушки.
- **S4-02 Имена и пути** — см. specs/S4.md, дельты:
  - v2 №5: `ESCAPED_NAME`; путь к ОС — только `to_os` / `join_os`.
  - v2 №6: эталонная `normalize_local` для сверки с `Location::local`.
  - ADR-0009: uri и `%`-экранирование — в core, S4 не дублирует.
  - S1 §7.3: `Location::local` не делает путь абсолютным — это делает вызывающий (следствие).
  - v5 №12: манифест `longPathAware` — S9; verbatim-пути в S4 всё равно обязательны (следствие).
- **S4-03 Batcher и ListingService (базовый)** — см. specs/S4.md, дельты:
  - v2 №1: `Chunk{entries, visible}` уже в core — промежуточный `Chunk(Vec)` не нужен.
  - v2 №3: `LoadReq.seq` — в каждый `Order`. v2 №7: листинг на `Lane::Interactive`.
  - ADR-0010: `listing_msg(tab, generation, …)`, `LoadReq.generation`. v5 №10: спаны `list.*`.
  - S1 §7.2/§7.3: app на `FakeListing` (`demo_services`). Следствие: заменить в окне; `FakeListing` оставить для `--smoke` и тестов S2/S3.
  - Следствие: `VfsListing::new(reg, cfg, sender, rt)` из S4 §2.2 до `Core::new` не собрать (Runtime создаёт Core). Решение сборщика: `rt` приходит в каждый `load/arrange`, а `panel.refresh` (stale-кэш, S4-16) отправляется из задачи сообщением `msg(|cx| cx.dispatch(…))`; `CommandSender` в конструкторе не нужен.
  - Бюджеты 10-16: первый видимый кадр < 250 мс, табы грузятся под DWM cloak ≤ 300 мс.
- **S4-04 Natural sort и ключи** — см. specs/S4.md, дельты: S0-decisions №10 (natural по умолчанию; в `VfsCfg.natural_sort` «№5» = S0 №10).
- **S4-05 arrange: фильтр и сортировка** — см. specs/S4.md, дельты:
  - v2 №3 (`ArrangeReq.seq`, `Order.seq`, `arrange_seq` — в core). ADR-0010 (`ArrangeReq.generation`).
  - S0-decisions №8: hidden — Windows `HIDDEN` (в т. ч. с `SYSTEM`), dotfiles — только при `dotfiles_hidden_on_windows`. `FakeListing` считает `HIDDEN || DOTFILE` — эталон с него не брать (следствие).
  - S0-decisions №10.
- **S4-06 Геометрический пересорт при стриминге** — см. specs/S4.md, дельты: v2 №1 (`visible`), v2 №3 (устаревший `Order` по `seq` отбрасывает core), v5 №10 (спаны `list.*`).
- **S4-07 Windows fast path** — см. specs/S4.md, дельты: v2 №5 (непарные суррогаты → `ESCAPED_NAME`); S0-decisions №8; «Платформы» (перф только на стенде Windows 11); v5 №13 (`gen_tree` → `urfin-testdata`); v5 «perf-evidence» (JSON прогона в `docs/perf/runs/`).
- **S4-08 Linux fast path** — см. specs/S4.md, дельты: v2 №5; «Платформы»: Linux tier 2 — CI ubuntu, цель 70 мс без гейта (следствие); v5 №13.
- **S4-09 macOS fast path** — см. specs/S4.md, дельты: «Платформы» и «Все сессии» — пассив, конец очереди, только компиляция и неблокирующий CI; NFD-имена не решены (docs/macos-notes.md).
- **S4-10 stat, open, space, roots** — см. specs/S4.md, дельты: v2 №12 (`VolumeInfo` — API `urfin_vfs::volume` у S4, sys-часть S5-02; отдельной задачи нет — ближе всего эта, следствие; нужен S2-10 и S2-18); S0-decisions №13 (метаданные как Explorer, ACL — опция); «Платформы».
- **S4-11 Бенч-харнесс и базлайн** — см. specs/S4.md, дельты: v5 №13 (`gen_tree` → `urfin-testdata`, dev-dependency); v5 №11 (`dhat-heap`/`profile` заводит S9); v5 «Пороги регрессий» и `perf-evidence`; «Платформы» (только NTFS/ReFS); прогоны — через `xtask perf` (S9). **Вопрос владельцу** (следствие): цель §7 «peak RSS листинга 1M < 110 МБ» не помещается в бюджет процесса ≤ 200 МБ (бюджеты 10-16) при базе 161–163 МБ.
- **S4-12 Watcher: Windows backend** — см. specs/S4.md, дельты: v2 №7 (поток — `spawn_dedicated`), v2 №14 (debounce 50 / max 250 мс — в hub), «Платформы».
- **S4-13 Watcher: notify (macOS/Linux) и polling** — см. specs/S4.md, дельты: «Платформы» (FSEvents — пассив; inotify — tier 2); v2 №7. Следствие: polling нужен и на Windows (UNC/SMB → poll) — эту часть до S4-14. Сделано: polling (`vfs::Poller`, PR S4-13); адаптер `notify` (новая зависимость — согласовать с владельцем) — в конце M1e вместе с S4-08.
- **S4-14 WatchHub, debounce, refresh-путь** — см. specs/S4.md, дельты: v2 №4 (`tab_event`/`TabEvent` в core), v2 №14 (дедуп `panel.refresh` по табу в pump; debounce 50/250 мс), v2 №7, ADR-0010 (подписка по `(TabId, generation)`). Следствие: refresh — через `cx.dispatch` в `Msg` (см. S4-03).
- **S4-15 Инкрементальный Patch** — см. specs/S4.md, дельты: v2 №2 (`Patch` в core правит выделение, stats, курсор, `pending_focus`; отбрасывается при несовпадении generation/len/seq), v2 №3, ADR-0010 (`DirIndex` на `(tab, generation, len)`), v5 `perf-evidence`.
- **S4-16 Кэш каталогов** — см. specs/S4.md, дельты: v2 №4 (`TabEvent::Left{entries: Arc}` без копирования); S0-decisions №9 (мгновенно + тихое обновление); бюджеты 10-16: кэш 200k ≈ 10–14 МБ входит в «свою память» < 50 МБ (следствие).
- **S4-17 Асинхронный размер каталогов** — см. specs/S4.md, дельты: v2 №2 (`Patch.update`), v2 №5 (`SIZE_KNOWN`), v2 №8 (`JobSpec{priority, owner_ns: "vfs"}`; реестр задачу не запускает — полосу выбирает handler, S1 §7.3), v2 №7, v5 №14 (общий обходчик `urfin_vfs::walk` — S4, отдельной задачи нет, следствие), S0-decisions №6 (по команде; авто для SSD — опция). Сделано: `urfin_vfs::walk`, handler `vfs` (`vfs.dir_size`), потоки по типу тома; дедуп хардлинков на unix — вместе с S4-08 (`statx` даёт `nlink`/`ino`).
- **S4-18 Классы иконок** — см. specs/S4.md, дельты: S0-decisions №7 (≈ 15 классов, Segoe Fluent Icons, режим «без иконок»), №32 (архив / исполняемые / временные — согласовать с `Archive`/`Exec`, следствие); следствие: enum сверить с таблицей S2 §1.6 (Parent, оверлей Cloud).
- **S4-19 Финальный перф-проход и сравнение с Explorer** — см. specs/S4.md, дельты: «Платформы» (APFS и Finder не делаем); бюджеты 10-16; v5 пороги и `perf-evidence`; сверка спеки с кодом по образцу S1 §7 (следствие).

## S2 — Дизайн и UI

- **S2-01 Крейт urfin-i18n** — см. specs/S2.md, дельты: v3 №8 (крейт листовой); v3 «Зависимости»: одобрены только `fluent-bundle` и `unic-langid` — `fluent-langneg`, `sys-locale`, `jiff`, `arrayvec`, `itoa` из S2 §8 проверить по deny/лицензиям; `arc-swap` удалён в S1-14 — вернуть, если нужен; v3 №14 (`keybar-<id>` с фолбэком `cmd-<id>`); S0-decisions №4 (EN+RU, lint на сырые строки); `General.language` = BCP-47.
- **S2-02 i18n-линты и каркас ui** — см. specs/S2.md, дельты: ADR-0011 (`show(&mut self, root: &mut egui::Ui, …)`, pump — в `App::logic`); v3 №2 + №13 (ввод только через `raw_input_hook`, в `Focus::Panel` нет фокусируемых виджетов); v2 №10 (`UiState` в `AppState.ext`); v3 №1 (`UiState.overlay` ↔ `Focus::Overlay(OverlayKind)`); S1 §7.2 (клавиши пока в `App::raw_input_hook` — до S3); S1 §7.3 (`CommandId` 2–3 сегмента ≤ 24 байт; прежний кадр панели ≤ 150 мс); S0-decisions №4. Следствие: набор `ui.*` в S2 §3 расходится с S3 §3/§7 №5 (`ui.jobs`, `ui.split`, `ui.zoom`, `ui.keybar.toggle`, `ui.palette`, `ui.help`, `ui.drives`…) — согласовать до S3-09.
- **S2-03 Системный probe** — см. specs/S2.md, дельты: v4 №1 (`unsafe` в `ui::sys`, `windows-sys` в ui); «Принцип» (акцент и тема ОС); «Платформы»; бюджеты 10-16: probe параллельно `Core::new`, первый видимый кадр под DWM cloak (следствие).
- **S2-04 Шрифты и иконки** — см. specs/S2.md, дельты: v4 №3 (вшиты JetBrains Mono и подмножество Fluent System Icons; Segoe — с диска), v4 №4 / v3 №12 (моно — JetBrains Mono), S0-decisions №7, №16–20, «Принцип»; v5 №12 (манифест `PerMonitorV2` — сверить DPI); v3 №13 (`zoom_with_keyboard = false` уже ставит app); лицензии шрифтов — в `crates/ui/assets/fonts/`; шрифты — внутри спана `startup.fonts`; ADR-0012: бинарь уже 13,4 МиБ — проверить `ci/size.py` (следствие).
- **S2-05 Fluent-токены и стиль egui** — см. specs/S2.md, дельты: S0-decisions №31 (помеченные — `bg-accent` + `text-accent`, курсор — рамка 1 px `border-stronger`; переопределяет мокапы и S2 §12 №1), №32 (мягкие цвета по типам по умолчанию, `[ui.colors]`; в S2 §4 — `[[ui.highlight]]`; переопределяет S2 §1.3 и §12 №2), №33 (Mica только на каркасе), «Принцип», №16–20.
- **S2-06 Окно: заголовок с табами, Mica, фолбэки** — см. specs/S2.md, дельты: v4 №1; S0-decisions №33, «Принцип»; ADR-0012: рендер wgpu DX12 — «нет мерцания (glow, vsync)» читать как DX12 `AutoVsync`, прозрачность для Mica проверить на swapchain DX12 (следствие); S1 §7.3: в `app::sys` уже DWM cloak, `DWMWA_TRANSITIONS_FORCEDISABLED`, фон класса = цвет панели — согласовать, не дублировать; v3 №10 (`event_loop_builder` — S3-07, subclass — S2, порядок задаёт app); «Платформы». Ссылка «§11 №2» в «Зависит» — ошибка, это §10 «S3».
- **S2-07 Раскладка и Canvas** — см. specs/S2.md, дельты: v3 №11 + v5 №3 (слои: терминал → панели → cmdline → вьювер/редактор → оверлеи); S0-decisions №27; ADR-0011; `WindowGeom.split` пока 0,5 (S1 §7.3); «Принцип» (keybar обязательна).
- **S2-08 Форматирование ячеек** — см. specs/S2.md, дельты: v2 №5 (`SIZE_KNOWN`); «Принцип» (КБ/МБ/ГБ, tabular); S0-decisions №13; v5 №18 (VIRTUAL-панели — M5, следствие); v5 №11 (`count-alloc` — S9, до того свой тестовый `GlobalAlloc`).
- **S2-09 RowCache** — см. specs/S2.md, дельты: ADR-0010 (`CellKey { tab, generation, … }`), v2 №2 (`Patch.update` → промах только изменённых), v5 №11.
- **S2-10 FileList: Таблица (Full) и Wide** — см. specs/S2.md, дельты: v3 №3 (`panel.viewport{side, rows, cols?, top?}`, `rev` — только при изменении), v3 №13, S0-decisions №31, №32, №7, «Принцип», v2 №5; S1 §7.3 (`ScrollToCursor` не шлётся; удержание прежнего кадра ≤ 150 мс сохранить); ADR-0011; v5 №18 (M5).
- **S2-11 FileList: Список (Brief) и Medium** — см. specs/S2.md, дельты: v3 №3 (`cols`, `panel.cursor.left/right` — в core), S0-decisions №31.
- **S2-12 Breadcrumb** — см. specs/S2.md, дельты: ADR-0009 (архивный `Location` показывать декодированно); v4 №2 (IME в поле пути; app включает IME в оверлеях, кроме Menu); S1 §7.3 (относительный ввод — от `Tab.loc`); «Принцип»; v5 №18 (M5); меню chevron ← `Vfs::list` (S4-03).
- **S2-13 Табы в заголовке** — см. specs/S2.md, дельты: «Принцип» (набор табов на панель), S0-decisions №24, S1 §7.3 (`tab.goto/move`: `n` с 1, есть `side`), v3 №5.
- **S2-14 Keybar и меню F9** — см. specs/S2.md, дельты: v3 №14; «Принцип» (keybar обязательна, а S3 §3 привязывает `ui.keybar.toggle` — конфликт); S0-decisions №21, №22; v3 №1 (`Overlay(Menu)`); `KeyHints` реализует S3-04 (`Keymap::keybar`, `KeybarItem.label_key`), до того заглушка. TODO (R-01): пункт «О программе» — имя `ui-about-name` (Úrfionn) и пасхалка `ui-about-aka` (S0-decisions №49), ключи уже в Fluent.
- **S2-15 Баннеры: промпты, черновики, конфликт** — см. specs/S2.md, дельты: v3 №15 (`ops.draft`), №16 (`abort`/`q`, Esc = «позже»), №17 (`ops.mkdir {current?}`); v2 №16 (`PromptDetail::Conflict`), v2 №9 (`parked` → «ещё N»); v5 №7 (`PromptSpec.secret`); v4 №2 (IME в черновиках); S0-decisions №11; S1 §7.3: `prompt.cancel` отменяет задачу, «Позже» = действие S2; без маски — `UiEvent::Ext{sel.ask_mask / panel.ask_filter}`, а S2 §10 предлагает `ui.draft` — выбрать один механизм; вето выхода ставит `Focus::Prompt`; v5 №18 (M5).
- **S2-16 Rename по месту и quick search** — см. specs/S2.md, дельты: v4 №2; v3 №4 (`panel.cursor.match{mask, dir}` в core); v3 №13; v4 №5 (поиск по `Text`); S0-decisions №25. Следствие: `UiState.qsearch` дублирует `KeymapView.qs` (S3-06) — один владелец.
- **S2-17 Job-бар, тосты, баннер сбоя** — см. specs/S2.md, дельты: v2 №9 (`phase`, `total_final` → `≤`/`≥`, `parked`), v2 №8 (`Waiting`), v2 №15 (`Banner.actions` ≤ 3); S0-decisions №14 (относится к операциям S5; баннер падения приложения — «Показать логи» / «Закрыть», S1-12); S1 §7.3 (`app.dismiss_banner`), §7.1 (`retire`, `KEEP_FINISHED = 64`).
- **S2-18 Дерево, Инфо, Quick view** — см. specs/S2.md, дельты: v5 №3 (Quick view рисует `ui::view::quick` S7, `PreviewProvider` снимается; в M1 — текст/hex без trait, следствие); v3 №4 (`Tab.mode`); v2 №12 (`VolumeInfo` — S4-10); S0-decisions №13; «Принцип» (без превью и миниатюр).
- **S2-19 Custom columns (M2+)** — см. specs/S2.md, дельты: v3 №6 (`ViewMode::Custom(u8)` в core), S0-decisions №2. Вне M1.
- **S2-20 Перф кадра и аллокации** — см. specs/S2.md, дельты: v5 «Пороги регрессий» (CI 3 % инстр./аллок., 20 % A/B; стенд 10 % при CV ≤ 5 % — заменяют «CI ≤ 6 мс + 20 %» из S2 §6.9), `perf-evidence`, v5 №10 (спан `frame` уже в app), v5 №11; бюджеты 10-16; S0-decisions №33; ADR-0012: мерить на DX12.
- **S2-21 Снапшоты всех мокапов** — см. specs/S2.md, дельты: S0-decisions №31/№32 (снапшоты по новым стилям, не по мокапам S2 §2); `FakeListing` — feature `fake` в core; «Платформы» (скриншоты только Win11).
- **S2-22 Запись настроек из UI** — см. specs/S2.md, дельты: S0-decisions №1 (профиль ОС, portable); v2 №7 (Bulk). Следствие: `settings::set` нужен S3-10 для `keymap.preset`.

## S3 — Хоткеи

- **S3-01 Каркас и типы клавиш** — см. specs/S3.md, дельты: v4 №5 (латиница раскладки — по символу, иначе — по физической позиции US-QWERTY; `Stroke` несёт обе формы, следствие); v3 №13 (имена — `egui::Key::name()`).
- **S3-02 When-выражения и флаги** — см. specs/S3.md, дельты: v3 №1 (`Focus::Overlay(OverlayKind)` — enum, а не `Overlay(CompactString)` из S3 §7 №1), v5 №1 (`OverlayKind::Find`), v5 №2 (в `Editor` — только `editor` + `global`), v3 №2 (в Overlay/Cmdline/Terminal — свой контекст + `global`); `Focus::Prompt` при вето (S1 §7.3).
- **S3-03 Формат TOML и слои** — см. specs/S3.md, дельты: v4 №5 (`keymap.match = "auto" | "physical" | "logical"` — в S3 §2.1 нет `logical`); S0-decisions №21 (пресет far), №1; S1 §7.3 (`Value::Loc` пишется uri-строкой, читается как `Str`; `CommandId` 2–3 сегмента).
- **S3-04 Компиляция и поиск** — см. specs/S3.md, дельты: v3 №14 (`KeybarItem.label_key = "keybar-<id>"`), v5 №11, v5 «Пороги».
- **S3-05 KeyInput: конвейер событий** — см. specs/S3.md, дельты: v3 №2 (единая точка `raw_input_hook`), v3 №13 (печать → `cmdline.insert`); v4 №2, v5 №2 (IME — в app `ime_allowed`); v4 №5 (Alt+буква по `Text`, приоритет у биндинга); S0-decisions №25. S1 §7.2: сейчас `hard_key`/`take_keys` в app (Space = Insert, Shift+движение = `sel.move`, ввод пропускается при `Focus != Panel` и активном промпте) — заменить на `KeyInput::process`, поведение перенести в пресеты (следствие).
- **S3-06 Быстрый поиск и одиночный Alt** — см. specs/S3.md, дельты: v3 №4 (`panel.cursor.match` в core), v2 №10 (`KeymapView` в ext), v4 №5, S0-decisions №21 (auto: far → `alt`, dn → `alt_tap`); пересечение с S2-16 по `qsearch` (следствие).
- **S3-07 Windows: KeyNotes-хук** — см. specs/S3.md, дельты: v3 №10 (winit строго версии eframe — 0.30.13), v5 №5 (`unsafe` в `app::sys`); S1 §7.3 (точка `event_loop_builder` есть, хук пустой; в `app::sys::windows` уже cloak/DWM/EventLog); v4 №5; «Платформы» (`win-manual`).
- **S3-08 macOS: меню, Option, Cmd** — см. specs/S3.md, дельты: S0-decisions №23; «Платформы» (пассив, без ручной приёмки на Mac); v2 №13 (вето для Cmd+Q; docs/macos-notes.md: Cmd+Q, возможно, без `on_exit`), v3 №10, v5 №5.
- **S3-09 Пресеты base/far/dn** — см. specs/S3.md, дельты: S0-decisions №21 (Far по умолчанию + выбор [Far]/[DN] при первом запуске, баннер через `Banner.actions`), №22, №24, №25, №11, №34, №35, №36, №39; v5 №19 (утверждённые id и привязки); v3 №4, №17, №18, №14; v4 №5. Конфликты: keybar обязательна против `ui.keybar.toggle`; имена `ui.*` с S2 §3.
- **S3-10 Handler keymap и горячая перезагрузка** — см. specs/S3.md, дельты: v2 №15 (предупреждения → `Banner.actions`), S0-decisions №1, №2, v2 №7 (`keymap.dump` на Bulk). Следствие: запись `[keymap] preset` — после S2-22.
- **S3-11 Матрица платформ и регрессия ввода** — см. specs/S3.md, дельты: v4 №5 (матрица раскладок en-US, ru, uk, el, he, ar, ja, zh-Pinyin, ko, Dvorak, AZERTY, QWERTZ для Ctrl+C/V/A/Q/U/O/R, Alt+F7, Ctrl+[ ], Alt+буква; автотест + ручной чек-лист Windows); «Платформы» (Mac не делаем, Linux tier 2). Следствие: сюда же upstream-PR в egui-winit (numpad).

---

## Замечания сборщика

- **Нумерация в колонках «Откуда» дельт.** v2 «S4-1…S4-6» — номера запросов S4 §9, не задачи; v3 «S2-1…S2-5» — запросы S2 v1, их заменили v4 №1–4. В S4 «S0 №1–5» = v2 №1–5 (все уже в core); `VfsCfg` «решение №3 / №5» = S0-decisions №8 / №10; S4 §10 №1/№2/№4 → S0 №6/№7/№9; S3 §8 №1–5 → S0 №21–25; S2 §12 №1–3 → S0 №31–33.
- **Работы без задачи** (закреплены сборщиком): v2 №12 `VolumeInfo` → S4-10; v5 №14 `urfin_vfs::walk` → S4-17; upstream-PR egui-winit → S3-11; подключение `KeyInput` в `App::raw_input_hook` и удаление `hard_key` → S3-05 (бинды — S3-09).
- **S1-15 (single-instance)** — PR #21, ждёт ручной приёмки владельца; задачи M1 от него не зависят.
- **Вопрос владельцу** (к S4-11): память листинга 1M (§7 S4: < 110 МБ) против потолка процесса ≤ 200 МБ при базе 161–163 МБ.
