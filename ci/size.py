#!/usr/bin/env python3
"""Гейт размера бинаря: python3 ci/size.py <bin> [лимит_МБ].

МБ = 1024 * 1024 байт. Лимит — `LIMIT_MIB` (S0 №80, решение 2026-11-11
«exe-gate-50»); аргумент может его только поднять: старый вызов из `ci.yml`
с меньшим числом гейт не опускает. Проверяется только сам `urfin(.exe)`:
DLL ConPTY и дистрибутивы в бюджет не входят (S0 v5).
Размер пишется в $GITHUB_STEP_SUMMARY, если он задан. Вывод — ASCII:
консоль windows-раннера не UTF-8.
"""

import os
import sys

MIB = 1024 * 1024
LIMIT_MIB = 50.0


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print("usage: size.py <bin> [limit_mb]", file=sys.stderr)
        return 2
    path = sys.argv[1]
    limit_mb = max(LIMIT_MIB, float(sys.argv[2])) if len(sys.argv) == 3 else LIMIT_MIB
    size = os.path.getsize(path)
    ok = size <= limit_mb * MIB
    line = (
        f"{os.path.basename(path)}: {size:,} bytes ({size / MIB:.2f} MiB), "
        f"limit {limit_mb:g} MiB: {'OK' if ok else 'EXCEEDED'}"
    )
    print(line)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(f"### Binary size\n\n{line}\n")
    if not ok:
        print(f"::error file={path}::{line}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
