# S9-09: старт — от создания процесса (`GetProcessTimes` в `app::sys`)

(владелец, 2026-11-12, класс B) OK в чате на карточку S9-09: отсчёт старта ведётся от создания процесса, а не от входа в `main`. Исполняет [S9 §4](../specs/S9.md) «Старт».

- Время создания процесса — `GetProcessTimes` в `crates/app/src/sys/` (unsafe только там). Новая фаза `startup.process` — от создания процесса до входа в `main`: загрузчик ОС, DLL, CRT. Остальные фазы `startup.*` по-прежнему от `main`, имена не меняются (контракт S0 v5 №10).
- `--bench-startup` отдаёт `startup.process` в обеих строках JSON (в строке S9-05 — в `raw`) и память к первому кадру: `mem.ws`, `mem.ws_peak`, `mem.private`, `mem.private_peak` (байты).
- Профиль WPR — `crates/app/perf/urfin.wprp` (CPU, FileIO, DiskIO, ImageLoad, VirtualAlloc). Запись ETW (`--wpr`) требует прав администратора и идёт только на стенде.
