# Тексты интерфейса (S2).
ui-footer-selected = { $count } { $count ->
    [one] выбран
   *[other] выбрано
} · { $bytes }
ui-footer-items = { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} · { $bytes }
ui-delete-trash = В корзину { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $bytes })?
ui-conflict-head = Конфликт { $index } из { $total } · { $job }
ui-loading = Загрузка…
ui-listing-failed = Не удалось прочитать папку: { $error }
ui-lang-unavailable = Языка { $lang } нет, используется английский
ui-warn-no-system-font = Системный шрифт интерфейса не найден, используется встроенный
ui-type-folder = Папка
ui-type-file = Файл
ui-type-file-ext = Файл { $ext }
ui-type-app = Приложение
ui-type-archive = Архив { $ext }
ui-type-link = Ярлык
ui-col-name = Имя
ui-col-size = Размер
ui-col-mtime = Изменён
ui-col-ext = Расширение
ui-col-type = Тип
ui-col-ctime = Создан
ui-col-atime = Доступ
ui-col-attrs = Атр.
ui-columns-set = Набор «{ $name }»
ui-folder-empty = Папка пуста
ui-retry = Повторить
ui-loading-count = Загрузка… { $count }

# Breadcrumb (S2-12)
ui-menu-loading = Загрузка…
ui-menu-empty = Папок нет
ui-path-not-found = Не открыть { $path }: { $error }

ui-renderer-fallback = Графический драйвер сбоит — включён программный рендер

# Быстрый поиск и начатый аккорд в подвале панели (S3-06)
ui-qs-matches = Найдено: { $count }
ui-chord-pending = { $chord } … (ждёт следующую клавишу)

# Режимы панели: дерево, сведения, быстрый просмотр (S2-18)
ui-tree-title = Дерево · { $root }
ui-tree-title-empty = Дерево
ui-tree-hint = ← → свернуть/развернуть · Enter — открыть
ui-info-title = Сведения
ui-info-hint = Ctrl+L — закрыть · обновляется на лету
ui-info-volume = Том
ui-info-fs = Файловая система
ui-info-caps = Возможности
ui-info-free = Свободно
ui-info-free-value = { $free } из { $total } ({ $pct } %)
ui-info-media-net = сеть
ui-info-media-usb = съёмный
ui-info-cap-trash = корзина
ui-info-cap-ads = ADS
ui-info-cap-compression = сжатие
ui-info-cap-clone = клонирование блоков
ui-info-cap-case = регистр букв
ui-info-path = Путь
ui-info-content = Содержимое
ui-info-content-value = папок: { $dirs } · файлов: { $files }
ui-info-bytes = Объём файлов
ui-info-selected = Выделено
ui-info-cursor = Под курсором
ui-info-name = Имя
ui-info-type = Тип
ui-info-size = Размер
ui-info-mtime = Изменён
ui-info-attrs = Атрибуты
ui-info-kind-dir = папка
ui-info-kind-file = файл
ui-info-kind-link = ссылка
ui-info-kind-other = другое
ui-qview-hint = F3 — просмотр
ui-qview-binary = двоичный
ui-qview-dir = Папка — её содержимое в панели
ui-info-volume-local = Локальный диск

## Диски (D-03)
ui-drives-section-drives = Диски
ui-drives-section-network = Сеть
ui-drives-section-places = Места
ui-drives-free = свободно { $bytes }
ui-drives-offline = нет связи
ui-place-home = Домашняя папка
ui-place-desktop = Рабочий стол
ui-place-documents = Документы
ui-place-downloads = Загрузки
ui-size-dir = <DIR>

## Состояния папки (D-03)
ui-state-empty = Папка пуста
ui-state-denied = Нет доступа к папке
ui-state-denied-text = Читать эту папку могут только администраторы.
ui-state-offline = Нет связи с { $path }
ui-state-offline-text = Сетевой ресурс не отвечает. Остальная работа не ждёт.
ui-state-failed = Не удалось прочитать папку
ui-state-back = Назад
ui-state-retry = Повторить сейчас

ui-tree-label = дерево
ui-qv-title = Быстрый просмотр
ui-info-summary = Папка { $path }: файлов { $files }, папок { $dirs } · { $bytes }
ui-info-free-of = { $free } свободно из { $total }

# Мешочек «История» у пути панели (S7-25a): разделы; кнопка «Закладки»
ui-history-dirs = Папки
ui-history-files = Файлы
ui-bookmarks = Закладки

# Вкладка анализа места `du:` (F01-05, F01-06)
du-chip = Анализ
du-filter-dirs = По папкам
du-filter-large = Крупные файлы
du-filter-types = Типы
du-filter-old = Старые
du-filter-dups = Дубликаты
du-filter-temp = Временные
du-col-share = Доля в папке
du-total = { $files } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
} · { $size }
du-total-scanning = просмотрено { $files } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
} · { $size }
du-ago = посчитано { $time } назад
du-took = посчитано за { $time }
du-time-sec = { $sec } с
du-time-min-sec = { $min } мин { $sec } с
du-time-min = { $n } мин
du-time-h = { $n } ч
du-time-d = { $n } дн.
du-refresh = Обновить
du-counting = считается
du-marked = Отмечено { $count }
du-frees = освободится { $size }
du-delete-trash = Удалить в Корзину
du-unmark = Снять отметку

# Фильтры и карта анализа места (F01-07, F01-08)
du-col-files = Файлов
du-col-group = Группа
du-col-where = Где
du-age-label = старше
du-age-6m = 6 мес
du-age-1y = 1 год
du-age-2y = 2 года
du-age-5y = 5 лет
du-temp-warning = Проверьте перед удалением: здесь только подсказки, сами не отмечаются. Сборки и node_modules пересоздаются долго, в %TEMP% бывают файлы запущенных программ.
du-types-status = { $groups } { $groups ->
    [one] группа
    [few] группы
   *[many] групп
} · { $files } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
}
du-temp-status = Найдено { $count } · { $size }
du-col-dups = Группа · где лежит
du-dups-copies = { $count } { $count ->
    [one] копия
    [few] копии
   *[many] копий
}
du-dups-hash = ✓ совпадает по хэшу
du-dups-size = только размер
du-dups-summary = { $groups } { $groups ->
    [one] группа
    [few] группы
   *[many] групп
}, хэшем { $checked ->
    [one] проверена
   *[other] проверены
} { $checked }
du-dups-marked = отмечено { $count } · освободится { $size }
du-dups-verify = Проверить хэшем { $count } { $count ->
    [one] группу
    [few] группы
   *[many] групп
}
du-temp-select-all = Отметить все
du-temp-clean = Очистить выбранное…
du-temp-explain = { $name } — { $what }: { $after }
du-dups-trash = В Корзину
du-dups-hashing = Проверяю хэшем…
du-dups-status = { $groups } { $groups ->
    [one] группа
    [few] группы
   *[many] групп
} · { $files } { $files ->
    [one] файл
    [few] файла
   *[many] файлов
}
du-map-title = Карта
du-map-dirs = Папки
du-map-archives = Архивы
du-map-exec = Исполняемые
du-map-temp = Временные
du-map-other = Прочее
du-map-out = Вне фильтра
du-map-marked = Отмечено
du-map-more = ещё
du-map-small = мелкие файлы
du-map-scanning = Карта — когда подсчёт закончится
