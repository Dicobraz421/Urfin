# Колонки схемы: `ColumnSpec`, `Entry.ext` — ячейки backend'а

(владелец, 2026-11-01: «вместе с F03-05 ввести общий механизм колонок ФС», класс B одобрен) Уточнение v6 №1 и №21 для реализации; публичный `Entry` не меняется (≤64 байт).

- `Vfs::columns() -> &'static [ColumnSpec]` — колонки после имени, слева направо; пусто — колонки вида панели (Full, Wide, наборы). `ColumnSpec { id, title /*ключ Fluent*/, width, right, view: ColumnView::{Text, Size, Date, Bar}, mono }`; `mono` — моноширинный текст (права `drwxr-xr-x`).
- Встроенные `id` берут данные из `Entry`: `size`, `mtime`, `ctime`, `atime`; вид `Bar` (`share`) — доля размера строки в сумме каталога (`size / parent_total`). Прочие `id` — «`Entry.ext`»: провайдер ячеек схемы `Vfs::ext(dir, entry, col) -> Option<ExtCell>` (`Text`/`Size`/`Date`). Зовётся из UI-потока на видимых строках, поэтому только память backend'а, без I/O: `sftp` кладёт права и владельца из `longname` при листинге.
- Реестр backend'ов для UI — `urfin_vfs::Schemes(Arc<VfsRegistry>)` в `AppState.ext` (кладёт handler `vfs`).
- Флаги `Caps::{READ_ONLY, SEQUENTIAL, VIRTUAL}` (v5 №14) заведены константами; `du:` — `VIRTUAL`.
- На механизме: `du:` (Доля · Размер · Изменён, раньше — `ui::columns::for_tab`) и `sftp:` (Размер · Изменён · Права · Владелец).
