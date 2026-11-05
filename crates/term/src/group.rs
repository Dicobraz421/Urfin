//! Мультивыполнение SSH (F03 §3.4, `screen-sshmulti`): ввод экрана группы — во все PTY
//! её сеансов. Модель — `urfin_core::net::Group`, команды `ssh.group.*` зовёт
//! `urfin-net`; здесь — куда идут клавиши и байты. Пока экран группы показан,
//! `term.input`, `term.key` и `term.paste` идут сюда: у колонки сеансов — ↑↓, Пробел,
//! Enter; иначе — через задержку первого Enter в `TermApi::broadcast`.

use std::time::{SystemTime, UNIX_EPOCH};

use urfin_core::net::{Group, NetModel, in_ssh_group, multi_exec_allowed};
use urfin_core::{AppState, CmdError, Command, Cx};

use crate::manager::{key_bytes, tr};
use crate::vt::input;
use crate::{TermState, panels};

fn denied() -> CmdError {
    CmdError::Rejected(tr("term-group-denied", &[]).into())
}

fn group_mut<'a>(cx: &'a mut Cx<'_>) -> Option<(&'a mut Group, &'a mut u64, usize)> {
    let m = cx.state.ext.get_mut::<NetModel>()?;
    let n = m.terms.len();
    Some((m.group.as_mut()?, &mut m.rev, n))
}

fn group(s: &AppState) -> Option<&Group> {
    s.ext.get::<NetModel>()?.group.as_ref()
}

/// `ssh.group.edit` (Ctrl+Alt+G): открыть экран группы (сеанс — первый SSH, если активен
/// не SSH); на экране группы — колонка ↔ ввод. `ask` — `[ssh].multi_exec_confirm`.
pub fn edit(cx: &mut Cx<'_>, ask: bool) -> Result<(), CmdError> {
    if !multi_exec_allowed(cx.state) {
        return Err(denied());
    }
    let active = cx
        .state
        .ext
        .get::<TermState>()
        .and_then(|t| t.sessions.active())
        .map(|s| s.origin);
    let m = cx
        .state
        .ext
        .get_mut::<NetModel>()
        .ok_or_else(|| CmdError::Rejected("ssh is not ready".into()))?;
    m.rev += 1;
    match m.group.as_mut() {
        Some(g) if g.on_screen => {
            g.list = !g.list || !g.active();
            return Ok(());
        }
        // Панели показаны (Ctrl+O), группа ждёт: снова её экран.
        Some(g) => g.list = true,
        None => {
            let first = m.terms.first().map(|t| t.id);
            let Some(first) = first else {
                return Err(CmdError::Rejected(tr("term-group-no-ssh", &[]).into()));
            };
            let at = active.and_then(|a| m.terms.iter().position(|t| t.id == a));
            m.group = Some(Group::new(
                ask,
                at.unwrap_or(0),
                active.filter(|_| at.is_none()),
            ));
            if at.is_none() {
                crate::activate(cx, first)?;
            }
        }
    }
    panels(cx, false);
    Ok(())
}

/// `ssh.group.toggle` (Пробел в колонке, клик по строке): сеанс `id` или строка курсора.
pub fn toggle(cx: &mut Cx<'_>, id: Option<u32>) -> Result<(), CmdError> {
    if !multi_exec_allowed(cx.state) {
        return Err(denied());
    }
    let Some(m) = cx.state.ext.get_mut::<NetModel>() else {
        return Ok(());
    };
    let Some(g) = m.group.as_mut() else {
        return Ok(());
    };
    let at = m.terms.get(g.cursor).map(|t| t.id);
    if let Some(id) = id.or(at) {
        g.toggle(id);
        m.rev += 1;
    }
    Ok(())
}

/// `ssh.group.leave` (Esc): в строке подтверждения — «нет», `Ctrl+U` во все сеансы;
/// иначе выход из группы.
pub fn leave(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let Some((g, rev, _)) = group_mut(cx) else {
        return Ok(());
    };
    *rev += 1;
    if g.asking() {
        let bytes = g.reject();
        let ids = g.members.clone();
        return crate::broadcast(cx, &ids, &bytes);
    }
    close(cx);
    Ok(())
}

/// Группы больше нет; локальный сеанс, с которого пришли, снова активен.
fn close(cx: &mut Cx<'_>) {
    let Some(m) = cx.state.ext.get_mut::<NetModel>() else {
        return;
    };
    let back = m.group.take().and_then(|g| g.back);
    m.rev += 1;
    if let Some(b) = back {
        let _ = crate::activate(cx, b);
    }
}

/// Каждый pump: закрытый сеанс уходит из группы; политика запретила на лету или
/// SSH-сеансов не осталось — группы нет.
pub(crate) fn pump(cx: &mut Cx<'_>) {
    let Some(m) = cx.state.ext.get::<NetModel>() else {
        return;
    };
    let Some(g) = m.group.as_ref() else {
        return;
    };
    if m.terms.is_empty() || !multi_exec_allowed(cx.state) {
        close(cx);
        return;
    }
    let gone: Vec<u32> = g
        .members
        .iter()
        .copied()
        .filter(|id| !m.terms.iter().any(|t| t.id == *id))
        .collect();
    if !gone.is_empty()
        && let Some((g, rev, _)) = group_mut(cx)
    {
        for id in gone {
            g.remove(id);
        }
        *rev += 1;
    }
}

/// Ввод на экране группы: `None` — команда идёт обычным путём.
pub(crate) fn route(cx: &mut Cx<'_>, cmd: &Command) -> Option<Result<(), CmdError>> {
    if !in_ssh_group(cx.state)
        || !matches!(cmd.id.as_str(), "term.input" | "term.key" | "term.paste")
    {
        return None;
    }
    // Вставка без текста — сначала буфер у окна, текст вернётся этой же командой.
    if cmd.id.as_str() == "term.paste" && cmd.args.str("text").is_none() {
        return None;
    }
    if group(cx.state)?.list {
        list_input(cx, cmd);
        return Some(Ok(()));
    }
    Some(send(cx, cmd))
}

/// Клавиши у колонки сеансов: в PTY не идёт ничего.
fn list_input(cx: &mut Cx<'_>, cmd: &Command) {
    let space = cmd.id.as_str() == "term.input" && cmd.args.str("text") == Some(" ");
    let key = cmd
        .args
        .str("key")
        .filter(|_| cmd.id.as_str() == "term.key");
    if space {
        let _ = toggle(cx, None);
        return;
    }
    let Some((g, rev, n)) = group_mut(cx) else {
        return;
    };
    let last = n.saturating_sub(1);
    match key {
        Some("up") => g.cursor = g.cursor.min(last).saturating_sub(1),
        Some("down") => g.cursor = (g.cursor + 1).min(last),
        Some("home") => g.cursor = 0,
        Some("end") => g.cursor = last,
        Some("enter") if g.active() => g.list = false,
        _ => return,
    }
    *rev += 1;
}

/// Секунды Unix сейчас.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Ввод — через задержку первого Enter во все сеансы группы. Клавиша кодируется для
/// каждого сеанса его режимами (app cursor, win32-input-mode); группа судит о ней по
/// коду без режимов: Enter в win32-input-mode — не `\r`.
fn send(cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
    let a = &cmd.args;
    let key = a.str("key").filter(|_| cmd.id.as_str() == "term.key");
    let mods = a.str("mods");
    let plain = match (cmd.id.as_str(), key) {
        (_, Some(k)) => {
            key_bytes(k, mods, input::Modes::default()).ok_or_else(|| CmdError::Arg {
                name: "key".into(),
                msg: "unknown key".into(),
            })?
        }
        ("term.paste", _) => {
            let t = a.str("text").unwrap_or_default();
            let t = t.trim_end_matches(['\r', '\n']);
            let term = cx
                .state
                .ext
                .get::<TermState>()
                .map(|st| st.sessions.input_modes())
                .unwrap_or_default();
            let mut out = Vec::with_capacity(t.len() + 12);
            input::paste(t, term, &mut out);
            out
        }
        _ => a.str("text").unwrap_or_default().as_bytes().to_vec(),
    };
    let now = now();
    let Some((g, rev, _)) = group_mut(cx) else {
        return Ok(());
    };
    let out = g.feed(&plain, now);
    let ids = g.members.clone();
    *rev += 1;
    if out.is_empty() {
        return Ok(());
    }
    match key.filter(|_| out == plain) {
        Some(k) => crate::broadcast_with(cx, &ids, |modes| {
            key_bytes(k, mods, modes).unwrap_or_default()
        }),
        None => crate::broadcast(cx, &ids, &out),
    }
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
