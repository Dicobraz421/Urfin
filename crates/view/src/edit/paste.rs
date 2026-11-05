//! Большая вставка (ADR-0023, бюджет S7-36: «вставка 100 МБ — кадр p99 ≤ 6 мс, готово ≤ 1 с»).
//! Концы строк, кодирование и запись байт буфера правок на UI-потоке стоили кадра 250–440 мс
//! на 100 МБ. От [`BIG`] байт текст готовится на Interactive lane и ложится во временный файл
//! (`RewriteOut`, как большой итог «Заменить все»), UI-поток вставляет один кусок. Правки за
//! время подготовки не мешают: место вставки — выделение и курсор на момент готовности.
//! Символа нет в кодировке документа — обычный путь: вопрос задаёт движок.

use std::borrow::Cow;
use std::io;
use std::time::Instant;

use urfin_core::{Cx, Lane, Msg, Runtime};

use super::EditorDoc;
use super::input::{Act, Sel, encode, norm_eol};
use super::write::{RewriteOut, Rewritten};
use crate::enc::Enc;
use crate::index::Sep;
use crate::{DocId, ViewState};

/// Текст длиннее — подготовка вне UI-потока.
pub const BIG: usize = 1 << 20;

/// Большая вставка без колонки: текст и «символа нет — заменить».
fn big_text(a: Act) -> Result<(String, bool), Act> {
    match a {
        Act::Paste {
            text,
            replace,
            column: false,
        } if text.len() >= BIG => Ok((text, replace)),
        Act::Input { text, replace } if text.len() >= BIG => Ok((text.into_string(), replace)),
        a => Err(a),
    }
}

/// Большая вставка уходит в фон (`None`), всё остальное возвращается вызвавшему.
pub(super) fn start(d: &mut EditorDoc, a: Act, rt: &Runtime) -> Option<Act> {
    let env = d.env();
    let Some(e) = d.engine.as_deref() else {
        return Some(a);
    };
    if env.readonly || matches!(e.sel.sel, Sel::Block(_)) {
        return Some(a);
    }
    let (text, replace) = match big_text(a) {
        Ok(t) => t,
        Err(a) => return Some(a),
    };
    let (enc, sep) = (e.enc, e.text.sep());
    let (doc, eol) = (d.id, env.eol_new);
    let temp = std::env::temp_dir().join("Urfin");
    d.pasting = true;
    rt.spawn(
        Lane::Interactive,
        "edit.paste",
        d.cancel.clone(),
        move |t| {
            let _span = tracing::debug_span!("edit.paste", bytes = text.len()).entered();
            let name = format!("paste-{}-{}", std::process::id(), doc.0);
            let res = prepare(&text, enc, eol, replace, sep, &temp, &name);
            let msg = Ready {
                doc,
                text: res.is_err().then_some((text, replace)),
                rw: res.ok(),
            };
            let _ = t.sink.send(Box::new(msg), &t.cancel);
        },
    );
    None
}

/// Концы строк и кодировка документа — во временный файл.
fn prepare(
    text: &str,
    enc: Enc,
    eol: crate::detect::Eol,
    replace: bool,
    sep: Sep,
    dir: &std::path::Path,
    name: &str,
) -> io::Result<Rewritten> {
    let s = norm_eol(text, eol);
    let bytes: Cow<'_, [u8]> = if enc.enc == encoding_rs::UTF_8 {
        Cow::Borrowed(s.as_bytes())
    } else {
        Cow::Owned(encode(&enc, &s, replace).map_err(|_| io::Error::other("unmappable"))?)
    };
    let mut out = RewriteOut::new(sep, bytes.len() as u64, dir, name)?;
    out.write(&bytes)?;
    Ok(out.finish(Vec::new()))
}

/// Текст подготовлен; `rw` нет — не вышло, текст вернулся.
#[derive(Debug)]
struct Ready {
    doc: DocId,
    rw: Option<Rewritten>,
    text: Option<(String, bool)>,
}

impl Msg for Ready {
    fn name(&self) -> &'static str {
        "edit.paste_ready"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(d) = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc))
        else {
            return;
        };
        d.pasting = false;
        if let Some((text, replace)) = self.text {
            // Вопрос о символе и ошибка записи — обычным путём.
            let a = Act::Paste {
                text,
                replace,
                column: false,
            };
            d.act_now(a, rt);
            return;
        }
        let (Some(rw), false) = (self.rw, d.env().readonly) else {
            cx.keep_rev();
            return;
        };
        let Some(e) = d.engine.as_deref_mut() else {
            return;
        };
        let done = e.paste_rewrite(rw, Instant::now());
        d.after(&done);
        d.settle(rt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_text_stays_on_the_ui_thread() {
        let a = Act::Paste {
            text: "x".into(),
            replace: false,
            column: false,
        };
        assert!(big_text(a).is_err());
    }

    #[test]
    fn column_paste_is_not_big() {
        let a = Act::Paste {
            text: "x".repeat(BIG),
            replace: false,
            column: true,
        };
        assert!(big_text(a).is_err());
    }

    /// Файл вставки — те же байты, что дал бы обычный путь: концы строк и UTF-16.
    #[test]
    fn prepare_matches_the_plain_path() {
        let dir = tempfile::tempdir().unwrap();
        let text = "строка\r\nвторая\nтретья\r".repeat(100);
        for enc in [Enc::utf8(), Enc::new(encoding_rs::UTF_16LE)] {
            let eol = crate::detect::Eol::CrLf;
            let sep = Sep::of(&enc, eol);
            let rw = prepare(&text, enc, eol, false, sep, dir.path(), "p").unwrap();
            let want = encode(&enc, &norm_eol(&text, eol), false).unwrap();
            let total: u64 = super::super::piece::bytes_of(&rw.pieces);
            assert_eq!(total, want.len() as u64);
            let mut got = vec![0u8; want.len()];
            crate::source::read_full(&*rw.src, 0, &mut got).unwrap();
            assert_eq!(got, want);
        }
    }
}
