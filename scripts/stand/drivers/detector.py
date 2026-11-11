"""Внешний детектор завершения операции (S9 §3.3, S9-11).

Для инструментов, которые не сообщают конец сами (Far, TC — после решения владельца; и
для сверки с `IFileOperation`): раз в 100 мс читаются счётчики I/O процесса
(`GetProcessIoCounters`) и состояние dst (число файлов и байты обходом `os.scandir`).
Конец — последняя активность (середина между опросом без изменений и опросом с ними)
перед 500 мс тишины; при заданном ожидании
dst должен с ним совпасть. Точность — ± половина интервала опроса (плюс время обхода dst).

Обход большого dst дорогой, поэтому он идёт, только когда I/O процесса стих (или каждый
десятый опрос): пока процесс пишет, достаточно счётчиков.
"""

from __future__ import annotations

import os
import time
from dataclasses import dataclass, field

from . import win


def tree_stats(root: str) -> tuple[int, int]:
    """(файлов, байт) под `root`; нет каталога — (0, 0). Ссылки не разворачиваются."""
    files = size = 0
    stack = [root]
    while stack:
        d = stack.pop()
        try:
            it = os.scandir(d)
        except OSError:
            continue
        with it:
            for e in it:
                try:
                    if e.is_dir(follow_symlinks=False):
                        stack.append(e.path)
                    else:
                        files += 1
                        size += e.stat(follow_symlinks=False).st_size
                except OSError:
                    pass
    return files, size


@dataclass
class Sample:
    """Один опрос: метка (мс от старта), сумма счётчиков I/O, состояние dst (или None)."""

    t: float
    io: int | None
    dst: tuple[int, int] | None


@dataclass
class Detection:
    t_end: float | None  # мс от старта: последняя активность перед тишиной
    matched: bool  # dst совпал с ожиданием (или ожидания нет)
    polls: int
    final: tuple[int, int] | None
    samples: list[Sample] = field(default_factory=list)


def detect_end(
    samples: list[Sample],
    quiet_ms: float = 500.0,
    expected: tuple[int, int] | None = None,
) -> Detection:
    """Конец по записанным опросам: чистая функция (selftest, повторный разбор).

    Активность — изменение `io` или `dst` против прошлого опроса.
    Конец найден, если после последней активности прошло ≥ `quiet_ms` и (при ожидании)
    последний известный dst равен ожиданию.
    """
    last_change = samples[0].t if samples else None
    final = None
    seen = False
    prev: Sample | None = None
    for s in samples:
        changed = s.io is not None and prev is not None and prev.io is not None and s.io != prev.io
        # dst сравнивается только с обходом на прошлом опросе: разница с давним обходом —
        # догоняющий снимок того, что уже видели счётчики I/O, а не новая активность.
        if s.dst is not None:
            if prev is not None and prev.dst is not None and s.dst != prev.dst:
                changed = True
            final = s.dst
        if changed:
            # Активность была между прошлым опросом и этим: середина — ошибка ± интервал/2.
            last_change = (prev.t + s.t) / 2
            seen = True
        prev = s
    matched = expected is None or final == expected
    # Без ожидания тишина до первой активности — ещё не начало, а не конец.
    started = seen or expected is not None
    done = bool(samples) and started and samples[-1].t - last_change >= quiet_ms
    return Detection(last_change if done and matched else None, matched, len(samples), final, samples)


def watch(
    t0: int,
    pid: int | None,
    dst: str | None,
    expected: tuple[int, int] | None = None,
    interval: float = 0.1,
    quiet_ms: float = 500.0,
    timeout: float = 3600.0,
) -> Detection:
    """Живой опрос от метки `t0` (QPC): процесс `pid` (может быть None) и каталог `dst`."""
    io = win.ProcIo(pid) if pid else None
    samples: list[Sample] = []
    walk_every = 10
    try:
        n = 0
        while True:
            t = win.ms_between(t0, win.ticks())
            v = io.total() if io else None
            quiet = not samples or v is None or v == samples[-1].io
            d = tree_stats(dst) if dst and (quiet or n % walk_every == 0) else None
            samples.append(Sample(t, v, d))
            n += 1
            det = detect_end(samples, quiet_ms, expected)
            if det.t_end is not None:
                return det
            if t > timeout * 1000.0:
                return det
            time.sleep(interval)
    finally:
        if io:
            io.close()


def selftest() -> None:
    # Копия: I/O растёт до 200 мс, dst обходится только в тишине (300 мс — догоняющий снимок).
    s = [Sample(0, 10, (0, 0)), Sample(100, 20, None), Sample(200, 30, None)]
    s += [Sample(300 + 100 * i, 30, (5, 50)) for i in range(4)]  # до 600 мс
    assert detect_end(s, expected=(5, 50)).t_end is None  # тишина 450 мс < 500
    s.append(Sample(700, 30, (5, 50)))
    d = detect_end(s, expected=(5, 50))
    assert d.t_end == 150 and d.matched, d  # середина между 100 и 200 мс
    # Ожидание не совпало — конца нет, даже при тишине.
    d = detect_end(s, expected=(6, 60))
    assert d.t_end is None and not d.matched
    # Только dst (процесс не известен): обходы подряд, изменение между 100 и 200 мс.
    rm = [Sample(0, None, (3, 30)), Sample(100, None, (1, 10)), Sample(200, None, (0, 0))]
    rm += [Sample(300 + 100 * i, None, (0, 0)) for i in range(6)]
    assert detect_end(rm, expected=(0, 0)).t_end == 150
    # dst меняется в тишине I/O на обходах подряд (пишет другой процесс) — это активность.
    ext = [Sample(100 * i, 5, (i, i)) for i in range(4)] + [Sample(400 + 100 * i, 5, (3, 3)) for i in range(6)]
    assert detect_end(ext).t_end == 250
    # Тишина до старта без ожидания — не конец.
    idle = [Sample(100 * i, 7, None) for i in range(8)]
    assert detect_end(idle).t_end is None
