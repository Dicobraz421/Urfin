"""Драйверы конкурентов для `perf.py` (S9-11, S9 §3.3), только Windows.

Каждый драйвер — инструмент сценария: `available()` (None — можно, иначе причина пропуска),
`prepare(job)` (не в замере), `run(job)` → метрики в мс. `done` — метрика «до конца», по ней
считаются отношение к Urfin, CV и повтор набора.

| Драйвер | Сценарии | Как меряется |
|---|---|---|
| `explorer` | list, copy, move, delete | UIA (`explorer.ps1 -Mode list`): t_first — первый `ListItem`, t_done — счётчик строки состояния; операции — `IFileOperation`, t_done — возврат `PerformOperations` |
| `robocopy` | copy | `/E /MT:16 /NFL /NDL /NP /R:0` (крупные — `/J`), код выхода < 8 |
| `dir` | list | `cmd /c dir /b /a`, вывод в NUL |
| `gci` | list | `Get-ChildItem -Force` внутри PowerShell (без старта процесса) |
| `rd` | delete | `cmd /c rd /s /q` |

Синтетического ввода нет (ни SendInput, ни клавиш): Far и Total Commander ждут решения
владельца. Окно Проводника драйвер открывает сам и закрывает сам по своему хэндлу
(`WindowPattern.Close`); чужие окна и процессы не трогаются. Внешний детектор завершения —
`detector.py`.
"""

from __future__ import annotations

import os
from dataclasses import dataclass


@dataclass
class ListJob:
    path: str
    expect_visible: int | None = None  # что покажет Проводник (без скрытых)


@dataclass
class CopyJob:
    src: str
    dst_parent: str
    name: str
    unbuffered: bool = False  # robocopy /J для крупных файлов

    @property
    def dst(self) -> str:
        return os.path.join(self.dst_parent, self.name)


@dataclass
class DeleteJob:
    target: str


class Tool:
    name = "?"
    done = "t_done"
    kinds: tuple[str, ...] = ()

    def available(self) -> str | None:
        return None

    def prepare(self, job) -> None:  # noqa: B027 — по умолчанию ничего
        pass

    def run(self, job) -> dict[str, float]:
        raise NotImplementedError


def tools() -> dict[str, Tool]:
    from . import cli, explorer

    out: dict[str, Tool] = {}
    for t in (explorer.Explorer(), cli.Robocopy(), cli.Dir(), cli.Gci(), cli.Rd()):
        out[t.name] = t
    return out


def selftest() -> None:
    from . import cli, detector, explorer

    detector.selftest()
    cli.selftest()
    explorer.selftest()
