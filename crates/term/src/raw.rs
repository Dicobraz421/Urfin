//! Терминал под мышью и буфером (S6-07, S6 §3.4): `term.copy`, `term.paste`,
//! `term.select_all`, `term.scroll`, `term.mouse`; Ctrl+C с выделением.
//!
//! Буфер обмена — у окна: `clip.write` (текст в буфер), `clip.read` (текст буфера
//! обратно командой `term.paste text=…`).

use std::sync::Arc;

use urfin_core::{Args, CmdError, CommandId, Cx, UiEvent};

use crate::TermState;
use crate::vt::input::{self, Mods, MouseButton};
use crate::vt::select::{MouseAction, MouseIn, MouseKind, ScrollBy};
use crate::vt::shared::TermShared;

/// Сетка под вводом: показанная явно (сцены) или активного сеанса.
pub(crate) fn shared(st: &TermState) -> Option<Arc<TermShared>> {
    st.preview
        .clone()
        .or_else(|| st.sessions.active().and_then(|s| s.shared().cloned()))
}

fn clip_write(cx: &mut Cx<'_>, text: String) {
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("clip.write"),
        args: Args::default().with("text", text),
    });
}

/// `term.copy` (Ctrl+Shift+C, Ctrl+Ins): выделение — в буфер, выделение снимается.
/// `false` — выделения нет.
pub(crate) fn copy(cx: &mut Cx<'_>) -> bool {
    let Some(sh) = cx.state.ext.get::<TermState>().and_then(shared) else {
        return false;
    };
    let Some(text) = sh.selection_text() else {
        return false;
    };
    sh.clear_selection();
    clip_write(cx, text);
    true
}

/// Перед вводом с клавиатуры: выделение снять, вид — вниз к курсору.
pub(crate) fn before_input(st: &TermState) {
    if let Some(sh) = shared(st) {
        sh.clear_selection();
        if sh.display_offset() > 0 {
            sh.scroll(ScrollBy::Bottom);
        }
    }
}

/// `term.paste` (Ctrl+Shift+V, Shift+Ins). Без текста — сначала буфер у окна.
/// Многострочный текст в shell без bracketed paste — после inline-вопроса (cmd).
pub(crate) fn paste(cx: &mut Cx<'_>, text: Option<&str>) -> Result<(), CmdError> {
    let Some(t) = text else {
        cx.ui(UiEvent::Ext {
            id: CommandId::from_static("clip.read"),
            args: Args::default().with("then", "term.paste"),
        });
        return Ok(());
    };
    let t = t.trim_end_matches(['\r', '\n']);
    if t.is_empty() {
        return Ok(());
    }
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    let modes = st.sessions.input_modes();
    if t.contains('\n') && !modes.bracketed_paste {
        return crate::paste_lines(cx, t);
    }
    before_input(st);
    let mut bytes = Vec::with_capacity(t.len() + 12);
    input::paste(t, modes, &mut bytes);
    st.sessions.write(&st.config, rt, bytes, false)
}

/// `term.select_all`.
pub(crate) fn select_all(st: &TermState) {
    if let Some(sh) = shared(st) {
        sh.select_all();
    }
}

/// `term.scroll {lines|pages}`: `−` — вверх (к истории), как Shift+PgUp.
pub(crate) fn scroll(st: &TermState, a: &Args) {
    let Some(sh) = shared(st) else {
        return;
    };
    let clamp = |v: i64| i32::try_from(v.clamp(-100_000, 100_000)).unwrap_or(0);
    let by = match (a.int("lines"), a.int("pages")) {
        (Some(l), _) => ScrollBy::Lines(-clamp(l)),
        (None, Some(p)) => ScrollBy::Pages(-clamp(p)),
        (None, None) => ScrollBy::Bottom,
    };
    sh.scroll(by);
}

fn mods_of(s: Option<&str>) -> Mods {
    let mut m = Mods::NONE;
    for p in s.unwrap_or("").split('+') {
        match p {
            "ctrl" => m.ctrl = true,
            "alt" => m.alt = true,
            "shift" => m.shift = true,
            _ => {}
        }
    }
    m
}

/// Аргументы `term.mouse` → событие.
pub(crate) fn mouse_in(a: &Args) -> Result<MouseIn, CmdError> {
    let bad = |name: &str, msg: &str| CmdError::Arg {
        name: name.into(),
        msg: msg.into(),
    };
    let kind = match a.str("kind").unwrap_or_default() {
        "down" => MouseKind::Down,
        "move" => MouseKind::Move,
        "up" => MouseKind::Up,
        "wheel" => MouseKind::Wheel,
        _ => return Err(bad("kind", "down|move|up|wheel")),
    };
    let button = match a.str("button") {
        Some("left") => Some(MouseButton::Left),
        Some("middle") => Some(MouseButton::Middle),
        Some("right") => Some(MouseButton::Right),
        None | Some("none") => None,
        Some(_) => return Err(bad("button", "left|middle|right")),
    };
    let cell = |k: &str| u16::try_from(a.int(k).unwrap_or(0).clamp(0, 9999)).unwrap_or(0);
    Ok(MouseIn {
        kind,
        button,
        row: cell("row"),
        col: cell("col"),
        right: a.bool("right").unwrap_or(false),
        mods: mods_of(a.str("mods")),
        clicks: u32::try_from(a.int("clicks").unwrap_or(1).clamp(1, 3)).unwrap_or(1),
        lines: i32::try_from(a.int("lines").unwrap_or(0).clamp(-100, 100)).unwrap_or(0),
    })
}

/// `term.mouse`: отчёт приложению, выделение или прокрутка (S6 §3.4).
pub(crate) fn mouse(cx: &mut Cx<'_>, a: &Args) -> Result<(), CmdError> {
    let e = mouse_in(a)?;
    let rt = cx.rt;
    let Some(st) = cx.state.ext.get_mut::<TermState>() else {
        return Ok(());
    };
    let Some(sh) = shared(st) else {
        return Ok(());
    };
    let modes = if st.preview.is_some() {
        sh.input_modes()
    } else {
        st.sessions.input_modes()
    };
    match crate::vt::select::mouse_action(&e, modes) {
        MouseAction::Send(bytes) if st.preview.is_none() => {
            st.sessions.write(&st.config, rt, bytes, false)?;
        }
        MouseAction::Select(kind) => sh.select_start(e.row, e.col, e.right, kind),
        MouseAction::Extend => sh.select_update(e.row, e.col, e.right),
        MouseAction::Scroll(n) => sh.scroll(ScrollBy::Lines(n)),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "raw_tests.rs"]
mod integration;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_args() {
        let a = Args::default()
            .with("kind", "down")
            .with("button", "left")
            .with("row", 3i64)
            .with("col", 7i64)
            .with("mods", "shift+alt")
            .with("clicks", 2i64);
        let e = mouse_in(&a).unwrap();
        assert_eq!(
            (e.kind, e.button, e.row, e.col),
            (MouseKind::Down, Some(MouseButton::Left), 3, 7)
        );
        assert!(e.mods.shift && e.mods.alt && !e.mods.ctrl);
        assert_eq!(e.clicks, 2);
        assert!(mouse_in(&Args::default().with("kind", "jump")).is_err());
    }
}
