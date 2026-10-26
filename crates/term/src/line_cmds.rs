//! Команды строки ввода для истории и комплита (S6-16, S6-17): Alt+F8, Ctrl+E/Ctrl+X,
//! Tab, ghost-текст и список над cmdline (`cmdline.menu.*`).

use std::path::PathBuf;

use urfin_core::{CmdError, Command, Cx, Lane, Runtime};

use crate::TermState;
use crate::complete::{self, DirCache, Query};
use crate::menu::{Menu, MenuKind, ROWS};
use crate::quote::Style;

/// Команда — про список, историю или комплит: выполнить здесь. `None` — обычная
/// правка строки (тогда список комплита закрывается, листание истории сбрасывается).
pub(crate) fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    let id = cmd.id.as_str();
    let a = &cmd.args;
    let want = crate::manager::session_cwd(&cx.state.active_tab().loc);
    let st = cx.state.ext.get_mut::<TermState>()?;
    // Открыт список истории: набор и Backspace — в его фильтр.
    let history_open = st.menu.as_ref().is_some_and(Menu::is_history);
    match id {
        "cmdline.insert" | "cmdline.backspace" if history_open => {
            if let Some(Menu {
                kind: MenuKind::History { filter, .. },
                ..
            }) = st.menu.as_mut()
            {
                if id == "cmdline.insert" {
                    filter.push_str(&crate::printable(a.str("text").unwrap_or_default()));
                } else {
                    filter.pop();
                }
            }
            refilter(st);
            return Some(Ok(()));
        }
        "cmdline.history" => {
            let shell = st.shell_key().unwrap_or_default();
            st.menu = Some(Menu::history(&st.history, &shell, "", false));
            st.hist_pos = None;
            return Some(Ok(()));
        }
        "cmdline.history.prev" | "cmdline.history.next" => {
            st.menu = None;
            browse(st, id == "cmdline.history.prev");
            return Some(Ok(()));
        }
        "cmdline.complete_native" => {
            st.menu = None;
            st.hist_pos = None;
            return Some(native_handoff(cx));
        }
        "cmdline.complete" => {
            st.hist_pos = None;
            complete_tab(cx, want);
            return Some(Ok(()));
        }
        id if id.starts_with("cmdline.menu.") => return Some(menu_cmd(cx, id, a.bool("exec"))),
        // → и End в конце строки принимают ghost (S6 §6.2).
        "cmdline.cursor.right" | "cmdline.end" if st.menu.is_none() => {
            if let Some(g) = st.ghost().map(str::to_owned) {
                st.line.insert(&g);
                st.hist_pos = None;
                return Some(Ok(()));
            }
        }
        _ => {}
    }
    // Другая правка: список комплита закрывается с выбранным вариантом, история — тоже.
    if !matches!(id, "cmdline.exec") {
        st.menu = None;
    }
    st.hist_pos = None;
    None
}

fn refilter(st: &mut TermState) {
    let TermState { menu, history, .. } = st;
    if let Some(m) = menu.as_mut() {
        m.refilter(history);
    }
}

/// Ctrl+E / Ctrl+X (Far): листание истории с префиксным поиском по набранному.
fn browse(st: &mut TermState, back: bool) {
    let Some(shell) = st.shell_key() else {
        return;
    };
    let Some(h) = st.history.get(&shell) else {
        return;
    };
    let (prefix, pos) = st
        .hist_pos
        .clone()
        .unwrap_or_else(|| (st.line.text().to_owned(), h.entries().len()));
    let found = if back {
        h.prev(&prefix, pos)
    } else {
        h.next(&prefix, pos)
    };
    match found {
        Some(i) => {
            let cmd = h.entries()[i].cmd.clone();
            st.line.set(&cmd);
            st.hist_pos = Some((prefix, i));
        }
        // Вперёд за последнюю — набранное.
        None if !back => {
            st.line.set(&prefix);
            st.hist_pos = None;
        }
        None => {}
    }
}

/// Правила кавычек и каталог активного сеанса (иначе — каталог панели).
fn shell_ctx(st: &TermState, want: Option<PathBuf>) -> (Style, PathBuf) {
    let slot = st.sessions.active();
    let style = slot
        .and_then(|s| st.sessions.shells().iter().find(|k| k.id == s.shell))
        .map_or(Style::Cmd, |k| Style::of(&k.kind));
    let cwd = slot
        .and_then(|s| s.cwd.clone())
        .or(want)
        .unwrap_or_else(|| PathBuf::from("."));
    (style, cwd)
}

/// Tab: открыт список комплита — следующий вариант; иначе собрать варианты (листинг
/// каталога — с диска на полосе Interactive, затем из кэша).
fn complete_tab(cx: &mut Cx<'_>, want: Option<PathBuf>) {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    if let Some(m) = st.menu.as_mut().filter(|m| !m.is_history()) {
        m.step(1);
        apply_selected(st);
        return;
    }
    st.menu = None;
    let (style, cwd) = shell_ctx(st, want);
    let line = st.line.text().to_owned();
    let cursor = st.line.cursor();
    ask_native(st, rt, &cwd, &line, cursor);
    let (_, word) = crate::quote::word_at(&line, cursor, style);
    let pq = complete::path_query(&word.text, &cwd);
    let cached = st.dir_cache.as_ref().is_some_and(|c| c.dir == pq.dir);
    if !cached {
        st.complete_wait = Some((line, cursor));
        let dir = pq.dir;
        rt.spawn(Lane::Interactive, "term.complete", rt.token(), move |t| {
            let names = complete::read_dir(&dir);
            let _ = t.sink.send(
                urfin_core::msg("term.complete", move |cx| {
                    listed(cx, DirCache { dir, names });
                }),
                &t.cancel,
            );
        });
        return;
    }
    run_complete(st, style, &cwd);
}

/// Листинг готов: в кэш; строка не менялась с Tab — дополнить.
fn listed(cx: &mut Cx<'_>, cache: DirCache) {
    let want = crate::manager::session_cwd(&cx.state.active_tab().loc);
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    st.dir_cache = Some(cache);
    let Some((line, cursor)) = st.complete_wait.take() else {
        return;
    };
    if st.line.text() != line || st.line.cursor() != cursor {
        return;
    }
    let (style, cwd) = shell_ctx(st, want);
    run_complete(st, style, &cwd);
}

fn run_complete(st: &mut TermState, style: Style, cwd: &std::path::Path) {
    let line = st.line.text().to_owned();
    let cursor = st.line.cursor();
    let cands = own_cands(st, style, cwd, &line, cursor);
    show(st, &line, cursor, cands);
}

/// Свои источники (S6-17) для строки и курсора.
fn own_cands(
    st: &TermState,
    style: Style,
    cwd: &std::path::Path,
    line: &str,
    cursor: usize,
) -> Vec<complete::Candidate> {
    let shell = st.shell_key().unwrap_or_default();
    let names = st
        .dir_cache
        .as_ref()
        .map(|c| c.names.as_slice())
        .unwrap_or_default();
    complete::candidates(
        &Query {
            line,
            cursor,
            style,
            cwd,
        },
        names,
        st.path_cmds.as_ref(),
        st.history.get(&shell),
    )
}

/// Один вариант — сразу в строку, несколько — список над cmdline.
fn show(st: &mut TermState, line: &str, cursor: usize, cands: Vec<complete::Candidate>) {
    match cands.len() {
        0 => {}
        1 => {
            let (text, cur) = complete::apply(line, &cands[0]);
            st.line.set_at(&text, cur);
        }
        _ => {
            st.menu = Some(Menu::complete(line, cursor, cands));
            apply_selected(st);
        }
    }
}

/// Сеанс PowerShell и сервер включён: спросить `TabExpansion2` (S6-18). Ответ придёт
/// сообщением; опоздавший (дольше `complete.timeout_ms`) отбрасывается.
fn ask_native(st: &mut TermState, rt: &Runtime, cwd: &std::path::Path, line: &str, cursor: usize) {
    if !st.config.complete.pwsh_server || st.pwsh_off {
        return;
    }
    let Some(slot) = st.sessions.active() else {
        return;
    };
    let Some(spec) = st.sessions.shells().iter().find(|k| k.id == slot.shell) else {
        return;
    };
    if !matches!(
        spec.kind,
        crate::shells::ShellKind::Pwsh | crate::shells::ShellKind::WinPowerShell
    ) {
        return;
    }
    let program = spec.program.clone();
    if st
        .pwsh
        .as_ref()
        .is_none_or(|(p, s)| *p != program || !s.alive())
    {
        let sink = rt.sink();
        let dead = rt.sink();
        let server = crate::pwsh_server::Server::start(
            rt,
            &program,
            move |reply| {
                let _ = sink.send(
                    urfin_core::msg("term.native", move |cx| native_reply(cx, reply)),
                    &urfin_core::CancelToken::new(),
                );
            },
            move |failed_start| {
                let _ = dead.send(
                    urfin_core::msg("term.native.dead", move |cx| {
                        if let Some(st) = cx.state.ext.get_mut::<TermState>() {
                            st.pwsh = None;
                            // Не стартовал (нет pwsh, CLM) — до перезапуска не пробовать.
                            st.pwsh_off |= failed_start;
                        }
                    }),
                    &urfin_core::CancelToken::new(),
                );
            },
        );
        st.pwsh = Some((program, server));
    }
    st.next_native += 1;
    let id = st.next_native;
    let req = crate::pwsh_server::Req {
        id,
        cwd: cwd.to_path_buf(),
        line: line.to_owned(),
        cursor,
    };
    if st.pwsh.as_ref().is_some_and(|(_, s)| s.ask(req)) {
        let timeout = std::time::Duration::from_millis(u64::from(st.config.complete.timeout_ms));
        st.native_wait = Some(NativeWait {
            id,
            line: line.to_owned(),
            cursor,
            cwd: cwd.to_path_buf(),
            deadline: std::time::Instant::now() + timeout,
        });
    }
}

/// `cmdline.complete_native` (Shift+Tab, S6 §6.2): строка — в строку ввода самого shell
/// (сперва её очистка: хук Ctrl+Alt+Shift+F11 или Esc у cmd), затем Tab — дальше
/// работает родной комплит (`PSReadLine` `MenuComplete`, readline, zsh, fish). До
/// следующего промпта — вид «экран», ввод в PTY; Enter выполняет, команда попадает в
/// историю через `633;E`.
fn native_handoff(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    if st.line.is_empty() {
        return Ok(());
    }
    let Some(slot) = st.sessions.active() else {
        return Ok(());
    };
    let cmd = st
        .sessions
        .shells()
        .iter()
        .find(|k| k.id == slot.shell)
        .is_none_or(|k| matches!(k.kind, crate::shells::ShellKind::Cmd));
    let mut bytes: Vec<u8> = if cmd {
        b"\x1b".to_vec()
    } else if slot.integrated && slot.hooks {
        b"\x1b[23;8~".to_vec()
    } else {
        Vec::new()
    };
    bytes.extend_from_slice(st.line.text().replace(['\r', '\n'], " ").as_bytes());
    bytes.push(b'\t');
    st.sessions.write(&st.config, rt, bytes, false)?;
    st.line.take();
    st.sessions.set_raw();
    crate::panels(cx, false);
    Ok(())
}

/// Ожидаемый ответ сервера комплита.
#[derive(Clone, Debug)]
pub(crate) struct NativeWait {
    id: u64,
    line: String,
    cursor: usize,
    cwd: PathBuf,
    deadline: std::time::Instant,
}

/// Ответ `TabExpansion2`: вовремя и строка не правилась — родные варианты первыми,
/// затем свои (S6 §6.2: native > пути > PATH > история), без повторов.
fn native_reply(cx: &mut Cx<'_>, reply: crate::pwsh_server::Reply) {
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return;
    };
    let Some(w) = st.native_wait.take() else {
        return;
    };
    if w.id != reply.id || std::time::Instant::now() > w.deadline {
        return;
    }
    let own_menu = st
        .menu
        .as_ref()
        .is_some_and(|m| matches!(&m.kind, MenuKind::Complete { base, .. } if *base == w.line));
    if !own_menu && st.line.text() != w.line {
        return;
    }
    let style = Style::Pwsh;
    let mut cands = reply.cands;
    for c in own_cands(st, style, &w.cwd, &w.line, w.cursor) {
        if !cands.iter().any(|n| n.insert == c.insert) {
            cands.push(c);
        }
    }
    st.menu = None;
    st.line.set_at(&w.line, w.cursor);
    show(st, &w.line, w.cursor, cands);
}

/// Выбранный вариант комплита — в строку (поверх строки до Tab).
fn apply_selected(st: &mut TermState) {
    let Some(m) = st.menu.as_ref() else {
        return;
    };
    let MenuKind::Complete { base, .. } = &m.kind else {
        return;
    };
    if let Some(c) = m.cands.get(m.sel) {
        let (text, cur) = complete::apply(base, c);
        st.line.set_at(&text, cur);
    }
}

/// `cmdline.menu.*`.
fn menu_cmd(cx: &mut Cx<'_>, id: &str, exec: Option<bool>) -> Result<(), CmdError> {
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    let Some(m) = st.menu.as_mut() else {
        return Ok(());
    };
    let page = isize::try_from(ROWS - 1).unwrap_or(1);
    match id {
        "cmdline.menu.up" => m.step(-1),
        "cmdline.menu.down" => m.step(1),
        "cmdline.menu.page_up" => m.step(-page),
        "cmdline.menu.page_down" => m.step(page),
        "cmdline.menu.close" => {
            if let Some(Menu {
                kind: MenuKind::Complete { base, cursor },
                ..
            }) = st.menu.take()
            {
                st.line.set_at(&base, cursor);
            }
            return Ok(());
        }
        "cmdline.menu.accept" => {
            let m = st.menu.take();
            if let Some(m) = m.filter(Menu::is_history)
                && let Some(item) = m.item(&st.history, m.sel)
                && let Some((k, i)) = item.at
                && let Some(e) = st.history.get(&k).and_then(|h| h.entries().get(i))
            {
                let cmd = e.cmd.clone();
                st.line.set(&cmd);
                if exec == Some(true) {
                    cx.dispatch(Command::new("cmdline.exec"));
                }
            }
            return Ok(());
        }
        "cmdline.menu.delete" | "cmdline.menu.lock" => {
            let Some((k, i)) = m.is_history().then(|| m.hits.get(m.sel).cloned()).flatten() else {
                return Ok(());
            };
            let Some(h) = st.history.by_shell.get_mut(&k) else {
                return Ok(());
            };
            let changed = if id == "cmdline.menu.delete" {
                h.remove(i)
            } else {
                h.toggle_lock(i)
            };
            if changed {
                st.history.save(rt, &k);
            }
            refilter(st);
            return Ok(());
        }
        "cmdline.menu.scope" => {
            if let MenuKind::History { all, .. } = &mut m.kind {
                *all = !*all;
            }
            m.sel = 0;
            refilter(st);
            return Ok(());
        }
        _ => return Err(crate::not_yet(&Command::new("cmdline.menu.close"))),
    }
    if !m.is_history() {
        apply_selected(st);
    }
    Ok(())
}

/// Скан PATH для комплита команд (полоса Interactive, один раз; дальше — по mtime на
/// каждом Tab в позиции команды не чаще раза в 10 с).
pub(crate) fn scan_path(st: &mut TermState, rt: &Runtime) {
    if st.path_scan {
        return;
    }
    st.path_scan = true;
    let mut cmds = st.path_cmds.clone().unwrap_or_default();
    rt.spawn(Lane::Interactive, "term.path", rt.token(), move |t| {
        let path = std::env::var("PATH").unwrap_or_default();
        cmds.refresh(&path, &complete::pathext());
        let _ = t.sink.send(
            urfin_core::msg("term.path", move |cx| {
                if let Some(st) = cx.state.ext.get_mut::<TermState>() {
                    st.path_cmds = Some(cmds);
                }
            }),
            &t.cancel,
        );
    });
}
