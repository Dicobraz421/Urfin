# Реестр команд Urfin

> Генерируется из реестра команд (core + ui + vfs) тестом `commands_doc_is_current`
> (`crates/ui/legacy/tests.rs`). Руками не править. Обновить:
> `URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-ui commands_doc`.

Команда — имя функции Urfin. Её принимают меню F9 (`menu.toml`), keymap (S3) и командная
строка в одном виде: `id арг=значение …`, например `panel.sort key=size` или
`panel.go path="C:\Program Files"`.

Значения аргументов:
- `bool` — `true` / `false`;
- `int` — целое;
- `str` — слово или строка в кавычках, кавычка внутри — `""`;
- `loc` — путь или uri (`local:C:\work`, архив — `local:C:\a.zip>>zip:/in`);
- `list` — `[a, b]`.

`?` — необязательный аргумент. `side` — `left | right | active | passive`, по умолчанию активная панель.

**Проверка в runtime.** При загрузке `menu.toml` каждая команда проверяется по этому реестру. Id должен быть известен, аргументы — описаны и нужного типа, обязательные — заданы. Иначе пункт скрыт, а предупреждение показывается тостом.

**Клавиши** — пресет Far (заглушка до keymap S3). **Меню F9** — где команда стоит во вшитом `menu.toml`.

## `panel` (29)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `panel.cursor.up` | `n?:int` | Cursor up | Курсор вверх | — | — |
| `panel.cursor.down` | `n?:int` | Cursor down | Курсор вниз | — | — |
| `panel.cursor.page_up` | `n?:int` | Page up | Страница вверх | — | — |
| `panel.cursor.page_down` | `n?:int` | Page down | Страница вниз | — | — |
| `panel.cursor.left` | `n?:int` | Column left | Колонка влево | — | — |
| `panel.cursor.right` | `n?:int` | Column right | Колонка вправо | — | — |
| `panel.cursor.home` | — | First item | Первый элемент | — | — |
| `panel.cursor.end` | — | Last item | Последний элемент | — | — |
| `panel.cursor.to` | `name:str` | Go to name | Перейти к имени | — | — |
| `panel.cursor.match` | `mask:str`, `dir?:str` | Find by mask | Найти по маске | — | — |
| `panel.viewport` | `side?:str`, `rows:int`, `cols?:int`, `top?:int` | Viewport | Область просмотра | — | — |
| `panel.enter` | — | Open | Открыть | — | — |
| `panel.up` | — | Parent folder | Родительская папка | — | — |
| `panel.root` | — | Root folder | Корень диска | — | — |
| `panel.back` | — | Back | Назад | — | commands |
| `panel.forward` | — | Forward | Вперёд | — | commands |
| `panel.go` | `path:loc`, `side?:str` | Go to folder | Перейти в папку | — | — |
| `panel.refresh` | `tab?:int` | Refresh | Обновить | — | left, right |
| `panel.switch` | `side?:str` | Switch panel | Другая панель | Tab | — |
| `panel.swap` | — | Swap panels | Поменять панели местами | Ctrl+U | commands |
| `panel.focus` | `side?:str` | Focus panel | Перейти в панель | — | — |
| `panel.toggle` | `side:str` | Show or hide panel | Показать или скрыть панель | Ctrl+F1 (side=left), Ctrl+F2 (side=right) | — |
| `panel.sort` | `key:str`, `desc?:bool` | Sort | Сортировка | Ctrl+F3 (key=name), Ctrl+F4 (key=ext), Ctrl+F5 (key=mtime), Ctrl+F6 (key=size), Ctrl+F7 (key=unsorted) | left, right |
| `panel.view_mode` | `mode:str` | View mode | Вид | Ctrl+1 (mode=brief), Ctrl+2 (mode=medium), Ctrl+3 (mode=full), Ctrl+4 (mode=wide) | left, right |
| `panel.mode` | `mode:str`, `side?:str` | Panel mode | Режим панели | — | — |
| `panel.filter` | `mask?:str` | Filter | Фильтр | — | — |
| `panel.hidden.toggle` | — | Show hidden files | Показывать скрытые файлы | — | left, right |
| `panel.bookmark.set` | `n:int` | Set bookmark | Поставить закладку | — | — |
| `panel.bookmark.go` | `n:int` | Go to bookmark | Перейти к закладке | — | — |

## `selection` (9)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `sel.toggle` | `advance?:bool` | Toggle selection | Пометить | Ins | — |
| `sel.move` | `motion:str` | Select while moving | Пометить с движением | — | — |
| `sel.all` | — | Select all files | Пометить все файлы | — | files |
| `sel.none` | — | Clear selection | Снять пометку | — | files |
| `sel.invert` | `dirs?:bool` | Invert selection | Инвертировать пометку | Gray * | files |
| `sel.restore` | — | Restore selection | Вернуть пометку | — | files |
| `sel.add` | `mask?:str` | Select by mask | Пометить по маске | — | — |
| `sel.remove` | `mask?:str` | Deselect by mask | Снять пометку по маске | — | — |
| `sel.same` | `by:str`, `on?:bool` | Select same | Пометить такие же | — | — |

## `tab` (7)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `tab.new` | `side?:str` | New tab | Новая вкладка | Ctrl+Shift+T | commands |
| `tab.dup` | `side?:str` | Duplicate tab | Копия вкладки | — | commands |
| `tab.close` | `side?:str` | Close tab | Закрыть вкладку | Ctrl+Shift+W | commands |
| `tab.next` | `side?:str` | Next tab | Следующая вкладка | — | — |
| `tab.prev` | `side?:str` | Previous tab | Предыдущая вкладка | — | — |
| `tab.goto` | `n:int`, `side?:str` | Go to tab | Перейти на вкладку | — | — |
| `tab.move` | `n:int`, `side?:str` | Move tab | Переместить вкладку | — | — |

## `app` (4)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `app.quit` | — | Quit | Выход | F10 | — |
| `app.dismiss_banner` | — | Dismiss banner | Скрыть уведомление | — | — |
| `app.reload_config` | — | Reload configuration | Перечитать настройки | — | options |
| `app.save_session` | — | Save session | Сохранить сеанс | Shift+F9 | options |

## `job` (5)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `job.cancel` | `id:int` | Cancel task | Отменить задачу | — | — |
| `job.pause` | `id:int` | Pause task | Приостановить задачу | — | — |
| `job.resume` | `id:int` | Resume task | Продолжить задачу | — | — |
| `job.retry` | `id:int` | Retry task | Повторить задачу | — | — |
| `job.priority` | `id:int`, `p:int` | Task priority | Приоритет задачи | — | — |

## `prompt` (2)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `prompt.reply` | `id:int`, `choice:str`, `input?:str`, `all?:bool` | Answer | Ответить | — | — |
| `prompt.cancel` | `id:int` | Dismiss | Закрыть | — | — |

## `ui` (43)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `ui.field.remember` | `kind:str`, `text:str` | Remember field input | Запомнить ввод поля | — | — |
| `ui.complete` | `kind:str`, `text:str`, `history?:bool` | Complete field | Дополнить поле | — | — |
| `ui.complete.close` | — | Close completion | Закрыть дополнение | — | — |
| `ui.menu.sort` | `side?:str` | Sort menu | Меню сортировки | Ctrl+F12 | — |
| `ui.history.dirs` | — | Folder history | История папок | Alt+F12 | — |
| `ui.menu` | `menu?:str`, `side?:str`, `path?:loc` | Menu | Меню | F9 | — |
| `ui.utilities` | — | Utilities | Утилиты | — | — |
| `ui.qsearch` | `text?:str` | Quick search | Быстрый поиск | — | — |
| `ui.rename` | — | Rename in place | Переименовать на месте | — | files |
| `ui.path_edit` | `side?:str`, `path?:str` | Edit path | Ввести путь | — | left, right |
| `ui.draft` | `command:str`, `arg:str`, `init?:str` | Command argument | Аргумент команды | Gray + (command=sel.add arg=mask), Gray − (command=sel.remove arg=mask) | files |
| `ui.cancel` | — | Close | Закрыть | — | — |
| `ui.prompt.hide` | `id?:int` | Hide question | Свернуть вопрос | — | — |
| `ui.prompt.show` | `id?:int` | Show question | Развернуть вопрос | — | — |
| `ui.jobs.toggle` | — | Expand tasks | Развернуть задачи | — | — |
| `ui.drives` | `side?:str` | Drives | Диски | Alt+F1 (side=left), Alt+F2 (side=right) | — |
| `ui.zoom` | `delta?:int` | Zoom | Масштаб | — | options |
| `ui.split` | `delta?:int`, `reset?:bool` | Panel split | Раздел панелей | — | — |
| `ui.lang` | `lang:str` | Language | Язык | — | options |
| `ui.columns` | `set:str` | Columns | Колонки | — | — |
| `ui.panels.toggle_all` | `show?:bool` | Hide or show panels | Скрыть или показать панели | Ctrl+O | commands |
| `ui.copy_names` | `full?:bool` | Copy names | Копировать имена | — | — |
| `ui.tree.up` | — | Tree: up | Дерево: вверх | — | — |
| `ui.tree.down` | — | Tree: down | Дерево: вниз | — | — |
| `ui.tree.page_up` | — | Tree: page up | Дерево: страница вверх | — | — |
| `ui.tree.page_down` | — | Tree: page down | Дерево: страница вниз | — | — |
| `ui.tree.home` | — | Tree: first | Дерево: в начало | — | — |
| `ui.tree.end` | — | Tree: last | Дерево: в конец | — | — |
| `ui.tree.expand` | — | Tree: expand | Дерево: развернуть | — | — |
| `ui.tree.collapse` | — | Tree: collapse | Дерево: свернуть | — | — |
| `ui.tree.enter` | — | Tree: open | Дерево: открыть | — | — |
| `ui.tree.select` | `n:int` | Tree: select | Дерево: выбрать | — | — |
| `ui.theme.set` | `id:str` | Theme | Тема | — | options |
| `ui.theme.next` | — | Next theme | Следующая тема | — | options |
| `ui.theme.follow_os` | — | Theme as in Windows | Тема как в Windows | — | options |
| `ui.theme.reload` | `if_changed?:bool` | Reload themes | Перечитать темы | — | — |
| `ui.density.set` | `px:int` | Row height | Высота строки | — | — |
| `ui.icons.set` | `mode:str` | File icons | Иконки файлов | — | — |
| `ui.settings` | `section?:str`, `close?:bool` | Settings | Настройки | — | options |
| `ui.setting` | `key:str`, `value:str` | Change a setting | Изменить настройку | — | — |
| `ui.about` | — | About | О программе | — | options |
| `ui.welcome` | `kind?:str`, `from?:str` | Welcome | Приветствие | — | — |
| `ui.welcome.done` | `preset?:str`, `follow_os?:bool` | Close welcome | Закрыть приветствие | — | — |

## `vfs` (1)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `vfs.dir_size` | `all?:bool` | Directory sizes | Размер папок | — | commands |

## `du` (8)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `du.open` | `loc?:loc`, `drive?:bool` | Disk usage… | Анализ места… | — | utilities |
| `du.refresh` | `full?:bool` | Refresh disk usage | Обновить анализ места | — | — |
| `du.stop` | — | Stop disk usage scan | Остановить анализ места | — | — |
| `du.filter` | `kind:str` | Disk usage filter | Фильтр анализа места | — | — |
| `du.map` | — | Disk usage map | Карта занятого места | — | — |
| `du.verify_hash` | — | Verify duplicates by hash | Проверить дубликаты хэшем | — | — |
| `du.elevate` | — | Count as administrator | Досчитать с правами администратора | — | — |
| `du.old_age` | `age:str` | Old files age | Порог старых файлов | — | — |

## `ops` (15)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `ops.copy` | `dest?:str`, `current?:bool`, `confirmed?:bool`, `conflict?:str`, `verify?:str`, `filter?:str`, `queue?:str` | Copy | Копировать | F5, Shift+F5 (current=true) | files |
| `ops.move` | `dest?:str`, `current?:bool`, `confirmed?:bool`, `conflict?:str`, `verify?:str`, `filter?:str`, `queue?:str` | Move or rename | Перенести или переименовать | F6 | files |
| `ops.delete` | `permanent?:bool`, `current?:bool`, `confirmed?:bool` | Delete | Удалить | F8, Shift+F8 (current=true) | files |
| `ops.mkdir` | `name?:str`, `current?:bool` | Make folder | Создать папку | F7 | files |
| `ops.rename` | `to?:str` | Rename | Переименовать | Shift+F6 | — |
| `ops.multi_rename` | `pattern?:str`, `regex?:bool`, `apply?:bool` | Multi-rename | Групповое переименование | — | files |
| `ops.attrs` | `set?:str`, `clear?:str` | Attributes | Атрибуты | — | files |
| `ops.touch` | `mtime?:str` | Set time | Изменить время | — | — |
| `ops.link` | `kind?:str`, `dest?:str` | Make link | Создать ссылку | Alt+F6 | — |
| `ops.compare` | `mode?:str` | Compare folders | Сравнить папки | — | files, commands |
| `ops.sync` | `dir?:str` | Synchronize folders | Синхронизировать папки | — | commands |
| `ops.retry` | `id:int` | Retry failed | Повторить неудавшиеся | — | — |
| `ops.priority` | `id:int`, `prio?:str` | Operation priority | Приоритет операции | — | — |
| `ops.run_now` | `id:int` | Run now | Запустить сейчас | — | — |
| `ops.recover` | `journal:str`, `action:str` | Recover operation | Восстановить операцию | — | — |

## `file` (6)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `file.open` | `elevated?:bool` | Open | Открыть | — | — |
| `file.open_with` | — | Open with | Открыть с помощью | — | — |
| `file.reveal` | — | Show in Explorer | Показать в Проводнике | — | — |
| `file.clip.copy` | — | Copy to clipboard | Копировать в буфер | — | — |
| `file.clip.cut` | — | Cut to clipboard | Вырезать в буфер | — | — |
| `file.clip.paste` | `move?:bool` | Paste from clipboard | Вставить из буфера | — | — |

## `term` (25)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `term.toggle` | — | Show or hide panels | Показать или скрыть панели | — | — |
| `term.shell` | `id:str` | Switch shell | Сменить shell | — | — |
| `term.shell_menu` | — | Shell menu | Выбор shell | — | — |
| `term.new` | `shell?:str` | New session | Новый сеанс | — | — |
| `term.next` | — | Next session | Следующий сеанс | — | — |
| `term.prev` | — | Previous session | Предыдущий сеанс | — | — |
| `term.close` | `id?:int` | Close session | Закрыть сеанс | — | — |
| `term.restart` | — | Restart session | Перезапустить сеанс | — | — |
| `term.kill` | — | Kill process | Прервать процесс | — | — |
| `term.focus` | — | Focus terminal | Перейти в терминал | — | — |
| `term.input` | `text:str` | Send text | Отправить текст | — | — |
| `term.key` | `key:str`, `mods?:str` | Send key | Отправить клавишу | — | — |
| `term.scroll` | `lines?:int`, `pages?:int` | Scroll output | Прокрутить вывод | — | — |
| `term.copy` | — | Copy selection | Копировать выделенное | — | — |
| `term.paste` | `text?:str` | Paste | Вставить | — | — |
| `term.select_all` | — | Select all output | Выделить весь вывод | — | — |
| `term.clear` | — | Clear output | Очистить вывод | — | — |
| `term.open` | `path?:str`, `verb?:str` | Open file | Открыть файл | — | — |
| `term.exec` | `cmd:str`, `cwd?:str` | Run command | Выполнить команду | — | — |
| `term.mouse` | `kind:str`, `button?:str`, `row?:int`, `col?:int`, `right?:bool`, `mods?:str`, `clicks?:int`, `lines?:int` | Mouse in terminal | Мышь в терминале | — | — |
| `term.resize` | `cols:int`, `rows:int` | Terminal area size | Размер области терминала | — | — |
| `term.prev_prompt` | — | Previous prompt | Предыдущий промпт | — | — |
| `term.next_prompt` | — | Next prompt | Следующий промпт | — | — |
| `term.copy_output` | — | Copy last command output | Копировать вывод последней команды | — | — |
| `term.find` | `pattern?:str`, `back?:bool` | Find in output | Найти в выводе | — | — |

## `cmdline` (35)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `cmdline.insert` | `text:str` | Insert text | Вставить текст | — | — |
| `cmdline.cursor.left` | — | Cursor left | Курсор влево | — | — |
| `cmdline.cursor.right` | — | Cursor right | Курсор вправо | — | — |
| `cmdline.word.left` | — | Word left | Слово влево | — | — |
| `cmdline.word.right` | — | Word right | Слово вправо | — | — |
| `cmdline.home` | — | Line start | В начало строки | — | — |
| `cmdline.end` | — | Line end | В конец строки | — | — |
| `cmdline.backspace` | — | Delete left | Удалить слева | — | — |
| `cmdline.delete` | — | Delete right | Удалить справа | — | — |
| `cmdline.del.word_left` | — | Delete word left | Удалить слово слева | — | — |
| `cmdline.del.word_right` | — | Delete word right | Удалить слово справа | — | — |
| `cmdline.del.to_end` | — | Delete to end | Удалить до конца | — | — |
| `cmdline.clear` | — | Clear line | Очистить строку | — | — |
| `cmdline.undo` | — | Undo | Отменить | — | — |
| `cmdline.redo` | — | Redo | Повторить | — | — |
| `cmdline.exec` | `detached?:bool` | Run command | Выполнить команду | — | — |
| `cmdline.history` | — | Command history | История команд | — | — |
| `cmdline.history.prev` | — | Previous command | Предыдущая команда | — | — |
| `cmdline.history.next` | — | Next command | Следующая команда | — | — |
| `cmdline.complete` | — | Complete | Дополнить | — | — |
| `cmdline.complete_native` | — | Shell completion | Дополнение shell | — | — |
| `cmdline.insert_name` | `what:str`, `side?:str` | Insert name | Вставить имя | — | — |
| `cmdline.insert_selection` | — | Insert selected names | Вставить имена выделенных | — | — |
| `cmdline.apply` | — | Apply to selection | Применить к выделенным | — | — |
| `cmdline.copy` | — | Copy command line | Копировать строку | — | — |
| `cmdline.menu.up` | — | List: up | Список: вверх | — | — |
| `cmdline.menu.down` | — | List: down | Список: вниз | — | — |
| `cmdline.menu.page_up` | — | List: page up | Список: страница вверх | — | — |
| `cmdline.menu.page_down` | — | List: page down | Список: страница вниз | — | — |
| `cmdline.menu.accept` | `exec?:bool` | List: insert | Список: вставить | — | — |
| `cmdline.menu.delete` | — | List: delete | Список: удалить | — | — |
| `cmdline.menu.lock` | — | List: pin | Список: закрепить | — | — |
| `cmdline.menu.scope` | — | List: all shells | Список: все shells | — | — |
| `cmdline.menu.close` | — | List: close | Список: закрыть | — | — |
| `cmdline.paste` | `text?:str` | Paste | Вставить | — | — |

## `keymap` (11)

| Команда | Аргументы | EN | RU | Клавиши (Far) | Меню F9 |
|---|---|---|---|---|---|
| `keymap.reload` | — | Reload key bindings | Перечитать раскладку клавиш | — | options |
| `keymap.preset` | `name:str` | Key bindings preset | Пресет раскладки клавиш | — | — |
| `keymap.show` | — | Key bindings | Раскладка клавиш | — | — |
| `keymap.dump` | `path?:str` | Export key bindings | Выгрузить раскладку клавиш | — | options |
| `keymap.qs.start` | `text?:str` | Quick search | Быстрый поиск | — | — |
| `keymap.qs.input` | `text:str` | Quick search: type | Быстрый поиск: ввод | — | — |
| `keymap.qs.next` | — | Quick search: next match | Быстрый поиск: следующее | — | — |
| `keymap.qs.prev` | — | Quick search: previous match | Быстрый поиск: предыдущее | — | — |
| `keymap.qs.backspace` | — | Quick search: erase a character | Быстрый поиск: стереть символ | — | — |
| `keymap.qs.cancel` | — | Close quick search | Закрыть быстрый поиск | — | — |
| `keymap.pending` | `chord?:str` | Pending key chord | Начатое сочетание клавиш | — | — |
