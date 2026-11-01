//! Запуск менеджера (F02 §2.3, §3.1) поверх `term::proc`: пайпы, `CREATE_NO_WINDOW`,
//! stderr вместе с stdout, построчный поток (`decode.rs`) в `RunCx::lines`, таймаут и
//! отмена убивают всё дерево. Вызывать не на UI-потоке: функции ждут конца процесса.
//!
//! Запрос (`run_query`) держит весь вывод и после выхода процесса добивает дерево:
//! хвосты запроса не нужны. Действие (`run_action`) держит хвост вывода, дерево
//! живёт: инсталлятор мог запустить программу. Если она держит вывод открытым, чтение
//! бросается через [`GRACE`] после выхода процесса.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use compact_str::CompactString;
use urfin_term::proc::{PipedChild, PipedFlags, spawn_piped};

use crate::api::{ActionPlan, LineSink, PkgError, RunCx};
use crate::decode::{Decoder, LineSplitter};

/// Шаг проверки отмены и таймаута.
const TICK: Duration = Duration::from_millis(20);
/// Действие: сколько ждать конца вывода после выхода процесса.
pub const GRACE: Duration = Duration::from_secs(2);
/// Строк в хвосте ошибки.
const TAIL_LINES: usize = 5;
/// Строк вывода, которые держит действие.
const ACTION_KEEP: usize = 50;

/// Вывод запроса.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub code: u32,
    /// Строки после `decode.rs`.
    pub lines: Vec<String>,
}

impl Output {
    /// Код 0 — вывод; иначе `PkgError::Exit` с хвостом.
    pub fn ok(self) -> Result<Output, PkgError> {
        if self.code == 0 {
            Ok(self)
        } else {
            Err(PkgError::Exit {
                code: self.code,
                tail: tail(&self.lines),
            })
        }
    }
}

/// Итог успешного действия.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionDone {
    Ok,
    /// Код из `reboot_codes`: нужна перезагрузка.
    Reboot,
}

/// Последние непустые строки — для ошибки.
pub fn tail(lines: &[String]) -> CompactString {
    let mut last: Vec<&str> = lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(TAIL_LINES)
        .collect();
    last.reverse();
    last.join("\n").into()
}

/// OEM-кодовая страница системы (`Nls\CodePage\OEMCP`); не прочиталась — 437.
pub fn system_decoder() -> Decoder {
    static CP: OnceLock<u16> = OnceLock::new();
    Decoder::new(*CP.get_or_init(|| {
        windows_registry::LOCAL_MACHINE
            .open(r"SYSTEM\CurrentControlSet\Control\Nls\CodePage")
            .and_then(|k| k.get_string("OEMCP"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(437)
    }))
}

/// Запрос: `argv` до конца, весь вывод. Таймаут — `cx.timeout`.
pub fn run_query(argv: &[OsString], cx: &RunCx) -> Result<Output, PkgError> {
    run_with(argv, cx, system_decoder(), Mode::Query).map(|(code, lines)| Output { code, lines })
}

/// Действие без повышения прав: строки — в `cx.lines`, код — по `success_codes` и
/// `reboot_codes` плана. Повышенные (`plan.elevate`, choco) идут через UAC в своём
/// окне (F02 §8), не сюда.
pub fn run_action(plan: &ActionPlan, cx: &RunCx) -> Result<ActionDone, PkgError> {
    if plan.elevate {
        return Err(PkgError::Spawn("elevated plan needs UAC".into()));
    }
    let (code, lines) = run_with(&plan.argv, cx, system_decoder(), Mode::Action)?;
    if plan.success_codes.contains(&code) {
        Ok(ActionDone::Ok)
    } else if plan.reboot_codes.contains(&code) {
        Ok(ActionDone::Reboot)
    } else {
        Err(PkgError::Exit {
            code,
            tail: tail(&lines),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Query,
    Action,
}

fn kill(child: &PipedChild) {
    if let Err(e) = child.kill_tree() {
        tracing::warn!(pid = child.pid(), error = %e, "pkg: kill_tree failed");
    }
}

/// Читатель вывода: строки — в приёмник и в канал; читает до EOF, даже если канал
/// закрыт (иначе ребёнок встанет на полном пайпе).
fn read_lines(
    mut out: std::fs::File,
    decoder: Decoder,
    sink: Option<&LineSink>,
    tx: &Sender<String>,
) {
    let mut split = LineSplitter::default();
    let mut emit = |raw: &[u8]| {
        let line = decoder.line(raw);
        if let Some(s) = sink {
            s(&line);
        }
        let _ = tx.send(line);
    };
    let mut buf = vec![0u8; 8192];
    loop {
        match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => split.push(&buf[..n], &mut emit),
        }
    }
    split.finish(&mut emit);
}

pub(crate) fn run_with(
    argv: &[OsString],
    cx: &RunCx,
    decoder: Decoder,
    mode: Mode,
) -> Result<(u32, Vec<String>), PkgError> {
    let flags = PipedFlags {
        no_window: true,
        merge_stderr: true,
        kill_on_drop: mode == Mode::Query,
    };
    let mut child = spawn_piped(argv, None, &[], flags).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PkgError::NotFound,
        _ => PkgError::Spawn(e.to_string().into()),
    })?;
    let stdout = child.stdout.take().expect("stdout pipe");
    let (tx, rx) = mpsc::channel::<String>();
    let sink = cx.lines.clone();
    let reader = std::thread::Builder::new()
        .name("pkg.proc-out".into())
        .spawn(move || read_lines(stdout, decoder, sink.as_ref(), &tx))
        .map_err(|e| {
            kill(&child);
            PkgError::Spawn(e.to_string().into())
        })?;
    let deadline = cx.timeout.map(|t| Instant::now() + t);
    let keep = match mode {
        Mode::Query => usize::MAX,
        Mode::Action => ACTION_KEEP,
    };
    let mut lines = VecDeque::new();
    let mut code = None;
    let mut exited_at = None;
    loop {
        let stop = if cx.cancel.is_cancelled() {
            Some(PkgError::Cancelled)
        } else if deadline.is_some_and(|d| Instant::now() >= d) {
            Some(PkgError::Timeout)
        } else {
            None
        };
        if let Some(e) = stop {
            // Дерево мертво — пайп закрыт, читатель выходит сам.
            kill(&child);
            let _ = reader.join();
            return Err(e);
        }
        match rx.recv_timeout(TICK) {
            Ok(line) => {
                if lines.len() == keep {
                    lines.pop_front();
                }
                lines.push_back(line);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Вывод закрыт всем деревом: остаётся код выхода.
            Err(RecvTimeoutError::Disconnected) => {
                let Some(c) = code.or_else(|| child.wait_timeout(Some(TICK))) else {
                    continue;
                };
                let _ = reader.join();
                return Ok((c, lines.into()));
            }
        }
        if code.is_none() {
            code = child.wait_timeout(Some(Duration::ZERO));
            if code.is_some() {
                exited_at = Some(Instant::now());
                if mode == Mode::Query {
                    kill(&child);
                }
            }
        }
        if let (Some(c), Some(at)) = (code, exited_at)
            && at.elapsed() >= GRACE
        {
            // Вывод держит потомок вне нашего ожидания (запущенная программа):
            // читатель дочитает без нас.
            tracing::debug!(pid = child.pid(), "pkg: output kept open after exit");
            drop(reader);
            return Ok((c, lines.into()));
        }
    }
}

#[cfg(test)]
#[path = "proc_tests.rs"]
mod tests;
