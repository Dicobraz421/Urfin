//! Подсветка и свёртка документа редактора (S7-35, ADR-0023 §7, S7 §5.7): когда
//! разбирать, задача на `Lane::Interactive`, приём итога, сдвиг отрезков правками до
//! нового разбора, `editor.fold`. Разбор и запрос — `syntax::edit` в фоне по снимку; на
//! UI-потоке — только сдвиг готовых отрезков (O(отрезков видимой части)).
//!
//! - Только документы ≤ 16 МБ (`MARKUP_MAX`) с известным языком и `editor.highlight`.
//! - Правка → задача: снимок → текст → одна правка прежнего дерева (`Tree::edit`) →
//!   разбор с ним → запрос по видимой части ± [`MARGIN`] → отрезки и места свёртки.
//!   Задача одна: правки за время разбора не отменяют его (отмена с перезапуском на
//!   каждое нажатие при разборе дольше паузы между нажатиями не даёт цветов вовсе —
//!   замер 2026-11-02), а сливаются в следующую задачу по свежему снимку. Отмена —
//!   закрытие, перечитывание, выключение подсветки.
//! - До ответа отрезки сдвигаются правками (`TextDoc::take_log`): цвет остаётся на своём
//!   тексте, набранное внутри отрезка — его цвета.

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use super::fold::Fold;
use super::input::Selections;
use super::piece::{TextEdit, TextSnap};
use super::{EdState, EditorDoc};
use crate::syntax::edit::{Feed, FoldAt, Parsed, feed_text, folds, parse, spans};
use crate::syntax::{Cancelled, Span, grammar};
use crate::viewer::MARKUP_MAX;
use crate::{DocId, ViewState};

/// Запрос подсветки — видимая часть и столько байт по краям.
const MARGIN: u64 = 64 << 10;
/// Новый запрос, когда видимое подошло к краю посчитанного ближе этого.
const EDGE: u64 = 16 << 10;
/// Правок за раз больше — отрезки не сдвигаются, а ждут разбора.
const SHIFT_MAX: usize = 64;
/// Правок за время задачи больше — её итог не сдвигается (O(правок × отрезков) на
/// UI-потоке), на экране остаются уже сдвинутые отрезки до следующей задачи.
const SINCE_SHIFT: usize = 8;

/// Подсветка документа редактора.
#[derive(Debug, Default)]
pub struct Hl {
    pass: u64,
    cancel: CancelToken,
    /// Идёт задача: правок текста у её снимка.
    busy: Option<u64>,
    /// Правки после снимка идущей задачи (её итог сдвигается ими); `None` — потеряны.
    since: Option<Vec<TextEdit>>,
    /// Последний разбор и правок текста у него.
    parsed: Option<(Arc<Parsed>, u64)>,
    /// Отрезки ролей по байтам документа (по возрастанию, без пересечений).
    pub spans: Arc<[Span]>,
    /// Места свёртки видимой части (по заголовкам).
    pub folds: Arc<[FoldAt]>,
    /// Байты, для которых посчитаны отрезки и места свёртки.
    covered: Range<u64>,
    /// Отрезки — по тексту после последней правки.
    fresh: bool,
    /// Грамматики нет: не разбирать.
    dead: bool,
    /// Правка, ждущая новых цветов, и сколько ждала прошлая (мс; замер S7-35).
    pending_since: Option<Instant>,
    pub last_ms: Option<f32>,
    /// Сколько раз цвета догоняли текст (замер: каждый раз — новое `last_ms`).
    pub fresh_n: u64,
}

impl Hl {
    /// Разбор или запрос в работе.
    pub fn busy(&self) -> bool {
        self.busy.is_some()
    }

    /// Документ пересобран или закрыт: задача больше не нужна.
    pub(crate) fn cancel_job(&self) {
        self.cancel.cancel();
    }

    /// Правки до нового разбора: сдвиг отрезков, мест свёртки и посчитанного.
    fn shift(&mut self, log: &[TextEdit]) {
        if log.len() > SHIFT_MAX {
            self.drop_spans();
            return;
        }
        let mut sp: Vec<Span> = self.spans.to_vec();
        let mut fo: Vec<FoldAt> = self.folds.to_vec();
        for e in log {
            sp = shift_spans(&sp, e);
            fo.retain_mut(|f| shift_fold(f, e));
            self.covered = shift_range(&self.covered, e);
        }
        self.spans = sp.into();
        self.folds = fo.into();
    }

    fn drop_spans(&mut self) {
        self.spans = Arc::from(Vec::new());
        self.folds = Arc::from(Vec::new());
        self.covered = 0..0;
    }
}

/// Сдвиг позиции правкой: внутри убранного — к её началу.
fn shift_off(x: u64, e: &TextEdit) -> u64 {
    if x <= e.at {
        x
    } else if x >= e.at + e.old {
        x - e.old + e.new
    } else {
        e.at
    }
}

fn shift_range(r: &Range<u64>, e: &TextEdit) -> Range<u64> {
    shift_off(r.start, e)..shift_off(r.end, e).max(e.at.min(r.end))
}

/// Отрезки после правки: до неё — как были, после — сдвиг, набранное внутри отрезка —
/// в нём, задетое удалением — обрезается.
fn shift_spans(list: &[Span], e: &TextEdit) -> Vec<Span> {
    let (at, end) = (e.at, e.at + e.old);
    let mv = |x: u32| -> u32 { u32::try_from(u64::from(x) - e.old + e.new).unwrap_or(u32::MAX) };
    let mut out = Vec::with_capacity(list.len() + 1);
    for s in list {
        let (a, b) = (u64::from(s.start), u64::from(s.end));
        if b <= at {
            out.push(*s);
        } else if a >= end {
            out.push(Span {
                start: mv(s.start),
                end: mv(s.end),
                tag: s.tag,
            });
        } else if e.old == 0 {
            out.push(Span {
                start: s.start,
                end: mv(s.end),
                tag: s.tag,
            });
        } else {
            if a < at {
                out.push(Span {
                    start: s.start,
                    end: u32::try_from(at).unwrap_or(u32::MAX),
                    tag: s.tag,
                });
            }
            if b > end {
                out.push(Span {
                    start: u32::try_from(at + e.new).unwrap_or(u32::MAX),
                    end: mv(s.end),
                    tag: s.tag,
                });
            }
        }
    }
    out
}

/// Место свёртки после правки; задетое — `false`.
fn shift_fold(f: &mut FoldAt, e: &TextEdit) -> bool {
    let end = e.at + e.old;
    if end <= f.head && !(e.old == 0 && e.at == f.head) {
        f.head = f.head - e.old + e.new;
        f.start = f.start - e.old + e.new;
        f.end = f.end - e.old + e.new;
        true
    } else {
        e.at >= f.end
    }
}

/// Что нужно задаче.
struct Job {
    doc: DocId,
    pass: u64,
    edits: u64,
    /// Снимок, если нужен разбор (иначе — только запрос по прежнему).
    snap: Option<TextSnap>,
    prev: Option<Arc<Parsed>>,
    range: Range<u64>,
    lang: &'static str,
    feed: Feed,
}

/// Итог задачи.
#[derive(Debug)]
enum Out {
    /// Грамматики нет.
    Dead,
    Done {
        parsed: Arc<Parsed>,
        spans: Arc<[Span]>,
        folds: Arc<[FoldAt]>,
    },
}

fn work(job: &Job, stop: &dyn Fn() -> bool) -> Result<Out, Cancelled> {
    let Some(g) = grammar(job.lang) else {
        return Ok(Out::Dead);
    };
    let parsed = match (&job.snap, &job.prev) {
        (Some(snap), prev) => {
            let len = usize::try_from(snap.len()).map_err(|_| Cancelled)?;
            let mut raw = vec![0u8; len];
            let n = snap.read_at(0, &mut raw).map_err(|e| {
                tracing::warn!(error = %e, "editor: highlight read failed");
                Cancelled
            })?;
            raw.truncate(n);
            if stop() {
                return Err(Cancelled);
            }
            let text = feed_text(raw, job.feed);
            Arc::new(parse(&g, prev.as_deref(), text, job.feed, stop)?)
        }
        (None, Some(p)) => p.clone(),
        (None, None) => return Err(Cancelled),
    };
    let r = usize::try_from(job.range.start).unwrap_or(usize::MAX)
        ..usize::try_from(job.range.end).unwrap_or(usize::MAX);
    let sp = spans(&g, &parsed, r.clone(), stop)?;
    let fo = folds(&parsed, r);
    Ok(Out::Done {
        parsed,
        spans: sp.into(),
        folds: fo.into(),
    })
}

fn spawn(rt: &Runtime, job: Job, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "edit.syntax", cancel, move |cx| {
        let _span = tracing::debug_span!("edit.syntax", lang = job.lang).entered();
        let stop = || cx.cancel.is_cancelled();
        let Ok(out) = work(&job, &stop) else {
            return;
        };
        let msg = HlMsg {
            doc: job.doc,
            pass: job.pass,
            edits: job.edits,
            range: job.range.clone(),
            out,
        };
        let _ = cx.sink.send(Box::new(msg), &cx.cancel);
    });
}

/// Итог разбора → документ редактора.
#[derive(Debug)]
struct HlMsg {
    doc: DocId,
    pass: u64,
    edits: u64,
    range: Range<u64>,
    out: Out,
}

impl Msg for HlMsg {
    fn name(&self) -> &'static str {
        "edit.syntax"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(d) = view.editor_mut(self.doc) {
            d.hl_apply(*self, rt);
        }
    }
}

impl EditorDoc {
    /// Отрезки подсветки по байтам документа (сдвинуты правками), если она есть.
    pub fn hl_spans(&self) -> Option<&Arc<[Span]>> {
        self.hl.as_ref().map(|h| &h.spans).filter(|s| !s.is_empty())
    }

    /// Места свёртки видимой части.
    pub fn hl_folds(&self) -> &[FoldAt] {
        self.hl.as_ref().map_or(&[], |h| &h.folds)
    }

    /// `editor.highlight` (команда, конфиг): выключено — отрезки уходят, включено —
    /// разбор.
    pub(crate) fn set_highlight(&mut self, on: bool, rt: &Runtime) {
        if self.hl_on != on {
            self.hl_on = on;
            self.hl_sync(rt);
            self.bump();
        }
    }

    /// Подсветка включена и возможна: `editor.highlight`, язык, ≤ 16 МБ.
    fn hl_wanted(&self) -> bool {
        self.hl_on
            && self.lang.is_some()
            && !matches!(self.state, EdState::Loading { .. } | EdState::Failed)
            && self
                .engine
                .as_deref()
                .is_some_and(|e| e.len() <= MARKUP_MAX)
    }

    /// После действия, страниц, прокрутки: правки → сдвиг; текст или видимое ушли от
    /// посчитанного — задача (одна за раз).
    pub(crate) fn hl_sync(&mut self, rt: &Runtime) {
        let wanted = self.hl_wanted();
        let lang = self.lang;
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        let log = e.text.take_log();
        if !wanted {
            if let Some(h) = self.hl.take() {
                h.cancel_job();
                self.bump();
            }
            return;
        }
        let h = self.hl.get_or_insert_with(Hl::default);
        if h.dead {
            return;
        }
        match &log {
            Some(l) if l.is_empty() => {}
            Some(l) => {
                h.shift(l);
                h.fresh = false;
                h.pending_since.get_or_insert_with(Instant::now);
            }
            None => {
                h.drop_spans();
                h.fresh = false;
                h.pending_since.get_or_insert_with(Instant::now);
            }
        }
        if h.busy.is_some() {
            let keep = match (&mut h.since, &log) {
                (Some(s), Some(l)) if s.len() + l.len() <= SHIFT_MAX => {
                    s.extend_from_slice(l);
                    true
                }
                (None, _) => true,
                _ => false,
            };
            if !keep {
                h.since = None;
            }
        }
        let edits = e.text.edits();
        if h.busy.is_some() {
            // Правки сольются в следующую задачу (по итогу этой).
            return;
        }
        let vis = e.visible_range();
        let len = e.len();
        let parsed_now = h.parsed.as_ref().is_some_and(|(_, at)| *at == edits);
        let need = vis.start.saturating_sub(EDGE)..vis.end.saturating_add(EDGE).min(len);
        if parsed_now && h.fresh && h.covered.start <= need.start && need.end <= h.covered.end {
            return;
        }
        let Some(lang) = lang else {
            return;
        };
        let range = vis.start.saturating_sub(MARGIN)..vis.end.saturating_add(MARGIN).min(len);
        let cancel = self.cancel.child();
        h.pass += 1;
        h.cancel = cancel.clone();
        let prev = h.parsed.as_ref().map(|(p, _)| p.clone());
        h.busy = Some(edits);
        h.since = Some(Vec::new());
        let job = Job {
            doc: self.id,
            pass: h.pass,
            edits,
            snap: (!parsed_now).then(|| e.text.snap()),
            prev,
            range,
            lang,
            feed: Feed::of(e.enc.enc),
        };
        spawn(rt, job, cancel);
    }

    /// Итог задачи: свежий — на экран (сдвинутый правками, сделанными за время задачи);
    /// текст с тех пор менялся — следующая задача.
    fn hl_apply(&mut self, m: HlMsg, rt: &Runtime) {
        let Some(h) = self.hl.as_mut() else {
            return;
        };
        if m.pass != h.pass {
            return;
        }
        h.busy = None;
        match m.out {
            Out::Dead => {
                h.dead = true;
                h.drop_spans();
            }
            Out::Done {
                parsed,
                spans,
                folds,
            } => {
                h.parsed = Some((parsed, m.edits));
                match h.since.take() {
                    Some(s) if s.is_empty() => {
                        (h.spans, h.folds, h.covered) = (spans, folds, m.range);
                        h.fresh = true;
                        if let Some(t) = h.pending_since.take() {
                            h.last_ms = Some(t.elapsed().as_secs_f32() * 1000.0);
                            h.fresh_n += 1;
                        }
                    }
                    Some(s) if s.len() <= SINCE_SHIFT => {
                        (h.spans, h.folds, h.covered) = (spans, folds, m.range);
                        h.shift(&s);
                        h.fresh = false;
                    }
                    // Много правок или счёт потерян: итог устарел — на экране остаются
                    // сдвинутые отрезки (или ничего), следующая задача — сразу.
                    Some(_) | None => h.fresh = false,
                }
            }
        }
        self.bump();
        self.hl_sync(rt);
    }

    /// `editor.fold`: свернуть или развернуть место с заголовком на строке `head`
    /// (`None` — строка курсора, иначе ближайшее объемлющее курсор). `true` — изменилось.
    pub(crate) fn fold_toggle(&mut self, head: Option<u64>) -> bool {
        let cands = self.hl.as_ref().map(|h| h.folds.clone());
        let Some(e) = self.engine.as_deref_mut() else {
            return false;
        };
        let caret = e.sel.caret;
        let line = match head {
            Some(h) => h,
            None => {
                match crate::layout::line_start_before(&e.text.view(), caret, &e.seg_layout()) {
                    Ok(h) => h,
                    Err(_) => return false,
                }
            }
        };
        if e.text.folds.unfold(line) {
            self.bump();
            return true;
        }
        let cands = cands.unwrap_or_else(|| Arc::from(Vec::new()));
        let c = cands.iter().find(|f| f.head == line).or_else(|| {
            head.is_none()
                .then(|| {
                    cands
                        .iter()
                        .filter(|f| f.head <= caret && caret < f.end)
                        .max_by_key(|f| f.head)
                })
                .flatten()
        });
        let Some(c) = c.copied() else {
            return false;
        };
        e.text.folds.fold(Fold {
            head: c.head,
            hide: c.start..c.end,
        });
        if (c.start..c.end).contains(&caret) {
            e.sel = Selections::at(c.head);
        }
        let _ = e.reveal();
        self.cursor = e.cursor();
        self.bump();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Tag;

    fn sp(a: u32, b: u32) -> Span {
        Span {
            start: a,
            end: b,
            tag: Tag::String,
        }
    }

    #[test]
    fn spans_follow_edits() {
        let list = [sp(0, 4), sp(10, 20), sp(30, 32)];
        // Набор внутри отрезка — в нём; после — сдвиг.
        let e = TextEdit {
            at: 12,
            old: 0,
            new: 3,
        };
        assert_eq!(
            shift_spans(&list, &e),
            vec![sp(0, 4), sp(10, 23), sp(33, 35)]
        );
        // Набор на границе — не в отрезке.
        let e = TextEdit {
            at: 4,
            old: 0,
            new: 1,
        };
        assert_eq!(
            shift_spans(&list, &e),
            vec![sp(0, 4), sp(11, 21), sp(31, 33)]
        );
        // Удаление через границу — обрезка.
        let e = TextEdit {
            at: 2,
            old: 10,
            new: 0,
        };
        assert_eq!(
            shift_spans(&list, &e),
            vec![sp(0, 2), sp(2, 10), sp(20, 22)]
        );
    }

    #[test]
    fn fold_places_follow_edits() {
        let mut f = FoldAt {
            head: 10,
            start: 20,
            end: 40,
        };
        assert!(shift_fold(
            &mut f,
            &TextEdit {
                at: 0,
                old: 0,
                new: 2
            }
        ));
        assert_eq!((f.head, f.start, f.end), (12, 22, 42));
        assert!(!shift_fold(
            &mut f,
            &TextEdit {
                at: 30,
                old: 1,
                new: 0
            }
        ));
        let mut g = FoldAt {
            head: 10,
            start: 20,
            end: 40,
        };
        assert!(shift_fold(
            &mut g,
            &TextEdit {
                at: 50,
                old: 1,
                new: 0
            }
        ));
        assert_eq!(
            shift_range(
                &(5..50),
                &TextEdit {
                    at: 10,
                    old: 5,
                    new: 0
                }
            ),
            5..45
        );
    }
}

#[cfg(test)]
#[path = "hl_tests.rs"]
mod flow_tests;
