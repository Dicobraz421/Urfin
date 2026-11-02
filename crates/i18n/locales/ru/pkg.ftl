# Менеджеры пакетов (urfin-pkg, F02).
pkg-not-yet = Пока не сделано: { $what }
# choco меняет систему только с правами администратора: запрос UAC перед действием (F02 §8).
pkg-choco-uac = choco требует прав администратора — будет запрос UAC
# Состояние задачи, если пользователь отклонил запрос UAC.
pkg-uac-declined = отклонено
# Повышенный choco после отмены продолжает работу: наш процесс его не остановит.
pkg-cancel-elevated = прервано, choco мог продолжить работу в своём окне

## Вкладка пакетов `pkg:` (F02-08, `screen-pkginstalled`, `screen-pkgupdates`).
pkg-chip = Пакеты
pkg-tab = pkg:
pkg-col-name = Программа
pkg-col-version = Версия
pkg-col-available = Доступна
pkg-col-mgr = Менеджер
pkg-col-publisher = Издатель, источник
# Установленная программа, которую не ведёт ни один менеджер (winget видит её в «Программах» без источника).
pkg-outside = вне менеджера
# У scoop издателя нет: бакет, из которого программа.
pkg-bucket = bucket { $name }
pkg-filter-installed = Установленные
pkg-filter-updates = Обновления
pkg-filter-search = Найти
# Справа в заголовке вкладки.
pkg-count = { $count ->
    [one] { $count } программа
    [few] { $count } программы
   *[many] { $count } программ
}
pkg-of = { $n } из { $total }
# Статус-строка: когда менеджеры спрашивали в последний раз.
pkg-checked-today = Проверено сегодня в { $time }
pkg-checked-on = Проверено { $date } в { $time }
pkg-checking = Проверяется…
pkg-never = Ещё не проверено
pkg-no-managers = Менеджеры пакетов не найдены
# Менеджер, чей последний запрос не удался: имя и ошибка.
pkg-failed = { $mgr }: { $error }
pkg-important = важное
# Строка внизу «Обновлений» при отметке.
pkg-upgrade-n = Обновить { $count }
pkg-upgrade = Обновить
pkg-upgrade-all = Обновить все { $count }
pkg-cancel = Отмена
# Подсказки клавиш вкладки.
pkg-key-upgrade = Обновить
pkg-key-uninstall = Удалить
pkg-key-check = Проверить обновления
pkg-key-output = Вывод в терминале
pkg-key-mark = Отметить
pkg-key-upgrade-marked = Обновить отмеченные
pkg-key-jobs = Задачи

## Поиск (F02-09, `screen-pkgsearch`, `screen-pkgmgrpick`).
pkg-col-name-search = Программа · пакет
pkg-col-source = Источник
pkg-col-publisher-search = Издатель
# Вариант первого менеджера по порядку `[pkg].order`.
pkg-recommended = рекомендуется
pkg-search-title = Найти пакет
# «Найти пакет [7zip] в [Все ▾]».
pkg-search-in = в
pkg-search-all = Все
pkg-search-go = Найти
# Мешочек менеджеров: «Все» ищет во всех сразу.
pkg-search-parallel = параллельно
pkg-search-absent = не найден на компьютере
pkg-search-order = Порядок: { $order }
# Строка ответов под полем: «winget · 4», «scoop ищет…», «choco не ответил».
pkg-search-busy = ищет…
pkg-search-failed = не ответил
pkg-search-empty = Ничего не нашлось
pkg-search-none = Менеджеры пакетов не найдены
pkg-search-chip = Найти «{ $q }»
pkg-search-note = { $groups ->
    [one] { $groups } программа
    [few] { $groups } программы
   *[many] { $groups } программ
} · { $variants ->
    [one] { $variants } вариант
    [few] { $variants } варианта
   *[many] { $variants } вариантов
}
pkg-key-results = К результатам
pkg-key-manager = Менеджер
pkg-key-close = Закрыть строку
pkg-key-install-marked = Установить отмеченные
pkg-key-choose = Выбор
pkg-key-apply = Применить
pkg-key-close-list = Закрыть список

## Действия (F02-10, `screen-pkginstall`).
pkg-busy-install = устанавливается…
pkg-busy-upgrade = обновляется…
pkg-busy-uninstall = удаляется…
pkg-busy-queued = в очереди
pkg-busy-uac = ждёт подтверждения UAC
# Подтверждение F8 одной строкой.
pkg-uninstall-one = Удалить { $name } ({ $mgr })?
pkg-uninstall-n = Удалить { $count ->
    [one] { $count } пакет
    [few] { $count } пакета
   *[many] { $count } пакетов
}?
pkg-uninstall-yes = Удалить
# Заголовок пакетной задачи: «winget upgrade · 3 пакета».
pkg-job-many = { $cmd } · { $count ->
    [one] { $count } пакет
    [few] { $count } пакета
   *[many] { $count } пакетов
}
pkg-reboot = Чтобы закончить установку, перезагрузите компьютер
pkg-later = Позже
