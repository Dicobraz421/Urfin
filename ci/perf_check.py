#!/usr/bin/env python3
"""Проверка прогона стенда по реестру бюджетов (S9-06, S9 §2).

    python ci/perf_check.py <run.json>… [--budgets ci/budgets.toml] [--all]
    python ci/perf_check.py selftest

Прогон — JSON `scripts/stand/perf.py` (схема `urfin-perf/1`). Строка реестра с `metric`
берёт медиану результата с этим ключом, с `ratio` — отношение медиан (конкурент / Urfin).
Печатает таблицу ✓/✗ по строкам, у которых в прогоне есть данные (`--all` — и остальные,
«не мерено»); код выхода 1 — хотя бы одно нарушение, 2 — ошибка входа.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover — Python < 3.11
    tomllib = None

HERE = os.path.dirname(os.path.abspath(__file__))
BUDGETS = os.path.join(HERE, "budgets.toml")
OPS = {
    "<": lambda a, b: a < b,
    "<=": lambda a, b: a <= b,
    ">": lambda a, b: a > b,
    ">=": lambda a, b: a >= b,
}
CHECKS = {"stand", "ci-abs", "ci-instr", "ci-ab"}
FIELDS = ("id", "what", "value", "unit", "op", "check", "where", "threshold", "source")

# Строки §2 (S9.md), кроме macOS/Linux (S0 №57). Selftest требует их все в реестре.
SPEC_IDS = (
    "S.1 S.1a S.2 S.3 S.3a S.4 S.5 S.6 C.1 C.2 C.3 C.4 "
    "L.1 L.2 L.3 L.4 L.5 L.6 L.7 L.8 L.9 L.10 L.11 F.1 F.2 F.3 F.4 "
    "T.1 T.2 T.3 T.4 T.5 T.6 T.7 "
    "O.1 O.2 O.3 O.4 O.5 O.6 O.7 O.8 O.9 O.10 O.11 O.12 O.13 O.14 O.15 O.16 O.17 O.18"
).split()


def load_budgets(path: str | None = None) -> list[dict]:
    if tomllib is None:
        raise SystemExit("нужен Python ≥ 3.11 (tomllib)")
    with open(path or BUDGETS, "rb") as f:
        rows = tomllib.load(f).get("budget", [])
    errs = lint(rows)
    if errs:
        raise SystemExit("budgets.toml: " + "; ".join(errs))
    return rows


def lint(rows: list[dict]) -> list[str]:
    errs = []
    seen = set()
    for i, r in enumerate(rows):
        name = f"{r.get('id', '?')}{'/' + r['case'] if r.get('case') else ''}"
        for k in FIELDS:
            if k not in r:
                errs.append(f"{name}: нет {k}")
        if r.get("op") not in OPS:
            errs.append(f"{name}: op {r.get('op')!r}")
        if r.get("check") not in CHECKS:
            errs.append(f"{name}: check {r.get('check')!r}")
        if not isinstance(r.get("value"), (int, float)):
            errs.append(f"{name}: value не число")
        if "ratio" in r and (not isinstance(r["ratio"], list) or len(r["ratio"]) != 2):
            errs.append(f"{name}: ratio = [числитель, знаменатель]")
        if name in seen:
            errs.append(f"{name}: повтор (i={i})")
        seen.add(name)
    return errs


def medians(run: dict) -> dict[str, float]:
    return {r["key"]: r["median"] for r in run.get("results", []) if r.get("median") is not None}


def evaluate(run: dict, budgets: list[dict], include_unmeasured: bool = False) -> list[dict]:
    """Строки реестра против прогона: {id, case, actual, ok (None — не мерено), keys, …}."""
    med = medians(run)
    out = []
    for b in budgets:
        actual = None
        keys: list[str] = []
        if "metric" in b:
            keys = [b["metric"]]
            actual = med.get(b["metric"])
        elif "ratio" in b:
            keys = list(b["ratio"])
            num, den = med.get(keys[0]), med.get(keys[1])
            if num is not None and den:
                actual = num / den
        ok = None if actual is None else OPS[b["op"]](actual, b["value"])
        if ok is None and not include_unmeasured:
            continue
        out.append({**b, "case": b.get("case", ""), "actual": actual, "ok": ok, "keys": keys})
    return out


def num(v) -> str:
    if v is None:
        return "—"
    s = f"{v:.2f}" if abs(v) < 100 else f"{v:.0f}"
    if "." in s:
        s = s.rstrip("0").rstrip(".")
    return s.replace(".", ",")


def table(rows: list[dict]) -> str:
    lines = ["| | Бюджет | Что | Факт | Норма | Где |", "|---|---|---|---:|---|---|"]
    for r in rows:
        mark = "—" if r["ok"] is None else ("✓" if r["ok"] else "✗")
        name = r["id"] + (f" {r['case']}" if r["case"] else "")
        fact = "не мерено" if r["actual"] is None else f"{num(r['actual'])} {r['unit']}"
        lines.append(f"| {mark} | {name} | {r['what']} | {fact} | {r['op']} {num(r['value'])} {r['unit']} | {r['check']} |")
    return "\n".join(lines)


def check_files(paths: list[str], budgets_path: str | None, include_all: bool) -> int:
    budgets = load_budgets(budgets_path)
    code = 0
    for p in paths:
        try:
            with open(p, encoding="utf-8") as f:
                run = json.load(f)
        except (OSError, ValueError) as e:
            print(f"{p}: {e}", file=sys.stderr)
            return 2
        if run.get("schema") != "urfin-perf/1":
            print(f"{p}: не прогон perf.py (schema {run.get('schema')!r})", file=sys.stderr)
            return 2
        rows = evaluate(run, budgets, include_all)
        bad = [r for r in rows if r["ok"] is False]
        print(f"\n{os.path.basename(p)}: {run.get('suite')} · {run.get('mode')} · {run.get('host')} · {run.get('date', '')[:10]}")
        print(table(rows) if rows else "нет строк реестра с данными в этом прогоне")
        print(f"нарушений: {len(bad)}")
        if bad:
            code = 1
    return code


def selftest() -> int:
    budgets = load_budgets()
    ids = {b["id"] for b in budgets}
    missing = [i for i in SPEC_IDS if i not in ids]
    assert not missing, f"нет в реестре: {missing}"
    extra = sorted(ids - set(SPEC_IDS))
    assert not extra, f"лишние id: {extra}"
    s4 = [b for b in budgets if b["id"] == "S.4"][0]
    assert s4["value"] == 50 and s4["unit"] == "MiB", s4
    f1 = {b["case"]: b["value"] for b in budgets if b["id"] == "F.1"}
    assert f1 == {"p50": 3, "p99": 6, "fullscreen-p99": 8, "oneline-p50": 3.5, "oneline-p99": 7, "resize-p50": 9, "resize-p99": 12}, f1
    assert lint([{"id": "X", "op": "≈", "check": "?"}])  # кривая строка ловится

    def run(size_mib: float) -> dict:
        return {
            "schema": "urfin-perf/1", "suite": "exe", "mode": "warm", "host": "stand", "date": "2026-11-12",
            "results": [
                {"key": "exe/urfin/size_mib", "median": size_mib},
                {"key": "L1/explorer/t_done", "median": 1200.0},
                {"key": "L1/urfin/t_sorted", "median": 60.0},
            ],
        }

    rows = {r["id"] + r["case"]: r for r in evaluate(run(51.0), budgets)}
    assert rows["S.4"]["ok"] is False and rows["L.7100k"]["ok"] is True and abs(rows["L.7100k"]["actual"] - 20) < 1e-9
    assert "L.2100k" not in rows  # не мерено — не в таблице без --all
    assert any(r["ok"] is None for r in evaluate(run(51.0), budgets, True))
    with tempfile.TemporaryDirectory() as tmp:
        p = os.path.join(tmp, "run.json")
        for size, want in ((51.0, 1), (44.5, 0)):
            with open(p, "w", encoding="utf-8") as f:
                json.dump(run(size), f)
            got = check_files([p], None, False)
            assert got == want, f"S.4 = {size} МиБ: код {got}, ждали {want}"
        with open(p, "w", encoding="utf-8") as f:
            f.write("{}")
        assert check_files([p], None, False) == 2
    print("\nperf_check selftest: ok")
    return 0


def main(argv: list[str]) -> int:
    for s in (sys.stdout, sys.stderr):
        try:
            s.reconfigure(encoding="utf-8", errors="replace")  # консоль cp1251 и ✓/✗
        except (AttributeError, ValueError):
            pass
    if argv[:1] == ["selftest"]:
        return selftest()
    paths, budgets, include_all = [], None, False
    it = iter(argv)
    for a in it:
        if a == "--budgets":
            budgets = next(it, None)
        elif a == "--all":
            include_all = True
        else:
            paths.append(a)
    if not paths:
        print(__doc__, file=sys.stderr)
        return 2
    return check_files(paths, budgets, include_all)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
