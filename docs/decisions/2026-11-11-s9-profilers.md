# S9: Tracy и dhat — только под features; манифест exe — свой вместо встроенного в gpui

(владелец, 2026-11-11, класс B) OK на карточку S9-29: профилировщики подключаются только под features и в выпуск не попадают; встроенный манифест gpui выключаем в пользу своего (S9-17). Исполняет №58 ([S0-decisions](../specs/S0-decisions.md)) и [S9 §4](../specs/S9.md).

- `tracing-tracy` и `tracy-client` (спаны и кадры, S9-10) — в app optional под feature `profile`; `dhat` (профиль кучи, S9-09) — под `dhat-heap`. Порт Tracy — только localhost (`only-localhost`), сети в продукте по-прежнему нет.
- В release-графе `urfin-app` этих features и крейтов нет — проверяет `ci/release_features.py`.
- Манифест exe — свой, `crates/app/urfin.manifest` (S9-17: `longPathAware`, `activeCodePage` UTF-8 и всё, что даёт манифест gpui). Встроенный манифест gpui (`gpui-pre`, feature `windows-manifest`) в exe не остаётся, иначе два манифеста конфликтуют.
- Проверка S9-29 (dev-сборка, `mt.exe -inputresource:urfin.exe;#1`): в exe манифест gpui — `asInvoker`, Windows 10, `PerMonitorV2`, `SegmentHeap`, Common-Controls 6. Без форка он не выключается: `gpui-pre-platform` под Windows включает `windows-manifest` явно, а `gpui-kit`, `gpui-base`, `gpui-component` и `gpui-kit-assets` берут `gpui-pre` с features по умолчанию. Как его убрать, решает владелец до S9-17.
