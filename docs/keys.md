# Сочетания клавиш

<!-- Генерируется тестом `keys_doc_is_current` (crates/keymap/tests/known_commands.rs):
URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-keymap keys_doc -->

Встроенные пресеты (S3 §3). Пресет — `[keymap] preset` в `config.toml`, свои привязки — `keymaps/user.toml` рядом с ним (S3 §2.2). Команда работает, когда её сессия есть в сборке (`docs/COMMANDS.md`). `cmdline_empty` — командная строка пуста. ОС: W — Windows, M — macOS, L — Linux.

## far

### `panel`

| Сочетание | Команда | ОС |
|---|---|---|
| `Up` | `panel.cursor.up` | W M L |
| `Down` | `panel.cursor.down` | W M L |
| `PageUp` | `panel.cursor.page_up` | W M L |
| `PageDown` | `panel.cursor.page_down` | W M L |
| `Home` | `panel.cursor.home` | W M L |
| `End` | `panel.cursor.end` | W M L |
| `Ctrl+PageDown` | `panel.enter` | W M L |
| `Ctrl+PageUp` | `panel.up` | W M L |
| `Ctrl+Backslash` | `panel.root` | W M L |
| `Tab` | `panel.switch` | W M L |
| `Ctrl+U` | `panel.swap` | W M L |
| `Ctrl+R` | `panel.refresh` | W M L |
| `Ctrl+F1` | `panel.toggle side=left` | W L |
| `Ctrl+F2` | `panel.toggle side=right` | W L |
| `Ctrl+P` | `panel.toggle side=passive` | W M L |
| `Ctrl+O` | `ui.panels.toggle_all` | W M L |
| `Ctrl+L` | `panel.mode mode=info` | W M L |
| `Ctrl+Q` | `panel.mode mode=qview` | W M L |
| `Ctrl+T` | `panel.mode mode=tree` | W M L |
| `Ctrl+1` | `panel.view_mode mode=brief` | W M L |
| `Ctrl+2` | `panel.view_mode mode=medium` | W M L |
| `Ctrl+3` | `panel.view_mode mode=full` | W M L |
| `Ctrl+4` | `panel.view_mode mode=wide` | W M L |
| `Alt+F1` | `ui.drives side=left` | W M |
| `Alt+F2` | `ui.drives side=right` | W M |
| `Alt+F12` | `ui.history.dirs` | W M L |
| `Ctrl+F9` | `ui.utilities` | W M L |
| `Insert` | `sel.toggle advance=true` | W M L |
| `Shift+Up` | `sel.move motion=up` | W M L |
| `Shift+Down` | `sel.move motion=down` | W M L |
| `Shift+PageUp` | `sel.move motion=page_up` | W M L |
| `Shift+PageDown` | `sel.move motion=page_down` | W M L |
| `Shift+Home` | `sel.move motion=home` | W M L |
| `Shift+End` | `sel.move motion=end` | W M L |
| `GrayPlus` | `ui.draft command=sel.add arg=mask` | W M L |
| `GrayMinus` | `ui.draft command=sel.remove arg=mask` | W M L |
| `GrayStar` | `sel.invert` | W M L |
| `Ctrl+GrayStar` | `sel.invert dirs=true` | W M L |
| `F1` | `ui.help` | W M L |
| `F2` | `ui.user_menu` | W M L |
| `F3` | `viewer.open` | W M L |
| `Alt+F3` | `viewer.open alt=true` | W M L |
| `F4` | `editor.open` | W M L |
| `Alt+Shift+F4` | `editor.open alt=true` | W M L |
| `Shift+F4` | `editor.new` | W M L |
| `F5` | `ops.copy` | W M L |
| `F6` | `ops.move` | W M L |
| `F7` | `ops.mkdir` | W M L |
| `F8` | `ops.delete` | W M L |
| `Shift+F8` | `ops.delete current=true` | W M L |
| `Shift+Delete` | `ops.delete permanent=true` | W M L |
| `Ctrl+A` | `ops.attrs` | W M L |
| `Ctrl+I` | `panel.filter` | W M L |
| `Shift+F1` | `arc.add` | W M L |
| `Shift+F2` | `arc.extract` | W M L |
| `Alt+F7` | `search.files` | W M |
| `Alt+F8` | `cmdline.history` | W M |
| `Ctrl+Tab` | `tab.next` | W M L |
| `Ctrl+Shift+Tab` | `tab.prev` | W M L |
| `Ctrl+Shift+P` | `ui.palette` | W M L |
| `Left` | `panel.cursor.left` | W M L |
| `Right` | `panel.cursor.right` | W M L |
| `Ctrl+Shift+PageDown` | `arc.enter` | W M L |
| `Alt+Left` | `panel.back` | W M L |
| `Alt+Right` | `panel.forward` | W M L |
| `Ctrl+H` | `panel.hidden.toggle` | W M L |
| `Ctrl+F3` | `panel.sort key=name` | W L |
| `Ctrl+F4` | `panel.sort key=ext` | W L |
| `Ctrl+F5` | `panel.sort key=mtime` | W L |
| `Ctrl+F6` | `panel.sort key=size` | W L |
| `Ctrl+F7` | `panel.sort key=unsorted` | W L |
| `Ctrl+F12` | `ui.menu.sort` | W M L |
| `Ctrl+Shift+0` | `panel.bookmark.set n=0` | W M L |
| `Ctrl+Shift+1` | `panel.bookmark.set n=1` | W M L |
| `Ctrl+Shift+2` | `panel.bookmark.set n=2` | W M L |
| `Ctrl+Shift+3` | `panel.bookmark.set n=3` | W M L |
| `Ctrl+Shift+4` | `panel.bookmark.set n=4` | W M L |
| `Ctrl+Shift+5` | `panel.bookmark.set n=5` | W M L |
| `Ctrl+Shift+6` | `panel.bookmark.set n=6` | W M L |
| `Ctrl+Shift+7` | `panel.bookmark.set n=7` | W M L |
| `Ctrl+Shift+8` | `panel.bookmark.set n=8` | W M L |
| `Ctrl+Shift+9` | `panel.bookmark.set n=9` | W M L |
| `RCtrl+0` | `panel.bookmark.go n=0` | W M L |
| `RCtrl+1` | `panel.bookmark.go n=1` | W M L |
| `RCtrl+2` | `panel.bookmark.go n=2` | W M L |
| `RCtrl+3` | `panel.bookmark.go n=3` | W M L |
| `RCtrl+4` | `panel.bookmark.go n=4` | W M L |
| `RCtrl+5` | `panel.bookmark.go n=5` | W M L |
| `RCtrl+6` | `panel.bookmark.go n=6` | W M L |
| `RCtrl+7` | `panel.bookmark.go n=7` | W M L |
| `RCtrl+8` | `panel.bookmark.go n=8` | W M L |
| `RCtrl+9` | `panel.bookmark.go n=9` | W M L |
| `Shift+GrayPlus` | `sel.all` | W M L |
| `Shift+GrayMinus` | `sel.none` | W M L |
| `Ctrl+GrayPlus` | `sel.same by=ext on=true` | W M L |
| `Ctrl+GrayMinus` | `sel.same by=ext on=false` | W M L |
| `Alt+GrayPlus` | `sel.same by=name on=true` | W M L |
| `Alt+GrayMinus` | `sel.same by=name on=false` | W M L |
| `Ctrl+M` | `sel.restore` | W M L |
| `Ctrl+Shift+Insert` | `ui.copy_names full=false` | W M L |
| `Alt+Shift+Insert` | `ui.copy_names full=true` | W M L |
| `Ctrl+Alt+Insert` | `ui.copy_names full=true` | W M L |
| `Ctrl+Shift+C` | `file.clip.copy` | W M L |
| `Ctrl+Shift+X` | `file.clip.cut` | W M L |
| `Ctrl+Shift+V` | `file.clip.paste` | W M L |
| `Ctrl+Shift+F3` | `viewer.open internal=true` | W M L |
| `Ctrl+Shift+F4` | `editor.open internal=true` | W M L |
| `Shift+F5` | `ops.copy current=true` | W M L |
| `Shift+F6` | `ui.rename` | W M L |
| `Alt+F6` | `ops.link` | W M L |
| `Ctrl+Alt+Enter` | `file.open elevated=true` | W M L |
| `Ctrl+G` | `cmdline.apply` | W M L |
| `Shift+F3` | `arc.commands` | W M L |
| `Alt+F10` | `search.dirs` | W M |
| `Alt+F11` | `viewer.history` | W M L |
| `Ctrl+Shift+T` | `tab.new` | W M L |
| `Ctrl+Shift+W` | `tab.close` | W M L |
| `Ctrl+Shift+D` | `tab.dup` | W M L |
| `F9` | `ui.menu` | W M L |
| `F10` | `app.quit` | W M L |
| `F11` | `ui.jobs.toggle` | W M L |
| `F12` | `ui.screens` | W M L |
| `Alt+Shift+F9` | `ui.settings` | W M L |
| `Ctrl+B` | `ui.keybar.toggle` | W M L |
| `Ctrl+Up` | `ui.split.height delta=-1` | W L |
| `Ctrl+Down` | `ui.split.height delta=1` | W L |
| `Alt+F9` | `ui.window.maximize` | W M L |
| `Shift+F9` | `app.save_session` | W M L |
| `Ctrl+Cmd+1` | `panel.toggle side=left` | M |
| `Ctrl+Cmd+2` | `panel.toggle side=right` | M |
| `Cmd+Down` | `panel.enter` | M |
| `Cmd+Up` | `panel.up` | M |
| `Cmd+R` | `panel.refresh` | M |
| `Shift+Cmd+C` | `ui.drives` | M |
| `Shift+Cmd+G` | `ui.path_edit` | M |
| `Shift+Cmd+Equals` | `ui.draft command=sel.add arg=mask` | M |
| `Shift+Cmd+Minus` | `ui.draft command=sel.remove arg=mask` | M |
| `Shift+Cmd+8` | `sel.invert` | M |
| `Alt+Shift+Cmd+8` | `sel.invert dirs=true` | M |
| `Shift+Cmd+Slash` | `ui.help` | M |
| `Cmd+Y` | `viewer.open` | M |
| `Alt+F4` | `editor.open alt=true` | M |
| `Shift+Cmd+N` | `ops.mkdir` | M |
| `Cmd+Backspace` | `ops.delete` | M |
| `Alt+Cmd+Backspace` | `ops.delete permanent=true` | M |
| `Cmd+I` | `ops.attrs` | M |
| `Cmd+F` | `search.files` | M |
| `Shift+Cmd+P` | `ui.palette` | M |
| `Cmd+Q` | `app.quit` | M |
| `Cmd+M` | `ui.window.minimize` | M |
| `Cmd+H` | `ui.window.hide` | M |
| `Cmd+Equals` | `ui.zoom delta=1` | M |
| `Cmd+Minus` | `ui.zoom delta=-1` | M |
| `Cmd+0` | `ui.zoom` | M |
| `Cmd+OpenBracket` | `panel.back` | M |
| `Cmd+CloseBracket` | `panel.forward` | M |
| `Shift+Cmd+Period` | `panel.hidden.toggle` | M |
| `Ctrl+Alt+Cmd+1` | `panel.sort key=name` | M |
| `Ctrl+Alt+Cmd+2` | `panel.sort key=ext` | M |
| `Ctrl+Alt+Cmd+5` | `panel.sort key=mtime` | M |
| `Ctrl+Alt+Cmd+6` | `panel.sort key=size` | M |
| `Ctrl+Alt+Cmd+7` | `panel.sort key=unsorted` | M |
| `Alt+Cmd+0` | `panel.bookmark.go n=0` | M |
| `Alt+Cmd+1` | `panel.bookmark.go n=1` | M |
| `Alt+Cmd+2` | `panel.bookmark.go n=2` | M |
| `Alt+Cmd+3` | `panel.bookmark.go n=3` | M |
| `Alt+Cmd+4` | `panel.bookmark.go n=4` | M |
| `Alt+Cmd+5` | `panel.bookmark.go n=5` | M |
| `Alt+Cmd+6` | `panel.bookmark.go n=6` | M |
| `Alt+Cmd+7` | `panel.bookmark.go n=7` | M |
| `Alt+Cmd+8` | `panel.bookmark.go n=8` | M |
| `Alt+Cmd+9` | `panel.bookmark.go n=9` | M |
| `Cmd+A` | `sel.all` | M |
| `Alt+Cmd+A` | `sel.none` | M |
| `Alt+Cmd+C` | `ui.copy_names full=true` | M |
| `Alt+Cmd+V` | `file.clip.paste move=true` | M |
| `Cmd+T` | `tab.new` | M |
| `Cmd+W` | `tab.close` | M |
| `Shift+Cmd+D` | `tab.dup` | M |
| `Shift+Cmd+CloseBracket` | `tab.next` | M |
| `Shift+Cmd+OpenBracket` | `tab.prev` | M |
| `Cmd+1` | `tab.goto n=1` | M |
| `Cmd+2` | `tab.goto n=2` | M |
| `Cmd+3` | `tab.goto n=3` | M |
| `Cmd+4` | `tab.goto n=4` | M |
| `Cmd+5` | `tab.goto n=5` | M |
| `Cmd+6` | `tab.goto n=6` | M |
| `Cmd+7` | `tab.goto n=7` | M |
| `Cmd+8` | `tab.goto n=8` | M |
| `Cmd+9` | `tab.goto n=9` | M |
| `Cmd+Comma` | `ui.settings` | M |
| `Alt+Cmd+Left` | `ui.split delta=-1` | M |
| `Alt+Cmd+Right` | `ui.split delta=1` | M |
| `Alt+Cmd+Up` | `ui.split.height delta=-1` | M |
| `Alt+Cmd+Down` | `ui.split.height delta=1` | M |
| `Ctrl+Cmd+F` | `ui.window.maximize` | M |
| `Alt+Shift+F1` | `ui.drives side=left` | L |
| `Alt+Shift+F2` | `ui.drives side=right` | L |
| `Alt+Shift+F7` | `search.files` | L |
| `Alt+Shift+F8` | `cmdline.history` | L |
| `Alt+Shift+F10` | `search.dirs` | L |

### `panel && cmdline_empty`

| Сочетание | Команда | ОС |
|---|---|---|
| `Enter` | `panel.enter` | W M L |
| `Ctrl+D` | `ui.menu menu=bookmarks` | W M L |
| `Shift+F10` | `ui.menu menu=context` | W M L |
| `Backspace` | `panel.up` | W M L |
| `Space` | `sel.toggle advance=true` | W M L |
| `Ctrl+Insert` | `ui.copy_names full=false` | W M L |
| `Shift+Enter` | `file.reveal` | W M L |
| `Delete` | `ops.delete` | W M L |
| `Ctrl+Left` | `ui.split delta=-1` | W L |
| `Ctrl+Right` | `ui.split delta=1` | W L |
| `Cmd+C` | `file.clip.copy` | M |
| `Cmd+V` | `file.clip.paste` | M |

### `panel && tree && cmdline_empty`

| Сочетание | Команда | ОС |
|---|---|---|
| `Up` | `ui.tree.up` | W M L |
| `Down` | `ui.tree.down` | W M L |
| `PageUp` | `ui.tree.page_up` | W M L |
| `PageDown` | `ui.tree.page_down` | W M L |
| `Home` | `ui.tree.home` | W M L |
| `End` | `ui.tree.end` | W M L |
| `Right` | `ui.tree.expand` | W M L |
| `Left` | `ui.tree.collapse` | W M L |
| `Enter` | `ui.tree.enter` | W M L |

### `panel \|\| cmdline`

| Сочетание | Команда | ОС |
|---|---|---|
| `Ctrl+Y` | `cmdline.clear` | W M L |
| `Ctrl+E` | `cmdline.history.prev` | W M L |
| `Ctrl+X` | `cmdline.history.next` | W M L |
| `Ctrl+Home` | `cmdline.home` | W M L |
| `Ctrl+End` | `cmdline.end` | W M L |
| `Ctrl+Backspace` | `cmdline.del.word_left` | W M L |
| `Ctrl+Delete` | `cmdline.del.word_right` | W M L |
| `Ctrl+K` | `cmdline.del.to_end` | W M L |
| `Ctrl+Enter` | `cmdline.insert_name what=name` | W M L |
| `Ctrl+OpenBracket` | `cmdline.insert_name what=path side=left` | W M L |
| `Ctrl+CloseBracket` | `cmdline.insert_name what=path side=right` | W M L |
| `Shift+Insert` | `cmdline.paste` | W M L |
| `Ctrl+V` | `cmdline.paste` | W M L |
| `Ctrl+S` | `cmdline.cursor.left` | W M L |
| `Ctrl+D` | `cmdline.cursor.right` | W M L |
| `Ctrl+J` | `cmdline.insert_name what=name` | W M L |
| `Ctrl+F` | `cmdline.insert_name what=full` | W M L |
| `Ctrl+Shift+Enter` | `cmdline.insert_name what=name side=passive` | W M L |
| `Ctrl+Semicolon` | `cmdline.insert_name what=full side=passive` | W M L |
| `Ctrl+Shift+OpenBracket` | `cmdline.insert_name what=path side=active` | W M L |
| `Ctrl+Shift+CloseBracket` | `cmdline.insert_name what=path side=passive` | W M L |
| `Cmd+Left` | `cmdline.home` | M |
| `Cmd+Right` | `cmdline.end` | M |
| `Alt+Delete` | `cmdline.del.word_right` | M |

### `(panel \|\| cmdline) && !cmdline_empty`

| Сочетание | Команда | ОС |
|---|---|---|
| `Enter` | `cmdline.exec` | W M L |
| `Shift+Enter` | `cmdline.exec detached=true` | W M L |
| `Esc` | `cmdline.clear` | W M L |
| `Ctrl+Left` | `cmdline.word.left` | W L |
| `Ctrl+Right` | `cmdline.word.right` | W L |
| `Ctrl+Insert` | `cmdline.copy` | W M L |
| `Tab` | `cmdline.complete` | W M L |
| `Shift+Tab` | `cmdline.complete_native` | W M L |
| `Backspace` | `cmdline.backspace` | W M L |
| `Delete` | `cmdline.delete` | W M L |
| `Ctrl+S` | `cmdline.cursor.left` | W M L |
| `Ctrl+D` | `cmdline.cursor.right` | W M L |
| `Ctrl+Z` | `cmdline.undo` | W M L |
| `Ctrl+Shift+Z` | `cmdline.redo` | W M L |
| `Alt+Left` | `cmdline.word.left` | M |
| `Alt+Right` | `cmdline.word.right` | M |
| `Cmd+V` | `cmdline.paste` | M |
| `Cmd+C` | `cmdline.copy` | M |
| `Alt+Backspace` | `cmdline.del.word_left` | M |

### `cmdline`

| Сочетание | Команда | ОС |
|---|---|---|
| `Left` | `cmdline.cursor.left` | W M L |
| `Right` | `cmdline.cursor.right` | W M L |
| `Home` | `cmdline.home` | W M L |
| `End` | `cmdline.end` | W M L |
| `Backspace` | `cmdline.backspace` | W M L |
| `Delete` | `cmdline.delete` | W M L |
| `Enter` | `cmdline.exec` | W M L |

### `terminal`

| Сочетание | Команда | ОС |
|---|---|---|
| `Ctrl+O` | `ui.panels.toggle_all` | W M L |
| `Ctrl+Shift+C` | `term.copy` | W M L |
| `Ctrl+Insert` | `term.copy` | W M L |
| `Ctrl+Shift+V` | `term.paste` | W M L |
| `Shift+Insert` | `term.paste` | W M L |
| `Shift+PageUp` | `term.scroll pages=-1` | W M L |
| `Shift+PageDown` | `term.scroll pages=1` | W M L |

### `overlay && qs`

| Сочетание | Команда | ОС |
|---|---|---|
| `Ctrl+Enter` | `keymap.qs.next` | W M L |
| `Down` | `keymap.qs.next` | W M L |
| `Up` | `keymap.qs.prev` | W M L |
| `Backspace` | `keymap.qs.backspace` | W M L |
| `Esc` | `keymap.qs.cancel` | W M L |
| `Ctrl+Shift+Enter` | `keymap.qs.prev` | W M L |

### `overlay`

| Сочетание | Команда | ОС |
|---|---|---|
| `Esc` | `ui.cancel` | W M L |

### `viewer`

| Сочетание | Команда | ОС |
|---|---|---|
| `Esc` | `viewer.close` | W M L |
| `F10` | `viewer.close` | W M L |
| `F2` | `viewer.wrap` | W M L |
| `F4` | `viewer.hex` | W M L |
| `F7` | `viewer.search` | W M L |
| `Shift+F7` | `viewer.search_next` | W M L |
| `Alt+F7` | `viewer.search_prev` | W M |
| `Up` | `viewer.scroll dir=up` | W M L |
| `Down` | `viewer.scroll dir=down` | W M L |
| `Left` | `viewer.scroll dir=left` | W M L |
| `Right` | `viewer.scroll dir=right` | W M L |
| `PageUp` | `viewer.scroll dir=page_up` | W M L |
| `PageDown` | `viewer.scroll dir=page_down` | W M L |
| `Home` | `viewer.scroll dir=home` | W M L |
| `End` | `viewer.scroll dir=end` | W M L |
| `Shift+F4` | `viewer.mode` | W M L |
| `F8` | `viewer.codepage` | W M L |
| `Shift+F8` | `viewer.codepage_menu` | W M L |
| `Alt+F8` | `viewer.goto` | W M |
| `Alt+Backspace` | `viewer.back` | W M L |
| `Shift+F` | `viewer.follow` | W M L |
| `Ctrl+C` | `viewer.copy` | W M L |
| `Ctrl+Insert` | `viewer.copy` | W M L |
| `Ctrl+A` | `viewer.select_all` | W M L |
| `F3` | `viewer.close` | W M L |
| `Num5` | `viewer.close` | W M L |
| `Shift+F2` | `viewer.wrap_mode` | W M L |
| `F6` | `viewer.to_editor` | W M L |
| `Space` | `viewer.search_next` | W M L |
| `GrayPlus` | `viewer.next_file` | W M L |
| `GrayMinus` | `viewer.prev_file` | W M L |
| `Ctrl+F10` | `viewer.reveal` | W M L |
| `Ctrl+Shift+0` | `viewer.bookmark.set n=0` | W M L |
| `Ctrl+Shift+1` | `viewer.bookmark.set n=1` | W M L |
| `Ctrl+Shift+2` | `viewer.bookmark.set n=2` | W M L |
| `Ctrl+Shift+3` | `viewer.bookmark.set n=3` | W M L |
| `Ctrl+Shift+4` | `viewer.bookmark.set n=4` | W M L |
| `Ctrl+Shift+5` | `viewer.bookmark.set n=5` | W M L |
| `Ctrl+Shift+6` | `viewer.bookmark.set n=6` | W M L |
| `Ctrl+Shift+7` | `viewer.bookmark.set n=7` | W M L |
| `Ctrl+Shift+8` | `viewer.bookmark.set n=8` | W M L |
| `Ctrl+Shift+9` | `viewer.bookmark.set n=9` | W M L |
| `Ctrl+0` | `viewer.bookmark.go n=0` | W M L |
| `Ctrl+1` | `viewer.bookmark.go n=1` | W M L |
| `Ctrl+2` | `viewer.bookmark.go n=2` | W M L |
| `Ctrl+3` | `viewer.bookmark.go n=3` | W M L |
| `Ctrl+4` | `viewer.bookmark.go n=4` | W M L |
| `Ctrl+5` | `viewer.bookmark.go n=5` | W M L |
| `Ctrl+6` | `viewer.bookmark.go n=6` | W M L |
| `Ctrl+7` | `viewer.bookmark.go n=7` | W M L |
| `Ctrl+8` | `viewer.bookmark.go n=8` | W M L |
| `Ctrl+9` | `viewer.bookmark.go n=9` | W M L |
| `Ctrl+Z` | `viewer.back` | W M L |
| `Ctrl+Left` | `viewer.scroll dir=left n=20` | W L |
| `Ctrl+Right` | `viewer.scroll dir=right n=20` | W L |
| `Cmd+W` | `viewer.close` | M |
| `Cmd+F` | `viewer.search` | M |
| `Cmd+G` | `viewer.search_next` | M |
| `Shift+Cmd+G` | `viewer.search_prev` | M |
| `Cmd+L` | `viewer.goto` | M |
| `Cmd+C` | `viewer.copy` | M |
| `Cmd+A` | `viewer.select_all` | M |
| `Shift+Cmd+CloseBracket` | `viewer.next_file` | M |
| `Shift+Cmd+OpenBracket` | `viewer.prev_file` | M |
| `Cmd+Z` | `viewer.back` | M |
| `Alt+Shift+F7` | `viewer.search_prev` | L |
| `Alt+Shift+F8` | `viewer.goto` | L |

### `editor`

| Сочетание | Команда | ОС |
|---|---|---|
| `F2` | `editor.save` | W M L |
| `Shift+F2` | `editor.save_as` | W M L |
| `Esc` | `editor.close` | W M L |
| `F7` | `editor.search` | W M L |
| `Ctrl+F7` | `editor.replace` | W L |
| `Shift+F7` | `editor.search_next` | W M L |
| `Alt+F7` | `editor.search_prev` | W M |
| `Ctrl+Y` | `editor.delete_line` | W M L |
| `Shift+F10` | `editor.save_close` | W M L |
| `Alt+F8` | `editor.goto` | W M |
| `F8` | `editor.codepage` | W M L |
| `Shift+F8` | `editor.codepage_menu` | W M L |
| `F10` | `editor.close` | W M L |
| `F4` | `editor.close` | W M L |
| `F6` | `editor.to_viewer` | W M L |
| `Ctrl+Z` | `editor.undo` | W M L |
| `Ctrl+Shift+Z` | `editor.redo` | W M L |
| `Alt+U` | `editor.block.shift_left` | W M L |
| `Alt+I` | `editor.block.shift_right` | W M L |
| `Cmd+S` | `editor.save` | M |
| `Shift+Cmd+S` | `editor.save_as` | M |
| `Cmd+W` | `editor.close` | M |
| `Cmd+F` | `editor.search` | M |
| `Alt+Cmd+F` | `editor.replace` | M |
| `Cmd+G` | `editor.search_next` | M |
| `Shift+Cmd+G` | `editor.search_prev` | M |
| `Cmd+L` | `editor.goto` | M |
| `Cmd+Z` | `editor.undo` | M |
| `Shift+Cmd+Z` | `editor.redo` | M |
| `Shift+Cmd+K` | `editor.delete_line` | M |
| `Alt+Cmd+S` | `editor.save_all` | M |
| `Cmd+OpenBracket` | `editor.block.shift_left` | M |
| `Cmd+CloseBracket` | `editor.block.shift_right` | M |
| `Alt+Shift+F7` | `editor.search_prev` | L |
| `Alt+Shift+F8` | `editor.goto` | L |

### `(panel \|\| cmdline) && cmdline_menu && !overlay && !prompt`

| Сочетание | Команда | ОС |
|---|---|---|
| `Up` | `cmdline.menu.up` | W M L |
| `Down` | `cmdline.menu.down` | W M L |
| `PageUp` | `cmdline.menu.page_up` | W M L |
| `PageDown` | `cmdline.menu.page_down` | W M L |
| `Enter` | `cmdline.menu.accept` | W M L |
| `Shift+Enter` | `cmdline.menu.accept exec=true` | W M L |
| `Delete` | `cmdline.menu.delete` | W M L |
| `Insert` | `cmdline.menu.lock` | W M L |
| `Ctrl+R` | `cmdline.menu.scope` | W M L |
| `Esc` | `cmdline.menu.close` | W M L |
| `Tab` | `cmdline.complete` | W M L |
| `Shift+Tab` | `cmdline.menu.up` | W M L |
| `Backspace` | `cmdline.backspace` | W M L |
| `Space` | `cmdline.insert text=" "` | W M L |

### `(panel \|\| cmdline) && cmdline_ghost`

| Сочетание | Команда | ОС |
|---|---|---|
| `Right` | `cmdline.cursor.right` | W M L |
| `End` | `cmdline.end` | W M L |


## dn

### `panel`

| Сочетание | Команда | ОС |
|---|---|---|
| `Up` | `panel.cursor.up` | W M L |
| `Down` | `panel.cursor.down` | W M L |
| `PageUp` | `panel.cursor.page_up` | W M L |
| `PageDown` | `panel.cursor.page_down` | W M L |
| `Home` | `panel.cursor.home` | W M L |
| `End` | `panel.cursor.end` | W M L |
| `Ctrl+PageDown` | `panel.enter` | W M L |
| `Ctrl+PageUp` | `panel.up` | W M L |
| `Ctrl+Backslash` | `panel.root` | W M L |
| `Tab` | `panel.switch` | W M L |
| `Ctrl+U` | `panel.swap` | W M L |
| `Ctrl+R` | `panel.refresh` | W M L |
| `Ctrl+F1` | `panel.toggle side=left` | W L |
| `Ctrl+F2` | `panel.toggle side=right` | W L |
| `Ctrl+P` | `panel.toggle side=passive` | W M L |
| `Ctrl+O` | `ui.panels.toggle_all` | W M L |
| `Ctrl+L` | `panel.mode mode=info` | W M L |
| `Ctrl+Q` | `panel.mode mode=qview` | W M L |
| `Ctrl+T` | `panel.mode mode=tree` | W M L |
| `Ctrl+1` | `panel.view_mode mode=brief` | W M L |
| `Ctrl+2` | `panel.view_mode mode=medium` | W M L |
| `Ctrl+3` | `panel.view_mode mode=full` | W M L |
| `Ctrl+4` | `panel.view_mode mode=wide` | W M L |
| `Alt+F1` | `ui.drives side=left` | W M |
| `Alt+F2` | `ui.drives side=right` | W M |
| `Alt+F12` | `ui.history.dirs` | W M L |
| `Ctrl+F9` | `ui.utilities` | W M L |
| `Insert` | `sel.toggle advance=true` | W M L |
| `Shift+Up` | `sel.move motion=up` | W M L |
| `Shift+Down` | `sel.move motion=down` | W M L |
| `Shift+PageUp` | `sel.move motion=page_up` | W M L |
| `Shift+PageDown` | `sel.move motion=page_down` | W M L |
| `Shift+Home` | `sel.move motion=home` | W M L |
| `Shift+End` | `sel.move motion=end` | W M L |
| `GrayPlus` | `ui.draft command=sel.add arg=mask` | W M L |
| `GrayMinus` | `ui.draft command=sel.remove arg=mask` | W M L |
| `GrayStar` | `sel.invert` | W M L |
| `Ctrl+GrayStar` | `sel.invert dirs=true` | W M L |
| `F1` | `ui.help` | W M L |
| `F2` | `ui.user_menu` | W M L |
| `F3` | `viewer.open` | W M L |
| `Alt+F3` | `viewer.open alt=true` | W M L |
| `F4` | `editor.open` | W M L |
| `Alt+Shift+F4` | `editor.open alt=true` | W M L |
| `Shift+F4` | `editor.new` | W M L |
| `F5` | `ops.copy` | W M L |
| `F6` | `ops.move` | W M L |
| `F7` | `ops.mkdir` | W M L |
| `F8` | `ops.delete` | W M L |
| `Shift+F8` | `ops.delete current=true` | W M L |
| `Shift+Delete` | `ops.delete permanent=true` | W M L |
| `Ctrl+A` | `ops.attrs` | W M L |
| `Ctrl+I` | `panel.filter` | W M L |
| `Shift+F1` | `arc.add` | W M L |
| `Shift+F2` | `arc.extract` | W M L |
| `Alt+F7` | `search.files` | W M |
| `Alt+F8` | `cmdline.history` | W M |
| `Ctrl+Tab` | `tab.next` | W M L |
| `Ctrl+Shift+Tab` | `tab.prev` | W M L |
| `Ctrl+Shift+P` | `ui.palette` | W M L |
| `Alt+R` | `panel.refresh` | W M L |
| `Alt+F5` | `ui.panels.toggle_all` | W M L |
| `Alt+I` | `panel.mode mode=info` | W M L |
| `Alt+B` | `ui.menu.sort` | W M L |
| `Alt+Delete` | `ui.draft command=panel.filter arg=mask` | W M L |
| `Alt+1` | `panel.bookmark.go n=1` | W M L |
| `Alt+2` | `panel.bookmark.go n=2` | W M L |
| `Alt+3` | `panel.bookmark.go n=3` | W M L |
| `Alt+4` | `panel.bookmark.go n=4` | W M L |
| `Alt+5` | `panel.bookmark.go n=5` | W M L |
| `Alt+6` | `panel.bookmark.go n=6` | W M L |
| `Alt+7` | `panel.bookmark.go n=7` | W M L |
| `Alt+8` | `panel.bookmark.go n=8` | W M L |
| `Alt+9` | `panel.bookmark.go n=9` | W M L |
| `Alt+C` | `ui.drives side=active` | W M L |
| `Alt+Backspace` | `ui.history.dirs` | W M L |
| `Alt+K` | `ui.panel.setup` | W M L |
| `Alt+S` | `ui.panel.setup` | W M L |
| `Ctrl+Shift+GrayPlus` | `sel.same by=ext on=true` | W M L |
| `Ctrl+Shift+GrayMinus` | `sel.same by=ext on=false` | W M L |
| `Alt+F6` | `ui.rename` | W M L |
| `Alt+E` | `ops.attrs` | W M L |
| `Ctrl+Alt+F6` | `ops.multi_rename` | W M |
| `Ctrl+C` | `ops.compare` | W M L |
| `Alt+G` | `vfs.dir_size` | W M L |
| `Alt+Enter` | `file.open_with` | W M L |
| `Alt+T` | `search.dirs` | W M L |
| `Alt+PageDown` | `viewer.history kind=view` | W M L |
| `Alt+PageUp` | `viewer.history kind=edit` | W M L |
| `Ctrl+F3` | `tab.new` | W L |
| `Ctrl+F4` | `tab.close` | W L |
| `F9` | `tab.next` | W M L |
| `Shift+F9` | `tab.prev` | W M L |
| `Alt+0` | `ui.screens` | W M L |
| `F11` | `ui.jobs.toggle` | W M L |
| `F10` | `ui.menu` | W M L |
| `Alt+F` | `ui.menu menu=files` | W M L |
| `Alt+U` | `ui.menu menu=commands` | W M L |
| `Alt+P` | `ui.menu` | W M L |
| `Alt+O` | `ui.menu menu=options` | W M L |
| `Alt+F11` | `ui.palette` | W M L |
| `Alt+Left` | `ui.split delta=-1` | W M L |
| `Alt+Right` | `ui.split delta=1` | W M L |
| `Alt+J` | `term.focus` | W M L |
| `Alt+X` | `app.quit` | W M L |
| `Ctrl+Cmd+1` | `panel.toggle side=left` | M |
| `Ctrl+Cmd+2` | `panel.toggle side=right` | M |
| `Cmd+Down` | `panel.enter` | M |
| `Cmd+Up` | `panel.up` | M |
| `Cmd+R` | `panel.refresh` | M |
| `Shift+Cmd+C` | `ui.drives` | M |
| `Shift+Cmd+G` | `ui.path_edit` | M |
| `Shift+Cmd+Equals` | `ui.draft command=sel.add arg=mask` | M |
| `Shift+Cmd+Minus` | `ui.draft command=sel.remove arg=mask` | M |
| `Shift+Cmd+8` | `sel.invert` | M |
| `Alt+Shift+Cmd+8` | `sel.invert dirs=true` | M |
| `Shift+Cmd+Slash` | `ui.help` | M |
| `Cmd+Y` | `viewer.open` | M |
| `Alt+F4` | `editor.open alt=true` | M |
| `Shift+Cmd+N` | `ops.mkdir` | M |
| `Cmd+Backspace` | `ops.delete` | M |
| `Alt+Cmd+Backspace` | `ops.delete permanent=true` | M |
| `Cmd+I` | `ops.attrs` | M |
| `Cmd+F` | `search.files` | M |
| `Shift+Cmd+P` | `ui.palette` | M |
| `Cmd+Q` | `app.quit` | M |
| `Cmd+M` | `ui.window.minimize` | M |
| `Cmd+H` | `ui.window.hide` | M |
| `Cmd+Equals` | `ui.zoom delta=1` | M |
| `Cmd+Minus` | `ui.zoom delta=-1` | M |
| `Cmd+0` | `ui.zoom` | M |
| `Cmd+T` | `tab.new` | M |
| `Cmd+W` | `tab.close` | M |
| `Alt+Shift+F1` | `ui.drives side=left` | L |
| `Alt+Shift+F2` | `ui.drives side=right` | L |
| `Alt+Shift+F7` | `search.files` | L |
| `Alt+Shift+F8` | `cmdline.history` | L |

### `panel && cmdline_empty`

| Сочетание | Команда | ОС |
|---|---|---|
| `Enter` | `panel.enter` | W M L |
| `Ctrl+D` | `ui.menu menu=bookmarks` | W M L |
| `Shift+F10` | `ui.menu menu=context` | W M L |
| `Backspace` | `panel.up` | W M L |
| `Space` | `sel.toggle advance=true` | W M L |
| `Ctrl+Insert` | `ui.copy_names full=false` | W M L |
| `Shift+Enter` | `file.reveal` | W M L |
| `Left` | `panel.cursor.left` | W M L |
| `Right` | `panel.cursor.right` | W M L |
| `Cmd+C` | `file.clip.copy` | M |
| `Cmd+V` | `file.clip.paste` | M |

### `panel && tree && cmdline_empty`

| Сочетание | Команда | ОС |
|---|---|---|
| `Up` | `ui.tree.up` | W M L |
| `Down` | `ui.tree.down` | W M L |
| `PageUp` | `ui.tree.page_up` | W M L |
| `PageDown` | `ui.tree.page_down` | W M L |
| `Home` | `ui.tree.home` | W M L |
| `End` | `ui.tree.end` | W M L |
| `Right` | `ui.tree.expand` | W M L |
| `Left` | `ui.tree.collapse` | W M L |
| `Enter` | `ui.tree.enter` | W M L |

### `panel \|\| cmdline`

| Сочетание | Команда | ОС |
|---|---|---|
| `Ctrl+Y` | `cmdline.clear` | W M L |
| `Ctrl+E` | `cmdline.history.prev` | W M L |
| `Ctrl+X` | `cmdline.history.next` | W M L |
| `Ctrl+Home` | `cmdline.home` | W M L |
| `Ctrl+End` | `cmdline.end` | W M L |
| `Ctrl+Backspace` | `cmdline.del.word_left` | W M L |
| `Ctrl+Delete` | `cmdline.del.word_right` | W M L |
| `Ctrl+K` | `cmdline.del.to_end` | W M L |
| `Ctrl+Enter` | `cmdline.insert_name what=name` | W M L |
| `Ctrl+OpenBracket` | `cmdline.insert_name what=path side=left` | W M L |
| `Ctrl+CloseBracket` | `cmdline.insert_name what=path side=right` | W M L |
| `Shift+Insert` | `cmdline.paste` | W M L |
| `Ctrl+V` | `cmdline.paste` | W M L |
| `Ctrl+Shift+Enter` | `cmdline.insert_name what=full` | W M L |
| `Cmd+Left` | `cmdline.home` | M |
| `Cmd+Right` | `cmdline.end` | M |
| `Alt+Delete` | `cmdline.del.word_right` | M |

### `(panel \|\| cmdline) && !cmdline_empty`

| Сочетание | Команда | ОС |
|---|---|---|
| `Enter` | `cmdline.exec` | W M L |
| `Shift+Enter` | `cmdline.exec detached=true` | W M L |
| `Esc` | `cmdline.clear` | W M L |
| `Ctrl+Left` | `cmdline.word.left` | W L |
| `Ctrl+Right` | `cmdline.word.right` | W L |
| `Ctrl+Insert` | `cmdline.copy` | W M L |
| `Tab` | `cmdline.complete` | W M L |
| `Shift+Tab` | `cmdline.complete_native` | W M L |
| `Backspace` | `cmdline.backspace` | W M L |
| `Delete` | `cmdline.delete` | W M L |
| `Ctrl+S` | `cmdline.cursor.left` | W M L |
| `Ctrl+D` | `cmdline.cursor.right` | W M L |
| `Ctrl+Z` | `cmdline.undo` | W M L |
| `Ctrl+Shift+Z` | `cmdline.redo` | W M L |
| `Left` | `cmdline.cursor.left` | W M L |
| `Right` | `cmdline.cursor.right` | W M L |
| `Alt+Left` | `cmdline.word.left` | M |
| `Alt+Right` | `cmdline.word.right` | M |
| `Cmd+V` | `cmdline.paste` | M |
| `Cmd+C` | `cmdline.copy` | M |

### `cmdline`

| Сочетание | Команда | ОС |
|---|---|---|
| `Left` | `cmdline.cursor.left` | W M L |
| `Right` | `cmdline.cursor.right` | W M L |
| `Home` | `cmdline.home` | W M L |
| `End` | `cmdline.end` | W M L |
| `Backspace` | `cmdline.backspace` | W M L |
| `Delete` | `cmdline.delete` | W M L |
| `Enter` | `cmdline.exec` | W M L |

### `terminal`

| Сочетание | Команда | ОС |
|---|---|---|
| `Ctrl+O` | `ui.panels.toggle_all` | W M L |
| `Ctrl+Shift+C` | `term.copy` | W M L |
| `Ctrl+Insert` | `term.copy` | W M L |
| `Ctrl+Shift+V` | `term.paste` | W M L |
| `Shift+Insert` | `term.paste` | W M L |
| `Shift+PageUp` | `term.scroll pages=-1` | W M L |
| `Shift+PageDown` | `term.scroll pages=1` | W M L |

### `overlay && qs`

| Сочетание | Команда | ОС |
|---|---|---|
| `Ctrl+Enter` | `keymap.qs.next` | W M L |
| `Down` | `keymap.qs.next` | W M L |
| `Up` | `keymap.qs.prev` | W M L |
| `Backspace` | `keymap.qs.backspace` | W M L |
| `Esc` | `keymap.qs.cancel` | W M L |
| `Shift+Enter` | `keymap.qs.prev` | W M L |

### `overlay`

| Сочетание | Команда | ОС |
|---|---|---|
| `Esc` | `ui.cancel` | W M L |

### `viewer`

| Сочетание | Команда | ОС |
|---|---|---|
| `Esc` | `viewer.close` | W M L |
| `F10` | `viewer.close` | W M L |
| `F2` | `viewer.wrap` | W M L |
| `F4` | `viewer.hex` | W M L |
| `F7` | `viewer.search` | W M L |
| `Shift+F7` | `viewer.search_next` | W M L |
| `Alt+F7` | `viewer.search_prev` | W M |
| `Up` | `viewer.scroll dir=up` | W M L |
| `Down` | `viewer.scroll dir=down` | W M L |
| `Left` | `viewer.scroll dir=left` | W M L |
| `Right` | `viewer.scroll dir=right` | W M L |
| `PageUp` | `viewer.scroll dir=page_up` | W M L |
| `PageDown` | `viewer.scroll dir=page_down` | W M L |
| `Home` | `viewer.scroll dir=home` | W M L |
| `End` | `viewer.scroll dir=end` | W M L |
| `Shift+F4` | `viewer.mode` | W M L |
| `F8` | `viewer.codepage` | W M L |
| `Shift+F8` | `viewer.codepage_menu` | W M L |
| `Alt+F8` | `viewer.goto` | W M |
| `Alt+Backspace` | `viewer.back` | W M L |
| `Shift+F` | `viewer.follow` | W M L |
| `Ctrl+C` | `viewer.copy` | W M L |
| `Ctrl+Insert` | `viewer.copy` | W M L |
| `Ctrl+A` | `viewer.select_all` | W M L |
| `Ctrl+L` | `viewer.search_next` | W M L |
| `Cmd+W` | `viewer.close` | M |
| `Cmd+F` | `viewer.search` | M |
| `Cmd+G` | `viewer.search_next` | M |
| `Shift+Cmd+G` | `viewer.search_prev` | M |
| `Cmd+L` | `viewer.goto` | M |
| `Cmd+C` | `viewer.copy` | M |
| `Cmd+A` | `viewer.select_all` | M |
| `Alt+Shift+F7` | `viewer.search_prev` | L |
| `Alt+Shift+F8` | `viewer.goto` | L |

### `editor`

| Сочетание | Команда | ОС |
|---|---|---|
| `F2` | `editor.save` | W M L |
| `Shift+F2` | `editor.save_as` | W M L |
| `Esc` | `editor.close` | W M L |
| `F7` | `editor.search` | W M L |
| `Ctrl+F7` | `editor.replace` | W L |
| `Shift+F7` | `editor.search_next` | W M L |
| `Alt+F7` | `editor.search_prev` | W M |
| `Ctrl+Y` | `editor.delete_line` | W M L |
| `Shift+F10` | `editor.save_close` | W M L |
| `Alt+F8` | `editor.goto` | W M |
| `F8` | `editor.codepage` | W M L |
| `Shift+F8` | `editor.codepage_menu` | W M L |
| `Ctrl+F2` | `editor.save_all` | W L |
| `Ctrl+F4` | `editor.close` | W L |
| `Alt+G` | `editor.goto` | W M L |
| `Alt+Backspace` | `editor.undo` | W M L |
| `Alt+Shift+Backspace` | `editor.redo` | W M L |
| `Cmd+S` | `editor.save` | M |
| `Shift+Cmd+S` | `editor.save_as` | M |
| `Cmd+W` | `editor.close` | M |
| `Cmd+F` | `editor.search` | M |
| `Alt+Cmd+F` | `editor.replace` | M |
| `Cmd+G` | `editor.search_next` | M |
| `Shift+Cmd+G` | `editor.search_prev` | M |
| `Cmd+L` | `editor.goto` | M |
| `Cmd+Z` | `editor.undo` | M |
| `Shift+Cmd+Z` | `editor.redo` | M |
| `Shift+Cmd+K` | `editor.delete_line` | M |
| `Alt+Cmd+S` | `editor.save_all` | M |
| `Alt+Shift+F7` | `editor.search_prev` | L |
| `Alt+Shift+F8` | `editor.goto` | L |

### `(panel \|\| cmdline) && cmdline_menu && !overlay && !prompt`

| Сочетание | Команда | ОС |
|---|---|---|
| `Up` | `cmdline.menu.up` | W M L |
| `Down` | `cmdline.menu.down` | W M L |
| `PageUp` | `cmdline.menu.page_up` | W M L |
| `PageDown` | `cmdline.menu.page_down` | W M L |
| `Enter` | `cmdline.menu.accept` | W M L |
| `Shift+Enter` | `cmdline.menu.accept exec=true` | W M L |
| `Delete` | `cmdline.menu.delete` | W M L |
| `Insert` | `cmdline.menu.lock` | W M L |
| `Ctrl+R` | `cmdline.menu.scope` | W M L |
| `Esc` | `cmdline.menu.close` | W M L |
| `Tab` | `cmdline.complete` | W M L |
| `Shift+Tab` | `cmdline.menu.up` | W M L |
| `Backspace` | `cmdline.backspace` | W M L |
| `Space` | `cmdline.insert text=" "` | W M L |

### `(panel \|\| cmdline) && cmdline_ghost`

| Сочетание | Команда | ОС |
|---|---|---|
| `Right` | `cmdline.cursor.right` | W M L |
| `End` | `cmdline.end` | W M L |

