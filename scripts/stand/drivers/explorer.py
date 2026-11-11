"""Проводник: обёртка над `explorer.ps1` (UIA-листинг, `IFileOperation`).

Метки скрипта — QPC (`Stopwatch.GetTimestamp`), те же часы, что `win.ticks()`, поэтому
детектор (`detector.watch`) сверяется с концом `PerformOperations` без пересчёта.
"""

from __future__ import annotations

import os
import subprocess

from . import CopyJob, DeleteJob, ListJob, Tool, win
from .cli import parse_last_json

PS1 = os.path.join(os.path.dirname(os.path.abspath(__file__)), "explorer.ps1")


def argv(mode: str, **kw) -> list[str]:
    """Командная строка `explorer.ps1`: `-File` (политика RemoteSigned локальный скрипт пускает)."""
    out = ["powershell", "-NoProfile", "-NonInteractive", "-STA", "-File", PS1, "-Mode", mode]
    for k, v in kw.items():
        if v is True:
            out.append(f"-{k}")
        elif v is not None and v is not False:
            out += [f"-{k}", str(v)]
    return out


def spawn(mode: str, **kw) -> subprocess.Popen:
    return subprocess.Popen(
        argv(mode, **kw),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        creationflags=win.popen_flags(),
    )


def finish(p: subprocess.Popen, timeout: float | None = None) -> dict:
    out, err = p.communicate(timeout=timeout)
    v = parse_last_json(out.decode("utf-8", "replace"))
    if not v:
        raise RuntimeError(f"explorer.ps1: нет JSON (код {p.returncode}): {err.decode('utf-8', 'replace')[-300:]}")
    if not v.get("ok"):
        raise RuntimeError(f"explorer.ps1 {v.get('mode')}: {v.get('error') or v}")
    return v


def metrics(v: dict) -> dict[str, float]:
    """Метрики из JSON скрипта: мс от `t_start`."""
    freq = int(v["freq"])
    t0 = int(v["t_start"])
    out = {"t_done": win.ms_between(t0, int(v["t_done"]), freq)}
    if v.get("t_first") is not None:
        out["t_first"] = win.ms_between(t0, int(v["t_first"]), freq)
    if v.get("count") is not None and int(v["count"]) >= 0:
        out["entries"] = float(v["count"])
    return out


class Explorer(Tool):
    name = "explorer"
    kinds = ("list", "copy", "move", "delete")

    def __init__(self, silent: bool = False) -> None:
        # silent — без диалога прогресса (FOF_SILENT); на стенде по умолчанию диалог есть (§3.3).
        self.silent = silent

    def available(self) -> str | None:
        if not win.IS_WIN:
            return "не Windows"
        return None if os.path.exists(PS1) else "нет explorer.ps1"

    def run(self, job) -> dict[str, float]:
        if isinstance(job, ListJob):
            kw = {"Path": job.path}
            if job.expect_visible is not None:
                kw["Expect"] = job.expect_visible
            return metrics(finish(spawn("list", **kw)))
        if isinstance(job, CopyJob):
            v = finish(spawn("copy", Src=job.src, Dst=job.dst_parent, Name=job.name, Silent=self.silent))
            return metrics(v)
        if isinstance(job, DeleteJob):
            return metrics(finish(spawn("delete", Src=job.target, Silent=self.silent)))
        raise TypeError(job)


def selftest() -> None:
    a = argv("copy", Src="C:\\s", Dst="D:\\w", Name="c1", Silent=True, Expect=None)
    assert a[-7:] == ["-Src", "C:\\s", "-Dst", "D:\\w", "-Name", "c1", "-Silent"], a
    m = metrics({"freq": 1000, "t_start": 100, "t_first": 150, "t_done": 1300, "count": 7})
    assert m == {"t_done": 1200.0, "t_first": 50.0, "entries": 7.0}, m
    assert "t_first" not in metrics({"freq": 10, "t_start": 0, "t_done": 5, "t_first": None})
