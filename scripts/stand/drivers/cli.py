"""CLI-базлайны (S9 §3.3): robocopy, `cmd dir`, `Get-ChildItem`, `rd /s /q`.

Время — от запуска процесса до выхода (как у hyperfine), кроме `gci`: там PowerShell сам
меряет `Get-ChildItem`, иначе в листинг попал бы старт PowerShell (~0,3 с).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess

from . import CopyJob, DeleteJob, ListJob, Tool, win

ROBOCOPY_FLAGS = ["/E", "/MT:16", "/NFL", "/NDL", "/NP", "/R:0"]


def _timed(argv: list[str], env: dict | None = None) -> tuple[float, subprocess.CompletedProcess]:
    t0 = win.ticks()
    p = subprocess.run(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        creationflags=win.popen_flags(),
        check=False,
    )
    return win.ms_between(t0, win.ticks()), p


def robocopy_argv(job: CopyJob) -> list[str]:
    argv = ["robocopy", job.src, job.dst, *ROBOCOPY_FLAGS]
    if job.unbuffered:
        argv.append("/J")
    return argv


def robocopy_ok(code: int) -> bool:
    """robocopy: 0–7 — успех (биты «скопировано», «лишние», «несовпадения»), ≥ 8 — ошибка."""
    return 0 <= code < 8


class Robocopy(Tool):
    name = "robocopy"
    kinds = ("copy",)

    def available(self) -> str | None:
        return None if shutil.which("robocopy") else "нет robocopy"

    def run(self, job: CopyJob) -> dict[str, float]:
        ms, p = _timed(robocopy_argv(job))
        if not robocopy_ok(p.returncode):
            raise RuntimeError(f"robocopy: код {p.returncode}")
        return {"t_done": ms}


class Dir(Tool):
    name = "dir"
    kinds = ("list",)

    def run(self, job: ListJob) -> dict[str, float]:
        ms, p = _timed(["cmd", "/d", "/c", "dir", "/b", "/a", job.path])
        if p.returncode != 0:
            raise RuntimeError(f"dir: код {p.returncode}")
        return {"t_done": ms}


GCI_PS = (
    "$sw=[Diagnostics.Stopwatch]::StartNew();"
    "$n=@(Get-ChildItem -Force -LiteralPath $env:URFIN_PERF_PATH).Count;"
    "$sw.Stop();"
    "Write-Output ('{\"ms\":'+$sw.Elapsed.TotalMilliseconds.ToString([Globalization.CultureInfo]::InvariantCulture)+',\"n\":'+$n+'}')"
)


def parse_last_json(text: str) -> dict | None:
    """Последняя строка stdout, которая разбирается как объект JSON."""
    for line in reversed(text.splitlines()):
        line = line.strip()
        if line.startswith("{"):
            try:
                v = json.loads(line)
            except ValueError:
                continue
            if isinstance(v, dict):
                return v
    return None


class Gci(Tool):
    name = "gci"
    kinds = ("list",)

    def run(self, job: ListJob) -> dict[str, float]:
        env = dict(os.environ, URFIN_PERF_PATH=job.path)
        wall, p = _timed(["powershell", "-NoProfile", "-NonInteractive", "-Command", GCI_PS], env)
        v = parse_last_json(p.stdout.decode("utf-8", "replace"))
        if p.returncode != 0 or not v:
            raise RuntimeError(f"gci: код {p.returncode}")
        return {"t_done": float(v["ms"]), "wall": wall, "entries": float(v["n"])}


class Rd(Tool):
    name = "rd"
    kinds = ("delete",)

    def run(self, job: DeleteJob) -> dict[str, float]:
        ms, p = _timed(["cmd", "/d", "/c", "rd", "/s", "/q", job.target])
        if p.returncode != 0 or os.path.exists(job.target):
            raise RuntimeError(f"rd: код {p.returncode}")
        return {"t_done": ms}


def selftest() -> None:
    j = CopyJob("C:\\a", "D:\\w", "c1", unbuffered=True)
    assert robocopy_argv(j) == ["robocopy", "C:\\a", os.path.join("D:\\w", "c1"), *ROBOCOPY_FLAGS, "/J"]
    assert robocopy_ok(0) and robocopy_ok(1) and robocopy_ok(7)
    assert not robocopy_ok(8) and not robocopy_ok(16)
    assert parse_last_json('noise\n{"ms":12.5,"n":3}\n') == {"ms": 12.5, "n": 3}
    assert parse_last_json("bench ui=gpui a=1\n{bad\n") is None
