# SSH и SFTP (urfin-net, F-03).
net-source-urfin = Урфин
net-source-config = ssh_config
net-source-putty = PuTTY
net-source-history = история
net-note-unsupported = не поддерживается: { $directive } (строка { $line })
net-note-pageant = ключ через Pageant
# Колонки панели sftp:// (F03 §2.6) и заголовок.
net-col-perms = Права
net-col-owner = Владелец
net-head-ping = { $count } · { $ms } мс
net-head-cached = { $count } · из кэша
# Статус панели sftp:// (F03-06, №92): чем вошли.
net-login-pageant = ключ из агента (Pageant)
net-login-agent = ключ из агента
net-login-key = ключ { $file }
net-login-password = пароль
net-login-command = пароль из менеджера
net-login-interactive = код
net-login-open = без входа
# Обрыв связи (F03-06, screen-sftplost) и таб без подключения.
net-lost = Связь с { $profile } потеряна
net-lost-wait = · переподключение через { $s } с (попытка { $attempt } из { $tries })
net-lost-trying = · переподключение (попытка { $attempt } из { $tries })
net-lost-now = Переподключить сейчас
net-lost-disconnect = Отключиться
net-off = Отключено от { $profile }
net-off-note = · список — с последнего чтения
net-off-reconnect = Переподключить
net-state-off = Не подключено к { $profile }
net-state-off-text = Таб восстановлен из сессии; подключение — только по команде.
net-state-connect = Подключить
# Подключения (F03-05): меню дисков, строки, «Подключения».
net-drives-section = SSH
net-auth-agent = Агент
net-auth-key = Ключ
net-auth-password = Пароль
net-auth-command = Команда
net-via-agent = вход через агент
net-via-key = вход по ключу
net-via-password = вход по паролю
net-via-command = пароль из менеджера
net-connect-failed = Не удалось подключиться к { $profile }: { $error }
net-never = не подключались
net-min-ago = { $n } мин назад
net-today = сегодня { $time }
net-yesterday = вчера { $time }
net-yesterday-menu = вчера, { $time }
net-status-profile = Профиль { $profile } · { $auth }
net-status-connections = SSH: { $n } { $n ->
    [one] подключение
    [few] подключения
   *[many] подключений
}
net-status-hostkey = Подключение: ключ сервера · 2 из 3
net-status-auth = Подключение: вход · 3 из 3
net-status-stopped = Подключение остановлено
net-key-connect = Подключить
net-key-new = Новое подключение
net-key-edit = Изменить профиль
net-key-disconnect = Отключить
net-key-variant = Вариант
net-key-host-history = История хостов
net-key-auth = Способ входа
net-key-next-button = Следующая кнопка
net-key-copy-fp = Копировать отпечаток
net-key-use-cmd = Подставить команду
net-key-close-list = Закрыть список
net-key-manager = Из менеджера паролей
net-key-connect-to = Подключиться
net-key-terminal = Терминал
net-key-add = Добавить
net-key-change = Изменить
net-key-remove = Удалить
net-hint-config = config · Host { $name }
net-hint-history = история · { $when }
net-hint-profile = профиль { $name }
net-form-sources = Источники: ~\.ssh\config · история · known_hosts · профили
net-form-new = Новое подключение
net-form-edit = Изменить подключение
net-field-host = Хост
net-field-user = Пользователь
net-field-port = Порт
net-field-via = Через
net-field-auth = Вход
net-save-profile = Сохранить профиль
net-connect = Подключить
net-hostkey-unknown = Ключ сервера { $host } ещё не известен
net-hostkey-verify = — сверьте отпечаток с администратором
net-trust = Доверять и подключить
net-once = Только в этот раз
net-hostkey-changed = Ключ сервера { $host } изменился — возможна подмена
net-was = Был
net-now = Сейчас
net-replace = Заменить ключ и подключиться…
net-replace-note = потребует ввести имя хоста
net-host-name = Имя хоста
net-replace-go = Заменить и подключиться
net-password-for = Пароль для { $who }
net-passphrase-for = Фраза-пароль ключа { $key }
net-from-manager = Из менеджера паролей
net-remember = Запомнить
net-remember-locked = Задано политикой организации · { $source }
net-pm-passwork = Пассворк
net-pm-note = {"{"}host{"}"} и {"{"}user{"}"} — из подключения; команда сохранится в профиле
net-set-ssh = SSH
net-set-conn = Подключения
net-conn-search = Имя, хост или пользователь
net-conn-add = Добавить
net-conn-edit = Изменить
net-conn-remove = Удалить
net-conn-import = Импорт…
net-col-source = Источник
net-col-name = Имя
net-col-host = Хост
net-col-user = Пользователь
net-col-auth = Вход
net-col-via = Через
net-col-last = Последнее
net-conn-count = { $n } { $n ->
    [one] профиль
    [few] профиля
   *[many] профилей
} · { $config } из ~/.ssh/config · { $putty } из PuTTY
net-conn-no-passwords = Пароли в файл не пишутся
net-props = Свойства
net-prop-key = Ключ
net-prop-key-note = фразу-пароль спросит при входе
net-prop-hostkey = Ключ сервера
net-prop-hostkey-known = совпадает с known_hosts
net-prop-hostkey-none = нет в known_hosts
net-prop-start = Начальная папка
net-prop-start-note = панель sftp:// и терминал откроются здесь
net-prop-bookmarks = Закладки
net-prop-bookmarks-note = в панели sftp:// этого профиля
net-prop-notes = Импорт
