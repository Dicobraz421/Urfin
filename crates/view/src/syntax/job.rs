//! Задача подсветки вьювера на `Lane::Interactive` (S7 §5.7, «Разметка» и быстрый
//! просмотр): байты источника (≤ 16 МБ) → текст UTF-8 (другие кодировки — с картой
//! смещений, как у Markdown-документа) → грамматика (вшитая или пакет) → отрезки ролей по
//! байтам источника. У файла больше [`SPLIT_MIN`] сначала — начало (первый экран), затем весь.
//! Отмена — токеном (смена кодировки, режима, закрытие).

use std::sync::Arc;

use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use super::{Cancelled, Span, grammar, highlight};
use crate::enc::Enc;
use crate::md::job::{Decoded, cut_point};
use crate::md::{FIRST_CHUNK, SPLIT_MIN};
use crate::source::{ByteSource, read_full};
use crate::{DocId, ViewState};

/// Подсветка документа вьювера.
#[derive(Debug, Default)]
pub struct SynState {
    /// Кодировка, под которую идёт или прошёл разбор.
    pub enc: Option<Enc>,
    pub pass: u64,
    pub cancel: CancelToken,
    /// Отрезки по байтам источника (по возрастанию, без пересечений).
    pub spans: Option<Arc<[Span]>>,
    /// Разобран весь документ.
    pub complete: bool,
}

/// Что нужно задаче.
#[derive(Debug)]
pub(crate) struct SynJob {
    pub doc: DocId,
    pub pass: u64,
    pub src: Arc<dyn ByteSource>,
    pub enc: Enc,
    pub lang: &'static str,
}

/// Читает источник, разбирает и отдаёт отрезки в `emit` (`true` — весь документ).
pub(crate) fn load(
    src: &dyn ByteSource,
    enc: Enc,
    lang: &str,
    cancel: &CancelToken,
    mut emit: impl FnMut(Arc<[Span]>, bool),
) -> Result<(), Cancelled> {
    let Some(g) = grammar(lang) else {
        emit(Arc::from(Vec::new()), true);
        return Ok(());
    };
    let len = src.len();
    let owned;
    let bytes = if let Some(m) = src.mapped() {
        &m[..usize::try_from(len).unwrap_or(m.len()).min(m.len())]
    } else {
        let mut v = vec![0u8; usize::try_from(len).unwrap_or(0)];
        let n = read_full(src, 0, &mut v).map_err(|_| Cancelled)?;
        v.truncate(n);
        owned = v;
        &owned[..]
    };
    let stop = || cancel.is_cancelled();
    if stop() {
        return Err(Cancelled);
    }
    let dec = Decoded::new(bytes, enc);
    let to_src = |list: Vec<Span>| -> Arc<[Span]> {
        list.into_iter()
            .map(|s| Span {
                start: dec.to_src(s.start),
                end: dec.to_src(s.end),
                tag: s.tag,
            })
            .filter(|s| s.start < s.end)
            .collect()
    };
    if len > SPLIT_MIN {
        let cut = cut_point(&dec.text, FIRST_CHUNK);
        if cut < dec.text.len() {
            emit(to_src(highlight(&g, &dec.text[..cut], &stop)?), false);
        }
    }
    emit(to_src(highlight(&g, &dec.text, &stop)?), true);
    Ok(())
}

/// Задача `view.syntax`.
pub(crate) fn spawn(rt: &Runtime, job: SynJob, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "view.syntax", cancel, move |cx| {
        let _span = tracing::debug_span!("view.syntax", lang = job.lang).entered();
        let (doc, pass) = (job.doc, job.pass);
        let _ = load(
            &*job.src,
            job.enc,
            job.lang,
            &cx.cancel,
            |spans, complete| {
                let msg = SynMsg {
                    doc,
                    pass,
                    spans,
                    complete,
                };
                let _ = cx.sink.send(Box::new(msg), &cx.cancel);
            },
        );
    });
}

/// Отрезки подсветки → документ вьювера.
#[derive(Debug)]
pub struct SynMsg {
    pub doc: DocId,
    pub pass: u64,
    pub spans: Arc<[Span]>,
    pub complete: bool,
}

impl Msg for SynMsg {
    fn name(&self) -> &'static str {
        "view.syntax"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let Some(d) = view.viewer_mut(self.doc) else {
            return;
        };
        d.syn_apply(self.pass, self.spans, self.complete);
    }
}
