# Свой манифест exe — заменой ресурса mt.exe в CI

(владелец, 2026-11-12, класс B) S9-17 (#671): gpui встраивает свой манифест (feature `windows-manifest`, без форка не выключается), второй ресурс #1 линкер не принимает (`CVT1100: duplicate resource`), `/MANIFESTINPUT` его не сливает.

- После `cargo build --release` в job `release` (`ci.yml`) и в `release.yml` ресурс #1 заменяется `crates/app/urfin.manifest` (ключи gpui + `longPathAware`, `activeCodePage` UTF-8): `mt.exe -manifest … -outputresource:urfin.exe;#1`.
- Шаг стоит до проверки размера, smoke и подписи minisign; затем `-inputresource` и проверка обоих ключей — без них сборка падает.
- Когда gpui позволит выключить свой манифест — перейти на встраивание из `build.rs`, шаг CI убрать.
