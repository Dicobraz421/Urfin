//! `panel.*`: курсор, навигация, вид, сортировка, закладки.

use compact_str::CompactString;

use super::{N, SIDE, active_tab_mut, arg, choice, meta, rejected, side_arg, tab_id};
use crate::bus::Handler;
use crate::cmd::{CmdError, Command, CommandMeta, SideRef, ValueKind};
use crate::ids::{Side, TabId};
use crate::loc::Location;
use crate::mask::Mask;
use crate::msg::Cx;
use crate::nav::{self, child_loc, hist_entry, row_of_name};
use crate::services::TabEvent;
use crate::state::{Focus, UiEvent};
use crate::tab::{ListingStatus, PanelMode, Row, SortKey, Tab, ViewMode};

pub(crate) struct PanelHandler;

static COMMANDS: [CommandMeta; 29] = [
    meta("panel.cursor.up", "Cursor up", &[N], true),
    meta("panel.cursor.down", "Cursor down", &[N], true),
    meta("panel.cursor.page_up", "Page up", &[N], true),
    meta("panel.cursor.page_down", "Page down", &[N], true),
    meta("panel.cursor.left", "Column left", &[N], true),
    meta("panel.cursor.right", "Column right", &[N], true),
    meta("panel.cursor.home", "First item", &[], false),
    meta("panel.cursor.end", "Last item", &[], false),
    meta(
        "panel.cursor.to",
        "Go to name",
        &[arg("name", ValueKind::Str, true)],
        false,
    ),
    meta(
        "panel.cursor.match",
        "Find by mask",
        &[
            arg("mask", ValueKind::Str, true),
            arg("dir", ValueKind::Str, false),
        ],
        false,
    ),
    meta(
        "panel.viewport",
        "Viewport",
        &[
            SIDE,
            arg("rows", ValueKind::Int, true),
            arg("cols", ValueKind::Int, false),
            arg("top", ValueKind::Int, false),
        ],
        false,
    ),
    meta("panel.enter", "Open", &[], false),
    meta("panel.up", "Parent folder", &[], false),
    meta("panel.root", "Root folder", &[], false),
    meta("panel.back", "Back", &[], false),
    meta("panel.forward", "Forward", &[], false),
    meta(
        "panel.go",
        "Go to folder",
        &[arg("path", ValueKind::Loc, true), SIDE],
        false,
    ),
    meta(
        "panel.refresh",
        "Refresh",
        &[arg("tab", ValueKind::Int, false)],
        false,
    ),
    meta("panel.switch", "Switch panel", &[SIDE], false),
    meta("panel.swap", "Swap panels", &[], false),
    meta("panel.focus", "Focus panel", &[SIDE], false),
    meta(
        "panel.toggle",
        "Show or hide panel",
        &[arg("side", ValueKind::Str, true)],
        false,
    ),
    meta(
        "panel.sort",
        "Sort",
        &[
            arg("key", ValueKind::Str, true),
            arg("desc", ValueKind::Bool, false),
        ],
        false,
    ),
    meta(
        "panel.view_mode",
        "View mode",
        &[arg("mode", ValueKind::Str, true)],
        false,
    ),
    meta(
        "panel.mode",
        "Panel mode",
        &[arg("mode", ValueKind::Str, true), SIDE],
        false,
    ),
    meta(
        "panel.filter",
        "Filter",
        &[arg("mask", ValueKind::Str, false)],
        false,
    ),
    meta("panel.hidden.toggle", "Show hidden files", &[], false),
    meta(
        "panel.bookmark.set",
        "Set bookmark",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
    meta(
        "panel.bookmark.go",
        "Go to bookmark",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
];

impl Handler for PanelHandler {
    fn namespace(&self) -> &'static str {
        "panel"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        if let Some(what) = cmd.id.as_str().strip_prefix("panel.cursor.") {
            return cursor(cx, what, cmd);
        }
        if changes_session(cmd) {
            cx.state.mark_session_dirty();
        }
        match cmd.id.as_str() {
            "panel.viewport" => viewport(cx, cmd),
            "panel.enter" => enter(cx, cmd),
            "panel.up" => up(cx),
            "panel.root" => root(cx),
            "panel.back" | "panel.forward" => history(cx, cmd.id.as_str() == "panel.back"),
            "panel.go" => {
                let side = side_arg(cx, a, SideRef::Active);
                let loc = a.loc("path").expect("validated: path is required");
                nav::navigate(cx, tab_id(cx, side), loc, None, 0, true);
            }
            "panel.refresh" => {
                let id = match a.int("tab") {
                    Some(t) => {
                        TabId(u32::try_from(t).map_err(|_| CmdError::arg("tab", "out of range"))?)
                    }
                    None => tab_id(cx, cx.state.active),
                };
                if cx.state.tab(id).is_none() {
                    return Err(CmdError::arg("tab", "no such tab"));
                }
                nav::refresh(cx, id);
            }
            "panel.switch" => {
                let to = match a.side("side") {
                    Some(_) => side_arg(cx, a, SideRef::Active),
                    None => cx.state.active.other(),
                };
                if cx.state.panel(to).visible {
                    cx.state.active = to;
                } else {
                    cx.keep_rev();
                }
            }
            "panel.swap" => {
                cx.state.panels.swap(0, 1);
                cx.state.active = cx.state.active.other();
            }
            "panel.focus" => {
                cx.state.active = side_arg(cx, a, SideRef::Active);
                if cx.state.focus != Focus::Panel {
                    cx.state.focus = Focus::Panel;
                    cx.ui(UiEvent::FocusChanged(Focus::Panel));
                }
            }
            "panel.toggle" => toggle(cx, side_arg(cx, a, SideRef::Active)),
            "panel.sort" => sort(cx, cmd)?,
            "panel.view_mode" => {
                active_tab_mut(cx).view_mode = choice(
                    a,
                    "mode",
                    &[
                        ("brief", ViewMode::Brief),
                        ("medium", ViewMode::Medium),
                        ("full", ViewMode::Full),
                        ("wide", ViewMode::Wide),
                    ],
                )?;
            }
            "panel.mode" => {
                let mode = choice(
                    a,
                    "mode",
                    &[
                        ("files", PanelMode::Files),
                        ("info", PanelMode::Info),
                        ("qview", PanelMode::QView),
                        ("tree", PanelMode::Tree),
                    ],
                )?;
                let side = side_arg(cx, a, SideRef::Passive);
                let t = cx.state.panel_mut(side).tab_mut();
                t.mode = if t.mode == mode {
                    PanelMode::Files
                } else {
                    mode
                };
            }
            "panel.filter" => filter(cx, cmd)?,
            "panel.hidden.toggle" => {
                let t = active_tab_mut(cx);
                t.show_hidden = !t.show_hidden;
                let id = t.id;
                nav::arrange(cx, id);
            }
            "panel.bookmark.set" | "panel.bookmark.go" => bookmark(cx, cmd)?,
            _ => unreachable!("registered command without handling: {}", cmd.id),
        }
        Ok(())
    }
}

/// `panel.cursor.*` в активном табе.
fn cursor(cx: &mut Cx<'_>, what: &str, cmd: &Command) -> Result<(), CmdError> {
    let a = &cmd.args;
    let n = a.int("n").unwrap_or(1).max(1);
    let t = active_tab_mut(cx);
    let page = i64::from(t.cursor.page.max(1));
    match what {
        "up" => move_by(t, -n),
        "down" => move_by(t, n),
        "page_up" => move_by(t, -n * page),
        "page_down" => move_by(t, n * page),
        "left" => move_by(t, -n * column_step(t)),
        "right" => move_by(t, n * column_step(t)),
        "home" => t.set_cursor_row(0),
        "end" => t.set_cursor_row(t.rows().saturating_sub(1)),
        "to" => cursor_to(cx, a.str("name").unwrap_or_default())?,
        "match" => cursor_match(cx, a.str("mask").unwrap_or_default(), cmd)?,
        _ => unreachable!("registered command without handling: {}", cmd.id),
    }
    Ok(())
}

/// Команды, меняющие сохраняемое в сессии (навигация и сортировка отмечают сами).
fn changes_session(cmd: &Command) -> bool {
    matches!(
        cmd.id.as_str(),
        "panel.switch"
            | "panel.swap"
            | "panel.focus"
            | "panel.toggle"
            | "panel.view_mode"
            | "panel.mode"
            | "panel.bookmark.set"
    )
}

fn move_by(t: &mut Tab, delta: i64) {
    let last = i64::from(t.rows().saturating_sub(1));
    let row = (i64::from(t.cursor.row) + delta).clamp(0, last.max(0));
    t.set_cursor_row(u32::try_from(row).unwrap_or(0));
}

/// Шаг Left/Right: колонка в многоколоночных видах, иначе страница.
fn column_step(t: &Tab) -> i64 {
    let page = i64::from(t.cursor.page.max(1));
    if t.cols > 1 {
        (page / i64::from(t.cols)).max(1)
    } else {
        page
    }
}

fn cursor_to(cx: &mut Cx<'_>, name: &str) -> Result<(), CmdError> {
    let t = active_tab_mut(cx);
    if let Some(r) = row_of_name(t, name) {
        t.set_cursor_row(r);
    } else if matches!(t.listing.status, ListingStatus::Loading { .. }) {
        t.pending_focus = Some(name.into());
    } else {
        return Err(rejected("core-not-found", &[("name", name)]));
    }
    Ok(())
}

fn cursor_match(cx: &mut Cx<'_>, mask: &str, cmd: &Command) -> Result<(), CmdError> {
    #[derive(Copy, Clone)]
    enum Dir {
        First,
        Next,
        Prev,
    }
    let m = Mask::parse(mask).map_err(|e| CmdError::arg("mask", e.to_string()))?;
    let dir = if cmd.args.get("dir").is_some() {
        choice(
            &cmd.args,
            "dir",
            &[
                ("first", Dir::First),
                ("next", Dir::Next),
                ("prev", Dir::Prev),
            ],
        )?
    } else {
        Dir::First
    };
    let t = active_tab_mut(cx);
    let rows = t.rows();
    if rows == 0 {
        return Err(rejected("core-no-match", &[("mask", mask)]));
    }
    let cur = t.cursor.row;
    let candidates: Box<dyn Iterator<Item = u32>> = match dir {
        Dir::First => Box::new(0..rows),
        Dir::Next => Box::new((1..=rows).map(|k| (cur + k) % rows)),
        Dir::Prev => Box::new((1..=rows).map(|k| (cur + rows - k % rows) % rows)),
    };
    let hit = candidates
        .into_iter()
        .find(|&r| matches!(t.row(r), Some(Row::Entry { entry, .. }) if m.matches(&entry.name)));
    match hit {
        Some(r) => {
            t.set_cursor_row(r);
            Ok(())
        }
        None => Err(rejected("core-no-match", &[("mask", mask)])),
    }
}

/// Размер видимой области от UI: без изменений — без `rev`; `top` прокручивает,
/// не двигая курсор (S0 v3 №3).
fn viewport(cx: &mut Cx<'_>, cmd: &Command) {
    let a = &cmd.args;
    let side = side_arg(cx, a, SideRef::Active);
    let t = cx.state.panel_mut(side).tab_mut();
    let rows = u32::try_from(a.int("rows").unwrap_or(0)).unwrap_or(0);
    let cols = u32::try_from(a.int("cols").unwrap_or(1))
        .unwrap_or(1)
        .max(1);
    let page = rows.saturating_mul(cols);
    let scroll = a.int("top");
    let mut next = t.cursor;
    next.page = page;
    next.top = scroll
        .map_or(t.cursor.top, |v| u32::try_from(v).unwrap_or(0))
        .min(t.rows().saturating_sub(page.max(1)));
    // Окно уменьшилось: страница короче — курсор остаётся видимым (иначе он за
    // нижним краем, и PgDn уводит его дальше). Прокрутка колесом (`top`) — как есть.
    if scroll.is_none() {
        next.clamp(t.rows());
    }
    if (t.cursor.page, t.cols, t.cursor.top) == (next.page, cols, next.top) {
        cx.keep_rev();
        return;
    }
    t.cursor = next;
    t.cols = cols;
    t.touch();
}

/// Каталог — первый `OpenResolver::enter_dir`, иначе войти; `..` — наверх; файл — первый
/// `OpenResolver::resolve`, иначе `file.open` (S5).
fn enter(cx: &mut Cx<'_>, cmd: &Command) {
    enum Act {
        Nothing,
        Up,
        Dir(TabId, Location),
        Open(Command),
    }
    let services = cx.services;
    let t = active_tab_mut(cx);
    let act = match t.current() {
        None => Act::Nothing,
        Some(Row::Parent) => Act::Up,
        Some(Row::Entry { entry, .. }) if entry.kind.is_dir_like() => services
            .open_resolvers
            .iter()
            .find_map(|r| r.enter_dir(&t.loc, entry, &cmd.args))
            .map_or_else(|| Act::Dir(t.id, child_loc(&t.loc, &entry.name)), Act::Open),
        Some(Row::Entry { entry, .. }) => Act::Open(
            services
                .open_resolvers
                .iter()
                .find_map(|r| r.resolve(&t.loc, entry, &cmd.args))
                .unwrap_or_else(|| Command::new("file.open")),
        ),
    };
    match act {
        Act::Nothing => cx.keep_rev(),
        Act::Up => up(cx),
        Act::Dir(id, loc) => nav::navigate(cx, id, loc, None, 0, true),
        Act::Open(c) => {
            cx.dispatch(c);
            cx.keep_rev();
        }
    }
}

/// Наверх с курсором на покинутом каталоге. Из корня контейнера — в каталог
/// контейнера с курсором на нём (S8).
fn up(cx: &mut Cx<'_>) {
    let t = active_tab_mut(cx);
    let id = t.id;
    let (target, focus) = match t.loc.path.parent() {
        Some(p) if !p.as_os_str().is_empty() => (
            t.loc.parent(),
            t.loc.file_name().map(|n| n.to_string_lossy().into()),
        ),
        _ => match t.loc.outer.as_deref() {
            Some(outer) => (
                outer.parent(),
                outer.file_name().map(|n| n.to_string_lossy().into()),
            ),
            None => (None, None),
        },
    };
    match target {
        Some(loc) => nav::navigate(cx, id, loc, focus, 0, true),
        None => cx.keep_rev(),
    }
}

/// В корень: внутри контейнера — корень контейнера, иначе — корень тома.
fn root(cx: &mut Cx<'_>) {
    let t = active_tab_mut(cx);
    let id = t.id;
    let target = if t.loc.outer.is_some() {
        Location {
            backend: t.loc.backend.clone(),
            authority: t.loc.authority.clone(),
            path: "/".into(),
            outer: t.loc.outer.clone(),
        }
    } else {
        let mut l = t.loc.clone();
        while let Some(p) = l.parent() {
            l = p;
        }
        l
    };
    if target == t.loc {
        cx.keep_rev();
    } else {
        nav::navigate(cx, id, target, None, 0, true);
    }
}

fn history(cx: &mut Cx<'_>, back: bool) {
    let t = active_tab_mut(cx);
    let id = t.id;
    let cur = hist_entry(t);
    let to = if back {
        t.history.back(cur)
    } else {
        t.history.forward(cur)
    };
    match to {
        Some(h) => nav::navigate(cx, id, h.loc, h.focus, h.top, false),
        None => cx.keep_rev(),
    }
}

fn toggle(cx: &mut Cx<'_>, side: Side) {
    let services = cx.services;
    let p = cx.state.panel_mut(side);
    p.visible = !p.visible;
    let (visible, id) = (p.visible, p.tab().id);
    services.listing.tab_event(if visible {
        TabEvent::Shown(id)
    } else {
        TabEvent::Hidden(id)
    });
    if !visible && cx.state.active == side && cx.state.panel(side.other()).visible {
        cx.state.active = side.other();
    }
}

/// Повтор той же `key` без `desc` инвертирует направление.
fn sort(cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
    let key = choice(
        &cmd.args,
        "key",
        &[
            ("name", SortKey::Name),
            ("ext", SortKey::Ext),
            ("size", SortKey::Size),
            ("mtime", SortKey::Mtime),
            ("unsorted", SortKey::Unsorted),
        ],
    )?;
    let t = active_tab_mut(cx);
    let desc = match cmd.args.bool("desc") {
        Some(d) => d,
        None if t.sort.key == key => !t.sort.desc,
        None => false,
    };
    t.sort.key = key;
    t.sort.desc = desc;
    let id = t.id;
    nav::arrange(cx, id);
    Ok(())
}

/// Без `mask` — запрос маски у UI (S2); пустая маска снимает фильтр.
fn filter(cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
    let Some(mask) = cmd.args.str("mask") else {
        cx.ui(UiEvent::Ext {
            id: crate::cmd::CommandId::from_static("panel.ask_filter"),
            args: crate::cmd::Args::default(),
        });
        return Ok(());
    };
    let f = if mask.trim().is_empty() {
        None
    } else {
        Some(Mask::parse(mask).map_err(|e| CmdError::arg("mask", e.to_string()))?)
    };
    let t = active_tab_mut(cx);
    t.filter = f;
    let id = t.id;
    nav::arrange(cx, id);
    Ok(())
}

fn bookmark(cx: &mut Cx<'_>, cmd: &Command) -> Result<(), CmdError> {
    let n = cmd.args.int("n").unwrap_or(-1);
    let slot = usize::try_from(n)
        .ok()
        .filter(|&i| i < 10)
        .ok_or_else(|| CmdError::arg("n", "expected 0..9"))?;
    if cmd.id.as_str() == "panel.bookmark.set" {
        let loc = cx.state.active_tab().loc.clone();
        cx.state.bookmarks[slot] = Some(loc);
        return Ok(());
    }
    let Some(loc) = cx.state.bookmarks[slot].clone() else {
        let n: CompactString = n.to_string().into();
        return Err(rejected("core-no-bookmark", &[("n", &n)]));
    };
    let id = tab_id(cx, cx.state.active);
    nav::navigate(cx, id, loc, None, 0, true);
    Ok(())
}
