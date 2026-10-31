//! Команды редактора S7-15: `editor.open`, `editor.close`, `editor.codepage` (F8 / меню),
//! `editor.reload`, `editor.eol`; события `ViewInbox` документа (`Adopted`, `Snapshot`).

use encoding_rs::Encoding;
use ropey::Rope;
use urfin_core::{Args, CmdError, Command, Cx, Focus, Level, Location, Row, UiEvent};

use super::{EditorDoc, LoadOpts};
use crate::config::EditorDefault;
use crate::detect::{Eol, tld_for_lang};
use crate::enc::Enc;
use crate::handler::{not_yet, refocus, set_focus, view_mut};
use crate::history::HistKind;
use crate::inbox::After;
use crate::{Doc, DocId, Placement, ViewState};

pub(crate) fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    match cmd.id.as_str() {
        "editor.open" => open(cmd, cx),
        "editor.close" => close(cmd, cx),
        _ => on_doc(cmd, cx),
    }
}

/// Параметры загрузки по `[editor]` и языку интерфейса.
fn load_opts(view: &ViewState, loc: &Location, enc: Option<&'static Encoding>) -> LoadOpts {
    let lang = urfin_i18n::current().lang().to_string();
    LoadOpts {
        enc,
        limit: u64::from(view.editor.max_size_mb.max(1)) << 20,
        tld: tld_for_lang(&lang),
        eol: view.editor.eol,
        name: loc
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// Строка или колонка с 1 из аргумента → с 0.
fn from_one(n: Option<i64>) -> Option<u32> {
    n.map(|n| u32::try_from(n.max(1) - 1).unwrap_or(u32::MAX))
}

/// `editor.open {path?, alt?, internal?, line?, col?}` (S7 §5.1): путь из аргумента или под
/// курсором; уже открытый файл — его экран наверх. Внешний редактор — S7-19.
fn open(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let a = &cmd.args;
    let loc = match a.loc("path") {
        Some(l) if l.path.is_relative() => {
            let base = cx.state.active_tab().loc.clone();
            Location {
                path: base.path.join(&l.path),
                ..base
            }
        }
        Some(l) => l,
        None => {
            let tab = cx.state.active_tab();
            match tab.current() {
                Some(Row::Entry { entry, .. }) if !entry.kind.is_dir_like() => Location {
                    backend: tab.loc.backend.clone(),
                    path: tab.loc.path.join(entry.name.as_str()),
                    outer: tab.loc.outer.clone(),
                },
                _ => {
                    cx.keep_rev();
                    return Ok(());
                }
            }
        }
    };
    let rt = cx.rt;
    let view = view_mut(cx)?;
    let external = a.bool("alt").unwrap_or(false)
        || (view.editor.default == EditorDefault::External && !a.bool("internal").unwrap_or(false));
    if external {
        return Err(not_yet(cmd));
    }
    let open_id = view
        .screens
        .iter()
        .copied()
        .find(|id| matches!(view.doc(*id), Some(Doc::Editor(d)) if d.loc == loc));
    if let Some(id) = open_id {
        view.activate(id);
        set_focus(cx, Focus::Editor);
        return Ok(());
    }
    let memo = view
        .viewer
        .remember_pos
        .then(|| view.history.get_kind(&loc, HistKind::Edit).cloned())
        .flatten();
    let start = match from_one(a.int("line")) {
        Some(line) => Some((line, from_one(a.int("col")).unwrap_or(0))),
        None => memo
            .as_ref()
            .map(|m| (u32::try_from(m.top).unwrap_or(0), m.hscroll)),
    };
    let enc = memo
        .and_then(|m| m.enc)
        .and_then(|l| Encoding::for_label(l.as_bytes()));
    let id = view.alloc_id();
    let mut d = EditorDoc::with_cancel(id, loc, Placement::Window, rt.token().child());
    d.start = start;
    let opts = load_opts(view, &d.loc, enc);
    let reg = view.vfs.clone();
    d.load(rt, &reg, opts);
    view.insert(Doc::Editor(d));
    view.screens.push(id);
    set_focus(cx, Focus::Editor);
    Ok(())
}

/// Активный документ редактора.
fn active_editor(view: &ViewState) -> Option<DocId> {
    view.active()
        .filter(|id| matches!(view.doc(*id), Some(Doc::Editor(_))))
}

/// `editor.close` (Esc): документ без правок закрывается, позиция — в историю правок.
/// С правками — подтверждение одной строкой (S7-17), пока отказ.
fn close(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let view = view_mut(cx)?;
    let Some(id) = active_editor(view) else {
        cx.keep_rev();
        return Ok(());
    };
    if view.editor_mut(id).is_some_and(|d| d.dirty) {
        return Err(not_yet(cmd));
    }
    let remember = view.viewer.remember_pos;
    if let Some(Doc::Editor(mut d)) = view.remove(id) {
        d.close();
        if remember && d.is_ready() {
            view.history.record(d.memo());
        }
    }
    refocus(cx);
    Ok(())
}

/// Команды над активным документом редактора.
fn on_doc(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let rt = cx.rt;
    let view = view_mut(cx)?;
    let Some(id) = active_editor(view) else {
        cx.keep_rev();
        return Ok(());
    };
    let ring = view.viewer.codepage_ring.clone();
    let reg = view.vfs.clone();
    let Some(loc) = view.doc(id).map(|d| d.loc().clone()) else {
        return Ok(());
    };
    let opts = load_opts(view, &loc, None);
    let Some(d) = view.editor_mut(id) else {
        return Ok(());
    };
    let mut ext = None;
    let mut toast = None;
    match cmd.id.as_str() {
        "editor.codepage_menu" => ext = Some(Args::default()),
        "editor.reload" => {
            let enc = (!d.enc.auto).then_some(d.enc.enc);
            d.load(rt, &reg, LoadOpts { enc, ..opts });
        }
        "editor.codepage" => {
            if !d.is_ready() {
                cx.keep_rev();
                return Ok(());
            }
            let label = cmd
                .args
                .str("cp")
                .map(|l| l.trim().to_owned())
                .or_else(|| crate::viewer::ring_next(&ring, &d.enc, |l: &str| resolve(d, l)));
            let Some(label) = label else {
                cx.keep_rev();
                return Ok(());
            };
            let Some(target) = resolve(d, &label) else {
                return Err(CmdError::Arg {
                    name: "cp".into(),
                    msg: format!("unknown encoding {label}").into(),
                });
            };
            if target.enc == d.enc.enc && target.auto == d.enc.auto {
                cx.keep_rev();
                return Ok(());
            }
            if d.dirty {
                // Текст изменён: кодировка — только для сохранения (S7 §5.2).
                d.enc = Enc {
                    bom_len: d.enc.bom_len,
                    ..target
                };
                d.rev += 1;
                let mut fa = urfin_i18n::FluentArgs::new();
                fa.set("enc", target.enc.name());
                toast = Some(urfin_i18n::current().fmt("ui-edit-cp-on-save", &fa));
            } else {
                // Без правок — перечитать байты с диска в новой кодировке.
                let enc = (!target.auto).then_some(target.enc);
                d.load(rt, &reg, LoadOpts { enc, ..opts });
            }
        }
        "editor.eol" => {
            let to = cmd.args.str("to").unwrap_or_default();
            d.eol_new = match to {
                "lf" => Eol::Lf,
                "crlf" => Eol::CrLf,
                "cr" => Eol::Cr,
                other => {
                    return Err(CmdError::Arg {
                        name: "to".into(),
                        msg: format!("expected lf, crlf or cr, got {other}").into(),
                    });
                }
            };
            d.rev += 1;
            // Текст переводит фича UI одной undo-группой (S7 §5.2).
            ext = Some(Args::default().with("to", to));
        }
        _ => return Err(not_yet(cmd)),
    }
    if let Some(args) = ext {
        cx.ui(UiEvent::Ext {
            id: cmd.id.clone(),
            args: args.with("doc", id.0),
        });
    }
    if let Some(t) = toast {
        cx.toast(Level::Info, t);
    }
    Ok(())
}

/// Кодировка по метке кольца или «мешочка»: `auto` — детект последней загрузки.
fn resolve(d: &EditorDoc, label: &str) -> Option<Enc> {
    if label.eq_ignore_ascii_case("auto") {
        return Some(Enc {
            auto: true,
            ..d.detected
        });
    }
    Encoding::for_label(label.as_bytes()).map(Enc::new)
}

/// Загрузка не удалась: тост, документ закрывается.
pub(crate) fn fail_load(cx: &mut Cx<'_>, doc: DocId, err: &str) {
    tracing::warn!(error = %err, "editor open failed");
    if let Some(view) = cx.state.ext.get_mut::<ViewState>()
        && let Some(Doc::Editor(mut d)) = view.remove(doc)
    {
        d.close();
    }
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("id", "editor.open");
    args.set("error", err.to_owned());
    let text = urfin_i18n::current().fmt("core-command-failed", &args);
    cx.toast(Level::Error, text);
    refocus(cx);
}

/// `InboxMsg::Adopted`: UI забрал текст загрузки.
pub(crate) fn adopted(view: &mut ViewState, doc: DocId) -> bool {
    view.editor_mut(doc).is_some_and(EditorDoc::adopted)
}

/// `InboxMsg::Snapshot`: `Diff` — полоса изменений в фоне; сохранение — S7-17.
pub(crate) fn snapshot(
    view: &mut ViewState,
    doc: DocId,
    text: Rope,
    then: After,
    rt: &urfin_core::Runtime,
) -> bool {
    let Some(d) = view.editor_mut(doc) else {
        tracing::debug!(?doc, "view inbox: snapshot for a closed document");
        return false;
    };
    match then {
        After::Diff => d.diff_snapshot(text, rt),
        then => tracing::debug!(?doc, ?then, "view inbox: save is not handled yet"),
    }
    false
}
