//! Выделение и копирование (S7 §3.7, карточка S7-11): выделение мышью приходит из UI
//! (`InboxMsg::Select`), `viewer.select_all` — весь документ, `viewer.copy` декодирует
//! выделение в текущей кодировке на `Lane::Interactive` (Hex — строкой `DE AD BE EF`) и
//! отдаёт текст фиче окна (`UiEvent::Ext` `viewer.copy`), она пишет буфер обмена. Сверх
//! `viewer.copy_limit_mb` — тост. Бегунок полосы прокрутки — `InboxMsg::ScrollTo`.
//!
//! UI-поток файл не читает: копия идёт из источника в задаче, не из кэша страниц.

use std::fmt::Write as _;
use std::io;
use std::ops::Range;
use std::sync::Arc;

use urfin_core::{Args, CancelToken, CommandId, Cx, Lane, Level, Msg, UiEvent};

use super::ViewerDoc;
use crate::enc::Enc;
use crate::layout::Mode;
use crate::source::{ByteSource, read_full};
use crate::{DocId, ViewState};

/// Копия читается кусками: декодер переносит символ, разрезанный границей куска.
pub const COPY_CHUNK: usize = 1 << 20;

impl ViewerDoc {
    /// Выделение мышью (байты источника); пустое — снято. `true` — изменилось.
    pub fn select(&mut self, sel: Option<Range<u64>>) -> bool {
        let sel = sel
            .map(|r| r.start.min(self.len)..r.end.min(self.len))
            .filter(|r| r.start < r.end);
        if self.sel == sel {
            return false;
        }
        self.sel = sel;
        self.bump();
        true
    }

    /// `viewer.select_all`: весь документ (в тексте — без BOM).
    pub fn select_all(&mut self) -> bool {
        let from = if self.mode == Mode::Hex {
            0
        } else {
            u64::from(self.enc.bom_len).min(self.len)
        };
        self.select(Some(from..self.len))
    }

    /// Бегунок перетащен: верх экрана — на смещение `off` (Текст — к началу строки).
    pub fn scroll_to(&mut self, off: u64, rt: &urfin_core::Runtime) -> bool {
        let off = off.min(self.len.saturating_sub(1));
        if off == self.top && self.top_sub == 0 {
            return false;
        }
        self.top = off;
        self.top_sub = 0;
        if self.mode != Mode::Hex {
            self.realign_top();
        }
        self.settle(rt);
        self.bump();
        true
    }
}

/// Байты выделения `r` текстом: в кодировке `enc` (BOM не копируется, у UTF-16 — по
/// единицам кода) или Hex-строкой `DE AD BE EF`. Только вне UI-потока.
pub fn decode_range(
    src: &dyn ByteSource,
    r: Range<u64>,
    enc: Enc,
    hex: bool,
    cancel: &CancelToken,
) -> io::Result<String> {
    let mut start = r.start;
    let end = r.end.min(src.len());
    if !hex {
        start = start.max(u64::from(enc.bom_len));
        let u = u64::from(enc.unit());
        start = start / u * u;
    }
    let total = usize::try_from(end.saturating_sub(start)).unwrap_or(usize::MAX);
    let mut out = String::with_capacity(if hex { total * 3 } else { total });
    let mut dec = enc.enc.new_decoder_without_bom_handling();
    let mut buf = vec![0u8; COPY_CHUNK.min(total.max(1))];
    let mut at = start;
    while at < end {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let want = usize::try_from(end - at)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let got = read_full(src, at, &mut buf[..want])?;
        if got == 0 {
            break;
        }
        let chunk = &buf[..got];
        at += got as u64;
        if hex {
            for b in chunk {
                if !out.is_empty() {
                    out.push(' ');
                }
                let _ = write!(out, "{b:02X}");
            }
        } else {
            decode_into(&mut dec, chunk, &mut out, at >= end);
        }
    }
    if !hex && at < end {
        // Источник короче ожидаемого: хвост декодера.
        decode_into(&mut dec, &[], &mut out, true);
    }
    Ok(out)
}

/// Кусок в строку; не хватило места — ещё место и дальше.
fn decode_into(dec: &mut encoding_rs::Decoder, mut src: &[u8], out: &mut String, last: bool) {
    loop {
        out.reserve(
            dec.max_utf8_buffer_length(src.len())
                .unwrap_or(src.len() + 16),
        );
        let (res, read, _) = dec.decode_to_string(src, out, last);
        src = &src[read..];
        if res == encoding_rs::CoderResult::InputEmpty {
            return;
        }
    }
}

/// `viewer.select_all`: выделить весь документ (в документе Markdown — проекцию, это
/// делает фича окна).
pub(crate) fn select_all(cx: &mut Cx<'_>) {
    let Some((id, md)) = active(cx) else {
        cx.keep_rev();
        return;
    };
    if md {
        ext(cx, "viewer.select_all", id, None);
        return;
    }
    let changed = cx
        .state
        .ext
        .get_mut::<ViewState>()
        .and_then(|v| v.viewer_mut(id))
        .is_some_and(ViewerDoc::select_all);
    if !changed {
        cx.keep_rev();
    }
}

/// `viewer.copy`: выделение — в буфер обмена (декодирование — задача на Interactive lane).
pub(crate) fn copy(cx: &mut Cx<'_>) {
    let Some((id, md)) = active(cx) else {
        cx.keep_rev();
        return;
    };
    if md {
        // Документ Markdown: видимый текст выделения есть в UI (проекция).
        ext(cx, "viewer.copy", id, None);
        return;
    }
    let rt = cx.rt;
    let Some(view) = cx.state.ext.get::<ViewState>() else {
        return;
    };
    let limit = u64::from(view.viewer.copy_limit_mb) << 20;
    let Some(d) = view.viewer(id) else {
        return;
    };
    let (Some(sel), Some(src)) = (d.sel.clone(), d.src.clone()) else {
        cx.keep_rev();
        return;
    };
    if sel.end <= sel.start {
        cx.keep_rev();
        return;
    }
    if sel.end - sel.start > limit {
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("mb", u64::from(view.viewer.copy_limit_mb));
        let text = urfin_i18n::current().fmt("ui-view-copy-limit", &a);
        cx.toast(Level::Warn, text);
        return;
    }
    let (enc, hex, cancel) = (d.enc, d.mode == Mode::Hex, d.cancel.child());
    spawn_copy(rt, id, src, sel, enc, hex, cancel);
    cx.keep_rev();
}

/// Документ активного вьювера и признак документа Markdown.
fn active(cx: &Cx<'_>) -> Option<(DocId, bool)> {
    let v = cx.state.ext.get::<ViewState>()?;
    let id = v.active_viewer()?;
    let d = v.viewer(id)?;
    d.is_ready().then(|| (id, d.md_mode()))
}

fn ext(cx: &mut Cx<'_>, id: &'static str, doc: DocId, text: Option<String>) {
    let mut args = Args::default().with("doc", doc.0);
    if let Some(t) = text {
        args = args.with("text", t);
    }
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static(id),
        args,
    });
}

fn spawn_copy(
    rt: &urfin_core::Runtime,
    doc: DocId,
    src: Arc<dyn ByteSource>,
    sel: Range<u64>,
    enc: Enc,
    hex: bool,
    cancel: CancelToken,
) {
    rt.spawn(Lane::Interactive, "view.copy", cancel, move |t| {
        let _span = tracing::debug_span!("view.copy", bytes = sel.end - sel.start).entered();
        let res = match decode_range(&*src, sel, enc, hex, &t.cancel) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return,
            Err(e) => Err(e.to_string()),
        };
        let _ = t.sink.send(Box::new(CopiedMsg { doc, res }), &t.cancel);
    });
}

/// Выделение декодировано: текст — фиче окна (буфер обмена).
struct CopiedMsg {
    doc: DocId,
    res: Result<String, String>,
}

impl std::fmt::Debug for CopiedMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopiedMsg")
            .field("doc", &self.doc)
            .field("ok", &self.res.is_ok())
            .finish()
    }
}

impl Msg for CopiedMsg {
    fn name(&self) -> &'static str {
        "view.copied"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        cx.keep_rev();
        match self.res {
            Ok(text) => ext(cx, "viewer.copy", self.doc, Some(text)),
            Err(e) => {
                let mut args = urfin_i18n::FluentArgs::new();
                args.set("id", "viewer.copy");
                args.set("error", e);
                let text = urfin_i18n::current().fmt("core-command-failed", &args);
                cx.toast(Level::Error, text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::PAGE;
    use crate::source::OwnedSource;
    use encoding_rs::{IBM866, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251};

    fn copy(data: Vec<u8>, r: Range<u64>, enc: Enc) -> String {
        let src = OwnedSource::new(data);
        decode_range(&src, r, enc, false, &CancelToken::new()).unwrap()
    }

    /// Текст в кодировке `enc` так, что символ `ch` стоит на границе страницы (64 КБ) и
    /// куска копии.
    fn around_page(enc: &'static encoding_rs::Encoding, ch: &str) -> (Vec<u8>, String) {
        let pad = "a".repeat(PAGE as usize - 1);
        let text = format!("{pad}{ch}{ch}z");
        let (bytes, _, bad) = enc.encode(&text);
        assert!(!bad);
        let mut bytes = bytes.into_owned();
        if enc == UTF_16LE || enc == UTF_16BE {
            // `encode` UTF-16 не умеет (WHATWG) — вручную.
            bytes = text
                .encode_utf16()
                .flat_map(|u| {
                    if enc == UTF_16LE {
                        u.to_le_bytes()
                    } else {
                        u.to_be_bytes()
                    }
                })
                .collect();
        }
        (bytes, text)
    }

    #[test]
    fn single_byte_copies_unicode_across_pages() {
        for (enc, ch) in [(WINDOWS_1251, "Жё"), (IBM866, "Пр")] {
            let (bytes, text) = around_page(enc, ch);
            let n = bytes.len() as u64;
            assert_eq!(copy(bytes, 0..n, Enc::new(enc)), text, "{}", enc.name());
        }
    }

    #[test]
    fn utf8_and_utf16_split_at_chunk_and_page() {
        // Символ из 4 байт UTF-8 / суррогатная пара UTF-16 поперёк границы.
        for enc in [UTF_8, UTF_16LE, UTF_16BE] {
            let (bytes, text) = around_page(enc, "😀ж");
            let n = bytes.len() as u64;
            let mut long = bytes.clone();
            // Больше куска копии: граница `COPY_CHUNK` режет символ.
            while long.len() < COPY_CHUNK + 16 {
                long.extend_from_slice(&bytes);
            }
            let got = copy(long.clone(), 0..long.len() as u64, Enc::new(enc));
            assert!(got.starts_with(&text), "{}", enc.name());
            assert!(!got.contains('\u{FFFD}'), "{}", enc.name());
            assert_eq!(copy(bytes, 0..n, Enc::new(enc)), text, "{}", enc.name());
        }
    }

    #[test]
    fn bom_is_not_copied_and_utf16_aligns_to_units() {
        let mut data = vec![0xFF, 0xFE];
        data.extend("Да".encode_utf16().flat_map(u16::to_le_bytes));
        let enc = Enc {
            bom_len: 2,
            ..Enc::new(UTF_16LE)
        };
        assert_eq!(copy(data.clone(), 0..6, enc), "Да");
        // Начало посреди единицы — с её начала.
        assert_eq!(copy(data, 3..6, enc), "Да");
    }

    #[test]
    fn hex_copy_is_spaced_bytes() {
        let src = OwnedSource::new(vec![0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x01]);
        let s = decode_range(&src, 1..5, Enc::utf8(), true, &CancelToken::new()).unwrap();
        assert_eq!(s, "DE AD BE EF");
    }

    #[test]
    fn cancelled_copy_stops() {
        let src = OwnedSource::new(vec![b'a'; 16]);
        let c = CancelToken::new();
        c.cancel();
        let e = decode_range(&src, 0..16, Enc::utf8(), false, &c).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Interrupted);
    }
}
