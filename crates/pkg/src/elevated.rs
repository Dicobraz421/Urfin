//! Запуск с повышением прав через UAC (F02 §8, §3.3): так идут действия choco.
//! Помощника `urfin.exe --pkg-elevated` нет (решение владельца, S0 v6 №17).
//!
//! Без `unsafe`: обёртка — Windows PowerShell 5.1 (есть в каждой Windows) со
//! скриптом в `-EncodedCommand`. Скрипт вызывает `Process.Start` с
//! `UseShellExecute` и `Verb = runas` (это `ShellExecuteExW(runas)`): UAC показывает
//! сам `choco.exe`, choco идёт в своём консольном окне. Вызов блокируется до решения
//! пользователя (job «ждёт подтверждения UAC», §3.3); `ERROR_CANCELLED` (1223) —
//! отказ. Затем скрипт ждёт выхода choco и печатает маркеры:
//! `<<URFIN>> started <pid>`, `<<URFIN>> exit <код>`, `<<URFIN>> declined`,
//! `<<URFIN>> failed <текст>`.
//!
//! Вывод повышенного процесса в наш пайп не попадает: ход берётся из журнала choco
//! (`chocolatey.log`, только новое с момента запуска, без `DEBUG`) и построчно уходит
//! в `RunCx::lines` (ленточный сеанс). Отмена убивает обёртку; повышенный choco в наш
//! job не входит и продолжает работу ([`crate::choco::CANCEL_ELEVATED`]).

use std::ffi::OsString;

use crate::api::PkgError;
use crate::scoop::{MARKER, ps_quote};

/// Аргумент командной строки Windows для `ProcessStartInfo.Arguments`: с пробелом —
/// в кавычках (хвостовые `\` удваиваются). Кавычки и управляющие символы не
/// допускаются: в белом списке их нет.
pub fn win_arg(arg: &str) -> Result<String, PkgError> {
    if arg.is_empty() || arg.chars().any(|c| c == '"' || c.is_control()) {
        return Err(PkgError::BadId(arg.into()));
    }
    if !arg.contains([' ', '\t']) {
        return Ok(arg.to_owned());
    }
    let trailing = arg.len() - arg.trim_end_matches('\\').len();
    Ok(format!("\"{arg}{}\"", "\\".repeat(trailing)))
}

/// Как показать запущенный процесс.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    /// Через UAC (`runas`), в своём окне.
    RunAs,
    /// Без повышения, окно скрыто: только тесты на подставном менеджере.
    #[cfg(test)]
    Hidden,
}

/// Скрипт обёртки для `cmd` (`cmd[0]` — программа).
pub fn launch_script(cmd: &[OsString], launch: Launch) -> Result<String, PkgError> {
    let (prog, args) = cmd
        .split_first()
        .ok_or_else(|| PkgError::Spawn("empty argv".into()))?;
    let prog = prog
        .to_str()
        .ok_or_else(|| PkgError::BadId(prog.to_string_lossy().into()))?;
    let args = args
        .iter()
        .map(|a| {
            a.to_str()
                .ok_or_else(|| PkgError::BadId(a.to_string_lossy().into()))
                .and_then(win_arg)
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let how = match launch {
        Launch::RunAs => "$si.Verb = 'runas'",
        #[cfg(test)]
        Launch::Hidden => "$si.WindowStyle = 'Hidden'",
    };
    Ok(format!(
        "$ErrorActionPreference = 'Stop'\n\
         try {{ [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false }} catch {{}}\n\
         $si = New-Object System.Diagnostics.ProcessStartInfo\n\
         $si.FileName = {prog}\n\
         $si.Arguments = {args}\n\
         $si.UseShellExecute = $true\n\
         {how}\n\
         try {{ $p = [System.Diagnostics.Process]::Start($si) }} catch {{\n\
         \x20 $e = $_.Exception; $c = 0\n\
         \x20 while ($e) {{ if ($e -is [System.ComponentModel.Win32Exception]) {{ $c = $e.NativeErrorCode; break }}; $e = $e.InnerException }}\n\
         \x20 if ($c -eq 1223) {{ '{MARKER} declined' }} else {{ '{MARKER} failed ' + $_.Exception.Message }}\n\
         \x20 exit 1\n\
         }}\n\
         if ($null -eq $p) {{ '{MARKER} failed no process'; exit 1 }}\n\
         '{MARKER} started ' + $p.Id\n\
         $p.WaitForExit()\n\
         '{MARKER} exit ' + $p.ExitCode\n\
         exit 0\n",
        prog = ps_quote(prog),
        args = ps_quote(&args),
    ))
}

/// Итог обёртки по маркерам.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Отказ в UAC.
    Declined,
    /// Не запустилось (текст исключения).
    Failed(String),
    /// Процесс завершился с кодом.
    Exit(u32),
    /// Маркеров нет (обёртка упала до них).
    Unknown,
}

/// Итог по выводу обёртки: последний маркер-итог.
pub fn outcome(lines: &[String]) -> Outcome {
    lines
        .iter()
        .rev()
        .find_map(|l| {
            let rest = l.trim().strip_prefix(MARKER)?.trim();
            let (word, arg) = rest.split_once(' ').unwrap_or((rest, ""));
            match word {
                "declined" => Some(Outcome::Declined),
                "failed" => Some(Outcome::Failed(arg.trim().to_owned())),
                // Код — u32 как у Windows; .NET печатает его со знаком.
                "exit" => arg
                    .trim()
                    .parse::<i32>()
                    .ok()
                    .map(|c| Outcome::Exit(c.cast_unsigned())),
                _ => None,
            }
        })
        .unwrap_or(Outcome::Unknown)
}

#[cfg(windows)]
pub(crate) use run::run;
#[cfg(all(windows, test))]
pub(crate) use run::run_with_launch;

#[cfg(windows)]
mod run {
    use std::collections::VecDeque;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use super::{Launch, Outcome, launch_script, outcome};
    use crate::api::{ActionPlan, Elevation, LineSink, PkgError, RunCx};
    use crate::choco::LogFilter;
    use crate::decode::{Decoder, LineSplitter};
    use crate::detect::{SystemProbe, windows_powershell};
    use crate::proc::{ActionDone, Mode, run_with, system_decoder, tail};
    use crate::scoop::powershell_argv;

    /// Шаг опроса журнала.
    const POLL: Duration = Duration::from_millis(250);
    /// Строк журнала для хвоста ошибки.
    const KEEP: usize = 50;

    /// Новое в журнале с момента создания: построчно, без `DEBUG`. Журнал
    /// пересоздан (стал короче) — читается с начала.
    struct LogTail {
        path: PathBuf,
        pos: u64,
        split: LineSplitter,
        filter: LogFilter,
        decoder: Decoder,
    }

    impl LogTail {
        fn at_end(path: PathBuf) -> LogTail {
            let pos = std::fs::metadata(&path).map_or(0, |m| m.len());
            LogTail {
                path,
                pos,
                split: LineSplitter::default(),
                filter: LogFilter::default(),
                decoder: system_decoder(),
            }
        }

        fn poll(&mut self, out: &mut impl FnMut(&str)) {
            let Ok(mut f) = std::fs::File::open(&self.path) else {
                return;
            };
            let len = f.metadata().map_or(0, |m| m.len());
            if len < self.pos {
                self.pos = 0;
            }
            if len == self.pos || f.seek(SeekFrom::Start(self.pos)).is_err() {
                return;
            }
            let mut buf = Vec::new();
            if let Ok(n) = f.take(len - self.pos).read_to_end(&mut buf) {
                self.pos += n as u64;
            }
            let (decoder, filter) = (&self.decoder, &mut self.filter);
            self.split.push(&buf, &mut |raw: &[u8]| {
                let line = decoder.line(raw);
                if let Some(text) = filter.line(&line) {
                    out(text);
                }
            });
        }
    }

    /// Действие с повышением: UAC и окно choco, ход — по журналу `log`.
    pub(crate) fn run(
        plan: &ActionPlan,
        log: Option<&Path>,
        cx: &RunCx,
    ) -> Result<ActionDone, PkgError> {
        run_with_launch(plan, log, cx, Launch::RunAs)
    }

    pub(crate) fn run_with_launch(
        plan: &ActionPlan,
        log: Option<&Path>,
        cx: &RunCx,
        launch: Launch,
    ) -> Result<ActionDone, PkgError> {
        let script = launch_script(&plan.argv, launch)?;
        let argv = powershell_argv(&windows_powershell(&SystemProbe), &script);
        let kept: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        if let Some(t) = &cx.tape {
            t.tape.head(&t.head);
        }
        let tail_thread = log.map(|log| {
            let mut t = LogTail::at_end(log.to_owned());
            let (kept, stop, sink) = (kept.clone(), stop.clone(), cx.lines.clone());
            let tape = cx.tape.clone();
            std::thread::Builder::new()
                .name("pkg.choco-log".into())
                .spawn(move || {
                    let mut emit = |line: &str| {
                        if let Some(t) = &tape {
                            t.tape.line(line);
                        }
                        if let Some(s) = &sink {
                            s(line);
                        }
                        let mut k = kept.lock().unwrap_or_else(PoisonError::into_inner);
                        if k.len() == KEEP {
                            k.pop_front();
                        }
                        k.push_back(line.to_owned());
                    };
                    loop {
                        let last = stop.load(Ordering::Acquire);
                        t.poll(&mut emit);
                        if last {
                            break;
                        }
                        std::thread::sleep(POLL);
                    }
                })
        });
        // Строки обёртки — маркеры, не для ленты.
        let wrapper = RunCx {
            cancel: cx.cancel.clone(),
            timeout: None,
            lines: None::<LineSink>,
            tape: None,
        };
        let res = run_with(&argv, &wrapper, system_decoder(), Mode::Action);
        stop.store(true, Ordering::Release);
        if let Some(Ok(t)) = tail_thread {
            let _ = t.join();
        }
        if let (Some(t), Err(e)) = (&cx.tape, &res) {
            t.tape.exit(Err(e));
        }
        let (code, lines) = res?;
        let kept: Vec<String> = kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let outcome = outcome(&lines);
        if let Some(t) = &cx.tape {
            t.tape.exit(match outcome {
                Outcome::Exit(c) => Ok(c),
                _ => Ok(code),
            });
        }
        match outcome {
            Outcome::Declined => Err(PkgError::Elevation(Elevation::Declined)),
            Outcome::Failed(msg) => Err(PkgError::Spawn(msg.into())),
            Outcome::Exit(c) if plan.success_codes.contains(&c) => Ok(ActionDone::Ok),
            Outcome::Exit(c) if plan.reboot_codes.contains(&c) => Ok(ActionDone::Reboot),
            Outcome::Exit(c) => Err(PkgError::Exit {
                code: c,
                tail: tail(&kept),
            }),
            Outcome::Unknown => Err(PkgError::Exit {
                code,
                tail: tail(&lines),
            }),
        }
    }
}
