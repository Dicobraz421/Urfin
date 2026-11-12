# Размеры и даты (urfin_i18n::fmt). Разделители дробной части и разрядов
# задаёт язык (fmt::NumFmt), а не этот файл.
fmt-unit-b = Б
fmt-unit-kb = КБ
fmt-unit-mb = МБ
fmt-unit-gb = ГБ
fmt-unit-tb = ТБ
fmt-unit-pb = ПБ
fmt-unit-eb = ЭБ
fmt-datetime = %d.%m.%y %H:%M
# $size — точный размер с разрядами, $count — то же число для плюрали.
fmt-bytes-exact = { $size } { $count ->
    [one] байт
    [few] байта
   *[many] байт
}
# Относительное время, общее для списков (история — S7-50). $n — целое число.
# Короткие — для узких колонок: сокращения не склоняются.
fmt-ago-now = только что
fmt-ago-min = { $n } мин назад
fmt-ago-hours = { $n } ч назад
fmt-ago-yesterday = вчера
# Полные — где есть место (подсказка, статус).
fmt-ago-min-long = { $n } { $n ->
    [one] минуту
    [few] минуты
   *[many] минут
} назад
fmt-ago-hours-long = { $n } { $n ->
    [one] час
    [few] часа
   *[many] часов
} назад
