"""Индекс карточек задач (скилл pets-dev, R-07): docs/tasks/README.md.

python scripts/tasks_index.py          # переписать индекс
python scripts/tasks_index.py --check  # код 1, если индекс устарел

Карточка — docs/tasks/<ID>.md по шаблону pets (`templates/task-card.md`): `# <ID>: <суть>`,
поля lane, deps, class, files, read, checks, manual. Закрытую карточку удаляет PR, который
её закрыл. Индекс руками не правится.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIR = ROOT / "docs" / "tasks"

GROUPS = [
    ("M3 — терминал (S6)", r"^S6-"),
    ("Хвосты M1/M2", r"^S[25]-"),
    ("ADR-0022 — фичи окна в своих модулях", r"^A22-"),
    ("M4 — вьювер и редактор (S7)", r"^S7-"),
    ("Дизайн", r"^D-"),
    ("F-00 Политики", r"^F00-"),
    ("F-01 Анализ места", r"^F01-"),
    ("F-02 Пакеты", r"^F02-"),
    ("F-03 SSH", r"^F03-"),
]

HEAD = """# Задачи — карточки

Генерируется `scripts/tasks_index.py` — руками не править. Процесс — скилл `pets-dev`:
карточка = задача, волна = 3–5 карточек одной полосы (`lanes.toml`) = один PR.

Старые пакеты `docs/TASKS-*.md` — архив (история и порядок); открытые задачи — здесь.
Закрыты или отменены без карточек: M0 (S1-*) — сделано; N-01 — закрыто решением №63;
S6-03, S3-08, S4-08, S4-09 — отменены (только Windows, №57); F-пакет — только после M3.
"""


def card(path: pathlib.Path) -> dict:
    lines = path.read_text(encoding="utf-8").splitlines()
    tid, _, title = lines[0].lstrip("# ").partition(": ")
    f = {"id": tid, "title": title, "file": path.name}
    for l in lines[1:12]:
        m = re.match(r"^(lane|deps|class|manual):\s*(.*)$", l)
        if m:
            f[m.group(1)] = m.group(2)
    return f


def render() -> str:
    cards = [card(p) for p in sorted(DIR.glob("*.md")) if p.name != "README.md"]
    out = [HEAD]
    for name, rx in GROUPS:
        rows = [c for c in cards if re.match(rx, c["id"])]
        if not rows:
            continue
        out.append(f"\n## {name}\n\n| Задача | Суть | Полоса | Класс | Зависит |\n|---|---|---|---|---|")
        for c in rows:
            deps = c.get("deps", "[]").strip("[]") or "—"
            out.append(f"| [{c['id']}]({c['file']}) | {c['title']} | {c.get('lane', '?')} "
                       f"| {c.get('class', '?')} | {deps} |")
    return "\n".join(out) + "\n"


def main() -> int:
    text = render()
    out = DIR / "README.md"
    if "--check" in sys.argv:
        if not out.exists() or out.read_text(encoding="utf-8") != text:
            print("docs/tasks/README.md устарел: python scripts/tasks_index.py")
            return 1
        return 0
    out.write_text(text, encoding="utf-8", newline="\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
