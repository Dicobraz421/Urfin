//! `term.exec {cmd, cwd?}` (S6 §9): строка — командой в сеансе терминала, как Enter в
//! cmdline. Её шлют вьювер (Alt+F3 с шаблоном `{path}`, S7 §3.1) и редактор
//! (консольный внешний редактор, S7 §5.6).
//!
//! Активный сеанс занят (команда идёт, есть процессы) — новый сеанс shell по
//! умолчанию. Команда уходит, когда сеанс у промпта (свежий cmd рисует `D` уже на
//! первом промпте); каталог — `cwd` или каталог активного таба (ленивая синхронизация
//! S6 §5.1). Фокус — в терминал (S7 §5.6); вызов из вьювера или редактора — по концу
//! команды (`D` после промптов cd, или выход процесса) фокус возвращается туда.

use std::path::PathBuf;

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

/// Команда идёт в сеансе `session`: по её концу фокус — `focus`.
#[derive(Debug)]
pub(crate) struct Back {
    session: SessionId,
    focus: Focus,
    /// Сколько `D` ещё до конца самой команды (промпты cd перед ней).
    skip: u32,
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
    let text = shell_line(&req.line, kind.as_ref());
    crate::run_line(st, rt, &text, req.cwd)?;
    let skip = st.sessions.cd_prompts;
    st.exec_back = matches!(req.back, Focus::Viewer | Focus::Editor).then_some(Back {
        session,
        focus: req.back,
        skip,
    });
    cx.state.focus = Focus::Terminal;
    cx.ui(UiEvent::FocusChanged(Focus::Terminal));
    Ok(())
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

/// Сеанс `id` закончил команду (`D`) или вышел (`exited`): конец команды `term.exec` —
/// фокус обратно во вьювер или редактор. Пользователь сам ушёл в вопрос, оверлей или
/// другой экран — не трогать.
pub(crate) fn finished(cx: &mut Cx<'_>, id: SessionId, exited: bool) {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    let Some(back) = st.exec_back.as_mut().filter(|b| b.session == id) else {
        return;
    };
    if !exited && back.skip > 0 {
        back.skip -= 1;
        return;
    }
    let focus = back.focus;
    st.exec_back = None;
    if !matches!(
        cx.state.focus,
        Focus::Terminal | Focus::Cmdline | Focus::Panel
    ) {
        return;
    }
    // Фокус задан явно: смена вида терминала (конец команды) его не переоценивает.
    st.screen = st.screen_view();
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
