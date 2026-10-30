## О программе / сплэш (R-01). Экранов ещё нет: TODO S2-14 (меню F9).
ui-about-name = Úrfionn
ui-about-aka = он же Урфин Джюс

## Темы (T-01, №59)
ui-theme-auto = Как в Windows
ui-theme-system-light = Системная светлая
ui-theme-system-dark = Системная тёмная
ui-theme-blue = Голубая
ui-theme-turquoise = Бирюза
ui-theme-grass = Травяная
ui-theme-sepia = Сепия
ui-theme-locked = Тема задана политикой организации
ui-theme-unknown = Неизвестная тема «{ $id }»
ui-theme-error = Тема не загружена: { $error }
ui-context-admin = Администратор
ui-context-remote = Удалённый сеанс

## Настройки (D-08)
ui-set-title = Настройки
ui-set-view = Вид
ui-set-panels = Панели
ui-set-keys = Клавиатура
ui-set-ops = Операции
ui-set-policies = Политики
ui-set-a11y = Доступность
ui-set-about = О программе
ui-set-theme = Тема
ui-set-follow-os = Как в Windows — светлая или тёмная и акцент из системы
ui-set-density = Высота строки
ui-set-icons = Иконки файлов
ui-set-icons-none = Без иконок
ui-set-icons-mono = Монохромные
ui-set-icons-color = Цветные
ui-set-lang = Язык
ui-set-fonts = Шрифты
ui-set-font-ui = Интерфейс — Segoe UI Variable, 13
ui-set-font-mono = Код и терминал — Cascadia Mono, 12,5
ui-set-panel-view = Вид панели
ui-set-view-mode = Режим по умолчанию
ui-set-for-new-tabs = для новых вкладок
ui-set-sort = Сортировка
ui-set-dirs-first = Папки сверху
ui-set-preset = Раскладка клавиш
ui-set-preset-note = своя раскладка — keymap.toml в папке настроек
ui-set-ops-delete = Удаление
ui-set-trash = Удаление
ui-set-trash-on = В корзину (Shift — насовсем)
ui-set-trash-note = F8 и Del
ui-set-fast-trash = Быстрая корзина
ui-set-fast-trash-on = Включена
ui-set-ops-copy = Копирование и перенос
ui-set-errors = Ошибки
ui-set-errors-ask = Спросить
ui-set-errors-skip = Пропустить
ui-set-errors-abort = Остановить
ui-set-verify = Проверка после записи
ui-set-verify-off = Выкл.
ui-set-verify-size = Размер
ui-set-verify-note = хэш — флажком в строке F5
ui-set-policies-title = Политики организации
ui-set-policies-note = Эти параметры задаёт организация: файл %ProgramData%\Urfin\policy.toml (TOML) или групповая политика (GPO). Если параметр задан в обоих, действует GPO. В Урфине их изменить нельзя — только администратор компьютера. Заданные параметры в других разделах отмечены замком.
ui-set-policy-param = Параметр
ui-set-policy-value = Значение
ui-set-policy-source = Источник
ui-set-policy-theme = Тема
ui-set-policy-locked = Запретить смену темы
ui-set-policy-unset = не задано
ui-set-yes = Да
ui-set-no = Нет
ui-set-by-policy = Задано политикой организации
ui-set-policy-trash = Удалять в корзину
ui-set-policy-fast-trash = Быстрая корзина
ui-set-policy-verify = Проверка после записи
ui-set-policy-password-cache = Кэш паролей SSH
ui-set-policy-ssh-recording = Запись сеансов SSH
ui-set-policy-multi-exec = Мультивыполнение SSH
ui-set-policy-du-helper = Помощник анализа места
ui-set-policy-du-mft = Чтение MFT
ui-set-policy-allowed = { $g ->
    [f] Разрешена
    [n] Разрешено
   *[m] Разрешён
}
ui-set-policy-forbidden = { $g ->
    [f] Запрещена
    [n] Запрещено
   *[m] Запрещён
}
ui-set-policy-rec-required = Обязательна
ui-set-policy-rec-forbidden = Запрещена
ui-set-policy-rec-user = На выбор пользователя
ui-set-verify-hash = Хэш
ui-set-policy-count = задано { $set } из { $total }
ui-set-read-only = Только чтение
ui-set-visibility = Видимость
ui-set-zoom = Масштаб
ui-set-zoom-reset = Сбросить
ui-set-version = Версия { $version }
ui-set-offline = Работает полностью офлайн
ui-set-license = Лицензия
ui-set-config-dir = Папка настроек
ui-set-log-dir = Журнал
ui-set-open = Открыть
ui-set-author = Антон Антропов
ui-set-applies-now = Применяется сразу
ui-keys-section = Раздел
ui-set-app-name = Урфин
ui-set-dirs = Папка уже есть
ui-set-dirs-merge = Объединить
ui-set-cloud = Файлы в облаке
ui-set-cloud-hydrate = Скачивать
ui-set-durability-group = Надёжность
ui-set-durability = Запись файлов
ui-set-durability-fast = Быстро
ui-set-durability-safe = Надёжно
ui-set-durability-note = надёжно — сброс каждого файла на диск, переживает отключение питания
