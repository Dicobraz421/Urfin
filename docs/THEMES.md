# Темы оформления (утверждено 2026-10-20)

Решения — S0-decisions №59–№61, задача — `docs/TASKS-THEMES.md`. Эталон — холст «Урфин — темы оформления» (шаблон окна, экраны F3/F4/F5/F8, поиск, настройки, конфликт, job-бар). Принцип: спокойно, почти ч/б, без чистого белого; цвет — только там, где он что-то значит (палитра B, [решение 2026-11-01](decisions/2026-11-01-colorize.md): тип, состояние, активная панель, клавиша, итог). Правка палитры — сразу во всех темах. Высокая контрастность — только авто из Windows, в списке тем нет. По умолчанию — «как в ОС» (light/dark + акцент ОС). Mica не используется: фон каркаса сплошной.

**Одна информация — одно место** (№61): имя файла — во вкладке; путь — в заголовке экрана; размер, кодировка, переводы строк, тип — в статус-строке; переключатели режимов — в заголовке; действия по клавишам — в панели клавиш; действия встроенной строки (поиск, подтверждение, конфликт) — на её кнопках с подписью клавиши, в панели клавиш не повторяются.

Роли: frame (каркас/заголовок), panel, header (статус-строка, плашки, подложка номеров строк), text, secondary, accent (акцент ОС в light/dark), accent_text (акцент для текста на `frame`/`header` — клавиши keybar, части чипов; `accent`, затемнённый до ≥4,5:1), accent_soft (тон заголовка и вкладки активной панели, шапки таблицы Markdown; 8 % `accent` на `panel`, в тёмной — 16 %), cursor, cursor_text, border, current_line (строка редактора), highlight (совпадения поиска), modified, exec, archive, context_admin, context_remote, warning_bg (подложка вопроса/предупреждения — баннер журнала, конфликт), danger_bg (подложка опасного действия — F8); shadow (тень всплывающих объектов: чёрный 16 % в светлых темах, 55 % в тёмной; в высокой контрастности — нет); syntax: keyword, string, number, comment. Контраст text-ролей и syntax к panel ≥4,5:1, text и secondary к warning_bg/danger_bg и к accent_soft — тоже; accent_text к frame и header ≥4,5:1; modified, exec, archive к cursor ≥4,5:1 (в тёмной modified и archive — 3,5 и 4,3, как до палитры B: отмеченная строка там — accent + полужирный); exec не совпадает по тону с accent.

```toml
[system-light]
frame="#E9E9E9" panel="#F4F4F4" header="#EEEEEE" text="#1B1B1B" secondary="#5F5F5F"
accent="#0067C0" accent_text="#0067C0" accent_soft="#E0E9F0" cursor="#D3E3F1" cursor_text="#1B1B1B" border="#DCDCDC"
current_line="#ECECEC" highlight="#F2E3A0"
modified="#BC2E19" exec="#19741E" archive="#8042CA" context_admin="#BC2E19" context_remote="#A86A12" warning_bg="#F8EBD3" danger_bg="#F7E2DD"
syntax = { keyword="#1F5FA8", string="#8A4B12", number="#6A3FA0", comment="#5F6B76" }

[system-dark]
frame="#1C1C1C" panel="#262626" header="#2B2B2B" text="#F2F2F2" secondary="#C6C6C6"
accent="#6CC4F2" accent_text="#6CC4F2" accent_soft="#313F47" cursor="#24557A" cursor_text="#FFFFFF" border="#3C3C3C"
current_line="#2E2E2E" highlight="#5C4A1C"
modified="#FF8C78" exec="#7FDB78" archive="#C9B4FF" context_admin="#FF8C78" context_remote="#FFC46B" warning_bg="#3D3220" danger_bg="#402622"
syntax = { keyword="#7FB8F0", string="#E3B07A", number="#C9B4FF", comment="#949494" }

[blue]
frame="#E1E9F1" panel="#EEF3F8" header="#E8EEF4" text="#1E2A36" secondary="#576470"
accent="#3A6EA5" accent_text="#396BA1" accent_soft="#E0E8F1" cursor="#D4E3F2" cursor_text="#1E2A36" border="#D3DDE8"
current_line="#E6EDF4" highlight="#F0E2A6"
modified="#B83419" exec="#19733A" archive="#8242C7" context_admin="#B83419" context_remote="#A86A12" warning_bg="#F5EAD6" danger_bg="#F4E2DE"
syntax = { keyword="#2F5F96", string="#8A4B12", number="#6A3FA0", comment="#5E6B78" }

[turquoise]
frame="#DFEDEB" panel="#EDF5F4" header="#E6F1EF" text="#1D2E2D" secondary="#526866"
accent="#267A75" accent_text="#24746F" accent_soft="#DDEBEA" cursor="#CFE9E6" cursor_text="#1D2E2D" border="#CFE1DE"
current_line="#E4F0EE" highlight="#F0E2A6"
modified="#B24019" exec="#27751A" archive="#8444C7" context_admin="#B24019" context_remote="#A86A12" warning_bg="#F4EAD6" danger_bg="#F4E2DC"
syntax = { keyword="#1F6F6A", string="#8A4B12", number="#6A3FA0", comment="#5A7270" }

[grass]
frame="#E3ECDD" panel="#EEF4EA" header="#E8F0E3" text="#222D1C" secondary="#5C6A54"
accent="#457933" accent_text="#437531" accent_soft="#E0EADB" cursor="#DCEBCF" cursor_text="#222D1C" border="#D5E0CC"
current_line="#E6EEE1" highlight="#F0E2A6"
modified="#B73F18" exec="#1A6E99" archive="#8547C8" context_admin="#B73F18" context_remote="#A86A12" warning_bg="#F3E9D4" danger_bg="#F3E1DA"
syntax = { keyword="#3E6E2E", string="#8A4B12", number="#2C5F8F", comment="#5F6D57" }
# exec не зелёный — не сливается с акцентом

[sepia]
frame="#E7DECF" panel="#F2ECE1" header="#ECE4D7" text="#3A3128" secondary="#6A5E50"
accent="#8C6035" accent_text="#835A31" accent_soft="#EAE1D3" cursor="#E8DCC6" cursor_text="#3A3128" border="#DACFBC"
current_line="#EBE3D6" highlight="#EAD49A"
modified="#B3341A" exec="#3D6D19" archive="#8B3FAF" context_admin="#B3341A" context_remote="#7A6A00" warning_bg="#EFDDBE" danger_bg="#EED5CB"
syntax = { keyword="#7A4A1F", string="#5E6A2E", number="#6A3F7A", comment="#7A6C5C" }
```

**Подсветка синтаксиса** (tree-sitter, S7 §5.7, S7-21): четыре роли `syntax` на всё — новых ролей нет, свои темы не ломаются. Захваты запросов сводятся к ролям по первому слову имени (`urfin_view::syntax::tag_of`):
- `keyword` — ключевые слова, типы, функции и команды (`cargo` в блоке PowerShell), теги XML, заголовки таблиц TOML, ключи YAML, секции INI, заголовки и маркеры Markdown;
- `string` — строки, символы, экранирование, регулярные выражения, код в Markdown, адреса ссылок;
- `number` — числа, `true`/`false`/`null`, встроенные константы, даты TOML;
- `comment` — комментарии; в Markdown — ограды блоков кода, `>` цитат, черта;
- как текст — ключи TOML/JSON/INI, переменные, свойства, операторы, скобки (захват `@none` у своих запросов).

Вшитые грамматики (JSON, TOML, YAML, INI, XML, Markdown) — со своими запросами `assets/syntax/<язык>/highlights.scm` по макетам (`screen-quickview.png`); пакеты грамматик рядом с exe — с запросами из репозиториев грамматик. В высокой контрастности все четыре роли — текст окна.

Плотность строк 20/24/28 px (умолч. 24). Иконки файлов: нет / Segoe Fluent моно / цветные по ролям типов (умолч. с 2026-11-01, палитра B). Отмеченные файлы — цвет accent + полужирный.
