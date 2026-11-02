# Модель подключений — в core, UI без зависимости от `urfin-net`

(владелец, 2026-11-02, класс B) В F03-05 (#260) `crates/ui` стал зависеть от `urfin-net`: строки подключения рисовались по `NetState`. Теперь данные, которые нужны UI, — в `urfin_core::net` как чистые типы без сокетов, крипто, russh и I/O:

- `NetModel` в `AppState.ext`: профили, история хостов, `known_hosts`, ключи серверов профилей, разовые профили, сеансы (`Live`, `LiveState`, `Phase`), вопросы подключения (`Ask`, `AskKind`, `PromptKind`), строка «Новое подключение» (`Form`), `rev`, путь `connections.toml`, пинг (`trait Ping`, реализует backend `sftp`); подсказки «Хоста» — `NetModel::host_hints`.
- Типы профиля и формата: `Profile`, `Auth`, `ProfileSource`, `Note`, `History`/`HistoryEntry`, `PasswordManager` (+ шаблоны), `DEFAULT_PORT`, `SFTP_SCHEME`, `sftp_profile(loc)`, даты `datetime`/`unix`. Формат `connections.toml` v1 и `history.toml` не меняется.

`urfin-net` наполняет модель в handler `ssh`/`sftp` и его `Msg` (как `du`/`pkg` — своё ext-состояние); у себя (`NetState`) держит рантайм, пул, backend `sftp`, кэш паролей, файл профилей для записи и каналы: управление сеансом — по профилю, ответ на вопрос — по `Ask::id` (нет канала — «Отмена»). Чтение и запись файлов — задачами `urfin-net`. `urfin-ui` читает только core; связывает net и core `urfin-app`.
