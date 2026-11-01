//! `term.exec {cmd, cwd?}` (S6 §9): строка — командой в сеансе терминала, как Enter в
//! cmdline. Её шлют вьювер (Alt+F3 с шаблоном `{path}`, S7 §3.1) и редактор
//! (консольный внешний редактор, S7 §5.6).
//!
//! Активный сеанс занят (команда идёт, есть процессы) — новый сеанс shell по
//! умолчанию. Команда уходит, когда сеанс у промпта (свежий cmd рисует `D` уже на
//! первом промпте); каталог — `cwd` или каталог активного таба (ленивая синхронизация
//! S6 §5.1). На время команды панели спрятаны (как Ctrl+O), фокус — в терминал
//! (S7 §5.6); по концу панели возвращаются, вызов из вьювера или редактора — фокус
//! туда. Строка служебная: в историю команд (S6-16) не пишется.
//!
//! Конец команды: с интеграцией — `D` после промптов cd; без неё маркеров нет — по
//! процессам job сеанса (`ActiveProcesses`): потомок shell появился и все вышли; не
//! появился за [`NO_CHILD`] — команда встроенная, кончилась. Таймаут бездействия не
//! годится: консольный редактор ждёт клавиш сколько угодно. Выход процесса сеанса —
//! конец в обоих случаях.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use urfin_core::{Args, CmdError, Cx, Focus, UiEvent};

use crate::TermState;
use crate::manager::{self, ExecReady};
use crate::session::SessionId;
use crate::shells::ShellKind;

/// Команда ждёт сеанса у промпта.
#[derive(Debug)]
pub(crate) struct Request {
    line: String,
    cwd: Option<PathBuf>,
    /// Фокус до вызова.
    back: Focus,
}

/// Без интеграции: потомок shell не появился за это время — команда встроенная
/// (`echo`, `type`), кончилась.
const NO_CHILD: Duration = Duration::from_millis(1500);

/// Шаг опроса процессов сеанса без интеграции.
const PROC_POLL: Duration = Duration::from_millis(100);

/// Команда идёт в сеансе `session`: панели спрятаны, по её концу — назад.
#[derive(Debug)]
pub(crate) struct Back {
    pub(crate) session: SessionId,
    /// Фокус по концу: вьювер или редактор; `None` — не трогать.
    focus: Option<Focus>,
    /// Сколько `D` ещё до конца самой команды (промпты cd перед ней).
    skip: u32,
    /// Без интеграции — конец по процессам job: момент отправки и «потомок был».
    procs: Option<(Instant, bool)>,
}

/// `term.exec`.
pub(crate) fn handle(cx: &mut Cx<'_>, a: &Args) -> Result<(), CmdError> {
    let line = a
        .str("cmd")
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| CmdError::Arg {
            name: "cmd".into(),
            msg: "empty command".into(),
        })?
        .to_owned();
    let cwd = a
        .str("cwd")
        .filter(|c| !c.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| manager::session_cwd(&cx.state.active_tab().loc));
    let back = cx.state.focus;
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    let free = st
        .sessions
        .active()
        .is_some_and(|s| !s.running && !s.raw && !s.busy());
    if !free {
        st.sessions.open(&st.config, rt, None, false, cwd.clone())?;
    }
    st.exec = Some(Request { line, cwd, back });
    pump(cx)
}

/// Ждущая команда — в сеанс, если он у промпта; фокус — в терминал.
pub(crate) fn pump(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    if st.exec.is_none() {
        return Ok(());
    }
    match st.sessions.exec_ready(&st.config, rt) {
        ExecReady::Wait => return Ok(()),
        ExecReady::Gone => {
            st.exec = None;
            return Ok(());
        }
        ExecReady::Go => {}
    }
    let (Some(req), Some(session)) = (st.exec.take(), st.sessions.active().map(|s| s.id)) else {
        return Ok(());
    };
    let kind = st.sessions.active_spec().map(|s| s.kind.clone());
    let integrated = st.sessions.active().is_some_and(|s| s.integrated);
    let text = shell_line(&req.line, kind.as_ref());
    crate::send_line(st, rt, &text, req.cwd)?;
    let skip = st.sessions.cd_prompts;
    st.exec_back = Some(Back {
        session,
        focus: matches!(req.back, Focus::Viewer | Focus::Editor).then_some(req.back),
        skip,
        procs: (!integrated).then(|| (Instant::now(), false)),
    });
    if !integrated {
        rt.waker().wake_after(PROC_POLL);
    }
    // Терминал виден сразу (как Ctrl+O): за показанными панелями смена вида
    // (`refocus`) увела бы фокус в панель. Автопоказ не нужен.
    st.submitted = None;
    st.auto_hidden = true;
    cx.state.focus = Focus::Terminal;
    cx.ui(UiEvent::FocusChanged(Focus::Terminal));
    crate::panels(cx, false);
    Ok(())
}

/// Сеанс без интеграции: конец команды по процессам job (каждый pump).
pub(crate) fn poll(cx: &mut Cx<'_>) {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    let Some(back) = st.exec_back.as_mut() else {
        return;
    };
    let Some((since, seen)) = back.procs.as_mut() else {
        return;
    };
    let id = back.session;
    let Some(slot) = st.sessions.slots().iter().find(|s| s.id == id) else {
        st.exec_back = None;
        return;
    };
    let busy = slot.busy();
    *seen |= busy;
    let done = if *seen {
        !busy
    } else {
        since.elapsed() >= NO_CHILD
    };
    if !done {
        rt.waker().wake_after(PROC_POLL);
        return;
    }
    st.sessions.mark_idle(id);
    end(cx);
}

/// Строка в синтаксисе shell: PowerShell строку в кавычках считает выражением —
/// запуск программы по пути в кавычках — через `&`.
pub(crate) fn shell_line(line: &str, kind: Option<&ShellKind>) -> String {
    let pwsh = matches!(kind, Some(ShellKind::Pwsh | ShellKind::WinPowerShell));
    if pwsh && line.starts_with(['"', '\'']) {
        format!("& {line}")
    } else {
        line.to_owned()
    }
}

/// Сеанс `id` закончил команду (`D`) или вышел (`exited`): конец команды `term.exec`.
/// Без интеграции `D` не ждём (конец — [`poll`]).
pub(crate) fn finished(cx: &mut Cx<'_>, id: SessionId, exited: bool) {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    let Some(back) = st.exec_back.as_mut().filter(|b| b.session == id) else {
        return;
    };
    if !exited {
        if back.procs.is_some() {
            return;
        }
        if back.skip > 0 {
            back.skip -= 1;
            return;
        }
    }
    end(cx);
}

/// Конец команды `term.exec`: панели — назад (вызов из панели — по
/// `term.return_to_panels`), фокус — во вьювер или редактор. Пользователь сам ушёл в
/// вопрос, оверлей или другой экран — фокус не трогать.
fn end(cx: &mut Cx<'_>) {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    let Some(back) = st.exec_back.take() else {
        return;
    };
    let show =
        std::mem::take(&mut st.auto_hidden) && (back.focus.is_some() || st.config.return_to_panels);
    // Фокус задан явно: смена вида терминала (конец команды) его не переоценивает.
    st.screen = st.screen_view();
    if show {
        crate::panels(cx, true);
    }
    let Some(focus) = back.focus else {
        return;
    };
    if !matches!(
        cx.state.focus,
        Focus::Terminal | Focus::Cmdline | Focus::Panel
    ) {
        return;
    }
    cx.state.focus = focus;
    cx.ui(UiEvent::FocusChanged(focus));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_program_in_powershell() {
        let ps = ShellKind::Pwsh;
        assert_eq!(
            shell_line(r#""C:\Program Files\x.exe" "a b""#, Some(&ps)),
            r#"& "C:\Program Files\x.exe" "a b""#
        );
        assert_eq!(shell_line(r#"vim "a b""#, Some(&ps)), r#"vim "a b""#);
        assert_eq!(
            shell_line(r#""C:\x.exe""#, Some(&ShellKind::Cmd)),
            r#""C:\x.exe""#
        );
        assert_eq!(shell_line("vim a", None), "vim a");
    }
}
