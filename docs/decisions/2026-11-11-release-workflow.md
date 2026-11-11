# Выпуск — по тегу `v*`: сборка, подпись minisign, GitHub Release

(владелец, 2026-11-11, класс B) Разрешено добавить `.github/workflows/release.yml`. Исполняет решение [«Обновление продуктов»](2026-11-11-self-update.md): программа ищет выпуск на GitHub Releases и проверяет подпись.

- **Триггер.** Тег `vX.Y.Z[-beta.N]` на main или ручной запуск с существующим тегом. Версия с дефисом — предварительный выпуск (бета-канал). Версия тега обязана совпадать с `version` в `Cargo.toml` — иначе выпуск красный.
- **Сборка** (windows) — как job `release` в `ci.yml`: release, гейт размера 50 МиБ, smoke, ConPTY, пакеты грамматик. Архив — `urfin-<версия>-windows-x64.zip`, файлы в корне, плюс `README.txt` (RU/EN) и маркер `urfin.portable`.
- **Публикация** (ubuntu, единственная с `contents: write`): подпись каждого `.zip` minisign (`.minisig`), `SHA256SUMS.txt`, `gh release create`; тело — `ci/release-notes.md`. Без секрета `UPDATE_SIGNING_KEY` выпуск красный, а не «без подписи».
- **Ключ** создаёт и хранит владелец: секрет `UPDATE_SIGNING_KEY`, переменная `UPDATE_PUBLIC_KEY` (строка `RW…`, та же, что вшита в программу). Порядок — pets `crates/pets-update/README.md`, раздел «Подпись в release-workflow».
- Тег `beta-*` в `ci.yml` (артефакт `urfin-Windows`) остаётся для внутренних сборок; выпуски для пользователей — только `v*`.
