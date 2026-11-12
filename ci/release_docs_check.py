"""Гейт выпуска Pets: полная сверка документации (скилл pets-dev, references/releases.md «Гейт документации», решение 2026-11-12).

python release_docs_check.py <версия> <glob>...
Нужен docs/releases/<версия>.md: каждый документ из glob'ов — строка «- [x] `путь` — итог». Glob — без рекурсии
(`docs/*.md` — только docs/), новый документ попадает в список сам. Непроставленная отметка, нет итога, пропущенный
или несуществующий путь, заголовок не той версии — выход 1 (выпуск не публикуется).
Копия файла лежит в каждом приложении (`.github/` или `ci/`): эталон — pets `skills/pets-dev/templates/release_docs_check.py`.
"""

import glob
import re
import sys

ITEM = re.compile(r"^- \[( |x|X)\] `([^`]+)`(.*)$")


def required(patterns: list[str]) -> set[str]:
    return {p for pattern in patterns for p in glob.glob(pattern)}


def check(version: str, text: str, need: set[str]) -> list[str]:
    errors: list[str] = []
    seen: set[str] = set()
    for n, line in enumerate(text.splitlines(), 1):
        m = ITEM.match(line.strip())
        if not m:
            continue
        mark, path, rest = m.groups()
        seen.add(path)
        if mark == " ":
            errors.append(f"строка {n}: `{path}` не сверен")
        elif not rest.strip(" —-:"):
            errors.append(f"строка {n}: `{path}` — нет итога (что обновлено или «без изменений»)")
        if path not in need:
            errors.append(f"строка {n}: `{path}` — такого документа нет")
    for path in sorted(need - seen):
        errors.append(f"`{path}` нет в сверке")
    if f"# Документация к выпуску {version}" not in text:
        errors.append(f"заголовок должен быть «# Документация к выпуску {version}»")
    return errors


def template(patterns: list[str]) -> str:
    lines = "\n".join(f"- [ ] `{p}` — " for p in sorted(required(patterns)))
    return f"# Документация к выпуску <версия>\n\nСверено с `main` на `<sha>`, изменения — `git log v<прошлая>..<sha>`.\n\n{lines}\n"


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    version, patterns = sys.argv[1], sys.argv[2:]
    if version == "--template":
        print(template(patterns), end="")
        return 0
    path = f"docs/releases/{version}.md"
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except FileNotFoundError:
        print(f"::error::нет {path}: перед тегом — полная сверка документации (шаблон: release_docs_check.py --template …)")
        return 1
    errors = check(version, text, required(patterns))
    for e in errors:
        print(f"::error file={path}::{e}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
