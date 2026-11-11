# Баннер-вопрос, конфликт и черновики (S2-15)
ui-prompt-more = ещё { $count } { $count ->
    [one] вопрос
    [few] вопроса
   *[many] вопросов
}
ui-draft-ops-multi_rename = Новые имена:
ui-mr-pattern-error = Шаблон: { $error }
ui-mr-problem-invalid = недопустимое имя
ui-mr-problem-duplicate = повтор
ui-mr-problem-exists = имя занято
ui-mr-summary = Изменится { $changed }, с проблемами { $problems } (их пропустим) · показано { $shown } · [N] имя, [E] расширение, [C:1:1:3] счётчик, [Y][M][D] дата, [U]/[L] регистр, «найти/заменить» — regex
ui-draft-ops-link = Ссылка в:
ui-draft-ops-attrs = Атрибуты (+R −H +S +A):
ui-draft-ops-touch = Время изменения:
ui-prompt-all = Для всех
ui-prompt-all-hint = Пробел; Shift+клавиша — ответ для всех
ui-prompt-new-name = Новое имя
ui-button-apply = Применить
ui-button-back = Назад
ui-button-later = Позже
ui-button-cancel = Отмена
ui-button-delete = Удалить
ui-button-permanent = Насовсем
ui-conflict-name = Имя
ui-conflict-size = Размер
ui-conflict-mtime = Изменён
ui-conflict-new = Новый
ui-conflict-old = Есть
ui-conflict-newer = ▲ новее на { $span }
ui-conflict-older = ▼ старше на { $span }
ui-span-min = { $n } мин
ui-span-hours = { $n } ч
ui-span-days = { $n } д
ui-draft-delete = В корзину { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $size })?
ui-draft-delete-permanent = Удалить НАВСЕГДА { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $size })? Отменить нельзя.
ui-draft-copy = Копировать { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $size }) в
ui-draft-move = Перенести { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $size }) в
ui-draft-copy-cmp = Копировать { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $size }) на другую сторону
ui-draft-move-cmp = Перенести { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} ({ $size }) на другую сторону
ui-draft-ops-mkdir = Новая папка
ui-draft-sel-add = Пометить по маске
ui-draft-sel-remove = Снять пометку по маске
ui-draft-panel-filter = Фильтр

# Переименование по месту (S2-16)
ui-rename-hint = Enter — переименовать · Esc — отмена

## Встроенные строки (D-04)
ui-strip-ifexists = Если файл есть
ui-strip-ifexists-ask = Спросить
ui-strip-ifexists-overwrite = Заменить
ui-strip-ifexists-skip = Пропустить
ui-strip-ifexists-newer = Заменить, если новее
ui-strip-keep-dates = Сохранять даты
ui-strip-keep-perms-dates = Сохранять права и даты
ui-strip-verify = Проверить хэш после копирования
ui-strip-verify-move = Проверить хэш перед удалением исходных
ui-strip-background = В фоне
ui-strip-matches = совпадает { $count }
ui-strip-mr-summary = изменится { $changed } · проблем { $problems }
ui-strip-conflict-of = { $name } · конфликт { $index } из { $total }
ui-strip-newer = новее
ui-strip-mkdir = Новая папка
ui-strip-mkdir-many = Несколько через ;
ui-strip-mkdir-nested = вложенные — через \
ui-strip-filter = Показывать
ui-strip-filter-matches = совпадает { $count } из { $total }
ui-strip-attrs = { $count ->
    [one] Атрибуты { $count } объекта
   *[other] Атрибуты { $count } объектов
}
ui-strip-attrs-mtime = Изменён
ui-strip-attrs-now = сейчас
ui-strip-attrs-varies = разные
ui-attr-readonly = Только чтение
ui-attr-hidden = Скрытый
ui-attr-system = Системный
ui-attr-archive = Архивный
ui-filter-chip = фильтр { $mask }
ui-panel-count-filtered = { $count } из { $total }
ui-button-create = Создать
ui-button-reset = Сбросить
ui-keys-apply = Применить
ui-keys-cancel = Отмена
ui-keys-name-ext = Имя ↔ расширение
ui-button-copy = Копировать
ui-button-move = Перенести
ui-button-mark = Отметить
ui-button-unmark = Снять отметку
ui-button-rename = Переименовать
ui-button-ok = ОК
ui-keys-qs-next = Следующее
ui-keys-qs-prev = Предыдущее
ui-keys-close = Закрыть

## Дополнение полей (№70)
ui-complete-history = история
ui-complete-folder = папка
ui-complete-here = в папке
ui-keys-complete-next = Следующий
ui-keys-insert = Вставить
ui-keys-field-history = История поля

## Строки операций (D-04): ссылка, время, переименование, F7, сравнение, синхронизация
ui-strip-link = Ссылка
ui-strip-link-to = на
ui-strip-link-kind = Тип
ui-strip-link-hint = Без режима разработчика нужны права администратора; для папки подойдёт Junction
ui-link-sym = Символическая
ui-link-hard = Жёсткая
ui-link-junction = Junction
ui-strip-touch = Время изменения
ui-strip-touch-deep = Для вложенных
ui-strip-touch-hint = создание и доступ — в «Атрибутах»
ui-strip-mr = Новые имена
ui-mr-tok-name = имя
ui-mr-tok-ext = расширение
ui-mr-tok-counter = счётчик
ui-mr-tok-date = дата
ui-mr-tok-replace = найти/замена через /
ui-mkdir-exists = есть
ui-mkdir-new = новая
ui-strip-sync = Синхронизировать:
ui-strip-sync-what = { $count } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
} · { $size }
ui-strip-sync-mirror = Удалять лишнее
ui-strip-sync-mirror-server = Удалять лишнее на сервере
ui-strip-sync-extra-server = на сервере лишних: { $n }
ui-strip-sync-go = Синхронизировать
ui-compare-title = Сравнение папок:
ui-compare-only-left = только слева { $n }
ui-compare-only-right = только справа { $n }
ui-compare-newer-left = новее слева { $n }
ui-compare-newer-right = новее справа { $n }
ui-compare-differs = различаются { $n }
ui-compare-equal = одинаковых { $n }
ui-compare-sync-lr = Синхронизировать →
ui-compare-sync-rl = Синхронизировать ←
ui-button-close = Закрыть
ui-tag-newer = новее
ui-tag-only-here = только здесь
ui-prompt-skip-all = Пропустить все такие
ui-job-waiting-for = ждёт ответа: { $what }
