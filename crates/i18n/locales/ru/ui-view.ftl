## Просмотр (S7 §3, §6): шапка, статус, тосты
ui-view-readonly = только чтение
ui-view-enc-auto = авто
ui-view-mode-text = Текст
ui-view-mode-markup = Разметка
ui-view-mode-hex = Hex
ui-view-mode-markup-off = Разметка: язык неизвестен или файл больше { $mb } МБ
ui-view-follow = Слежение
ui-view-to-panels = К панелям: { $path }
ui-view-lines = Стр { $from }–{ $to } из { $total } · { $pct } %
ui-view-line-approx = стр ≈{ $line }
ui-view-indexing = индексация { $pct } %
ui-view-searching = Поиск… { $pct } %
ui-view-wait-index = Ждём индекс… { $pct } % · Esc — отмена
ui-view-offsets = { $from }–{ $to } · { $pct } %
ui-view-offset = Смещение { $offset } · { $hex }h = { $dec } · «{ $char }»
ui-view-marks = { $count } { $count ->
    [one] отметка
    [few] отметки
   *[many] отметок
} «{ $text }»
ui-view-eol-mixed = Смешанные
ui-view-plain = Текст
ui-view-enc-rereads = Смена кодировки перечитает файл с диска
ui-view-utf32 = UTF-32 не поддерживается — показано в Hex
ui-view-copy-limit = Выделено больше { $mb } МБ — не скопировано
ui-view-truncated = Файл усечён — показан конец
ui-view-rotated = Файл заменён — открыт заново
ui-view-not-found = Не найдено: «{ $text }»
ui-view-history-empty = Просмотренных и изменённых файлов пока нет
ui-view-history-view = История просмотра
ui-view-history-edit = История правки
ui-view-history-find = Найти файл
ui-screens-title = Экраны
ui-screens-panels = Панели
ui-screens-view = просмотр
ui-screens-edit = правка
ui-view-external-local = Внешний просмотр — только для файлов на локальном диске
ui-view-bookmark-set = Закладка { $n } поставлена
ui-view-bookmark-none = Закладки { $n } нет — Ctrl+Shift+{ $n } ставит

## Быстрый просмотр (S7 §4)
ui-qv-lines = стр { $from }–{ $to } из { $total }
ui-qv-cloud = Файл в облаке — F3 для загрузки
ui-qv-dir-size = F3 — размер папки

## Поиск, замена, переход (S7 §3.5, §3.6, §5.2)
ui-find = Найти
ui-find-of = { $n } из { $total }
ui-find-of-more = { $n } из { $total }+
ui-find-none = Не найдено
ui-find-case = Регистр
ui-find-word = Слово
ui-find-regex = Регулярное
ui-find-hex = Hex
ui-find-wrapped = Продолжено с начала
ui-find-bad-regex = Ошибка в регулярном выражении: { $error }
ui-find-bad-hex = Hex: байты вида DE AD ?? EF
ui-replace = Заменить на
ui-replace-one = Заменить
ui-replace-all = Заменить все
ui-replace-groups = $1, $2 — группы из «Найти»
ui-replace-done = Заменено: { $count }
ui-goto-line = Перейти к строке
ui-goto-offset = Перейти к смещению
ui-goto-of-line = из { $total } · строка
ui-goto-of-line-col = из { $total } · строка:колонка
ui-goto-of-offset = из { $total } · смещение
ui-goto-invalid = Нужна строка, смещение (0x1F00, 1F00h) или процент
ui-button-go = Перейти
ui-keys-find-replace = Найти ↔ Заменить
ui-keys-select = Выбор
ui-keys-cycle = По кругу

## Кодировки (S7 §3.2, screen-viewerenc)
ui-enc-auto = Авто
ui-enc-auto-hint = определять по содержимому
ui-enc-unicode = Юникод
ui-enc-cyr-windows = Кириллица, Windows
ui-enc-cyr-dos = Кириллица, DOS
ui-enc-cyr-unix = Кириллица, Unix
ui-enc-ukrainian = Украинская
ui-enc-cyr-iso = Кириллица, ISO
ui-enc-cyr-mac = Кириллица, macOS
ui-enc-western = Западная
ui-enc-other = Другие

## Редактор (S7 §5): шапка, встроенные строки, вопросы, статус
ui-edit-added = +{ $count } добавлено
ui-edit-changed = { $count } { $count ->
    [one] изменена
   *[other] изменено
}
ui-edit-removed = −{ $count } удалено
ui-edit-loading = Загрузка… { $pct } %
ui-edit-too-big = Файл слишком большой для редактора ({ $size })
ui-edit-open-viewer = Открыть в просмотре
ui-edit-external = Внешний редактор
ui-edit-external-failed = Не удалось запустить внешний редактор: { $error }
ui-edit-lossy = Есть байты, недопустимые в { $enc }. Сохранение заменит их.
ui-edit-change-enc = Сменить кодировку
ui-edit-readonly-save = Файл только для чтения. Снять атрибут и сохранить?
ui-edit-clear-readonly = Снять и сохранить
ui-edit-keep-readonly = Сохранить, атрибут оставить
ui-edit-unmappable = { $count } { $count ->
    [one] символ
    [few] символа
   *[many] символов
} нельзя записать в { $enc }
ui-edit-save-utf8 = Сохранить в UTF-8
ui-edit-replace-q = Заменить на ?
ui-edit-disk-changed = Файл изменён на диске
ui-edit-reload = Перезагрузить
ui-edit-overwrite = Сохранить поверх
ui-edit-unsaved = Не сохранено: { $count }
ui-edit-save-all = Сохранить все
ui-edit-dont-save = Не сохранять
ui-edit-quit-kept = Выход отменён: «{ $name }» не сохранён
ui-edit-close-changed = «{ $name }» изменён. Сохранить?
ui-edit-save = Сохранить
ui-edit-cp-on-save = Документ изменён: { $enc } — при сохранении
ui-edit-remote-save-as = Файл не на этом компьютере: сохраните его в локальную папку через «Сохранить как»
ui-edit-saving = Сохранение { $name }
ui-edit-save-failed = Не удалось сохранить { $name }: { $error }
ui-edit-pos = Стр { $line }, Кол { $col }
ui-edit-lines = { $count } { $count ->
    [one] строка
    [few] строки
   *[many] строк
}
ui-edit-spaces = Пробелы: { $n }
ui-edit-tabs = Табуляция: { $n }
ui-edit-insert = Вставка
ui-edit-overtype = Замена
ui-edit-md-only = Только для Markdown-файлов

## Панель инструментов редактора (S7 §5.8, screen-editor, screen-editor-md)
ui-edit-tb-save = Сохранить
ui-edit-tb-undo = Отменить
ui-edit-tb-redo = Вернуть
ui-edit-tb-find = Найти
ui-edit-tb-replace = Заменить
ui-edit-tb-wrap = Перенос
ui-edit-tb-bold = Жирный
ui-edit-tb-italic = Курсив
ui-edit-tb-heading = Заголовок
ui-edit-tb-heading-0 = Обычный текст
ui-edit-tb-heading-n = Заголовок { $level }
ui-edit-tb-list = Список
ui-edit-tb-numbered = Нумерованный
ui-edit-tb-task = Задача
ui-edit-tb-link = Ссылка
ui-edit-tb-code = Код
ui-edit-tb-table = Таблица
ui-edit-tb-preview = Предпросмотр
ui-edit-tb-encoding = Кодировка
ui-edit-tb-eol = Конец строки
ui-edit-tb-more = Ещё
ui-edit-tb-no-undo = Нечего отменить
ui-edit-tb-no-redo = Нечего вернуть
ui-edit-tb-not-modified = Нет изменений
ui-edit-tb-too-big = Файл больше { $mb } МБ
ui-edit-tb-readonly = Только чтение

## Markdown-документ (S7 §3.10, §5.9, screen-viewer-md)
ui-md-img = картинка
ui-md-img-remote = из сети не загружается
ui-md-img-missing = файл не найден
ui-md-img-not-local = не на локальном диске
ui-md-img-format = формат не поддерживается
ui-md-img-too-big = больше { $mb } МБ
ui-md-hidden = { $count } { $count ->
    [one] картинка не показана
    [few] картинки не показаны
   *[many] картинок не показано
}
ui-md-hidden-remote = { $count } { $count ->
    [one] картинка из сети не показана
    [few] картинки из сети не показаны
   *[many] картинок из сети не показано
}
ui-md-link-open = Ctrl+клик — открыть в браузере
ui-md-details = Подробнее
ui-md-table-col = Столбец { $n }

## Настройки: Просмотр (S7 §8, screen-settings-viewer)
ui-set-viewer = Просмотр
ui-set-viewer-open = Открытие
ui-set-viewer-placement = Где открывать по F3
ui-set-viewer-window = Окно
ui-set-viewer-panel = Панель
ui-set-viewer-placement-note = панель — как быстрый просмотр
ui-set-viewer-external = Внешний просмотрщик
ui-set-viewer-external-default = Программа Windows для типа файла
ui-set-viewer-remember = Запоминать позицию
ui-set-viewer-remember-on = Позиция, кодировка и режим файла
ui-set-viewer-remember-note = { $count } последних · Alt+F11
ui-set-viewer-text = Текст
ui-set-viewer-codepage = Кодировка по умолчанию
ui-set-viewer-codepage-auto = Авто — по содержимому
ui-set-viewer-codepage-note = F8: { $ring }
ui-set-viewer-wrap = Перенос строк
ui-set-viewer-wrap-off = Не переносить
ui-set-viewer-wrap-words = По словам
ui-set-viewer-wrap-chars = По символам
ui-set-viewer-markdown = Markdown
ui-set-viewer-markdown-document = Документ
ui-set-viewer-markdown-source = Исходник
ui-set-viewer-markdown-note = вид .md в режиме «Разметка»
ui-set-tab-size = Табуляция
ui-set-viewer-hex-cols = Hex: байт в строке
ui-set-viewer-hex-auto = Авто
ui-set-viewer-hex-note = авто — по ширине окна
ui-set-viewer-large = Большие файлы и логи
ui-set-viewer-mmap = Отображение в память
ui-set-viewer-mmap-on = Для файлов от { $mb } МБ на локальном диске
ui-set-viewer-mmap-note = кроме изменённых за последние { $sec } с
ui-set-viewer-follow = Слежение за логом
ui-set-viewer-follow-on = Переоткрывать после ротации
ui-set-viewer-follow-note = Shift+F · проверка раз в { $ms } мс

## Настройки: Редактор (S7 §8, screen-settings-editor)
ui-set-editor = Редактор
ui-set-editor-text = Текст
ui-set-editor-font = Шрифт
ui-set-editor-font-value = { $family }, { $size }
ui-set-editor-indent = Отступы
ui-set-editor-indent-spaces = Пробелы вместо табов
ui-set-editor-wrap = Перенос строк
ui-set-editor-wrap-on = Переносить длинные строки
ui-set-editor-gutter = Поля
ui-set-editor-gutter-on = Номера строк, свёртка, мини-карта
ui-set-editor-toolbar = Панель инструментов
ui-set-editor-toolbar-on = Показывать над текстом
ui-set-editor-files = Файлы
ui-set-editor-new-cp = Кодировка новых файлов
ui-set-editor-utf8 = UTF-8 без BOM
ui-set-editor-utf8-bom = UTF-8 с BOM
ui-set-editor-eol = Переводы строк
ui-set-editor-eol-file = Как в файле
ui-set-editor-highlight = Подсветка синтаксиса
ui-set-editor-highlight-on = Включена
ui-set-editor-highlight-note = файлы больше { $mb } МБ — без подсветки
ui-set-editor-autosave = Автосохранение
ui-set-editor-autosave-off = Выключено
ui-set-editor-autosave-focus = При уходе с экрана
ui-set-editor-autosave-delay = Через секунду после правки
