# Фикстуры RAR

Создавать RAR кодом UnRAR нельзя (лицензия), WinRAR на машине сборки нет — здесь готовые
тестовые архивы крейта `unrar` 0.5.8 (MIT OR Apache-2.0, `data/` пакета на crates.io), без
изменений:

| Файл | Что проверяет |
|---|---|
| `version.rar` | RAR4 (2.9), один файл |
| `comment.rar` | RAR5, комментарий архива, stored, режим Unix |
| `solid.rar` | RAR5 solid |
| `crypted.rar` | RAR4, данные зашифрованы (`-p`), пароль `unrar` |
| `comment-hpw-password.rar` | RAR5, заголовки зашифрованы (`-hp`), пароль `password` |
| `archive.part1.rar` | первый том RAR4 без второго (`archive.part2.rar` нет) |
| `utf8.rar`, `unicode.rar` | имена не ASCII, пустой файл |

Ожидаемые значения — `7z l -slt` (7-Zip). Полного многотомного набора и solid с многими
файлами нет: их можно сделать WinRAR (`rar a -ma4 …`, `rar a -ma5 -s …`, `rar a -v20k …`,
`rar a -hp… …`) и положить сюда с командами.
