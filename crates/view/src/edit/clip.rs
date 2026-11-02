//! Буфер обмена редактора на движке (ADR-0023 §7; S7 §5.2). Карточка S7-29.
//!
//! Вне UI-потока (`OpenClipboard` может ждать программу-источник): вставка читает
//! `CF_UNICODETEXT` в `view::sys` на Interactive lane и приходит действием
//! [`Act::Paste`] (EOL → `eol_new`, одна группа отмены); копирование — до
//! `viewer.copy_limit_mb`, больше — тост с размером, вырезание тогда не выполняется.
//! Вырезание = копирование снимка + удаление выделения одной группой.

use std::io;

use urfin_core::{Cx, Lane, Level, Msg};

use super::input::Act;
use crate::enc::Enc;
use crate::{DocId, ViewState};

/// Активный документ на движке и его выделение.
fn target(cx: &Cx<'_>) -> Option<DocId> {
    let v = cx.state.ext.get::<ViewState>()?;
    let id = v.active_editor()?;
    v.editor(id)?.engine.as_ref()?;
    Some(id)
}

/// `editor.copy` / `editor.cut`: выделение — в буфер обмена (декодирование и запись — на
/// Interactive lane по снимку).
pub(crate) fn copy(cx: &mut Cx<'_>, cut: bool) {
    let rt = cx.rt;
    let Some(id) = target(cx) else {
        cx.keep_rev();
        return;
    };
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let limit_mb = view.viewer.copy_limit_mb;
    let Some(d) = view.editor_mut(id) else {
        return;
    };
    let Some(e) = d.engine.as_deref() else {
        return;
    };
    if matches!(e.sel.sel, super::input::Sel::Block(_)) {
        // Блок: строки — в handler по кэшам, запись в буфер — `settle` документа.
        let limit = u64::from(limit_mb) << 20;
        if !d.act(Act::BlockCopy { cut, limit }, rt) {
            cx.keep_rev();
        }
        return;
    }
    let Some(sel) = e.sel.stream() else {
        cx.keep_rev();
        return;
    };
    let size = sel.end - sel.start;
    if size > u64::from(limit_mb) << 20 {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("mb", size.div_ceil(1 << 20));
        let text = urfin_i18n::current().fmt("ui-edit-copy-too-big", &a);
        cx.toast(Level::Warn, text);
        return;
    }
    let (snap, enc) = (e.text.snap(), e.enc);
    rt.spawn(Lane::Interactive, "edit.copy", d.cancel.clone(), move |t| {
        let _span = tracing::debug_span!("edit.clip_write", bytes = size).entered();
        let res = (|| -> io::Result<()> {
            let mut buf = vec![0u8; size as usize];
            let n = snap.read_at(sel.start, &mut buf)?;
            buf.truncate(n);
            crate::sys::clip_write(&decode(&buf, enc), false)
        })();
        if let Err(e) = res {
            let _ = t.sink.send(
                Box::new(ClipFailed {
                    id: "editor.copy",
                    err: e.to_string(),
                }),
                &t.cancel,
            );
        }
    });
    if cut {
        d.cut(rt);
    } else {
        cx.keep_rev();
    }
}

/// Байты документа → текст (недекодируемые — U+FFFD).
fn decode(b: &[u8], enc: Enc) -> String {
    enc.enc.decode_without_bom_handling(b).0.into_owned()
}

/// `editor.paste`: текст буфера обмена читается вне UI-потока, затем — правка.
pub(crate) fn paste(cx: &mut Cx<'_>) {
    let rt = cx.rt;
    let Some(id) = target(cx) else {
        cx.keep_rev();
        return;
    };
    let Some(cancel) = cx
        .state
        .ext
        .get::<ViewState>()
        .and_then(|v| v.editor(id))
        .map(|d| d.cancel.clone())
    else {
        return;
    };
    rt.spawn(Lane::Interactive, "edit.paste", cancel, move |t| {
        let msg: Box<dyn Msg> = match crate::sys::clip_read() {
            Ok((text, _)) if text.is_empty() => return,
            Ok((text, column)) => Box::new(Pasted {
                doc: id,
                text,
                column,
            }),
            Err(e) => Box::new(ClipFailed {
                id: "editor.paste",
                err: e.to_string(),
            }),
        };
        let _ = t.sink.send(msg, &t.cancel);
    });
    cx.keep_rev();
}

/// Текст буфера обмена прочитан → вставка одной группой.
#[derive(Debug)]
struct Pasted {
    doc: DocId,
    text: String,
    /// Блок (`MSDEVColumnSelect`): вставка колонкой.
    column: bool,
}

impl Msg for Pasted {
    fn name(&self) -> &'static str {
        "edit.pasted"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let changed = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc))
            .is_some_and(|d| {
                d.act(
                    Act::Paste {
                        text: self.text,
                        replace: false,
                        column: self.column,
                    },
                    rt,
                )
            });
        if !changed {
            cx.keep_rev();
        }
    }
}

/// Текст блока → буфер обмена колонкой (вне UI-потока); документ — `settle`.
pub(crate) fn write_block(rt: &urfin_core::Runtime, cancel: urfin_core::CancelToken, text: String) {
    rt.spawn(Lane::Interactive, "edit.copy", cancel, move |t| {
        let _span = tracing::debug_span!("edit.clip_write", bytes = text.len()).entered();
        if let Err(e) = crate::sys::clip_write(&text, true) {
            let _ = t.sink.send(
                Box::new(ClipFailed {
                    id: "editor.copy",
                    err: e.to_string(),
                }),
                &t.cancel,
            );
        }
    });
}

/// Тост блочной операции (лимит копирования, блок выше 64 тыс. строк).
#[derive(Debug)]
pub(crate) struct BlockToast(pub String);

impl Msg for BlockToast {
    fn name(&self) -> &'static str {
        "edit.block_toast"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        cx.toast(Level::Warn, self.0);
    }
}

/// Буфер обмена недоступен.
#[derive(Debug)]
struct ClipFailed {
    id: &'static str,
    err: String,
}

impl Msg for ClipFailed {
    fn name(&self) -> &'static str {
        "edit.clip_failed"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        cx.keep_rev();
        super::engine::report_failed(cx, self.id, self.err);
    }
}
