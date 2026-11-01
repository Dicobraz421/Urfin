; TOML (screen-quickview.png): заголовки таблиц — keyword, ключи — как текст,
; строки — string, числа, даты и true/false — number.
(comment) @comment
(table "[" @keyword [(bare_key) (dotted_key) (quoted_key)] @keyword "]" @keyword)
(table_array_element "[[" @keyword [(bare_key) (dotted_key) (quoted_key)] @keyword "]]" @keyword)
(pair (quoted_key) @none)
(string) @string
[(integer) (float) (boolean)] @number
[(offset_date_time) (local_date_time) (local_date) (local_time)] @number
