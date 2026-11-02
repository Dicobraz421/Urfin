//! Задача подсветки вьювера на `Lane::Interactive` (S7 §5.7, «Разметка» и быстрый
//! просмотр): байты источника (≤ 16 МБ) → текст UTF-8 (другие кодировки — с картой
//! смещений, как у Markdown-документа) → грамматика (вшитая или пакет) → отрезки ролей по
//! байтам источника. У файла больше [`SPLIT_MIN`] сначала — начало (первый экран), затем весь.
//! Отмена — токеном (смена кодировки, режима, закрытие).

use std::sync::Arc;

use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use super::lex::{Feed, Lang, Src, Text};
use super::marks::{End, Marks, advance};
use super::win::{Window, window};
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
    /// Встроенный язык — окно (ADR-0024); `spans` тогда не нужны.
    pub win: Option<WinView>,
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

// ---- Окно (ADR-0024): проход по файлу и окно видимого ----

/// Источник вьювера как текст прохода.
pub(crate) struct SrcText(pub Arc<dyn ByteSource>);

impl Text for SrcText {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_at(off, buf)
    }
}

/// Подсветка окном у вьювера.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "флаги прохода, окна и экрана — независимы"
)]
pub struct WinView {
    pub lang: Lang,
    pub feed: Feed,
    pub enc: Enc,
    pub start: u64,
    pub len: u64,
    /// Последние точки прохода.
    pub marks: Option<Arc<Marks>>,
    /// Докуда проход дошёл.
    pub frontier: u64,
    /// Проход дошёл до конца.
    pub done: bool,
    pub(crate) pass_on: bool,
    /// Окно ждёт прохода.
    pub waiting: bool,
    pub(crate) win_pass: u64,
    pub(crate) win_busy: bool,
    /// Отрезки окна и где они верны.
    pub spans: Arc<[Span]>,
    pub covered: std::ops::Range<u64>,
    /// Окно — по текущему видимому (иначе — новое).
    pub fresh: bool,
    /// Свёртка окна неполна (проход не дошёл): окно заново, когда дойдёт.
    pub partial: bool,
}

impl WinView {
    pub fn new(lang: Lang, feed: Feed, enc: Enc, start: u64, len: u64) -> WinView {
        WinView {
            lang,
            feed,
            enc,
            start,
            len,
            marks: None,
            frontier: start,
            done: false,
            pass_on: false,
            waiting: false,
            win_pass: 0,
            win_busy: false,
            spans: Arc::from(Vec::new()),
            covered: 0..0,
            fresh: false,
            partial: false,
        }
    }
}

/// Проход публикует точки не чаще.
const PUBLISH: std::time::Duration = std::time::Duration::from_millis(100);
/// Шаг паузы прохода (публикация между шагами).
const PASS_SLICE: u64 = 32 << 20;

/// Проход по файлу в выделенном потоке: точки → `SynMarksMsg` (первые — сразу, дальше —
/// не чаще [`PUBLISH`], последние — в конце).
pub(crate) fn spawn_pass(
    rt: &Runtime,
    doc: DocId,
    pass: u64,
    src: Arc<dyn ByteSource>,
    w: &WinView,
    cancel: CancelToken,
) {
    let (lang, feed, start, len) = (w.lang, w.feed, w.start, w.len);
    rt.spawn_dedicated(format!("view.syntax.pass.{}", doc.0), cancel, move |cx| {
        let _span = tracing::debug_span!("view.syntax.pass", lang = lang.name()).entered();
        let text = SrcText(src);
        let s = Src {
            text: &text,
            feed,
            start,
        };
        let mut m = Marks::new(start, len);
        let stop = || cx.cancel.is_cancelled();
        let mut last = std::time::Instant::now();
        let mut first = true;
        loop {
            let until = m.v[m.ok - 1].at.saturating_add(PASS_SLICE);
            let Ok(end) = advance(&mut m, &s, lang, until, &stop) else {
                return;
            };
            let done = end != End::Paused;
            if done || first || last.elapsed() >= PUBLISH {
                first = false;
                last = std::time::Instant::now();
                let msg = SynMarksMsg {
                    doc,
                    pass,
                    marks: Arc::new(m.clone()),
                    done,
                };
                let _ = cx.sink.send(Box::new(msg), &cx.cancel);
            }
            if done {
                return;
            }
        }
    });
}

/// Окно видимого.
pub(crate) struct WinJob {
    pub doc: DocId,
    pub pass: u64,
    pub win_pass: u64,
    pub src: Arc<dyn ByteSource>,
    pub marks: Arc<Marks>,
    pub range: std::ops::Range<u64>,
    pub lang: Lang,
    pub feed: Feed,
    pub enc: Enc,
    pub start: u64,
}

pub(crate) fn spawn_window(rt: &Runtime, job: WinJob, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "view.syntax.window", cancel, move |cx| {
        let _span = tracing::debug_span!("view.syntax.window", lang = job.lang.name()).entered();
        let text = SrcText(job.src.clone());
        let s = Src {
            text: &text,
            feed: job.feed,
            start: job.start,
        };
        let stop = || cx.cancel.is_cancelled();
        let Ok(win) = window(
            &s,
            job.enc,
            job.lang,
            &job.marks,
            job.range.start,
            job.range.end,
            &stop,
        ) else {
            return;
        };
        let msg = SynWinMsg {
            doc: job.doc,
            pass: job.pass,
            win_pass: job.win_pass,
            win,
        };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

/// Точки прохода → документ вьювера.
#[derive(Debug)]
pub struct SynMarksMsg {
    pub doc: DocId,
    pub pass: u64,
    pub marks: Arc<Marks>,
    pub done: bool,
}

impl Msg for SynMarksMsg {
    fn name(&self) -> &'static str {
        "view.syntax.pass"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.viewer_mut(self.doc) {
            d.syn_marks(self.pass, self.marks, self.done, rt);
        }
    }
}

/// Окно → документ вьювера.
#[derive(Debug)]
pub struct SynWinMsg {
    pub doc: DocId,
    pub pass: u64,
    pub win_pass: u64,
    pub win: Option<Window>,
}

impl Msg for SynWinMsg {
    fn name(&self) -> &'static str {
        "view.syntax.window"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.viewer_mut(self.doc) {
            d.syn_window(self.pass, self.win_pass, self.win, rt);
        }
    }
}
