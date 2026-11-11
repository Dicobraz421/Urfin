#!/usr/bin/env python3
"""Харнесс замеров стенда (S9-04, S9 §3.1–3.2): прогоны сюит, отчёт, сравнение.

    python scripts/stand/perf.py run <сюита…|all> [--warm|--cold] [--tools urfin,explorer,robocopy]
        [--root N:\\ds] [--work N:\\perf-work] [--urfin C:\\Bench\\urfin\\urfin.exe] [--gen]
        [--runs 10] [--warmup 2] [--out <файл.json>] [--no-save] [--allow-busy] [--silent]
    python scripts/stand/perf.py report [--runs-dir docs/perf/runs] [--out docs/perf/report.md]
    python scripts/stand/perf.py compare <a.json> <b.json> [--pct 10]   # код 1 — регрессия
    python scripts/stand/perf.py detector-check --src <каталог> --work <каталог> [--runs 3]
    python scripts/stand/perf.py selftest

Вместо `xtask perf` из спеки — скрипт рядом с `bench-ab.ps1`; вместо hyperfine — свой цикл
(в стенде hyperfine нет, а Проводник и Urfin hyperfine всё равно не мерит).

Сюиты (датасеты — `<root>\\<ID>` из `urfin-testdata`, S9-02; `--gen` собирает недостающие):

| Сюита | Датасет | Инструменты | Что |
|---|---|---|---|
| `l1`, `l2` | L1 / L2 | `urfin` (`vfs_bench --core-only`), `urfin-stream` (`vfs_bench`), `explorer`, `dir`, `gci` | листинг: L.1–L.3, L.5, L.7 |
| `ops` | B2 | `urfin` (`--bench-scenario`, S9-05), `explorer` (`IFileOperation`), `robocopy`, `rd` | O.3 копия, O.9 удаление |
| `startup` `frame` `editor` `viewer` `search-frame` `sftp` | — / L1 / `--file` | `urfin --bench*` (JSON S9-05) | S.1, F.1, F.2 |
| `search` | `--corpus` (`search_bench gen`) | `search_bench run` | S8 §2.7 (сам считает медианы) |
| `bench_ops` | свои (S5-25) | тест `bench_ops` | S5 §7 (сам считает медианы) |
| `exe` | — | размер `urfin.exe` | S.4 |
| `all` | | всё, для чего есть данные | |

Методика (§3.2): перед сюитой фон < 2 % CPU за 10 с (`GetSystemTimes`; иначе ждём, после трёх
попыток — отказ, `--allow-busy` пишет прогон с пометкой). Warm: 2 прогона разогрева + 10,
инструменты чередуются по кругу; операции дольше 60 с — 1 + 3. У операций dst очищается
до прогона, затем пауза 5 с. CV > 5 % по метрике «до конца» → набор повторяется один раз,
потом метка `unstable`. Cold (`--cold --vhdx <файл>`): переподключение VHDX перед каждым
прогоном, 10 с простоя, 5 прогонов — только из PowerShell администратора (DEV.md, п. 5).

Сюиты `urfin --bench*` и `ops/urfin` ждут единую строку JSON S9-05; exe без неё пропускается
с пометкой «нужна сборка с S9-05». Exe — переносная копия (`urfin.portable` рядом), иначе отказ.

Схема прогона `urfin-perf/1` (`docs/perf/runs/<дата>-<хост>-<сюита>.json`):

    {
      "schema": "urfin-perf/1", "suite": "l1", "mode": "warm" | "cold",
      "date": "2026-11-12T10:00:00+03:00", "host": "STAND",
      "env": {"os", "os_build", "cpu", "cores", "threads", "ram_gb", "disk", "disk_bus", "fs",
              "defender": {...}, "power", "power_guid", "on_ac", "sha", "dirty", "python",
              "urfin": {"path", "size", "mtime", "s9_05"}, "explorer_ver", "robocopy_ver"},
      "background_cpu": 0.8, "busy": false, "warmup": 2, "runs": 10, "attempts": 1,
      "datasets": {"L1": {манифест .urfin-dataset.json}},
      "results": [{"key": "L1/urfin/t_sorted", "scenario": "L1", "tool": "urfin",
                   "metric": "t_sorted", "unit": "ms", "samples": [..], "n": 10,
                   "median": 55.7, "p95": 58.1, "min": 54.0, "mean": 56.0, "cv": 2.1,
                   "done": true, "unstable": false, "self_timed": false}],
      "skipped": [{"scenario": "L1", "tool": "explorer", "reason": "..."}]
    }

`p95` — при n ≥ 10, иначе null. CV — σ/среднее (σ по генеральной совокупности), %.
Отчёт `report` → `docs/perf/report.md` (последний прогон на хост × сюиту × режим × ФС, бюджеты
по `ci/budgets.toml` через `ci/perf_check.py`). `compare`: медиана B хуже A > `--pct` % при CV
обоих ≤ 5 % — регрессия (§2, «Пороги»: стенд, 10 %).
"""

from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
import json
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)

import drivers  # noqa: E402
from drivers import CopyJob, DeleteJob, ListJob, Tool, cli, detector, win  # noqa: E402
from drivers.explorer import Explorer  # noqa: E402

SCHEMA = "urfin-perf/1"
RUNS_DIR = os.path.join(REPO, "docs", "perf", "runs")
REPORT = os.path.join(REPO, "docs", "perf", "report.md")
CV_MAX = 5.0
LONG_MS = 60_000.0
S9_05_KEYS = ("t_first", "t_done", "t_sorted", "peak_rss", "frame_p50", "frame_p99", "allocs")
NEED_S9_05 = "нужна сборка с S9-05 (единая строка JSON у urfin --bench*)"
MARKER = ".urfin-perf-work"

# ---------- статистика ----------


def stats(xs: list[float]) -> dict:
    s = sorted(xs)
    n = len(s)
    if not n:
        return {"n": 0, "median": None, "p95": None, "min": None, "mean": None, "cv": None}
    med = s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2
    mean = sum(s) / n
    sd = math.sqrt(sum((x - mean) ** 2 for x in s) / n)
    p95 = s[min(n - 1, math.ceil(0.95 * n) - 1)] if n >= 10 else None
    cv = sd / mean * 100.0 if mean else 0.0
    r = lambda v: None if v is None else round(v, 3)  # noqa: E731
    return {"n": n, "median": r(med), "p95": r(p95), "min": r(s[0]), "mean": r(mean), "cv": round(cv, 2)}


def unit_of(metric: str) -> str:
    if metric.endswith("_mb"):
        return "MB"
    if metric.endswith("_mib"):
        return "MiB"
    if metric in ("allocs", "entries") or metric.endswith("_count"):
        return "count"
    return "ms"


class Results:
    """Выборки по ключу (сценарий, инструмент, метрика) и пропуски."""

    def __init__(self) -> None:
        self.samples: dict[tuple[str, str, str], list[float]] = {}
        self.done: set[tuple[str, str, str]] = set()
        self.self_timed: set[tuple[str, str, str]] = set()
        self.skipped: list[dict] = []
        self.unstable: set[tuple[str, str, str]] = set()

    def add(self, scenario: str, tool: str, metrics: dict[str, float], done: str | None) -> None:
        for m, v in metrics.items():
            if v is None:
                continue
            k = (scenario, tool, m)
            self.samples.setdefault(k, []).append(float(v))
            if m == done:
                self.done.add(k)

    def skip(self, scenario: str, tool: str, reason: str) -> None:
        if not any(s["scenario"] == scenario and s["tool"] == tool for s in self.skipped):
            self.skipped.append({"scenario": scenario, "tool": tool, "reason": reason})

    def noisy(self) -> list[tuple[str, str, str]]:
        out = []
        for k in self.done:
            if k in self.self_timed:
                continue
            cv = stats(self.samples[k])["cv"]
            if cv is not None and cv > CV_MAX and len(self.samples[k]) > 1:
                out.append(k)
        return out

    def to_json(self) -> list[dict]:
        out = []
        for (sc, tool, m), xs in sorted(self.samples.items()):
            st = stats(xs)
            out.append(
                {
                    "key": f"{sc}/{tool}/{m}",
                    "scenario": sc,
                    "tool": tool,
                    "metric": m,
                    "unit": unit_of(m),
                    "samples": [round(x, 3) for x in xs],
                    **st,
                    "done": (sc, tool, m) in self.done,
                    "unstable": (sc, tool, m) in self.unstable,
                    "self_timed": (sc, tool, m) in self.self_timed,
                }
            )
        return out


# ---------- окружение ----------

ENV_PS = r"""
$o=[ordered]@{}
[Console]::OutputEncoding=[Text.Encoding]::UTF8
try{ $pp=Get-CimInstance -Namespace root\cimv2\power -ClassName Win32_PowerPlan -Filter 'IsActive=True'
  $o.power=$pp.ElementName; if("$($pp.InstanceID)" -match '([0-9a-fA-F-]{36})'){ $o.power_guid=$Matches[1] } }catch{}
if(-not $o.power_guid){ if(((powercfg /getactivescheme) -join ' ') -match '([0-9a-fA-F-]{36})'){ $o.power_guid=$Matches[1] } }
$v=Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$pn="$($v.ProductName)"; if([int]$v.CurrentBuild -ge 22000){ $pn=$pn -replace 'Windows 10','Windows 11' }
$o.os="$pn $($v.DisplayVersion)"; $o.os_build="$($v.CurrentBuild).$($v.UBR)"
$c=Get-CimInstance Win32_Processor | Select-Object -First 1
$o.cpu=$c.Name.Trim(); $o.cores=$c.NumberOfCores; $o.threads=$c.NumberOfLogicalProcessors
$o.ram_gb=[math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory/1GB,1)
$L=$env:URFIN_PERF_DRIVE
if($L){ try{ $vol=Get-Volume -DriveLetter $L; $o.fs="$($vol.FileSystemType)"; $o.volume_label=$vol.FileSystemLabel
  $d=Get-Partition -DriveLetter $L | Get-Disk; $o.disk=$d.FriendlyName; $o.disk_bus="$($d.BusType)" }catch{} }
$df=[ordered]@{}
try{ $m=Get-MpComputerStatus; $df.realtime=$m.RealTimeProtectionEnabled; $df.mode="$($m.AMRunningMode)" }catch{}
try{ $df.performance_mode=(Get-MpPreference).PerformanceModeStatus }catch{}
$o.defender=$df
try{ $b=@(Get-CimInstance Win32_Battery); if($b.Count){ $o.on_ac=($b[0].BatteryStatus -eq 2) } else { $o.on_ac=$true } }catch{}
$o.explorer_ver=(Get-Item "$env:WINDIR\explorer.exe").VersionInfo.FileVersion
$o.robocopy_ver=(Get-Item "$env:WINDIR\System32\robocopy.exe").VersionInfo.FileVersion
$o | ConvertTo-Json -Compress
"""


POWER_PLANS = {
    "381b4222-f694-41f0-9685-ff5bb260df2e": "Сбалансированная",
    "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c": "Высокая производительность",
    "a1841308-3541-4fab-bc81-f71556f20b4a": "Экономия энергии",
    "e9a42b02-d5df-448d-aa00-03f14749eb61": "Максимальная производительность",
}


def git(*a: str) -> str:
    try:
        return subprocess.run(["git", "-C", REPO, *a], capture_output=True, text=True, check=False).stdout.strip()
    except OSError:
        return ""


def fingerprint(path_on_volume: str | None, urfin: dict | None) -> dict:
    env: dict = {"python": platform.python_version(), "sha": git("rev-parse", "HEAD"), "dirty": bool(git("status", "--porcelain", "--untracked-files=no"))}
    if win.IS_WIN:
        e = dict(os.environ)
        if path_on_volume:
            e["URFIN_PERF_DRIVE"] = win.volume_of(path_on_volume).rstrip(":")
        try:
            p = subprocess.run(
                ["powershell", "-NoProfile", "-NonInteractive", "-Command", ENV_PS],
                capture_output=True, env=e, creationflags=win.popen_flags(), check=False, timeout=120,
            )
            v = cli.parse_last_json(p.stdout.decode("utf-8", "replace"))
            if v:
                env.update(v)
            if not env.get("power") and env.get("power_guid"):
                env["power"] = POWER_PLANS.get(env["power_guid"].lower(), "своя схема")
        except (OSError, subprocess.TimeoutExpired) as err:
            env["fingerprint_error"] = str(err)
    else:
        env["os"] = platform.platform()
    env["temperature"] = None  # MSAcpi_ThermalZoneTemperature — только администратор
    if urfin:
        env["urfin"] = urfin
    return env


def background(args) -> tuple[float | None, bool]:
    """Фон < `--bg-max` % CPU за 10 с; до трёх попыток с паузой 20 с."""
    busy = None
    for i in range(args.bg_tries):
        busy = win.cpu_busy_pct(args.bg_seconds)
        if busy is None or busy < args.bg_max:
            print(f"фон: {busy if busy is None else round(busy, 2)} % CPU")
            return busy, False
        print(f"фон: {busy:.1f} % CPU ≥ {args.bg_max} % (попытка {i + 1}/{args.bg_tries})")
        if i + 1 < args.bg_tries:
            time.sleep(20)
    if not args.allow_busy:
        raise SystemExit(f"фон {busy:.1f} % CPU: стенд не тихий (--allow-busy — записать с пометкой)")
    return busy, True


# ---------- датасеты, бинарники ----------


def dataset(args, ident: str) -> tuple[str | None, dict | None]:
    """`<root>\\<ID>` с манифестом; `--gen` — собрать генератором urfin-testdata."""
    if not args.root:
        return None, None
    for name in (ident.upper(), ident.lower()):
        d = os.path.join(args.root, name)
        m = os.path.join(d, ".urfin-dataset.json")
        if os.path.isfile(m):
            with open(m, encoding="utf-8") as f:
                return d, json.load(f)
    if args.gen:
        exe = example(args, "urfin-testdata", "testdata")
        print(f"датасет {ident}: генерация в {args.root}")
        subprocess.run([exe, ident.lower(), "--root", args.root], check=True)
        return dataset(argparse.Namespace(**{**vars(args), "gen": False}), ident)
    return None, None


_examples: dict[tuple[str, str], str] = {}


def example(args, pkg: str, name: str) -> str:
    """Путь к release-примеру: `--bin-dir` или сборка `cargo build --release` (один раз за прогон)."""
    if (pkg, name) in _examples:
        return _examples[(pkg, name)]
    if args.bin_dir:
        p = os.path.join(args.bin_dir, name + (".exe" if win.IS_WIN else ""))
        if os.path.isfile(p):
            _examples[(pkg, name)] = p
            return p
    out = subprocess.run(
        ["cargo", "build", "--release", "-p", pkg, "--example", name, "--message-format=json-render-diagnostics"],
        cwd=REPO, stdout=subprocess.PIPE, check=True,
    ).stdout.decode("utf-8", "replace")
    for line in out.splitlines():
        try:
            v = json.loads(line)
        except ValueError:
            continue
        if v.get("reason") == "compiler-artifact" and v.get("executable") and v.get("target", {}).get("name") == name:
            _examples[(pkg, name)] = v["executable"]
    if (pkg, name) not in _examples:
        raise SystemExit(f"не нашёл пример {pkg}/{name} после cargo build")
    return _examples[(pkg, name)]


def urfin_info(path: str | None) -> dict | None:
    if not path or not os.path.isfile(path):
        return None
    st = os.stat(path)
    with open(path, "rb") as f:
        data = f.read()
    return {
        "path": path,
        "size": st.st_size,
        "mtime": dt.datetime.fromtimestamp(st.st_mtime).isoformat(timespec="seconds"),
        "portable": os.path.isfile(os.path.join(os.path.dirname(path), "urfin.portable")),
        # Строка JSON S9-05: имена ключей вшиты в exe.
        "s9_05": all(k.encode() in data for k in (b"frame_p99", b"peak_rss", b"t_sorted")),
    }


def parse_bench_json(text: str) -> dict | None:
    """Строка JSON S9-05 (`{"bench": <режим>, t_first…allocs, "raw": {…}}`) → метрики, мс/МБ."""
    for line in reversed(text.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            v = json.loads(line)
        except ValueError:
            continue
        if not isinstance(v, dict) or "bench" not in v:
            continue
        out: dict[str, float] = {}
        for k in S9_05_KEYS:
            x = v.get(k)
            if isinstance(x, (int, float)):
                if k == "peak_rss":
                    out["peak_rss_mb"] = x / (1 << 20)
                else:
                    out[k] = float(x)
        for k, x in (v.get("raw") or {}).items():
            if isinstance(x, (int, float)) and not isinstance(x, bool):
                out[f"raw.{k}"] = float(x)
        return out
    return None


def parse_vfs_bench(text: str) -> dict[str, dict[str, float]]:
    """JSON `vfs_bench` (S4-11) → {"urfin": core, "urfin-stream": stream}."""
    v = cli.parse_last_json(text)
    if not v:
        raise RuntimeError("vfs_bench: нет JSON")
    out: dict[str, dict[str, float]] = {}
    core = v.get("core") or {}
    if core:
        out["urfin"] = {
            "t_first": core.get("t_first_ms"),
            "t_sorted": core.get("t_sorted_ms"),
            "heap_peak_mb": core.get("heap_peak_mb"),
            "listing_peak_mb": v.get("listing_peak_ws_mb"),
            "entries": v.get("entries"),
        }
    s = v.get("stream")
    if s:
        out["urfin-stream"] = {"t_first": s.get("t_first_ms"), "t_done": s.get("t_done_ms")}
    return out


# ---------- инструменты Urfin ----------


class VfsBench(Tool):
    """Листинг Urfin без окна: `vfs_bench <каталог> --runs 1 [--core-only]` на каждый прогон."""

    kinds = ("list",)

    def __init__(self, args, stream: bool) -> None:
        self.args = args
        self.stream = stream
        self.name = "urfin-stream" if stream else "urfin"
        self.done = "t_done" if stream else "t_sorted"

    def run(self, job: ListJob) -> dict[str, float]:
        exe = example(self.args, "urfin-vfs", "vfs_bench")
        argv = [exe, job.path, "--runs", "1"] + ([] if self.stream else ["--core-only"])
        p = subprocess.run(argv, capture_output=True, creationflags=win.popen_flags(), check=False)
        if p.returncode != 0:
            raise RuntimeError(f"vfs_bench: код {p.returncode}")
        return parse_vfs_bench(p.stdout.decode("utf-8", "replace")).get(self.name, {})


class UrfinApp(Tool):
    """`urfin.exe --bench*` и `--bench-scenario` (S9-05): метрики из строки JSON."""

    name = "urfin"

    def __init__(self, args, info: dict | None, argv_of, done: str) -> None:
        self.args = args
        self.info = info
        self.argv_of = argv_of
        self.done = done

    def available(self) -> str | None:
        if not self.info:
            return "нет urfin.exe (--urfin)"
        if not self.info["portable"]:
            return "нет urfin.portable рядом с exe: замер только на переносной копии"
        if not self.info["s9_05"]:
            return NEED_S9_05
        return None

    def run(self, job) -> dict[str, float]:
        argv = [self.info["path"], *self.argv_of(job)]
        p = subprocess.run(argv, capture_output=True, check=False, timeout=3600)
        m = parse_bench_json(p.stdout.decode("utf-8", "replace"))
        if p.returncode != 0 or m is None:
            raise RuntimeError(f"{' '.join(argv[1:2])}: код {p.returncode}, {NEED_S9_05 if m is None else ''}")
        return m


def scenario_toml(op: str, job) -> str:
    """Сценарий `--bench-scenario` (формат — S9-05; пути — литеральные строки TOML)."""
    if op == "delete":
        body = f"op = \"delete\"\npath = '{job.target}'\n"
    else:
        body = f"op = \"{op}\"\nsrc = '{job.src}'\ndst = '{job.dst}'\n"
    return f"# perf.py → urfin --bench-scenario (S9-05)\n[[step]]\n{body}"


# ---------- цикл прогонов ----------


class Case:
    """Сценарий × инструмент: задание, подготовка, счётчики прогонов."""

    def __init__(self, scenario: str, tool: Tool, job, prepare=None) -> None:
        self.scenario = scenario
        self.tool = tool
        self.job = job
        self.prepare = prepare
        self.warm_left = 0
        self.runs_left = 0


def run_cases(args, cases: list[Case], res: Results) -> None:
    """Разогрев + прогоны, инструменты по кругу (фон делится поровну). Долгие (> 60 с) — 1 + 3."""
    live = []
    for c in cases:
        why = c.tool.available()
        if why:
            res.skip(c.scenario, c.tool.name, why)
            print(f"  {c.scenario}/{c.tool.name}: пропуск — {why}")
            continue
        c.warm_left, c.runs_left = args.warmup, args.runs
        live.append(c)
    rnd = 0
    while live:
        rnd += 1
        for c in list(live):
            try:
                if c.prepare:
                    c.prepare(c)
                if args.cold:
                    cold_reset(args)
                m = c.tool.run(c.job)
            except Exception as e:  # noqa: BLE001 — инструмент упал: пропуск, прогон идёт дальше
                res.skip(c.scenario, c.tool.name, f"ошибка: {e}")
                print(f"  {c.scenario}/{c.tool.name}: ошибка — {e}")
                live.remove(c)
                continue
            d = m.get(c.tool.done)
            tag = f"{d:.1f} мс" if isinstance(d, (int, float)) else "—"
            if c.warm_left > 0:
                c.warm_left -= 1
                if isinstance(d, (int, float)) and d > LONG_MS:
                    c.warm_left = 0
                    c.runs_left = min(c.runs_left, 3)
                print(f"  {c.scenario}/{c.tool.name}: разогрев {tag}")
                continue
            res.add(c.scenario, c.tool.name, m, c.tool.done)
            c.runs_left -= 1
            print(f"  {c.scenario}/{c.tool.name}: прогон {args.runs - c.runs_left} {tag}")
            if c.runs_left <= 0:
                live.remove(c)


def cold_reset(args) -> None:
    """Холодный кэш: переподключение VHDX (только администратор), затем 10 с простоя."""
    ps = (
        f"Dismount-DiskImage -ImagePath '{args.vhdx}' | Out-Null; "
        f"Mount-DiskImage -ImagePath '{args.vhdx}' | Out-Null"
    )
    subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", ps], check=True)
    if args.rammap:
        subprocess.run([args.rammap, "-Et"], check=True)
    time.sleep(10)


def is_admin() -> bool:
    if not win.IS_WIN:
        return False
    import ctypes

    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except OSError:
        return False


def measure(args, cases_of, res: Results) -> int:
    """Набор, при CV > 5 % — повтор один раз, потом `unstable`. Возвращает число попыток."""
    run_cases(args, cases_of(), res)
    noisy = res.noisy()
    if not noisy or args.no_repeat:
        return 1
    print(f"  CV > {CV_MAX} %: {', '.join('/'.join(k) for k in noisy)} — повтор набора")
    keep_skip = res.skipped
    fresh = Results()
    fresh.skipped = keep_skip
    run_cases(args, cases_of(), fresh)
    res.samples, res.done, res.skipped = fresh.samples, fresh.done, fresh.skipped
    res.unstable = set(fresh.noisy())
    return 2


# ---------- сюиты ----------


def visible_count(path: str) -> int:
    """Что покажет Проводник: записи без атрибута «скрытый»."""
    n = 0
    with os.scandir(path) as it:
        for e in it:
            try:
                attrs = getattr(e.stat(follow_symlinks=False), "st_file_attributes", 0)
            except OSError:
                continue
            if not attrs & 0x2:
                n += 1
    return n


def tool_wanted(args, name: str) -> bool:
    if not args.tools:
        return True
    return any(name == t or name.startswith(t + "-") for t in args.tools)


def suite_list(args, ident: str, ctx: dict, res: Results) -> None:
    d, manifest = dataset(args, ident)
    if not d:
        res.skip(ident, "*", f"нет датасета {ident} в --root (cargo run -p urfin-testdata --example testdata --release -- {ident.lower()} --root <том>)")
        return
    ctx["datasets"][ident] = manifest
    ctx.setdefault("volume_path", d)
    expect = visible_count(d) if tool_wanted(args, "explorer") else None
    reg = drivers.tools()
    tools: list[Tool] = [VfsBench(args, False), VfsBench(args, True), Explorer(silent=args.silent), reg["dir"], reg["gci"]]

    def cases():
        return [Case(ident, t, ListJob(d, expect)) for t in tools if tool_wanted(args, t.name)]

    ctx["attempts"] = max(ctx["attempts"], measure(args, cases, res))


def work_dir(args, near: str) -> str:
    w = args.work or os.path.join(os.path.dirname(os.path.abspath(near)), "perf-work")
    os.makedirs(w, exist_ok=True)
    open(os.path.join(w, MARKER), "a").close()
    return w


def rmtree_work(path: str, work: str) -> None:
    """Удаление только внутри рабочего каталога харнесса (маркер `.urfin-perf-work`)."""
    path = os.path.abspath(path)
    work = os.path.abspath(work)
    if not path.startswith(work + os.sep) or not os.path.isfile(os.path.join(work, MARKER)):
        raise RuntimeError(f"отказ удалять вне рабочего каталога: {path}")
    if os.path.exists(path):
        subprocess.run(["cmd", "/d", "/c", "rd", "/s", "/q", path], creationflags=win.popen_flags(), check=False)
    if os.path.exists(path):
        shutil.rmtree(path)


def suite_ops(args, ctx: dict, res: Results) -> None:
    d, manifest = dataset(args, "B2")
    if not d:
        res.skip("B2", "*", "нет датасета B2 в --root")
        return
    ctx["datasets"]["B2"] = manifest
    ctx.setdefault("volume_path", d)
    work = work_dir(args, d)
    reg = drivers.tools()
    info = ctx["urfin"]

    def urfin_op(op):
        def argv_of(job):
            p = os.path.join(work, f"scenario-{op}.toml")
            with open(p, "w", encoding="utf-8") as f:
                f.write(scenario_toml(op, job))
            return ["--bench-scenario", p]

        return UrfinApp(args, info, argv_of, "t_done")

    def clean_dst(c: Case) -> None:
        rmtree_work(c.job.dst, work)
        time.sleep(args.pause)

    def fresh_copy(c: Case) -> None:
        rmtree_work(c.job.target, work)
        p = subprocess.run(cli.robocopy_argv(CopyJob(d, work, os.path.basename(c.job.target))),
                           capture_output=True, creationflags=win.popen_flags(), check=False)
        if not cli.robocopy_ok(p.returncode):
            raise RuntimeError(f"подготовка копии: robocopy {p.returncode}")
        time.sleep(args.pause)

    exp = Explorer(silent=args.silent)

    def cases():
        out = []
        for t in (urfin_op("copy"), exp, reg["robocopy"]):
            if tool_wanted(args, t.name):
                out.append(Case("B2.copy", t, CopyJob(d, work, f"copy-{t.name}"), clean_dst))
        for t in (urfin_op("delete"), exp, reg["rd"]):
            if tool_wanted(args, t.name):
                out.append(Case("B2.delete", t, DeleteJob(os.path.join(work, f"del-{t.name}")), fresh_copy))
        return out

    ctx["attempts"] = max(ctx["attempts"], measure(args, cases, res))
    for n in os.listdir(work):
        if n != MARKER:
            rmtree_work(os.path.join(work, n), work)


URFIN_SUITES = {
    # сюита: (флаги, нужен датасет L1 / файл, метрика «до конца»)
    "startup": (["--bench-startup"], None, "t_first"),
    "frame": (["--bench"], "L1", "frame_p99"),
    "editor": (["--bench-editor"], "file", "frame_p99"),
    "viewer": (["--bench-viewer"], "file", "frame_p99"),
    "search-frame": (["--bench-search"], None, "frame_p99"),
    "sftp": (["--bench-sftp"], None, "frame_p99"),
}


def suite_urfin(args, name: str, ctx: dict, res: Results) -> None:
    flags, need, done = URFIN_SUITES[name]
    argv = list(flags)
    if need == "L1":
        d, manifest = dataset(args, "L1")
        if d:
            ctx["datasets"]["L1"] = manifest
            argv.append(d)
    elif need == "file":
        if not args.file:
            res.skip(name, "urfin", "нужен --file (testdata v1…v4)")
            return
        argv.append(args.file)
    tool = UrfinApp(args, ctx["urfin"], lambda _job: argv, done)
    ctx["attempts"] = max(ctx["attempts"], measure(args, lambda: [Case(name, tool, None)], res))


def flatten(prefix: str, v, out: dict[str, float]) -> None:
    if isinstance(v, bool):
        return
    if isinstance(v, (int, float)):
        out[prefix] = float(v)
    elif isinstance(v, dict):
        for k, x in v.items():
            flatten(f"{prefix}.{k}" if prefix else k, x, out)


def suite_search(args, ctx: dict, res: Results) -> None:
    if not args.corpus or not os.path.isdir(args.corpus):
        res.skip("search", "urfin", "нужен --corpus (search_bench gen <каталог>)")
        return
    exe = example(args, "urfin-search", "search_bench")
    p = subprocess.run([exe, "run", args.corpus, "--runs", str(min(args.runs, 5))], capture_output=True, check=False)
    v = cli.parse_last_json(p.stdout.decode("utf-8", "replace"))
    if p.returncode != 0 or not v:
        res.skip("search", "urfin", f"search_bench: код {p.returncode}")
        return
    m: dict[str, float] = {}
    flatten("", {k: x for k, x in v.items() if k not in ("runs", "cpus")}, m)
    add_self_timed(res, "search", "urfin", m)


def add_self_timed(res: Results, scenario: str, tool: str, m: dict[str, float]) -> None:
    res.add(scenario, tool, m, None)
    for k in m:
        res.self_timed.add((scenario, tool, k))


BENCH_OPS_ROW = re.compile(r"^\|\s*([^|]+?)\s*\|\s*([\d.]+)\s*\|\s*([\d.]+)\s*\|\s*([\d.]+)\s*%\s*\|\s*([^|]*?)\s*\|\s*(\d+)\s*\|")


def parse_bench_ops(text: str) -> list[tuple[str, str, dict[str, float]]]:
    """Таблицы `bench_ops` (S5-25): `### #3 …` и строки «инструмент | медиана, с | p95 | CV | TTFB | ошибок»."""
    out = []
    scen = None
    for line in text.splitlines():
        h = re.match(r"^###\s+#([\d/]+)", line)
        if h:
            scen = "bench_ops#" + h.group(1)
            continue
        r = BENCH_OPS_ROW.match(line)
        if scen and r:
            tool = r.group(1).strip()
            m = {"t_done": float(r.group(2)) * 1000, "p95": float(r.group(3)) * 1000, "cv_pct": float(r.group(4)), "errors_count": float(r.group(6))}
            if re.match(r"^[\d.]+$", r.group(5)):
                m["ttfb"] = float(r.group(5))
            out.append((scen, tool, m))
    return out


def suite_bench_ops(args, ctx: dict, res: Results) -> None:
    out = os.path.join(tempfile.mkdtemp(prefix="urfin-perf-"), "bench_ops.md")
    env = dict(os.environ, URFIN_BENCH_OUT=out, URFIN_BENCH_RUNS=str(min(args.runs, 5)))
    p = subprocess.run(
        ["cargo", "nextest", "run", "--release", "-p", "urfin-ops", "bench_ops", "--run-ignored", "only", "--no-capture"],
        cwd=REPO, env=env, check=False,
    )
    if p.returncode != 0 or not os.path.isfile(out):
        res.skip("bench_ops", "*", f"bench_ops: код {p.returncode}")
        return
    with open(out, encoding="utf-8") as f:
        for scen, tool, m in parse_bench_ops(f.read()):
            add_self_timed(res, scen, tool, m)


def suite_exe(args, ctx: dict, res: Results) -> None:
    info = ctx["urfin"]
    if not info:
        res.skip("exe", "urfin", "нет urfin.exe (--urfin)")
        return
    add_self_timed(res, "exe", "urfin", {"size_mib": info["size"] / (1 << 20)})


SUITES = ["l1", "l2", "ops", "search", "bench_ops", "exe", *URFIN_SUITES]
ALL = ["l1", "l2", "ops", "startup", "frame", "exe"]


def run_suite(args, name: str, info: dict | None) -> dict:
    res = Results()
    ctx: dict = {"datasets": {}, "attempts": 1, "urfin": info}
    print(f"сюита {name} ({'cold' if args.cold else 'warm'})")
    busy_pct, busy = background(args)
    if name in ("l1", "l2"):
        suite_list(args, name.upper(), ctx, res)
    elif name == "ops":
        suite_ops(args, ctx, res)
    elif name == "search":
        suite_search(args, ctx, res)
    elif name == "bench_ops":
        suite_bench_ops(args, ctx, res)
    elif name == "exe":
        suite_exe(args, ctx, res)
    else:
        suite_urfin(args, name, ctx, res)
    now = dt.datetime.now().astimezone()
    return {
        "schema": SCHEMA,
        "suite": name,
        "mode": "cold" if args.cold else "warm",
        "date": now.isoformat(timespec="seconds"),
        "host": platform.node(),
        "env": fingerprint(ctx.get("volume_path") or args.root, info),
        "background_cpu": None if busy_pct is None else round(busy_pct, 2),
        "busy": busy,
        "warmup": args.warmup,
        "runs": args.runs,
        "attempts": ctx["attempts"],
        "datasets": ctx["datasets"],
        "results": res.to_json(),
        "skipped": res.skipped,
    }


# ---------- схема ----------

REQUIRED = {
    "schema": str, "suite": str, "mode": str, "date": str, "host": str, "env": dict,
    "warmup": int, "runs": int, "results": list, "skipped": list,
}
RESULT_REQUIRED = {"key": str, "scenario": str, "tool": str, "metric": str, "unit": str, "samples": list, "n": int, "done": bool, "unstable": bool}


def validate(run: dict) -> list[str]:
    errs = []
    for k, t in REQUIRED.items():
        if not isinstance(run.get(k), t):
            errs.append(f"{k}: нужен {t.__name__}")
    if run.get("schema") != SCHEMA:
        errs.append(f"schema: {run.get('schema')!r} ≠ {SCHEMA}")
    if run.get("mode") not in ("warm", "cold"):
        errs.append("mode: warm|cold")
    for i, r in enumerate(run.get("results") or []):
        for k, t in RESULT_REQUIRED.items():
            if not isinstance(r.get(k), t):
                errs.append(f"results[{i}].{k}: нужен {t.__name__}")
        if isinstance(r.get("samples"), list) and r.get("n") != len(r["samples"]):
            errs.append(f"results[{i}].n ≠ len(samples)")
        if r.get("key") != f"{r.get('scenario')}/{r.get('tool')}/{r.get('metric')}":
            errs.append(f"results[{i}].key ≠ scenario/tool/metric")
    for i, s in enumerate(run.get("skipped") or []):
        if not all(isinstance(s.get(k), str) for k in ("scenario", "tool", "reason")):
            errs.append(f"skipped[{i}]: scenario, tool, reason")
    return errs


def run_path(run: dict, runs_dir: str) -> str:
    host = re.sub(r"[^A-Za-z0-9_-]+", "-", run["host"]).strip("-").lower() or "host"
    base = f"{run['date'][:10]}-{host}-{run['suite']}"
    p = os.path.join(runs_dir, base + ".json")
    i = 2
    while os.path.exists(p):
        p = os.path.join(runs_dir, f"{base}-{i}.json")
        i += 1
    return p


# ---------- отчёт и сравнение ----------


def perf_check():
    """`ci/perf_check.py` как модуль (бюджеты); нет файла — None."""
    p = os.path.join(REPO, "ci", "perf_check.py")
    if not os.path.isfile(p):
        return None
    spec = importlib.util.spec_from_file_location("perf_check", p)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def load_runs(runs_dir: str) -> list[tuple[str, dict]]:
    out = []
    for n in sorted(os.listdir(runs_dir)) if os.path.isdir(runs_dir) else []:
        if not n.endswith(".json"):
            continue
        try:
            with open(os.path.join(runs_dir, n), encoding="utf-8") as f:
                v = json.load(f)
        except (OSError, ValueError):
            continue
        if isinstance(v, dict) and v.get("schema") == SCHEMA:
            out.append((n, v))
    return out


def fmt(v, nd: int = 1) -> str:
    if v is None:
        return "—"
    s = f"{v:.{nd}f}" if abs(v) < 1000 else f"{v:.0f}"
    return s.replace(".", ",")


def done_of(run: dict) -> dict[tuple[str, str], dict]:
    return {(r["scenario"], r["tool"]): r for r in run["results"] if r["done"]}


def render_report(runs: list[tuple[str, dict]], budgets_path: str | None = None) -> str:
    pc = perf_check()
    budgets = pc.load_budgets(budgets_path) if pc else None
    latest: dict[tuple, tuple[str, dict]] = {}
    for n, r in runs:
        k = (r["host"], r["suite"], r["mode"], r["env"].get("fs") or "?")
        if k not in latest or r["date"] > latest[k][1]["date"]:
            latest[k] = (n, r)
    lines = [
        "# Замеры стенда (S9 §3.4)",
        "",
        "Генерируется: `python scripts/stand/perf.py report`, руками не править. Последний прогон",
        "на хост × сюиту × режим × ФС из `docs/perf/runs/`; ×Urfin — во сколько раз дольше Urfin",
        "(метрика «до конца»), бюджеты — `ci/budgets.toml`.",
    ]
    for mode in ("warm", "cold"):
        groups = sorted(k for k in latest if k[2] == mode)
        if not groups:
            continue
        lines += ["", f"## {mode}"]
        for k in groups:
            n, r = latest[k]
            env = r["env"]
            lines += [
                "",
                f"### {r['suite']} · {k[3]} · {r['host']} · {r['date'][:10]}",
                "",
                f"`{n}`: {env.get('os', '?')} {env.get('os_build', '')}, {env.get('cpu', '?')}, "
                f"{fmt(env.get('ram_gb'))} ГБ, диск {env.get('disk', '?')}, sha {str(env.get('sha', ''))[:9]}"
                f"{', фон ' + fmt(r.get('background_cpu')) + ' %' if r.get('background_cpu') is not None else ''}"
                f"{' — **фон выше нормы**' if r.get('busy') else ''}.",
                "",
            ]
            verdict = {}
            if pc and budgets is not None:
                for row in pc.evaluate(r, budgets):
                    for key in row.get("keys", []):
                        verdict.setdefault(key, []).append(f"{'✓' if row['ok'] else '✗'} {row['id']}")
            done = done_of(r)
            lines += ["| Сценарий | Инструмент | Метрика | Медиана | p95 | min | CV, % | ×Urfin | Бюджет |", "|---|---|---|---:|---:|---:|---:|---:|---|"]
            for x in r["results"]:
                ratio = "—"
                if x["done"] and not x["tool"].startswith("urfin"):
                    u = done.get((x["scenario"], "urfin"))
                    if u and u["median"] and x["median"] is not None:
                        ratio = "×" + fmt(x["median"] / u["median"])
                cv = fmt(x["cv"]) + (" unstable" if x["unstable"] else "")
                if x.get("self_timed"):
                    cv = "сам"
                metric = f"**{x['metric']}**" if x["done"] else x["metric"]
                lines.append(
                    f"| {x['scenario']} | {x['tool']} | {metric}, {x['unit']} | {fmt(x['median'])} | {fmt(x['p95'])} | "
                    f"{fmt(x['min'])} | {cv} | {ratio} | {', '.join(verdict.get(x['key'], [])) or '—'} |"
                )
            if r["skipped"]:
                lines.append("")
                for s in r["skipped"]:
                    lines.append(f"- пропуск {s['scenario']}/{s['tool']}: {s['reason']}")
    return "\n".join(lines) + "\n"


def compare(a: dict, b: dict, pct: float) -> tuple[list[str], bool]:
    """Медианы A → B. Регрессия: хуже > `pct` % при CV обоих ≤ 5 % (меньше — лучше)."""
    ra = {x["key"]: x for x in a["results"]}
    rb = {x["key"]: x for x in b["results"]}
    rows = ["| Ключ | A | B | Δ, % | CV A / B | |", "|---|---:|---:|---:|---|---|"]
    bad = False
    for k in sorted(set(ra) & set(rb)):
        x, y = ra[k], rb[k]
        if x["median"] in (None, 0) or y["median"] is None:
            continue
        d = (y["median"] - x["median"]) / x["median"] * 100.0
        steady = (x.get("cv") or 0) <= CV_MAX and (y.get("cv") or 0) <= CV_MAX
        mark = ""
        if d > pct:
            mark = "✗ регрессия" if steady else "хуже (шумно)"
            bad = bad or (steady and x["done"])
        elif d < -pct:
            mark = "лучше"
        rows.append(f"| {k} | {fmt(x['median'])} | {fmt(y['median'])} | {d:+.1f} | {fmt(x.get('cv'))} / {fmt(y.get('cv'))} | {mark} |")
    for k in sorted(set(ra) ^ set(rb)):
        rows.append(f"| {k} | {'есть' if k in ra else '—'} | {'есть' if k in rb else '—'} | | | только в одном |")
    return rows, bad


# ---------- детектор против IFileOperation (S9-11) ----------


def detector_check(args) -> int:
    """Копия Проводником (`IFileOperation`) и внешний детектор на ней: расхождение концов."""
    src = os.path.abspath(args.src)
    work = work_dir(args, src)
    expected = detector.tree_stats(src)
    print(f"детектор: {src} ({expected[0]} файлов, {expected[1] / (1 << 20):.1f} МиБ), {args.runs} прогона")
    from drivers import explorer as ex

    diffs = []
    for i in range(args.runs):
        name = f"det-{i}"
        dst = os.path.join(work, name)
        rmtree_work(dst, work)
        time.sleep(args.pause)
        t0 = win.ticks()
        p = ex.spawn("copy", Src=src, Dst=work, Name=name, Silent=args.silent, LingerMs=1500)
        det = detector.watch(t0, p.pid, dst, expected=expected, quiet_ms=500)
        v = ex.finish(p, timeout=600)
        t_op = win.ms_between(t0, int(v["t_done"]), int(v["freq"]))
        if det.t_end is None:
            print(f"  {i + 1}: детектор не дождался конца (dst {det.final}, ждали {expected})")
            diffs.append(None)
        else:
            diffs.append(det.t_end - t_op)
            print(f"  {i + 1}: IFileOperation {t_op:.0f} мс, детектор {det.t_end:.0f} мс, расхождение {det.t_end - t_op:+.0f} мс ({det.polls} опросов)")
        rmtree_work(dst, work)
    ok = all(d is not None and abs(d) <= 150 for d in diffs)
    print(f"детектор: {'✓' if ok else '✗'} |расхождение| ≤ 150 мс")
    return 0 if ok else 1


# ---------- selftest ----------

RUN_A = {
    "schema": SCHEMA, "suite": "l1", "mode": "warm", "date": "2026-11-12T10:00:00+03:00", "host": "stand",
    "env": {"os": "Windows 11 Pro 25H2", "os_build": "26300.1", "cpu": "Intel Core Ultra 5 225H", "ram_gb": 15.2, "fs": "NTFS", "disk": "WD SN5000S", "sha": "abc1234567"},
    "background_cpu": 0.7, "busy": False, "warmup": 2, "runs": 10, "attempts": 1, "datasets": {"L1": {"id": "L1"}},
    "results": [
        {"key": "L1/urfin/t_sorted", "scenario": "L1", "tool": "urfin", "metric": "t_sorted", "unit": "ms",
         "samples": [55.0] * 10, "n": 10, "median": 55.0, "p95": 55.0, "min": 55.0, "mean": 55.0, "cv": 1.0,
         "done": True, "unstable": False, "self_timed": False},
        {"key": "L1/urfin/t_first", "scenario": "L1", "tool": "urfin", "metric": "t_first", "unit": "ms",
         "samples": [1.5] * 10, "n": 10, "median": 1.5, "p95": 1.5, "min": 1.5, "mean": 1.5, "cv": 3.0,
         "done": False, "unstable": False, "self_timed": False},
        {"key": "L1/explorer/t_done", "scenario": "L1", "tool": "explorer", "metric": "t_done", "unit": "ms",
         "samples": [1210.0] * 10, "n": 10, "median": 1210.0, "p95": 1300.0, "min": 1150.0, "mean": 1220.0, "cv": 4.0,
         "done": True, "unstable": False, "self_timed": False},
    ],
    "skipped": [{"scenario": "L1", "tool": "gci", "reason": "проверка"}],
}


def _with(run: dict, key: str, median: float, cv: float) -> dict:
    out = json.loads(json.dumps(run))
    for r in out["results"]:
        if r["key"] == key:
            r["median"], r["cv"] = median, cv
    out["date"] = "2026-11-13T10:00:00+03:00"
    return out


def ps_parse_ok(path: str) -> str | None:
    """Синтаксис .ps1 парсером PowerShell (без запуска); None — без ошибок."""
    if not win.IS_WIN:
        return None
    cmd = (
        "$e=$null; [void][System.Management.Automation.Language.Parser]::ParseFile($env:URFIN_PS1,[ref]$null,[ref]$e);"
        "if($e){ $e | ForEach-Object { $_.Message }; exit 1 }"
    )
    p = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", cmd], capture_output=True,
                       env=dict(os.environ, URFIN_PS1=path), creationflags=win.popen_flags(), check=False)
    return None if p.returncode == 0 else p.stdout.decode("utf-8", "replace")


def selftest() -> int:
    st = stats([float(x) for x in range(1, 11)])
    assert st["median"] == 5.5 and st["p95"] == 10 and st["min"] == 1 and st["n"] == 10, st
    assert stats([2.0, 2.0])["cv"] == 0.0 and stats([1.0, 2.0, 3.0])["p95"] is None

    # Строка JSON S9-05 (зашитые примеры): `peak_rss` в байтах → МБ, null пропускается.
    m = parse_bench_json('bench ui=gpui rows=100000 list_p50=2.1\n{"bench":"ui","t_first":null,"t_done":null,'
                         '"t_sorted":null,"peak_rss":104857600,"frame_p50":2.1,"frame_p99":3.9,"allocs":null,'
                         '"raw":{"rows":100000,"list_p50":2.1,"ui":"gpui"}}\n')
    assert m == {"peak_rss_mb": 100.0, "frame_p50": 2.1, "frame_p99": 3.9, "raw.rows": 100000.0, "raw.list_p50": 2.1}, m
    m = parse_bench_json('{"bench":"scenario","t_first":3.5,"t_done":43000,"t_sorted":null,"peak_rss":null,'
                         '"frame_p50":null,"frame_p99":null,"allocs":120,"raw":{}}')
    assert m == {"t_first": 3.5, "t_done": 43000.0, "allocs": 120.0}, m
    assert parse_bench_json("bench startup paths=1.2 core=3.4\n") is None

    v = parse_vfs_bench('{"dir":"x","entries":99462,"runs":1,"os":"windows","stream":{"t_first_ms":0.9,"t_done_ms":40.1},'
                        '"core":{"t_first_ms":1.04,"t_sorted_ms":55.76,"heap_peak_mb":5.8},"peak_ws_mb":17.4,'
                        '"peak_private_mb":19.9,"listing_peak_ws_mb":12.3,"listing_peak_private_mb":18.1}')
    assert v["urfin"]["t_sorted"] == 55.76 and v["urfin"]["listing_peak_mb"] == 12.3 and v["urfin-stream"]["t_done"] == 40.1

    ops = parse_bench_ops("\n### #3 копия B2 (100000 файлов, 3100 МиБ), тот же том NVMe; #6 TTFB\n\n"
                          "| Инструмент | медиана, с | p95, с | CV | TTFB, мс | ошибок |\n|---|---:|---:|---:|---:|---:|\n"
                          "| Urfin | 43.00 | 73.70 | 12.0 % | 8 | 0 |\n| Explorer | 1877.00 | 1990.00 | 4.0 % | — | 0 |\n")
    assert ops[0] == ("bench_ops#3", "Urfin", {"t_done": 43000.0, "p95": 73700.0, "cv_pct": 12.0, "errors_count": 0.0, "ttfb": 8.0}), ops
    assert "ttfb" not in ops[1][2]

    toml = scenario_toml("copy", CopyJob("C:\\ds\\B2", "C:\\w", "copy-urfin"))
    assert "op = \"copy\"" in toml and "src = 'C:\\ds\\B2'" in toml

    # Results: CV > 5 % по метрике «до конца» — шумно; self_timed не считается.
    res = Results()
    for x in (100, 100, 100, 130):
        res.add("L1", "dir", {"t_done": x, "wall": x}, "t_done")
    assert res.noisy() == [("L1", "dir", "t_done")]
    out = res.to_json()
    assert not validate({**RUN_A, "results": out}), validate({**RUN_A, "results": out})

    # Схема и отчёт на зашитых прогонах.
    assert not validate(RUN_A), validate(RUN_A)
    bad = json.loads(json.dumps(RUN_A))
    bad["results"][0]["n"] = 3
    del bad["host"]
    assert len(validate(bad)) == 2, validate(bad)
    with tempfile.TemporaryDirectory() as tmp:
        run_b = _with(RUN_A, "L1/urfin/t_sorted", 70.0, 2.0)
        for i, r in enumerate((RUN_A, run_b)):
            with open(os.path.join(tmp, f"r{i}.json"), "w", encoding="utf-8") as f:
                json.dump(r, f)
        with open(os.path.join(tmp, "other.json"), "w", encoding="utf-8") as f:
            json.dump({"schema": "x"}, f)
        runs = load_runs(tmp)
        assert len(runs) == 2
        md = render_report(runs)
        assert "### l1 · NTFS · stand · 2026-11-13" in md and md.count("### l1") == 1, md
        assert "| L1 | explorer | **t_done**, ms | 1210 |" in md and "×17,3" in md, md
        assert "пропуск L1/gci: проверка" in md
        if perf_check():
            assert "✓ L.7" in md and "✓ L.3" in md and "✓ L.1" in md, md  # 1210 / 70 ≥ 5; 70 < 90; 1,5 < 5
        p = run_path(RUN_A, tmp)
        assert p.endswith("2026-11-12-stand-l1.json"), p
    rows, bad_ = compare(RUN_A, _with(RUN_A, "L1/urfin/t_sorted", 70.0, 2.0), 10.0)
    assert bad_ and any("✗ регрессия" in r for r in rows), rows
    rows, bad_ = compare(RUN_A, _with(RUN_A, "L1/urfin/t_sorted", 70.0, 9.0), 10.0)
    assert not bad_ and any("хуже (шумно)" in r for r in rows), rows
    rows, bad_ = compare(RUN_A, _with(RUN_A, "L1/urfin/t_sorted", 56.0, 1.0), 10.0)
    assert not bad_

    # Удаление только внутри рабочего каталога с маркером.
    with tempfile.TemporaryDirectory() as tmp:
        try:
            rmtree_work(os.path.join(tmp, "x"), os.path.join(tmp, "work"))
            raise AssertionError("rmtree_work без маркера")
        except RuntimeError:
            pass

    drivers.selftest()
    err = ps_parse_ok(os.path.join(HERE, "drivers", "explorer.ps1"))
    assert err is None, f"explorer.ps1: {err}"
    print("perf.py selftest: ok")
    return 0


# ---------- CLI ----------


def utf8_stdout() -> None:
    """Вывод в UTF-8 и при перенаправлении (иначе консоль cp1251 падает на ✓/✗)."""
    for s in (sys.stdout, sys.stderr):
        try:
            s.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass


def main() -> int:
    utf8_stdout()
    ap = argparse.ArgumentParser(description="Замеры стенда (S9-04)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("suites", nargs="+", help=f"{', '.join(SUITES)} или all")
    g = r.add_mutually_exclusive_group()
    g.add_argument("--warm", action="store_true")
    g.add_argument("--cold", action="store_true")
    r.add_argument("--tools", type=lambda s: [x.strip() for x in s.split(",") if x.strip()])
    r.add_argument("--root", help="каталог датасетов (<root>\\<ID>, S9-02)")
    r.add_argument("--work", help="рабочий каталог операций (тот же том)")
    r.add_argument("--urfin", default=os.environ.get("URFIN_EXE", r"C:\Bench\urfin\urfin.exe"))
    r.add_argument("--file", help="файл для editor/viewer")
    r.add_argument("--corpus", help="корпус search_bench")
    r.add_argument("--bin-dir", help="готовые примеры (vfs_bench.exe, …) вместо cargo build")
    r.add_argument("--gen", action="store_true", help="собрать недостающие датасеты")
    r.add_argument("--runs", type=int)
    r.add_argument("--warmup", type=int, default=2)
    r.add_argument("--pause", type=float, default=5.0, help="пауза после очистки dst, с")
    r.add_argument("--silent", action="store_true", help="Проводник без диалога прогресса")
    r.add_argument("--vhdx", help="--cold: VHDX тома датасетов")
    r.add_argument("--rammap", help="--cold: RAMMap64.exe (системный том)")
    r.add_argument("--bg-max", type=float, default=2.0)
    r.add_argument("--bg-seconds", type=float, default=10.0)
    r.add_argument("--bg-tries", type=int, default=3)
    r.add_argument("--allow-busy", action="store_true")
    r.add_argument("--no-repeat", action="store_true", help="без повтора набора при CV > 5 %%")
    r.add_argument("--out")
    r.add_argument("--no-save", action="store_true")
    rp = sub.add_parser("report")
    rp.add_argument("--runs-dir", default=RUNS_DIR)
    rp.add_argument("--out", default=REPORT)
    rp.add_argument("--budgets")
    c = sub.add_parser("compare")
    c.add_argument("a")
    c.add_argument("b")
    c.add_argument("--pct", type=float, default=10.0)
    d = sub.add_parser("detector-check")
    d.add_argument("--src", required=True)
    d.add_argument("--work")
    d.add_argument("--runs", type=int, default=3)
    d.add_argument("--pause", type=float, default=5.0)
    d.add_argument("--silent", action="store_true")
    sub.add_parser("selftest")
    a = ap.parse_args()

    if a.cmd == "selftest":
        return selftest()
    if a.cmd == "report":
        md = render_report(load_runs(a.runs_dir), a.budgets)
        with open(a.out, "w", encoding="utf-8", newline="\n") as f:
            f.write(md)
        print(f"отчёт: {a.out}")
        return 0
    if a.cmd == "compare":
        with open(a.a, encoding="utf-8") as f:
            ra = json.load(f)
        with open(a.b, encoding="utf-8") as f:
            rb = json.load(f)
        rows, bad = compare(ra, rb, a.pct)
        print("\n".join(rows))
        return 1 if bad else 0
    if a.cmd == "detector-check":
        return detector_check(a)

    if a.cold:
        if not is_admin():
            raise SystemExit("--cold: нужны права администратора (переподключение VHDX, RAMMap) — DEV.md, п. 5")
        if not a.vhdx:
            raise SystemExit("--cold: нужен --vhdx")
    if a.runs is None:
        a.runs = 5 if a.cold else 10
    names = []
    for s in a.suites:
        names += ALL if s == "all" else [s]
    for s in names:
        if s not in SUITES:
            raise SystemExit(f"нет сюиты {s}: {', '.join(SUITES)}")
    info = urfin_info(a.urfin)
    code = 0
    for s in names:
        run = run_suite(a, s, info)
        errs = validate(run)
        if errs:
            print("схема:", "; ".join(errs))
            code = 1
        for x in run["results"]:
            if x["done"]:
                print(f"  {x['key']}: медиана {fmt(x['median'])} {x['unit']}, CV {fmt(x['cv'])} %{' unstable' if x['unstable'] else ''}")
        if a.no_save:
            continue
        path = a.out if a.out and len(names) == 1 else run_path(run, RUNS_DIR)
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            json.dump(run, f, ensure_ascii=False, indent=1)
            f.write("\n")
        print(f"прогон: {path}")
    return code


if __name__ == "__main__":
    sys.exit(main())
