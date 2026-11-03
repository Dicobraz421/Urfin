# Модель «Пакетов» — в core, UI без зависимости от `urfin-pkg`

(владелец, 2026-11-02, класс B; «Модель Пакетов тоже в core — вынести так же, как подключения, отдельной волной») В F02-08 `crates/ui` стал зависеть от `urfin-pkg`: вкладка `pkg:` рисовалась по `PkgBook` и `PkgState`. Теперь, как у подключений ([решение «net-model-core»](2026-11-02-net-model-core.md)), всё, что видит UI, — в `urfin_core::pkg` чистыми типами, без процессов, реестра, разбора вывода менеджеров и I/O:

- Пакеты и менеджеры: `Mgr`, `PkgId` (текст `mgr:id`), `Package`, `Update`, `Hit`, `MatchKind`, `PkgOp`, область поиска `SearchIn`; адрес вкладки — `SCHEME`, `Filter`, `location`, `filter_of`.
- Вид вкладки: `View` (строки «Установленных» и «Обновлений» — `PkgRow`, `Rows`; поиск — `SearchView`, `SearchInfo`, `SearchRow`, `SearchKind`, `Answer`; ход действий — `Busy`), сводка `PkgInfo` (менеджеры и их ход `MgrLine`/`Run`, числа, баннер «важных», время проверки — Unix, нс).
- `PkgModel` в `AppState.ext`: `[pkg].order`, `PkgFeed` — опубликованный вид и `rev` (пишет книга `urfin-pkg` из потоков запросов, UI берёт готовый `Arc<View>`), запись фикстур сцен `--screens` (`trait PkgFixture`, реализует книга). Метаданные команд `pkg.*` — `urfin_core::pkg::COMMANDS`.

`urfin-pkg` наполняет модель: книга (`PkgBook`) сводит ответы менеджеров (§2.4) и публикует вид в `PkgFeed`; handler `pkg` ставит `PkgModel` в первом pump. Адаптеры, процессы, кэш `cache.json`, `managers.toml`, `check.json` и их форматы — у `urfin-pkg`, без изменений. `urfin-ui` читает только core; связывает pkg и core `urfin-app`.
