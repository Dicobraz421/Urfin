//! Поиск, замена и переход в редакторе (S7-18, S7 §5.2): строки «Найти» / «Заменить на» и
//! «Перейти к строке» — [`super::find::FindBar`] над статусом, пока фокус Core —
//! `Focus::Overlay(Find)`. Движок — `urfin_view::edit::find` (`regex-cursor` по Rope).
//!
//! Поиск идёт в фоне по снимку текста (клон Rope дёшев) при каждой смене образца или
//! текста; новый поиск отменяет прежний. Итог — до 10 000 совпадений: подсветка
//! `TextDecorationCollection` gpui-kit (`highlight`, текущее — заливка `accent.fill`,
//! текст `accent.on`), метки строк на мини-карте, счётчик «N из M». Совпадение становится
//! выделением `EditorState` (без фокуса — он у поля строки).
//!
//! «Заменить» — `EditorState::replace` выделенного совпадения (шаблон с группами `$1`),
//! «Заменить все» — новый текст целиком (в фоне, если длинный) одной правкой
//! `replace_all`: одна undo-группа.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::base::input::{
    EditorState, Position, Rope, RopeExt as _, TextDecoration, TextDecorationCollection,
};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, HighlightStyle, Hsla, Window};
use urfin_view::edit::find::{Finder, Found, Query};
use urfin_view::{DocId, Goto, SearchError};

use super::super::CoreHost;
use super::find::{FindBar, Flags};

/// Текст короче — «Заменить все» прямо в кадре, длиннее — в фоне.
const INLINE_REPLACE: usize = 1 << 20;

/// Куда встать, когда придёт итог поиска.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Jump {
    /// Образец сменился: первое совпадение от `anchor` (с переносом на начало).
    First(usize),
    /// Enter / Shift+F7: следующее (или предыдущее) от выделения.
    Next { back: bool },
}

/// Итог поиска документа.
pub(super) struct Hits {
    pub doc: DocId,
    pub q: Query,
    /// Версия текста (`DocView::ver`), по которой искали.
    pub ver: u64,
    pub finder: Option<Arc<Finder>>,
    pub error: Option<SearchError>,
    pub found: Arc<Found>,
}

/// Идущий поиск.
struct Job {
    doc: DocId,
    q: Query,
    ver: u64,
    stop: Arc<AtomicBool>,
}

/// Готовый текст «Заменить все».
pub(super) struct Replaced {
    pub doc: DocId,
    pub ver: u64,
    pub text: String,
    pub n: usize,
}

/// Подсветка совпадений документа: текущее (первым — его стиль главнее) и все.
pub(super) struct Decos {
    cur: TextDecorationCollection,
    all: TextDecorationCollection,
    /// Что показано: (версия итога, тема) и текущее.
    shown: Option<(usize, u64)>,
    cur_shown: Option<Range<usize>>,
}

/// Поиск и переход фичи `Editor`.
#[derive(Default)]
pub(super) struct Find {
    pub bar: Option<FindBar>,
    /// Последний образец и замена: F7 снова, Shift+F7 после закрытия строки.
    pub last: Option<Query>,
    pub last_repl: String,
    /// Образец строки, который уже видели (смена → `Jump::First`).
    seen: Option<Query>,
    pub jump: Option<Jump>,
    /// Действия строки, которым нужно окно: замена, Tab.
    pub acts: Vec<super::find::BarAction>,
    job: Option<Job>,
    pub hits: Option<Hits>,
    /// Итог фонового поиска.
    pub result: Option<Hits>,
    /// «Заменено: N» на месте счётчика — до смены образца.
    pub note: Option<String>,
    /// «Заменить все» в фоне и его итог.
    pub replacing: bool,
    pub replaced: Option<Replaced>,
    /// Счётчик итогов (подсветка сравнивает).
    pub seq: usize,
    /// Сцена `--screens`: тексты новой строки.
    pub demo: Option<(String, Option<String>)>,
}

/// Образец строки: текст и флажки.
pub(super) fn query(bar: &FindBar) -> Query {
    Query {
        text: bar.text(),
        case: bar.flags.case,
        word: bar.flags.word,
        regex: bar.flags.regex,
    }
}

/// Флажки строки по образцу.
pub(super) fn flags_of(q: &Query) -> Flags {
    Flags {
        case: q.case,
        word: q.word,
        regex: q.regex,
        hex: false,
    }
}

/// Индекс совпадения, равного выделению.
pub(super) fn ordinal(ranges: &[Range<usize>], sel: &Range<usize>) -> Option<usize> {
    let i = ranges.partition_point(|r| r.start < sel.start);
    ranges.get(i).filter(|r| *r == sel).map(|_| i)
}

/// Первое совпадение от `anchor`; нет — первое в документе.
pub(super) fn first_from(ranges: &[Range<usize>], anchor: usize) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    let i = ranges.partition_point(|r| r.start < anchor);
    Some(if i < ranges.len() { i } else { 0 })
}

/// Следующее (`back` — предыдущее) от выделения `sel` с переносом; `None` вперёд — за
/// концом списка (`more`: дальше ищет `find_from`).
pub(super) fn step(
    ranges: &[Range<usize>],
    sel: &Range<usize>,
    back: bool,
    more: bool,
) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    if back {
        // Последнее, кончившееся до начала выделения (и не оно само).
        let i = ranges.partition_point(|r| r.start < sel.start);
        return Some(if i == 0 { ranges.len() - 1 } else { i - 1 });
    }
    // Первое от конца выделения; пустое совпадение на месте пустого выделения — дальше.
    let i = ranges.partition_point(|r| r.start < sel.end);
    let i = match ranges.get(i) {
        Some(r) if r == sel => i + 1,
        _ => i,
    };
    if i < ranges.len() {
        Some(i)
    } else if more {
        None
    } else {
        Some(0)
    }
}

/// Куда ведёт переход (байт текста): строка[:колонка] (с 1), смещение, доля, сдвиг строк.
pub(super) fn goto_offset(g: Goto, rope: &Rope, cur_line: u32) -> usize {
    let lines = super::editor::line_count(rope).max(1);
    let last = u32::try_from(lines - 1).unwrap_or(u32::MAX);
    let to_line = |n: u64| u32::try_from(n).unwrap_or(u32::MAX).min(last);
    let pos = match g {
        Goto::Number { n, col } => Position::new(
            to_line(n.saturating_sub(1)),
            u32::try_from(col.unwrap_or(1).saturating_sub(1)).unwrap_or(u32::MAX),
        ),
        Goto::Offset(b) => {
            let mut b = usize::try_from(b).unwrap_or(usize::MAX).min(rope.len());
            while b > 0 && !rope.is_char_boundary(b) {
                b -= 1;
            }
            return b;
        }
        Goto::Percent(p) => Position::new(to_line(u64::from(last) * u64::from(p) / 10_000), 0),
        Goto::Rel(d) => {
            let l = i64::from(cur_line).saturating_add(d).max(0);
            Position::new(to_line(u64::try_from(l).unwrap_or(0)), 0)
        }
    };
    // Колонка за концом строки — конец строки (без `\r`).
    let line = pos.line as usize;
    let len = rope
        .slice_line(line)
        .to_string()
        .trim_end_matches('\r')
        .chars()
        .count();
    let col = (pos.character as usize).min(len);
    rope.position_to_offset(&Position::new(pos.line, u32::try_from(col).unwrap_or(0)))
}

impl Find {
    /// Строка документа `id` открыта и фокус у неё.
    pub(super) fn bar_for(&self, id: DocId, find_focus: bool) -> Option<&FindBar> {
        self.bar.as_ref().filter(|b| b.owner == id.0 && find_focus)
    }

    /// Итог для документа `id` и версии текста `ver` (свежий).
    pub(super) fn fresh(&self, id: DocId, ver: u64) -> Option<&Hits> {
        self.hits.as_ref().filter(|h| h.doc == id && h.ver == ver)
    }

    /// Идёт поиск (сцене — ждать).
    pub(super) fn busy(&self) -> bool {
        self.job.is_some() || self.replacing || !self.acts.is_empty() || self.result.is_some()
    }

    /// Образец строки сменился: прежние замечания уходят, выделение ждёт первое
    /// совпадение от `anchor`.
    pub(super) fn track_query(&mut self, q: &Query, anchor: usize) {
        if self.seen.as_ref() == Some(q) {
            return;
        }
        self.seen = Some(q.clone());
        self.note = None;
        if !q.text.is_empty() {
            self.jump = Some(Jump::First(anchor));
        }
    }

    /// Строка закрылась: подсветка уходит, образец помнится.
    pub(super) fn closed(&mut self) {
        self.seen = None;
        self.note = None;
        self.acts.clear();
        if let Some(j) = self.job.take() {
            j.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Нужен поиск `q` по документу `id` версии `ver`: запустить, если такого нет.
    pub(super) fn want(
        &mut self,
        id: DocId,
        q: &Query,
        ver: u64,
        rope: &Rope,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        if self
            .hits
            .as_ref()
            .is_some_and(|h| h.doc == id && h.ver == ver && h.q == *q)
            || self
                .job
                .as_ref()
                .is_some_and(|j| j.doc == id && j.ver == ver && j.q == *q)
        {
            return;
        }
        if let Some(j) = self.job.take() {
            j.stop.store(true, Ordering::Relaxed);
        }
        let finder = match Finder::new(q) {
            Ok(f) => Arc::new(f),
            Err(e) => {
                self.hits = Some(Hits {
                    doc: id,
                    q: q.clone(),
                    ver,
                    finder: None,
                    error: Some(e),
                    found: Arc::default(),
                });
                self.seq += 1;
                self.jump = None;
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        self.job = Some(Job {
            doc: id,
            q: q.clone(),
            ver,
            stop: Arc::clone(&stop),
        });
        let rope = rope.clone();
        let q = q.clone();
        let task = cx.background_spawn(async move {
            let found = finder.find_all(&rope, &stop);
            (finder, found, stop.load(Ordering::Relaxed))
        });
        cx.spawn_in(window, async move |this, cx| {
            let (finder, found, stopped) = task.await;
            let _ = this.update(cx, |h, cx| {
                if stopped {
                    return;
                }
                if let Some(e) = h.features.get_mut::<super::Editor>() {
                    let f = &mut e.find;
                    if f.job
                        .as_ref()
                        .is_some_and(|j| j.doc == id && j.ver == ver && j.q == q)
                    {
                        f.job = None;
                        f.result = Some(Hits {
                            doc: id,
                            q,
                            ver,
                            finder: Some(finder),
                            error: None,
                            found: Arc::new(found),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Итог фона → `hits`.
    pub(super) fn take_result(&mut self) {
        if let Some(r) = self.result.take() {
            self.hits = Some(r);
            self.seq += 1;
        }
    }

    /// Встать на совпадение по `jump`, если итог свежий. `true` — выделение сдвинулось.
    pub(super) fn do_jump(
        &mut self,
        id: DocId,
        ver: u64,
        state: &Entity<EditorState>,
        cx: &mut Context<CoreHost>,
    ) -> bool {
        let Some(jump) = self.jump else {
            return false;
        };
        if self.fresh(id, ver).is_none() {
            return false;
        }
        self.jump = None;
        let Some(h) = self.hits.as_ref() else {
            return false;
        };
        let ranges = &h.found.ranges;
        let st = state.read(cx);
        let sel = st.selected_range();
        let target = match jump {
            Jump::First(anchor) => first_from(ranges, anchor).map(|i| ranges[i].clone()),
            Jump::Next { back } => match step(ranges, &sel, back, h.found.more) {
                Some(i) => Some(ranges[i].clone()),
                // За 10 000-м совпадением — дальше от выделения, иначе — первое.
                None => h
                    .finder
                    .as_ref()
                    .and_then(|f| f.find_from(st.text(), sel.end.max(sel.start + 1)))
                    .or_else(|| ranges.first().cloned()),
            },
        };
        let Some(r) = target else {
            return false;
        };
        state.update(cx, |st, cx| st.set_selected_range(r, cx));
        true
    }

    /// Подсветка совпадений документа (`show` — строка открыта): все и текущее.
    #[allow(clippy::too_many_arguments, reason = "состояние документа и тема")]
    pub(super) fn paint(
        &self,
        decos: &mut Option<Decos>,
        id: DocId,
        show: bool,
        state: &Entity<EditorState>,
        colors: [Hsla; 3],
        theme_gen: u64,
        cx: &mut Context<CoreHost>,
    ) {
        let hits = self.hits.as_ref().filter(|h| h.doc == id && show);
        if hits.is_none() && decos.as_ref().is_none_or(|d| d.shown.is_none()) {
            return;
        }
        let [hl, fill, on] = colors;
        let d = decos.get_or_insert_with(|| {
            state.update(cx, |st, cx| {
                let cur = st.create_decorations_collection(Vec::new(), cx);
                let all = st.create_decorations_collection(Vec::new(), cx);
                Decos {
                    cur,
                    all,
                    shown: None,
                    cur_shown: None,
                }
            })
        });
        let Some(h) = hits else {
            d.all.clear(cx);
            d.cur.clear(cx);
            d.shown = None;
            d.cur_shown = None;
            return;
        };
        let key = (self.seq, theme_gen);
        if d.shown != Some(key) {
            let style = HighlightStyle {
                background_color: Some(hl),
                ..HighlightStyle::default()
            };
            let all = h
                .found
                .ranges
                .iter()
                .filter(|r| !r.is_empty())
                .map(|r| TextDecoration::new(r.clone(), style))
                .collect();
            d.all.set(all, cx);
            d.shown = Some(key);
            d.cur_shown = None;
        }
        let sel = state.read(cx).selected_range();
        let cur = ordinal(&h.found.ranges, &sel)
            .map(|_| sel)
            .filter(|r| !r.is_empty());
        if cur != d.cur_shown {
            match &cur {
                Some(r) => d.cur.set(
                    vec![TextDecoration::new(
                        r.clone(),
                        HighlightStyle {
                            color: Some(on),
                            background_color: Some(fill),
                            ..HighlightStyle::default()
                        },
                    )],
                    cx,
                ),
                None => d.cur.clear(cx),
            }
            d.cur_shown = cur;
        }
    }

    /// «Заменить все»: короткий текст — сразу, длинный — в фоне (итог — `replaced`).
    pub(super) fn replace_all(
        &mut self,
        (id, ver): (DocId, u64),
        finder: Arc<Finder>,
        tpl: String,
        rope: &Rope,
        window: &mut Window,
        cx: &mut Context<CoreHost>,
    ) {
        if rope.len() <= INLINE_REPLACE {
            let (text, n) = finder.replace_all(rope, &tpl);
            self.replaced = Some(Replaced {
                doc: id,
                ver,
                text,
                n,
            });
            return;
        }
        self.replacing = true;
        let rope = rope.clone();
        let task = cx.background_spawn(async move { finder.replace_all(&rope, &tpl) });
        cx.spawn_in(window, async move |this, cx| {
            let (text, n) = task.await;
            let _ = this.update(cx, |h, cx| {
                if let Some(e) = h.features.get_mut::<super::Editor>() {
                    e.find.replacing = false;
                    e.find.replaced = Some(Replaced {
                        doc: id,
                        ver,
                        text,
                        n,
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Счётчик строки: «N из M», «M совпадений», «Не найдено», ошибка regex, «Заменено».
    pub(super) fn counter(&self, id: DocId, sel: &Range<usize>) -> (String, bool) {
        let i18n = urfin_i18n::current();
        if let Some(n) = &self.note {
            return (n.clone(), false);
        }
        let Some(h) = self.hits.as_ref().filter(|h| h.doc == id) else {
            return (String::new(), false);
        };
        let mut a = urfin_i18n::FluentArgs::new();
        if let Some(e) = &h.error {
            return match e {
                SearchError::Empty => (String::new(), false),
                e => {
                    a.set("error", e.to_string());
                    (i18n.fmt("ui-find-bad-regex", &a), true)
                }
            };
        }
        let r = &h.found.ranges;
        if r.is_empty() {
            return (i18n.get("ui-find-none"), true);
        }
        a.set("total", r.len() as u64);
        let key = if let Some(i) = ordinal(r, sel) {
            a.set("n", i as u64 + 1);
            if h.found.more {
                "ui-find-of-more"
            } else {
                "ui-find-of"
            }
        } else if h.found.more {
            "ui-find-count-more"
        } else {
            "ui-find-count"
        };
        (i18n.fmt(key, &a), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_wraps() {
        let r = vec![2..4, 10..12, 20..22];
        assert_eq!(first_from(&r, 0), Some(0));
        assert_eq!(first_from(&r, 10), Some(1));
        assert_eq!(first_from(&r, 21), Some(0), "с начала");
        assert_eq!(step(&r, &(10..12), false, false), Some(2));
        assert_eq!(step(&r, &(20..22), false, false), Some(0));
        assert_eq!(step(&r, &(20..22), false, true), None, "дальше — find_from");
        assert_eq!(step(&r, &(10..12), true, false), Some(0));
        assert_eq!(step(&r, &(2..4), true, false), Some(2));
        assert_eq!(step(&r, &(5..5), false, false), Some(1));
        assert_eq!(ordinal(&r, &(10..12)), Some(1));
        assert_eq!(ordinal(&r, &(10..11)), None);
        // Пустые совпадения (`^`) — не стоять на месте.
        let e = vec![0..0, 5..5];
        assert_eq!(step(&e, &(0..0), false, false), Some(1));
    }

    #[test]
    fn goto_targets() {
        let rope = Rope::from("ab\r\ncdef\nxyz\n");
        let at = |g| goto_offset(g, &rope, 1);
        assert_eq!(at(Goto::Number { n: 2, col: Some(3) }), 6);
        assert_eq!(
            at(Goto::Number {
                n: 1,
                col: Some(99)
            }),
            2,
            "конец строки без \\r"
        );
        assert_eq!(at(Goto::Number { n: 99, col: None }), 9, "последняя строка");
        assert_eq!(at(Goto::Rel(1)), 9);
        assert_eq!(at(Goto::Rel(-5)), 0);
        assert_eq!(at(Goto::Percent(10_000)), 9);
        assert_eq!(at(Goto::Offset(5)), 5);
    }
}
