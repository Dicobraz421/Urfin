# Тексты core (urfin-core, crate::text::tr).
core-unknown-command = Неизвестная команда: { $id }
core-command-failed = { $id }: { $error }
core-task-panicked = Задача { $task } неожиданно завершилась с ошибкой
core-not-found = Не найдено: { $name }
core-no-match = Ничего не подходит под { $mask }
core-no-bookmark = Закладка { $n } пуста
core-last-tab = Последнюю вкладку закрыть нельзя
core-no-tab = Нет вкладки { $n }
core-config-warnings = В файле настроек ошибки ({ $count }): { $first }
core-crashed = Прошлый запуск Урфина завершился сбоем. Подробности: { $file }
core-action-show-logs = Открыть логи
core-action-dismiss = Скрыть
vfs-job-dir-size = Размер папок
vfs-dir-size-none = Нечего считать: встаньте на папку или выделите папки
ops-not-yet = Пока не сделано: { $what }
ops-nothing = Нечего делать: встаньте на файл или выделите файлы
ops-dest-empty = Не указано, куда
ops-dest-file = Назначение — файл: { $path }
ops-dest-mkdir-failed = Не создать { $path }: { $error }
ops-conflict-bad = Неизвестная политика конфликтов: { $what }
ops-filter-bad = Неверная маска файлов: { $mask }
ops-job-copy = { $n ->
    [one] Копировать { $name }
    [few] Копировать { $n } объекта
   *[many] Копировать { $n } объектов
}
ops-job-move = { $n ->
    [one] Перенести { $name }
    [few] Перенести { $n } объекта
   *[many] Перенести { $n } объектов
}
ops-copy-same = Нельзя скопировать файл сам в себя
ops-copy-into-self = Нельзя скопировать папку внутрь неё самой
ops-copy-type = Файл и папка с одним именем
ops-copy-no-name = Нет свободного имени для копии
ops-move-src-left = Скопировано, но источник не удалён: { $error }
ops-move-size = Копия { $got } Б вместо { $want } Б — источник не удалён
ops-move-flush = Копия не сброшена на диск: { $error } — источник не удалён
ops-copy-job-failed = Операция не выполнена: { $error }
## Операции SFTP (F03-07): job-бар
ops-net-wait = ждёт связи
ops-resume-from = докачка с { $pct } %
ops-copy-errors = { $n ->
    [one] Не удалось: { $n } объект
    [few] Не удалось: { $n } объекта
   *[many] Не удалось: { $n } объектов
} — { $first }
ops-conflict-exists = Файл уже есть
ops-conflict-other = Не удалось выполнить для файла
ops-conflict-body = Конфликт { $index } из { $total }: { $name } — { $cmp }
ops-conflict-body-short = Конфликт { $index } из { $total }: { $name }
ops-conflict-newer = новее
ops-conflict-older = старше
ops-conflict-same-time = то же время
ops-conflict-larger = больше на { $size }
ops-conflict-smaller = меньше на { $size }
ops-conflict-same-size = тот же размер
ops-issue-body = Проблема { $index } из { $total }: { $name } — { $error }
ops-issue-permission = Нет доступа
ops-issue-sharing = Файл используется
ops-issue-busy = Устройство занято или не готово
ops-issue-network = Сеть недоступна
ops-issue-not-found = Не найдено
ops-issue-other = Не удалось выполнить
ops-issue-low-space = Нет места на диске
ops-choice-retry = Повторить
ops-retry-nothing = Повторять нечего: ошибок нет или отчёт задачи уже не хранится
ops-recover-banner = Прервано при выходе: { $what } → { $dest }
ops-recover-banner-pct = Прервано при выходе: { $what } → { $dest } · выполнено ≈ { $n } %
ops-recover-more = ещё { $n }
ops-issue-holder = открыт в { $exe } (PID { $pid })
ops-issue-code = код { $code }
ops-issue-of = { $index } из { $total }
ops-recover-continue = Продолжить
ops-recover-clear = Очистить
ops-quit-title = Идут операции ({ $n })
ops-quit-body = «Прервать и выйти» — задачи остановятся; при следующем запуске их можно продолжить
ops-quit-wait = Дождаться
ops-quit-abort = Прервать и выйти
ops-issue-no-trash = В корзину нельзя — удалится насовсем
ops-no-trash-why = больше места корзины, сетевой диск или слишком длинный путь
ops-choice-delete-permanently = Удалить насовсем
ops-low-space-detail = нужно ≈{ $need }, свободно { $free }
ops-issue-too-large = Файл больше 4 ГБ — на этот диск (FAT32) не положить
ops-too-large-why = FAT32 не хранит файлы больше 4 ГБ; отформатируйте диск в exFAT или NTFS
ops-issue-invalid-name = Имя, которое Windows не откроет
ops-invalid-name-fix = исправленное имя: { $name }
ops-choice-fix-name = Исправить имя
ops-job-gone = Задача уже завершена
ops-verify-mismatch = контрольная сумма копии не совпала с источником
ops-verify-bad = Неизвестная проверка копии: { $what } (off, size, hash)
ops-mr-regex-form = regex: «найти/заменить», например ^img_/photo_
ops-mr-cycle-blocked = не переименован: цикл прерван на «{ $name }»
ops-job-multi-rename = Групповое переименование
ops-mr-nothing = Переименовывать нечего: имена не меняются
ops-mr-skipped = Пропущено с проблемами: { $n }
ops-job-link = Создание ссылки
ops-link-hard-dir = жёсткая ссылка бывает только на файл
ops-link-junction-file = junction бывает только на папку
ops-link-no-privilege = символическая ссылка требует режима разработчика Windows или прав администратора; для папки подойдёт junction
ops-issue-no-symlink = Нет прав на символическую ссылку
ops-choice-junction = Создать junction
ops-link-kind-bad = Неизвестный вид ссылки: { $what } (hard, sym, junction)
ops-attrs-bad = Неизвестный атрибут: { $what } (R, H, S, A)
ops-touch-bad = Не понять время: { $what } (например 2026-10-21 18:05 или now)
ops-compare-result = Сравнение: только слева { $only_left }, только справа { $only_right }, новее слева { $newer_left }, новее справа { $newer_right }, отличаются { $differs }, одинаковых { $equal }
ops-sync-nothing = Синхронизировать нечего: копировать в другую панель нечего
ops-sync-bad = Неизвестное направление синхронизации: { $what } (lr, rl)
ops-choice-overwrite = Заменить
ops-choice-skip = Пропустить
ops-choice-rename = Переименовать
ops-choice-newer = Заменить, если новее
ops-choice-cancel = Отмена
ops-copy-skipped = { $n ->
    [one] Пропущен { $n } объект
    [few] Пропущено { $n } объекта
   *[many] Пропущено { $n } объектов
}
ops-open-failed = Не открыть { $name }: { $error }
core-settings-not-saved = Настройки не сохранены в { $file }: { $error }
ops-mkdir-failed = Не создать { $name }: { $error }
ops-rename-failed = Не переименовать { $name }: { $error }
ops-exists = { $name } уже есть
ops-rename-bad = В имени не может быть \ и / — перенести в другую папку можно через F6
ops-job-trash = В корзину
ops-job-delete = Удаление
ops-delete-failed = Не удалено: { $error }
term-not-yet = Терминал: { $what } — ещё не сделано (S6)

## Названия задач для job-бара (D-06): «что → куда», вид пишет UI
ops-job-items = { $n ->
    [one] { $name }
    [few] { $n } объекта
   *[many] { $n } объектов
}
ops-job-transfer = { $n ->
    [one] { $name }
    [few] { $n } объекта
   *[many] { $n } объектов
} → { $dest }
ops-job-trash-what = { $n ->
    [one] { $name }
    [few] { $n } объекта
   *[many] { $n } объектов
} → Корзина

term-exited = [процесс завершён, код { $code }]
term-exited-unknown = [процесс завершён]
# SSH-терминал (F03 §3.1): ssh.exe вышел с ошибкой.
term-ssh-closed = [соединение закрыто, код { $code }]
term-ssh-reconnect = Enter — переподключить
term-start-failed = Не удалось запустить { $shell }: { $error }
term-unknown-shell = Неизвестный shell: { $id }
term-no-shells = Shell не найден
term-no-session = Нет сеанса терминала
term-quit-title = В терминале работают процессы ({ $n })
term-quit-body = «Завершить и выйти» закроет сеансы терминала и их программы
term-quit-cancel = Отмена
term-quit-terminate = Завершить и выйти
term-paste-title = Вставка в { $shell }: строк — { $n }
term-paste-body = { $shell } выполнит каждую строку сразу, без проверки
term-paste-insert = Вставить
term-paste-cancel = Отмена
term-integ-silent = { $shell }: интеграция shell не ответила (Clink или свой промпт?) — терминал работает без неё
term-complete-dir = папка
term-complete-file = файл
term-complete-command = команда
term-complete-history = история
term-open-nothing = Под курсором нет файла
term-open-failed = Не удалось открыть { $name }: { $error }
term-history-title = История · { $shell }
term-history-all = История · все shells
term-history-empty = Ничего не найдено
term-history-keys = Enter — вставить · Shift+Enter — выполнить · Del — удалить · Ins — закрепить · Ctrl+R — все shells
term-history-pinned = закреплено
term-history-native = импорт

# Анализ места (F-01): handler `du`
du-job = Анализ места: { $path }
du-job-changed = Анализ места, изменённые папки: { $path }
du-local-only = Анализ места — только для папок на этом компьютере
du-not-du = Это не вкладка анализа места
du-not-yet = Пока недоступно

# Досчёт с правами администратора (F01-12, ADR-0021)
du-elevate-job = Досчёт с правами администратора: { $count } { $count ->
    [one] папка
    [few] папки
   *[many] папок
}
du-elevate-uac = ждёт подтверждения UAC
du-elevate-running = Досчитываю с правами администратора…
du-elevate-declined = Отменено в UAC — ничего не досчитано
du-elevate-done = Досчитано с правами администратора: { $count } { $count ->
    [one] папка
    [few] папки
   *[many] папок
}, { $size }{ $left ->
    [0] {""}
   *[other] {" "}· ещё без доступа: { $left }
}
du-elevate-failed = Не удалось досчитать с правами администратора: { $reason }
du-elevate-exit = код выхода помощника { $code }
du-elevate-locked = urfin.exe или библиотеку рядом с ним держит на запись другая программа — закройте её и повторите
du-elevate-policy = Досчёт с правами администратора запрещён политикой организации
du-elevate-nothing = Нет папок без доступа
du-elevate-unavailable = Досчёт с правами администратора в этом режиме недоступен

# Фильтры анализа места (F01-08): группы «Типов», «Временные»
du-type-video = Видео
du-type-audio = Аудио
du-type-image = Изображения
du-type-doc = Документы
du-type-archive = Архивы
du-type-disk = Образы дисков
du-type-exec = Программы
du-type-code = Код
du-type-temp = Временные
du-type-other = Прочее
du-type-build = Сборки и кэши
du-dups-job = Дубликаты: сравнение начала файлов
du-dups-job-full = Дубликаты: проверка хэшем
du-types-more = ещё { $count } { $count ->
    [one] файл
    [few] файла
   *[many] файлов
}
du-temp-in = В { $root }
du-temp-system = Система и профиль
du-temp-what-temp = временные файлы программ
du-temp-what-wu = загрузки Windows Update
du-temp-what-do = кэш доставки обновлений
du-temp-what-dumps = дампы и отчёты об ошибках
du-temp-what-thumbs = кэш эскизов
du-temp-what-browser = кэш браузера
du-temp-what-devcache = кэш пакетов разработчика
du-temp-what-recycle = Корзина — очищать через Корзину
du-temp-what-cargo = сборки Cargo
du-temp-what-npm = пакеты npm
du-temp-what-file = временный файл
du-temp-what-wintemp = временные файлы Windows и служб
du-temp-label-temp = %TEMP%
du-temp-label-wu = Загрузки Windows Update
du-temp-label-do = Кэш оптимизации доставки
du-temp-label-dumps = Дампы памяти
du-temp-label-wer = Отчёты об ошибках Windows
du-temp-label-thumbs = Кэш эскизов
du-temp-label-chrome = Кэш Google Chrome
du-temp-label-edge = Кэш Microsoft Edge
du-temp-label-brave = Кэш Brave
du-temp-label-firefox = Кэш Firefox
du-temp-label-npm = Кэш npm
du-temp-label-pip = Кэш pip
du-temp-label-nuget = Пакеты NuGet
du-temp-label-cargo = Загрузки Cargo
du-temp-label-yarn = Кэш Yarn
du-temp-label-recycle = Корзина
du-effect-safe = безопасно
du-effect-recreated = пересоздастся
du-effect-loses = потеря данных
du-temp-after-temp = безопасно: занятые запущенными программами файлы пропустятся
du-temp-after-wu = безопасно: Windows скачает заново, если понадобится
du-temp-after-dumps = потеряете отчёты для разбора сбоев
du-temp-after-thumbs = эскизы в Проводнике появятся заново не сразу
du-temp-after-browser = вход на сайты и вкладки не пострадают, страницы сначала грузятся медленнее
du-temp-after-devcache = пакеты скачаются заново при следующей установке (нужна сеть)
du-temp-after-recycle = потеряете возможность восстановить удалённое — очищайте в самой Корзине
du-temp-after-cargo = пересоберётся при следующей сборке — это долго
du-temp-after-npm = восстановится командой npm install — нужна сеть, это долго
du-temp-after-cache = кэши программ пересоздадутся, первый запуск медленнее
du-temp-after-file = безопасно, если программа, создавшая файл, закрыта
du-clean-title = Очистить временные?
du-clean-body = В Корзину: { $count } { $count ->
    [one] строка
    [few] строки
   *[many] строк
}, освободится { $size }.
du-clean-note = • { $effect }: { $after } — { $count } · { $size }
du-clean-busy = Занятые файлы пропустятся; вернуть удалённое можно из Корзины.
du-clean-trash = В Корзину
du-clean-cancel = Отмена
du-temp-denied = нет доступа: { $count } { $count ->
    [one] папка
    [few] папки
   *[many] папок
}
