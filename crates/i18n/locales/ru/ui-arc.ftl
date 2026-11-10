## Архивы как папки (S8 §3, `screen-archive`, `screen-extract`, `screen-extractpw`).
## $size, $packed, $limit — готовые строки (`fmt-bytes`); $pct — целый процент.
## Общее с F5: «Если файл есть» и варианты — `ui-strip-ifexists*` (ui-strip.ftl),
## В фоне — `ui-strip-background`, Отмена — `ui-button-cancel`, Tab — `ui-keys-next-field`.

# Панель внутри архива (`screen-archive`)
arc-col-packed = Сжато
# Статус-строка: «имя · { arc-status-packed } · дата»
arc-status-packed = { $size }, сжато до { $packed }
arc-summary = архив { $size } · сжатие { $pct } %

# Строка распаковки, Shift+F2 (`screen-extract`)
arc-extract-title = Распаковать { $name } · { $size } в
arc-extract-title-items = Распаковать { $count } { $count ->
    [one] объект
    [few] объекта
   *[many] объектов
} из { $name } · { $size } в
arc-extract-title-many = Распаковать { $count } { $count ->
    [one] архив
    [few] архива
   *[many] архивов
} · { $size } в
arc-extract-subdir = В подпапку { $name }
arc-extract-subdir-each = Каждый в свою подпапку
arc-extract-keep-paths = Сохранять пути
arc-extract-go = Распаковать
arc-keys-path-history = История путей

# Пароль (`screen-extractpw`, §3.7); $method — AES-256, AES-128, ZipCrypto
arc-pw-label = Пароль
arc-pw-names-encrypted = { $method } · имена зашифрованы
arc-pw-wrong = { $left ->
    [one] Неверный пароль — осталась { $left } попытка
    [few] Неверный пароль — осталось { $left } попытки
   *[many] Неверный пароль — осталось { $left } попыток
}
arc-pw-exhausted = Неверный пароль — попыток не осталось
arc-pw-ask = Пароль для { $name }

# Zip-бомбы и спилл во временный каталог (§3.4)
arc-spill-ask = Распаковать { $size } во временный каталог?
arc-spill-big = больше лимита { $limit }
arc-spill-ratio = сжатие { $ratio }:1 — похоже на zip-бомбу
arc-spill-go = Распаковать

# Ошибки
arc-error-format = Неизвестный или неподдерживаемый формат архива
arc-error-corrupt = Архив повреждён
arc-error-multivolume = Многотомные архивы пока не поддерживаются
arc-error-too-deep = Вложенность архивов глубже { $max }
arc-error-crc = Ошибка CRC: { $name }
arc-error-no-password = Архив зашифрован: нужен пароль

# F4 внутри архива (§3.5)
arc-toast-readonly = Архив только для чтения — открыт просмотр

# Задачи и итог `arc.test`
arc-job-index = Чтение архива
arc-job-extract = Распаковка
arc-job-test = Проверка архива
arc-test-ok = { $count ->
    [one] Проверен { $count } файл — ошибок нет
    [few] Проверено { $count } файла — ошибок нет
   *[many] Проверено { $count } файлов — ошибок нет
}
arc-test-failed = { $errors ->
    [one] Ошибка в { $errors } файле из { $count }
   *[other] Ошибки в { $errors } файлах из { $count }
}
