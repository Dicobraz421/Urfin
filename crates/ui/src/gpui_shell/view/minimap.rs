//! Мини-карта редактора (S7-33, S7 §5.3, ADR-0023 §7, `screen-editor.png`): 88 px
//! справа, черта `border` слева; строка — 3 px штрихами слов (1 px на колонку), шаг 4;
//! рамка видимой части. Окно строк вокруг видимой части — из кэша страниц документа (как
//! VS Code на длинных файлах): доля прокрутки текста = доля прокрутки карты, на длинном
//! документе окно едет пропорционально; файл целиком не читается. Клик — `ScrollTo` на
//! эту строку. Штрих слова — цветом роли подсветки под его первым символом (S7-35), без
//! подсветки — `text.tertiary`, всё — с прозрачностью 55 %. Строка совпадения поиска —
//! штрихом 44 `highlight` вместо слов, текущего — `accent`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::{Bounds, canvas, fill, outline, point, px, size};
use smallvec::SmallVec;
use urfin_view::EditorDoc;
use urfin_view::edit::input::Engine;
use urfin_view::edit::piece::{TextEdit, TextView};
use urfin_view::layout::{LayoutSpec, RowText, VRow, render_row, row_up, rows_down};
use urfin_view::syntax::{Span, Tag, spans_in};

use super::super::theme::Theme;
use super::syntax::SynPalette;
use crate::tokens::Role;

/// Ширина карты.
pub(super) const W: f32 = 88.0;
/// Шаг строки и высота штриха.
const PITCH: f32 = 4.0;
const BAR_H: f32 = 3.0;
/// Поле сверху и прозрачность штрихов.
const PAD_TOP: f32 = 6.0;
const INK: f32 = 0.55;
/// Штрих строки совпадения и поля рамки видимой части.
const HIT_W: f32 = 44.0;
const FRAME_X: f32 = 4.0;
const FRAME_Y: f32 = 2.0;
/// Поле слева и пикселей на колонку.
const PAD: f32 = 8.0;
const COL: f32 = 1.0;
/// Колонок строки на карте не больше.
const MAX_COLS: u32 = 200;

/// Первая строка окна карты: видимые строки `[a, b)` из `total`, на карту влезает `fit`.
pub(super) fn first_line(a: u64, b: u64, total: u64, fit: u64) -> u64 {
    if total <= fit {
        return 0;
    }
    let vis = b.saturating_sub(a).max(1);
    if total <= vis {
        return 0;
    }
    // Доля прокрутки текста = доля прокрутки карты.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let m0 = (a as f64 / (total - vis) as f64 * (total - fit) as f64).round() as u64;
    m0.min(total - fit)
}

/// Штрихи строки: (колонка, длина) непробельных кусков (табы уже раскрыты раскладкой).
fn runs(line: &str) -> SmallVec<[(u16, u16); 8]> {
    let mut out = SmallVec::new();
    let mut start: Option<u16> = None;
    let mut col = 0u16;
    for ch in line.chars() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, col - s));
            }
        } else if start.is_none() {
            start = Some(col);
        }
        col += 1;
    }
    if let Some(s) = start {
        out.push((s, col - s));
    }
    out
}

/// Что определяет окно: правки, верх экрана, высота карты, строк в документе, подсветка.
type LinesKey = (u64, u64, u32, u64, u64, usize);

/// Строки окна карты: начало каждой (смещение документа) и штрихи.
#[derive(Default)]
pub(super) struct Lines {
    key: Option<LinesKey>,
    pub starts: Vec<u64>,
    /// Конец последней строки окна.
    pub end: u64,
    /// В окне есть ряды длинной строки: их начала — по разрезам, сдвиг их не описывает.
    cut: bool,
    bars: Vec<SmallVec<[(u16, u16, Tag); 8]>>,
}

/// Что сделала с окном правка внутри одной строки.
#[derive(Debug, PartialEq, Eq)]
enum Edited {
    /// Строку `k` нарисовать заново, те, что ниже, уже сдвинуты.
    Row(usize),
    /// Правка ниже окна.
    Below,
}

impl Lines {
    /// Правка без новых и убранных строк (их число то же): окно сдвинуто, штрихи заново
    /// нужны только её строке. `None` — окно собирать заново.
    fn edit(&mut self, e: TextEdit) -> Option<Edited> {
        let first = *self.starts.first()?;
        if e.at >= self.end {
            return Some(Edited::Below);
        }
        if self.cut || e.at < first {
            return None;
        }
        let k = self.starts.partition_point(|s| *s <= e.at) - 1;
        let next = self.starts.get(k + 1).copied().unwrap_or(self.end);
        if e.at + e.old >= next {
            return None;
        }
        for s in &mut self.starts[k + 1..] {
            *s = *s + e.new - e.old;
        }
        self.end = self.end + e.new - e.old;
        Some(Edited::Row(k))
    }
}

/// Состояние карты между кадрами: строки окна и где карта нарисована (клик).
#[derive(Default)]
pub(super) struct Map {
    pub lines: Rc<RefCell<Lines>>,
    /// Верх первой строки (px окна).
    pub top: Rc<std::cell::Cell<f32>>,
    scratch: RowText,
}

impl Map {
    /// Штрихи ряда `r`.
    fn bars(
        &mut self,
        v: &TextView<'_>,
        r: &VRow,
        wide: &LayoutSpec,
        syn: Option<&Arc<[Span]>>,
    ) -> SmallVec<[(u16, u16, Tag); 8]> {
        if render_row(v, r, 0, wide, &mut self.scratch).is_err() {
            return SmallVec::new();
        }
        let rt = &self.scratch;
        #[allow(clippy::cast_possible_truncation, reason = "символов — сотни")]
        let nch = rt.text.chars().count() as u32;
        let (a, z) = super::rows::shown_bytes(rt, nch, r);
        let mut list = syn.map_or(&[][..], |l| spans_in(l, a, z));
        runs(&rt.text)
            .into_iter()
            .map(|(c, n)| {
                // Слова и отрезки — по возрастанию.
                let at = rt.byte_at(u32::from(c));
                let k = at.map_or(0, |b| list.partition_point(|s| s.end <= b));
                list = &list[k..];
                let tag = list
                    .first()
                    .filter(|s| at.is_some_and(|b| s.start <= b))
                    .map_or(Tag::Plain, |s| s.tag);
                (c, n, tag)
            })
            .collect()
    }

    /// Нажатие: то же окно, та же раскладка, одна правка внутри строки — окно не собирается
    /// заново (десятки рядов на кадр). `false` — собирать.
    fn patch(&mut self, e: &Engine, key: LinesKey) -> bool {
        let Some(old) = self.lines.borrow().key else {
            return false;
        };
        let same = (old.1, old.2, old.3, old.4, old.5) == (key.1, key.2, key.3, key.4, key.5);
        if !same || old.0 + 1 != key.0 {
            return false;
        }
        let Some(ed) = e.text.last_edit(key.0) else {
            return false;
        };
        let mut l = self.lines.borrow_mut();
        let Some(what) = l.edit(ed) else {
            return false;
        };
        l.key = Some(key);
        let Edited::Row(k) = what else {
            return true;
        };
        let start = l.starts[k];
        drop(l);
        let seg = e.seg_layout();
        let v = e.text.view();
        let Ok(rows) = rows_down(&v, (start, 0), 1, &seg) else {
            return false;
        };
        let Some(r) = rows.first() else {
            return false;
        };
        let wide = LayoutSpec {
            width_cols: MAX_COLS,
            ..seg
        };
        let bars = self.bars(&v, r, &wide, None);
        self.lines.borrow_mut().bars[k] = bars;
        true
    }

    /// Строки окна вокруг видимой части: от верха экрана вверх на `back` логических
    /// строк, `fit` строк вниз — только по кэшам страниц (нет — сколько есть).
    pub(super) fn update(&mut self, d: &EditorDoc, first: Option<u64>, vis: u64, fit: u64) {
        let Some(e) = d.engine.as_deref() else {
            return;
        };
        let total = e.text.line_of(e.len()).get() + 1;
        let syn = d.hl_spans();
        let syn_key = syn.map_or(0, |a| Arc::as_ptr(a).cast::<()>() as usize);
        let key = (e.text.edits(), e.top, e.top_sub, fit, total, syn_key);
        if self.lines.borrow().key == Some(key) || (syn.is_none() && self.patch(e, key)) {
            return;
        }
        let a = first.unwrap_or_else(|| e.text.line_of(e.top).get());
        let m0 = first_line(a, a + vis, total, fit);
        let seg = e.seg_layout();
        let v = e.text.view();
        let mut p = (e.top, 0);
        for _ in m0..a {
            if p == (0, 0) {
                break;
            }
            match row_up(&v, p, &seg) {
                Ok(q) => p = q,
                Err(_) => break,
            }
        }
        let n = usize::try_from(fit).unwrap_or(usize::MAX);
        let rows = if let Ok(r) = rows_down(&v, p, n, &seg) {
            r
        } else {
            let (mut lo, mut hi) = (0usize, n.min(512));
            let mut best = SmallVec::new();
            while hi - lo > 1 {
                let mid = usize::midpoint(lo, hi);
                if let Ok(r) = rows_down(&v, p, mid, &seg) {
                    lo = mid;
                    best = r;
                } else {
                    hi = mid;
                }
            }
            best
        };
        let wide = LayoutSpec {
            width_cols: MAX_COLS,
            ..seg
        };
        let mut l = Lines {
            key: Some(key),
            starts: Vec::with_capacity(rows.len()),
            end: rows.last().map_or(e.top, |r| r.next),
            cut: rows.iter().any(|r| r.cut || r.cont),
            bars: Vec::with_capacity(rows.len()),
        };
        for r in &rows {
            l.starts.push(r.start);
            l.bars.push(self.bars(&v, r, &wide, syn));
        }
        *self.lines.borrow_mut() = l;
    }
}

/// Карта: черта слева, штрихи строк окна, рамка видимой части `vis` (байты), строки
/// совпадений `marks` (начала, по возрастанию), текущее — `cur`.
pub(super) fn map(
    m: &Map,
    vis: (u64, u64),
    theme: &Theme,
    marks: Option<Arc<[u64]>>,
    cur: Option<u64>,
) -> impl IntoElement {
    let lines = m.lines.clone();
    let top_cell = m.top.clone();
    let ink = theme.c(Role::TextTertiary);
    let frame_bg = theme.c(Role::CurrentLine);
    let frame_line = theme.c(Role::StrokeDivider);
    let mark = theme.c(Role::Highlight);
    let mark_cur = theme.c(Role::AccentFill);
    let pal = SynPalette::of(theme);
    canvas(
        |_, _, _| {},
        move |b: Bounds<gpui_kit::Pixels>, (), window, _cx| {
            let l = lines.borrow();
            let top = f32::from(b.top()) + PAD_TOP;
            top_cell.set(top);
            // Штрихов — тысячи: один слой вместо места каждого в `BoundsTree` (четверть
            // кадра редактора, S7-36).
            window.paint_layer(b, |window| {
                let line_of = |off: u64| l.starts.partition_point(|s| *s <= off).saturating_sub(1);
                // Рамка видимой части.
                if !l.starts.is_empty() && vis.1 > vis.0 {
                    let (i0, i1) = (line_of(vis.0), line_of(vis.1.saturating_sub(1)) + 1);
                    #[allow(clippy::cast_precision_loss, reason = "строки")]
                    let y0 = top + i0 as f32 * PITCH - FRAME_Y;
                    #[allow(clippy::cast_precision_loss, reason = "строки")]
                    let y1 = (top + i1 as f32 * PITCH + FRAME_Y).min(f32::from(b.bottom()));
                    if y1 > y0 {
                        let r = Bounds::new(
                            point(b.left() + px(FRAME_X), px(y0)),
                            size(b.size.width - px(2.0 * FRAME_X), px(y1 - y0)),
                        );
                        window.paint_quad(fill(r, frame_bg).corner_radii(px(3.0)));
                        window.paint_quad(
                            outline(r, frame_line, gpui_kit::BorderStyle::Solid)
                                .corner_radii(px(3.0)),
                        );
                    }
                }
                let max_cols = (f32::from(b.size.width) - PAD - 2.0) / COL;
                let marks: &[u64] = marks.as_deref().unwrap_or(&[]);
                for (k, bars) in l.bars.iter().enumerate() {
                    #[allow(clippy::cast_precision_loss, reason = "строки")]
                    let y = top + k as f32 * PITCH;
                    if y + PITCH > f32::from(b.bottom()) {
                        break;
                    }
                    let (a, z) = (l.starts[k], l.starts.get(k + 1).copied().unwrap_or(l.end));
                    let i = marks.partition_point(|m| *m < a);
                    let z = z.max(a + 1);
                    if marks.get(i).is_some_and(|m| *m < z) {
                        let on = cur.is_some_and(|c| (a..z).contains(&c));
                        window.paint_quad(fill(
                            Bounds::new(
                                point(b.left() + px(PAD), px(y)),
                                size(px(HIT_W), px(BAR_H)),
                            ),
                            if on { mark_cur } else { mark },
                        ));
                        continue;
                    }
                    for &(c, n, tag) in bars {
                        let (c, n) = (f32::from(c), f32::from(n));
                        if c >= max_cols {
                            break;
                        }
                        let w = n.min(max_cols - c) * COL;
                        window.paint_quad(fill(
                            Bounds::new(
                                point(b.left() + px(PAD + c * COL), px(y)),
                                size(px(w), px(BAR_H)),
                            ),
                            pal.color(tag).unwrap_or(ink).opacity(INK),
                        ));
                    }
                }
            });
        },
    )
    .size_full()
}

/// Строки, которые влезают на карту высотой `h`.
pub(super) fn fit(h: f32) -> u64 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let n = ((h - 2.0 * PAD_TOP) / PITCH).max(1.0) as u64;
    n
}

/// Начало строки карты под точкой `y` окна.
pub(super) fn line_at(m: &Map, y: f32) -> Option<u64> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строки"
    )]
    let k = ((y - m.top.get()).max(0.0) / PITCH) as usize;
    let l = m.lines.borrow();
    l.starts.get(k).or(l.starts.last()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_follows_scroll() {
        // Всё влезает — с начала.
        assert_eq!(first_line(0, 29, 41, 100), 0);
        // Начало и конец текста — начало и конец карты.
        assert_eq!(first_line(0, 40, 10_000, 150), 0);
        assert_eq!(first_line(9_960, 10_000, 10_000, 150), 9_850);
        let mid = first_line(4_980, 5_020, 10_000, 150);
        assert!((4_900..=4_960).contains(&mid), "{mid}");
        // 10 ГБ: окно рядом с видимой частью, а не с начала файла.
        let far = first_line(100_000_000, 100_000_040, 200_000_000, 150);
        assert!(far <= 100_000_000 && 100_000_000 - far < 150, "{far}");
    }

    fn lines(starts: &[u64], end: u64) -> Lines {
        Lines {
            starts: starts.to_vec(),
            end,
            bars: vec![SmallVec::new(); starts.len()],
            ..Lines::default()
        }
    }

    #[test]
    fn edit_in_a_row_shifts_the_rest() {
        let ins = |at, new| TextEdit { at, old: 0, new };
        let mut l = lines(&[10, 20, 30], 40);
        assert_eq!(l.edit(ins(25, 3)), Some(Edited::Row(1)));
        assert_eq!((l.starts.as_slice(), l.end), (&[10, 20, 33][..], 43));
        // На начале строки — её правка, конец перед разделителем — тоже.
        let mut l = lines(&[10, 20, 30], 40);
        assert_eq!(l.edit(ins(20, 1)), Some(Edited::Row(1)));
        assert_eq!(l.starts, [10, 20, 31]);
        let mut l = lines(&[10, 20, 30], 40);
        assert_eq!(l.edit(ins(19, 1)), Some(Edited::Row(0)));
        assert_eq!(l.starts, [10, 21, 31]);
        // Удаление внутри строки.
        let mut l = lines(&[10, 20, 30], 40);
        let del = TextEdit {
            at: 12,
            old: 3,
            new: 0,
        };
        assert_eq!(l.edit(del), Some(Edited::Row(0)));
        assert_eq!((l.starts.as_slice(), l.end), (&[10, 17, 27][..], 37));
        // Через границу строк, выше окна, ряды разрезов — собирать заново.
        let mut l = lines(&[10, 20, 30], 40);
        let cross = TextEdit {
            at: 18,
            old: 4,
            new: 4,
        };
        assert_eq!(l.edit(cross), None);
        assert_eq!(l.edit(ins(5, 1)), None);
        l.cut = true;
        assert_eq!(l.edit(ins(15, 1)), None);
        // Ниже окна — окно то же.
        assert_eq!(l.edit(ins(40, 1)), Some(Edited::Below));
        assert_eq!(l.starts, [10, 20, 30]);
    }

    #[test]
    fn word_runs() {
        assert_eq!(runs("  ab cd").as_slice(), &[(2, 2), (5, 2)]);
        assert_eq!(runs("    x").as_slice(), &[(4, 1)]);
        assert!(runs("   ").is_empty());
    }
}
