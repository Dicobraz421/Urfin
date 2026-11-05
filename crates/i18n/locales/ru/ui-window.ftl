## Каркас окна (D-02, №61)
ui-panel-count = { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
}
ui-status-selected = Выделено { $count } из { $total }
ui-status-selected-bytes = Выделено { $count } из { $total } · { $bytes }
ui-status-free = { $root } { $bytes } свободно
ui-status-shell-cwd = shell: { $dir }
ui-status-dir = { $name } · { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
}

## Каркас: табы, приветствие, ссылки (D-02, D-03)
ui-tabs-more = Скрытые вкладки
ui-keys-tab = Вкладка
ui-welcome-title = Добро пожаловать в Урфин
ui-welcome-keys = Клавиши
ui-welcome-hint = Alt+буква — поиск, F10 — выход
ui-welcome-theme = Тема как в Windows
ui-welcome-done = Готово
ui-welcome-later = Позже
ui-welcome-updated = Урфин обновлён до { $to } · с { $from }
ui-welcome-news = Что нового
ui-keys-preset = Far ↔ DN
ui-keys-flag = Флажок
ui-keys-next-field = Следующее поле
ui-link-broken = цель не найдена
ui-size-symlink = <SYMLINK>
ui-size-junction = <JUNCTION>
ui-size-link = <LINK>
# Подсветка вывода терминала (F03-10): чип в шапке и клавиша.
ui-term-hl-chip = Подсветка вывода
ui-keys-term-hl = Подсветка
# Запись сеанса (F03-12): чип, клавиша, статус.
ui-term-rec-chip = Запись { $time }
ui-term-rec-required = обязательна · политика · { $source }
ui-term-rec-stop = Остановить
ui-keys-term-rec = Запись
ui-term-rec-status = Запись в { $path } · asciicast v2, { $size }
ui-term-rec-starting = Запись · asciicast v2
# Мультивыполнение SSH (F03-11): экран группы, чип, статус, клавиши.
ui-group-title = Сеансы · группа ввода
ui-group-chip = Ввод в { $n } { $n ->
    [one] сеанс
    [few] сеанса
   *[many] сеансов
}:
ui-group-leave = Выйти
ui-group-code = код { $code }
ui-group-status = Группа: { $n } { $n ->
    [one] сеанс
    [few] сеанса
   *[many] сеансов
} из { $total }
ui-group-confirmed = первая команда подтверждена в { $time }
ui-group-ask = Отправить в { $n } { $n ->
    [one] сеанс
    [few] сеанса
   *[many] сеансов
}:
ui-group-ask-tail = ? Enter — да, Esc — нет
ui-keys-group = Группа
ui-keys-group-edit = Состав группы
ui-keys-group-leave = Выйти из группы
ui-keys-group-toggle = В группу / из группы
ui-keys-group-send = Отправить
ui-keys-group-cancel = Не отправлять
ui-keys-space = Пробел
