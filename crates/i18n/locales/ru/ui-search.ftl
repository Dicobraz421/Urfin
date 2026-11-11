## Поиск файлов (S8 §2.5, `screen-search`, `screen-searchpanel`, `screen-searchgrep`).
## $time — готовая строка: `du-time-sec` (десятые: 0,4 с), `du-time-min-sec`.
## Число совпадений в статус-строке — `ui-find-count`.

# Форма
ui-search-name = Имя
ui-search-text = Текст
ui-search-where = В
ui-search-recursive = Подкаталоги
ui-search-case-tip = Учитывать регистр
ui-search-regex-tip = Регулярное выражение

# Список результатов
ui-search-col-file = Файл
ui-search-col-match = Совпадение
ui-search-summary = Найдено { $count } { $count ->
    [one] совпадение
    [few] совпадения
   *[many] совпадений
} в { $files } { $files ->
    [one] файле
   *[other] файлах
}
ui-search-scanned = просмотрено { $count } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
} · { $time }

# Итог поиска только по имени (нет текста): `screen-search`
ui-search-found-files = { $count ->
    [one] Найден { $count } файл
    [few] Найдено { $count } файла
   *[many] Найдено { $count } файлов
}
ui-search-found-dirs = { $count ->
    [one] Найдена { $count } папка
    [few] Найдено { $count } папки
   *[many] Найдено { $count } папок
}

# Предпросмотр
ui-search-preview-line = стр { $line } · предпросмотр

# Статус-строка
ui-search-status = Поиск «{ $query }» в { $root }
ui-search-status-done = готово за { $time }

# Панель результатов
ui-search-chip = Поиск
ui-search-query-name = { $mask } в { $root }
ui-search-query-text = «{ $text }» в { $root }
ui-search-panel-progress = найдено { $found } · просмотрено { $scanned }
ui-search-panel-total = { $count } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
} · { $time }
ui-search-col-line = Стр
ui-search-col-hit = Первое совпадение

# Панель сравнения `cmp:` (D-12, `screen-cmppanel`)
ui-cmp-chip = Сравнение
ui-cmp-only_left = только слева
ui-cmp-only_right = только справа
ui-cmp-newer_left = новее слева
ui-cmp-newer_right = новее справа
ui-cmp-differs = отличается
ui-cmp-totals = Итоги сравнения
ui-cmp-progress = различий { $found } · просмотрено { $scanned }
ui-cmp-equal = одинаковых { $count }
ui-cmp-mode-content = по содержимому
ui-cmp-mode-quick = по размеру и дате
ui-cmp-left = слева { $size } · { $date }
ui-cmp-right = справа { $size } · { $date }

# Баннеры и ошибки запроса
ui-search-max-hits = Поиск остановлен: совпадений больше { $limit }. Уточните запрос
ui-search-denied = нет доступа к { $count } { $count ->
    [one] папке
   *[other] папкам
}
ui-search-bad-mask = Неверная маска файлов: { $mask }
ui-search-bad-regex = Ошибка в регулярном выражении: { $error }
ui-search-bad-where = Неверный путь: { $path }

# Команды экрана
cmd-ui-search-focus = Фокус на экране поиска
cmd-ui-search-row = Строка результатов
