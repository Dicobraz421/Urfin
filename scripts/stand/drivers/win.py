"""Win32 через ctypes для харнесса стенда (S9-04, S9-11): часы, CPU системы, I/O процесса.

Только чтение: ни одного вызова, меняющего систему. Часы — `QueryPerformanceCounter`,
тот же счётчик, что `[Diagnostics.Stopwatch]::GetTimestamp()` в PowerShell: метки
драйверов сравниваются без пересчёта. Не на Windows — заглушки на `time.perf_counter_ns`
(нужны только для selftest в облаке).
"""

from __future__ import annotations

import ctypes
import os
import sys
import time

IS_WIN = sys.platform == "win32"

if IS_WIN:
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _freq = ctypes.c_int64()
    _k32.QueryPerformanceFrequency(ctypes.byref(_freq))
    QPC_FREQ = _freq.value

    class _FT(ctypes.Structure):
        _fields_ = [("lo", wintypes.DWORD), ("hi", wintypes.DWORD)]

    class _IO(ctypes.Structure):
        _fields_ = [
            ("ReadOperationCount", ctypes.c_uint64),
            ("WriteOperationCount", ctypes.c_uint64),
            ("OtherOperationCount", ctypes.c_uint64),
            ("ReadTransferCount", ctypes.c_uint64),
            ("WriteTransferCount", ctypes.c_uint64),
            ("OtherTransferCount", ctypes.c_uint64),
        ]

    class _MEM(ctypes.Structure):
        _fields_ = [
            ("dwLength", wintypes.DWORD),
            ("dwMemoryLoad", wintypes.DWORD),
            ("ullTotalPhys", ctypes.c_uint64),
            ("ullAvailPhys", ctypes.c_uint64),
            ("ullTotalPageFile", ctypes.c_uint64),
            ("ullAvailPageFile", ctypes.c_uint64),
            ("ullTotalVirtual", ctypes.c_uint64),
            ("ullAvailVirtual", ctypes.c_uint64),
            ("ullAvailExtendedVirtual", ctypes.c_uint64),
        ]

    _k32.OpenProcess.restype = wintypes.HANDLE
    _k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _k32.GetProcessIoCounters.argtypes = [wintypes.HANDLE, ctypes.POINTER(_IO)]
    _k32.CloseHandle.argtypes = [wintypes.HANDLE]
else:  # pragma: no cover - облако, selftest
    QPC_FREQ = 1_000_000_000

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
CREATE_NO_WINDOW = 0x08000000


def ticks() -> int:
    """Метка QPC (на Windows — тот же счётчик, что Stopwatch в .NET)."""
    if IS_WIN:
        v = ctypes.c_int64()
        _k32.QueryPerformanceCounter(ctypes.byref(v))
        return v.value
    return time.perf_counter_ns()


def ms_between(a: int, b: int, freq: int = 0) -> float:
    return (b - a) * 1000.0 / (freq or QPC_FREQ)


def _ft(v: "_FT") -> int:
    return (v.hi << 32) | v.lo


def system_times() -> tuple[int, int] | None:
    """(занято, всего) в 100 нс с загрузки: `GetSystemTimes` (kernel включает idle)."""
    if not IS_WIN:
        return None
    idle, kern, user = _FT(), _FT(), _FT()
    if not _k32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kern), ctypes.byref(user)):
        return None
    total = _ft(kern) + _ft(user)
    return total - _ft(idle), total


def cpu_busy_pct(seconds: float) -> float | None:
    """Загрузка CPU системы за `seconds`, % от всех ядер (фон перед прогоном, §3.2)."""
    a = system_times()
    if a is None:
        return None
    time.sleep(seconds)
    b = system_times()
    if b is None:
        return None
    busy, total = b[0] - a[0], b[1] - a[1]
    return 100.0 * busy / total if total > 0 else 0.0


def ram_bytes() -> int | None:
    if not IS_WIN:
        return None
    m = _MEM()
    m.dwLength = ctypes.sizeof(_MEM)
    return m.ullTotalPhys if _k32.GlobalMemoryStatusEx(ctypes.byref(m)) else None


class ProcIo:
    """Счётчики I/O процесса (`GetProcessIoCounters`); хэндл держит их и после выхода."""

    def __init__(self, pid: int) -> None:
        self.h = _k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid) if IS_WIN else None

    def total(self) -> int | None:
        if not self.h:
            return None
        io = _IO()
        if not _k32.GetProcessIoCounters(self.h, ctypes.byref(io)):
            return None
        return (
            io.ReadOperationCount
            + io.WriteOperationCount
            + io.OtherOperationCount
            + io.ReadTransferCount
            + io.WriteTransferCount
        )

    def close(self) -> None:
        if self.h:
            _k32.CloseHandle(self.h)
            self.h = None


def popen_flags() -> int:
    """Флаги дочерних CLI: без своего консольного окна."""
    return CREATE_NO_WINDOW if IS_WIN else 0


def volume_of(path: str) -> str:
    """Буква тома пути (`N:`) или корень UNC."""
    drive, _ = os.path.splitdrive(os.path.abspath(path))
    return drive
