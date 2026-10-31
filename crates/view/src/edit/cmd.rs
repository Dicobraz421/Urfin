//! Команды редактора: S7-15 — `editor.open`, `editor.close`, `editor.codepage` (F8 /
//! меню), `editor.reload`, `editor.eol`; S7-17 — `editor.save` / `save_as` /
//! `save_close` / `save_all`, `editor.answer`, закрытие с вопросом; S7-19 — внешний
//! редактор (`alt`, `editor.default`), `editor.new`, `editor.to_viewer`. События
//! `ViewInbox` документа (`Adopted`, `Snapshot`).

use encoding_rs::{Encoding, UTF_8};
use ropey::Rope;
use urfin_core::{Args, CmdError, Command, Cx, Focus, Level, Location, Row, UiEvent};

use super::flow::{self, EdAsk};
use super::{EditorDoc, LoadOpts, NewDoc};
use crate::config::EditorDefault;
use crate::detect::{Eol, tld_for_lang};
use crate::enc::Enc;
use crate::handler::{not_yet, set_focus, view_mut};
use crate::history::HistKind;
use crate::inbox::After;
use crate::{Doc, DocId, Placement, ViewState};

pub(crate) fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    match cmd.id.as_str() {
        "editor.open" => open(cmd, cx, false),
        "editor.new" => open(cmd, cx, true),
        "editor.close" => close(cx),
        "editor.save" | "editor.save_as" | "editor.save_close" | "editor.save_all" => save(cmd, cx),
        "editor.answer" => flow::answer(cmd, cx),
        "editor.to_viewer" => to_viewer(cx),
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
        new: None,
    }
}

/// Строка или колонка с 1 из аргумента → с 0.
fn from_one(n: Option<i64>) -> Option<u32> {
    n.map(|n| u32::try_from(n.max(1) - 1).unwrap_or(u32::MAX))
}

/// Относительный путь — от `base`.
fn join(base: &Location, l: Location) -> Location {
    if l.path.is_relative() {
        Location {
            path: base.path.join(&l.path),
            ..base.clone()
        }
    } else {
        l
    }
}

/// `editor.open {path?, alt?, internal?, line?, col?}` (S7 §5.1) и `editor.new` (Shift+F4,
/// без пути — черновик имени `ui.draft`): путь из аргумента или под курсором; уже
/// открытый файл — его экран наверх. Внешний редактор — `alt` (Alt+Shift+F4), при
/// `editor.default = "external"` — наоборот (S7 §5.6). Новый файл создаётся первым
/// сохранением.
fn open(cmd: &Command, cx: &mut Cx<'_>, new: bool) -> Result<(), CmdError> {
    let a = &cmd.args;
    let mut from_cursor = false;
    let loc = match a.loc("path") {
        Some(l) => join(&cx.state.active_tab().loc, l),
        None if new => {
            flow::draft(cx, "editor.new", "");
            cx.keep_rev();
            return Ok(());
        }
        None => {
            let tab = cx.state.active_tab();
            match tab.current() {
                Some(Row::Entry { entry, .. }) if !entry.kind.is_dir_like() => {
                    from_cursor = true;
                    Location {
                        backend: tab.loc.backend.clone(),
                        path: tab.loc.path.join(entry.name.as_str()),
                        outer: tab.loc.outer.clone(),
                    }
                }
                _ => {
                    cx.keep_rev();
                    return Ok(());
                }
            }
        }
    };
    let rt = cx.rt;
    let view = view_mut(cx)?;
    let alt = a.bool("alt").unwrap_or(false);
    let external = !a.bool("internal").unwrap_or(false)
        && (alt != (view.editor.default == EditorDefault::External));
    if external {
        if !loc.is_local() {
            return Err(CmdError::Rejected(
                urfin_i18n::current().get("ui-view-external-local").into(),
            ));
        }
        let line = a.int("line").and_then(|n| u32::try_from(n.max(1)).ok());
        super::external::launch(cx, &loc.path, from_cursor, line);
        return Ok(());
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
    let mut opts = load_opts(view, &d.loc, enc);
    if new {
        opts.new = Some(NewDoc {
            enc: Encoding::for_label(view.editor.new_codepage.as_bytes()).unwrap_or(UTF_8),
            bom: view.editor.new_bom,
        });
    }
    let reg = view.vfs.clone();
    d.load(rt, &reg, opts);
    view.insert(Doc::Editor(d));
    view.screens.push(id);
    set_focus(cx, Focus::Editor);
    Ok(())
}

/// `editor.close` (Esc): вопрос на экране — убрать его; правки — вопрос «Сохранить?»
/// одной строкой (S7 §5.4); иначе документ закрывается, позиция — в историю правок.
fn close(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let view = view_mut(cx)?;
    let Some(id) = view.active_editor() else {
        cx.keep_rev();
        return Ok(());
    };
    let Some(d) = view.editor_mut(id) else {
        return Ok(());
    };
    if matches!(d.state, super::EdState::Saving(_)) {
        // Запись идёт: документ закроет `save_close`, а не Esc.
        cx.keep_rev();
        return Ok(());
    }
    if d.ask.is_some() {
        d.ask = None;
        d.pending = None;
        d.then_view = None;
        d.rev += 1;
        return Ok(());
    }
    if d.dirty {
        d.ask = Some(EdAsk::Close);
        d.rev += 1;
        return Ok(());
    }
    flow::close_doc(cx, id);
    Ok(())
}

/// `editor.save` (F2), `save_as {path?}` (Shift+F2; без пути — черновик имени),
/// `save_close` (Shift+F10), `save_all`: снимок текста у UI, запись — по его приходу.
fn save(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let id = cmd.id.as_str();
    let view = view_mut(cx)?;
    if id == "editor.save_all" {
        let docs: Vec<DocId> = view
            .editors()
            .filter(|d| d.dirty && d.is_ready())
            .map(|d| d.id)
            .collect();
        if docs.is_empty() {
            cx.keep_rev();
        }
        for doc in docs {
            flow::request_snapshot(cx, doc, &After::Save);
        }
        return Ok(());
    }
    let Some(doc) = view.active_editor() else {
        cx.keep_rev();
        return Ok(());
    };
    let Some(d) = view.editor(doc) else {
        return Ok(());
    };
    if !d.is_ready() {
        cx.keep_rev();
        return Ok(());
    }
    let (loc, local, clean, name) = (
        d.loc.clone(),
        d.loc.is_local(),
        !d.dirty && !d.is_new,
        d.name(),
    );
    let then = match id {
        "editor.save_as" => {
            let Some(l) = cmd.args.loc("path") else {
                let init = if local {
                    loc.path.display().to_string()
                } else {
                    name
                };
                flow::draft(cx, "editor.save_as", &init);
                cx.keep_rev();
                return Ok(());
            };
            After::SaveAs(join(&loc.parent().unwrap_or_else(|| loc.clone()), l))
        }
        _ if !local => {
            // Не-локальный файл (S8): только «Сохранить как» в локальный путь.
            cx.toast(
                Level::Warn,
                urfin_i18n::current().get("ui-edit-remote-save-as"),
            );
            flow::draft(cx, "editor.save_as", &name);
            return Ok(());
        }
        "editor.save_close" if clean => {
            flow::close_doc(cx, doc);
            return Ok(());
        }
        "editor.save_close" => After::SaveClose,
        _ => After::Save,
    };
    flow::request_snapshot(cx, doc, &then);
    Ok(())
}

/// `editor.to_viewer` (F6 в Far): вьювер на строке курсора; правки — сначала вопрос
/// «Сохранить?» (S7-19).
fn to_viewer(cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let view = view_mut(cx)?;
    let Some(id) = view.active_editor() else {
        cx.keep_rev();
        return Ok(());
    };
    let Some(d) = view.editor_mut(id) else {
        return Ok(());
    };
    if matches!(d.state, super::EdState::Saving(_)) {
        cx.keep_rev();
        return Ok(());
    }
    d.then_view = Some(if d.is_ready() { d.cursor.0 } else { 0 });
    if d.dirty {
        d.ask = Some(EdAsk::Close);
        d.rev += 1;
        return Ok(());
    }
    if d.is_new {
        // Файла нет — смотреть нечего.
        d.then_view = None;
    }
    flow::close_doc(cx, id);
    Ok(())
}

/// Перечитать документ с диска (`editor.reload`, «Перезагрузить» на строке внешнего
/// изменения): кодировка, выбранная вручную, сохраняется.
pub(crate) fn reload(cx: &mut Cx<'_>, doc: DocId) {
    let rt = cx.rt;
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let reg = view.vfs.clone();
    let Some(loc) = view.doc(doc).map(|d| d.loc().clone()) else {
        return;
    };
    let opts = load_opts(view, &loc, None);
    let Some(d) = view.editor_mut(doc) else {
        return;
    };
    let enc = (!d.enc.auto).then_some(d.enc.enc);
    d.ask = None;
    d.pending = None;
    d.load(rt, &reg, LoadOpts { enc, ..opts });
}

/// Команды над активным документом редактора.
fn on_doc(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let rt = cx.rt;
    let view = view_mut(cx)?;
    let Some(id) = view.active_editor() else {
        cx.keep_rev();
        return Ok(());
    };
    if cmd.id.as_str() == "editor.reload" {
        reload(cx, id);
        return Ok(());
    }
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
            if d.dirty || d.is_new {
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
    crate::handler::refocus(cx);
}

/// `InboxMsg::Adopted`: UI забрал текст загрузки.
pub(crate) fn adopted(view: &mut ViewState, doc: DocId) -> bool {
    view.editor_mut(doc).is_some_and(EditorDoc::adopted)
}

/// `InboxMsg::Snapshot { then: Diff }`: полоса изменений в фоне. Сохранение —
/// [`flow::start_save`] из `on_pump` (нужен весь `Cx`).
pub(crate) fn diff(view: &mut ViewState, doc: DocId, text: Rope, rt: &urfin_core::Runtime) {
    if let Some(d) = view.editor_mut(doc) {
        d.diff_snapshot(text, rt);
    } else {
        tracing::debug!(?doc, "view inbox: snapshot for a closed document");
    }
}
