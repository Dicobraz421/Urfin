//! Элемент правки редактора (S7-32, S7-33; ADR-0023 §7, S7 §5.2, §5.3, §6): видимые
//! строки документа на движке одним `canvas` — полоса изменений 3, номера 40, поле
//! свёртки 14 (пусто до S7-35), строки 20 px; поверх — текущая строка, выделение,
//! совпадения поиска (текущее — `accent.fill` / `accent.on`), курсор и composition IME.
//!
//! Кадр читает только кэши страниц документа (`TextView: ByteView`): `rows_down` от
//! `top`/`top_sub`, `render_row`, на `Missing` — скелетон (страницы просит handler).
//! Кэш `ShapedLine` — по тексту строки и её раскраске: правка перестраивает только свои
//! строки. Ввод: клавиши правки — команды keymap (S7-38); набранный текст и commit IME —
//! `EntityInputHandler` элемента ([`EditInput`]) → `InboxMsg::Input`; composition
//! рисуется поверх строки с подчёркиванием и в документ не идёт; координаты UTF-16 —
//! окно строки курсора (±2048 единиц). Мышь — `InboxMsg::Caret` (клик, двойной — слово,
//! тройной — строка, drag с `extend`, автопрокрутка у края).

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, Font, Hsla, Pixels,
    Point, ShapedLine, SharedString, TextAlign, TextRun, UTF16Selection, UnderlineStyle,
    WeakEntity, Window, canvas, fill, point, px, size,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_view::edit::input::{Area, Sel};
use urfin_view::index::LineGuess;
use urfin_view::layout::{RowText, VRow, col_of, render_row, rows_down};
use urfin_view::{Change, DocId, EditorDoc, InboxMsg};

use super::super::CoreHost;
use super::super::theme::Theme;
use super::rows::{byte_idx, char_at, char_of, hash_of, runs, skeleton_w};
use crate::tokens::Role;

/// Строка текста (S7 §5.3).
pub(super) const ROW_H: f32 = 20.0;
/// Полоса изменений, номера, свёртка и отступ текста.
const W_BAR: f32 = 3.0;
const W_NUM: f32 = 40.0;
const W_FOLD: f32 = 14.0;
const PAD_TEXT: f32 = 13.0;
/// Номер — от правого края поля номеров.
const NUM_PAD_R: f32 = 6.0;
/// Справа от текста до края.
const PAD_RIGHT: f32 = 8.0;
/// Мигание курсора (только при фокусе).
pub(super) const BLINK: Duration = Duration::from_millis(500);
/// Шаг автопрокрутки, пока выделение тянут за край.
pub(super) const AUTO_STEP: Duration = Duration::from_millis(50);
/// Окно строки курсора для IME — ±единиц UTF-16 (ADR-0023 §7).
const IME_WIN: usize = 2048;
/// Подсветка совпадений — по видимым байтам не больше.
const HL_MAX: u64 = 64 << 10;
const HL_COUNT: usize = 4096;

/// Начало текста от левого края элемента.
pub(super) fn text_x(gutter: bool) -> f32 {
    if gutter {
        W_BAR + W_NUM + W_FOLD + PAD_TEXT
    } else {
        W_BAR + PAD_TEXT
    }
}

/// Колонок и строк в элементе `w`×`h` px.
pub(super) fn grid(w: f32, h: f32, cw: f32, gutter: bool) -> (u32, u32) {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "колонок и строк — до тысяч"
    )]
    (
        ((w - text_x(gutter) - PAD_RIGHT) / cw).floor().max(8.0) as u32,
        (h / ROW_H).floor().max(1.0) as u32,
    )
}

// ---------------------------------------------------------------- ввод (IME)

/// Состояние ввода, общее у элемента и фичи: окно строки курсора, composition, события
/// handler'у.
#[derive(Default)]
pub(super) struct Ime {
    pub doc: Option<DocId>,
    /// Окно строки курсора (UTF-8) и курсор, выделение в нём (UTF-16).
    pub line: String,
    pub caret16: usize,
    pub sel16: Option<Range<usize>>,
    /// Ключ окна: ревизия документа и курсор.
    pub key: Option<(u64, u64)>,
    /// Курсор в окне (px окна) и ширина колонки: кандидатное окно IME.
    pub caret_at: Option<Bounds<Pixels>>,
    pub cw: f32,
    /// Composition: текст и выделение в нём (UTF-16).
    pub marked: Option<(String, Option<Range<usize>>)>,
    /// События handler'у до следующего прохода.
    pub out: Vec<InboxMsg>,
}

pub(super) type ImeRc = Rc<RefCell<Ime>>;

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Байтовое смещение строки по смещению UTF-16 (дальше конца — конец).
fn byte_of16(s: &str, u: usize) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= u {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}

impl Ime {
    /// Набранный текст или commit IME: composition снята, текст — одним `Input`.
    pub(super) fn commit(&mut self, text: &str) {
        self.marked = None;
        if let Some(doc) = self.doc
            && !text.is_empty()
        {
            self.out.push(InboxMsg::Input {
                doc,
                text: text.into(),
            });
        }
    }

    /// Composition: только в элементе (в документ не идёт).
    pub(super) fn mark(&mut self, text: &str, sel: Option<Range<usize>>) {
        self.marked = (!text.is_empty()).then(|| (text.to_owned(), sel));
    }

    /// Окно строки курсора из документа (байты — из кэшей; страниц нет — пусто).
    pub(super) fn update(&mut self, d: &EditorDoc) {
        let Some(e) = d.engine.as_deref() else {
            return;
        };
        let key = (d.rev, e.sel.caret);
        if self.key == Some(key) && self.doc == Some(d.id) {
            return;
        }
        self.key = Some(key);
        self.doc = Some(d.id);
        let caret = e.sel.caret;
        let (text, caret16) = e
            .caret_seg()
            .ok()
            .and_then(|seg| {
                let all = e.bytes(seg.start..seg.end.max(caret))?;
                let k = usize::try_from(caret - seg.start).ok()?.min(all.len());
                let dec = |b: &[u8]| e.enc.enc.decode_without_bom_handling(b).0.into_owned();
                let head = dec(&all[..k]);
                let text = dec(&all);
                Some((text, utf16_len(&head)))
            })
            .unwrap_or_default();
        // Окно ±IME_WIN единиц вокруг курсора.
        let from16 = caret16.saturating_sub(IME_WIN);
        let a = byte_of16(&text, from16);
        let b = byte_of16(&text, caret16 + IME_WIN);
        text[a..b].clone_into(&mut self.line);
        self.caret16 = caret16 - utf16_len(&text[..a]);
        self.sel16 = e.sel.stream().and_then(|r| {
            let other = if r.start == caret { r.end } else { r.start };
            let n = usize::try_from(other.abs_diff(caret)).ok()?;
            // Выделение в окне — только в пределах строки курсора (оценка по символам).
            let k = self.line.len().min(n);
            let len16 = utf16_len(self.line.get(..k).unwrap_or(""));
            Some(if other < caret {
                self.caret16.saturating_sub(len16)..self.caret16
            } else {
                self.caret16..self.caret16 + len16
            })
        });
    }
}

/// Сущность элемента правки: фокус и ввод текста (`EntityInputHandler`).
pub(super) struct EditInput {
    pub focus: FocusHandle,
    ime: ImeRc,
    host: WeakEntity<CoreHost>,
}

impl EditInput {
    pub(super) fn new(ime: ImeRc, host: WeakEntity<CoreHost>, cx: &mut Context<EditInput>) -> Self {
        EditInput {
            focus: cx.focus_handle(),
            ime,
            host,
        }
    }

    /// События handler'у и перерисовка — проходом хоста после этого события.
    fn wake(&self, window: &mut Window, cx: &mut App) {
        let host = self.host.clone();
        window.defer(cx, move |window, cx| {
            if let Some(h) = host.upgrade() {
                h.update(cx, |h, cx| {
                    h.sync_inbox(window);
                    cx.notify();
                });
            }
        });
    }
}

impl EntityInputHandler for EditInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let ime = self.ime.borrow();
        let len = utf16_len(&ime.line);
        let r = range.start.min(len)..range.end.min(len);
        *adjusted = Some(r.clone());
        let (a, b) = (byte_of16(&ime.line, r.start), byte_of16(&ime.line, r.end));
        Some(ime.line[a..b].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let ime = self.ime.borrow();
        let range = if let Some((t, sel)) = &ime.marked {
            if let Some(s) = sel {
                ime.caret16 + s.start..ime.caret16 + s.end
            } else {
                let end = ime.caret16 + utf16_len(t);
                end..end
            }
        } else {
            ime.sel16.clone().unwrap_or(ime.caret16..ime.caret16)
        };
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let ime = self.ime.borrow();
        let (t, _) = ime.marked.as_ref()?;
        Some(ime.caret16..ime.caret16 + utf16_len(t))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ime.borrow_mut().marked = None;
        self.wake(window, cx);
    }

    /// Набранный текст и commit IME — в документ (`InboxMsg::Input`); диапазон окна не
    /// нужен: заменяется выделение документа (composition в документ не попадала).
    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime.borrow_mut().commit(text);
        self.wake(window, cx);
    }

    /// Composition: только в элементе, рисуется поверх строки.
    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime.borrow_mut().mark(new_text, new_selected_range);
        self.wake(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let ime = self.ime.borrow();
        let at = ime.caret_at?;
        #[allow(clippy::cast_precision_loss, reason = "колонки — сотни")]
        let dx = (range.start as f32 - ime.caret16 as f32) * ime.cw;
        Some(Bounds::new(
            point(at.origin.x + px(dx), at.origin.y),
            at.size,
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

// ---------------------------------------------------------------- кадр

/// Раскраска строки (часть ключа кэша).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    text: u64,
    /// Символы текущего совпадения (текст `accent.on`).
    cur: Option<(u32, u32)>,
    style: u64,
}

/// Строка текста и её раскладка (hit-test мыши).
pub(super) struct Shaped {
    line: ShapedLine,
    rt: RowText,
}

/// Подсветка в строке: `(x0, x1, вид)` от начала текста.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Sel,
    /// Блочное выделение: прямоугольник по колонкам, без скругления (ряды смыкаются).
    Block,
    Hit,
    Cur,
}

/// Строка кадра.
struct Line {
    row: VRow,
    shaped: Option<Rc<Shaped>>,
    num: Option<ShapedLine>,
    change: Option<Change>,
    /// Над строкой удалены строки (черта).
    deleted: bool,
    current: bool,
    marks: SmallVec<[(f32, f32, Mark); 4]>,
}

/// Кадр элемента.
pub(super) struct Screen {
    lines: Vec<Line>,
    /// Курсор: строка кадра и `x` от начала текста.
    caret: Option<(usize, f32)>,
    /// Composition IME у курсора.
    marked: Option<ShapedLine>,
    pub cw: f32,
    gutter: bool,
    overwrite: bool,
    show_caret: bool,
    /// Логических строк на экране (мини-карта): номер первой и сколько.
    pub first_line: Option<LineGuess>,
    pub line_count: u64,
    /// Видимые байты.
    pub from: u64,
    pub to: u64,
}

/// Что под мышью: строки прошлого кадра и где они нарисованы.
#[derive(Default)]
pub(super) struct HitMap {
    pub x0: f32,
    pub y0: f32,
    rows: Vec<(VRow, Option<Rc<Shaped>>)>,
    len: u64,
    /// Ширина колонки и сдвиг (Alt+мышь — колонка и за концом строки).
    cw: f32,
    hscroll: u32,
}

impl HitMap {
    /// На сколько строк `y` за краем экрана (выше — меньше нуля); на экране — `None`.
    pub(super) fn past(&self, y: f32) -> Option<f32> {
        if self.rows.is_empty() {
            return None;
        }
        #[allow(clippy::cast_precision_loss, reason = "строк — десятки")]
        let bottom = self.y0 + self.rows.len() as f32 * ROW_H;
        if y < self.y0 {
            Some((y - self.y0) / ROW_H)
        } else if y >= bottom {
            Some((y - bottom) / ROW_H)
        } else {
            None
        }
    }

    /// Экранная колонка под мышью в ряду (с `hscroll`; за концом строки — тоже):
    /// блочное выделение мышью (Alt).
    pub(super) fn col_at(&self, x: f32) -> Option<u32> {
        if self.cw <= 0.0 || self.rows.is_empty() {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "колонки — сотни"
        )]
        let c = ((x - self.x0) / self.cw).round().max(0.0) as u32;
        Some(c.saturating_add(self.hscroll))
    }

    /// Байт под мышью: начало символа; за концом строки — её конец; ниже текста —
    /// конец последней строки.
    pub(super) fn byte_at(&self, x: f32, y: f32) -> Option<u64> {
        let (first, _) = self.rows.first()?;
        if y < self.y0 {
            return Some(first.start);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "строк — десятки"
        )]
        let i = ((y - self.y0) / ROW_H) as usize;
        let Some((r, sh)) = self.rows.get(i) else {
            let (last, _) = self.rows.last()?;
            return Some(if last.next >= self.len {
                self.len
            } else {
                last.end
            });
        };
        let Some(sh) = sh else {
            return Some(r.start);
        };
        #[allow(clippy::cast_possible_truncation, reason = "символов в строке — сотни")]
        let n = sh.rt.text.chars().count() as u32;
        let ci = char_at(&sh.line, &sh.rt.text, x - self.x0);
        Some(if ci >= n {
            r.end
        } else {
            sh.rt.byte_at(ci).unwrap_or(r.start).clamp(r.start, r.end)
        })
    }
}

pub(super) type HitRc = Rc<RefCell<HitMap>>;

/// Совпадения на экране — `(начало, конец)` байтов документа.
type Hits = Rc<[(u64, u64)]>;
/// Ключ совпадений экрана: видимые байты, matcher, правки.
type HitsKey = (u64, u64, usize, u64);
/// Метки полосы изменений видимых строк и их ключ: правки, диапазон.
type Marks = ((u64, u64, u64), Rc<[(u32, Change)]>);

/// Состояние элемента между кадрами: кэш строк, мышь, размер.
#[derive(Default)]
pub(super) struct EditView {
    map: FxHashMap<Key, (Rc<Shaped>, u64)>,
    frame: u64,
    scratch: RowText,
    hits: Option<(HitsKey, Hits)>,
    changes: Option<Marks>,
    pub hit: HitRc,
    /// Размер элемента прошлого кадра (px): окно текста для `Viewport`.
    pub size: Rc<Cell<Option<(f32, f32)>>>,
    /// Выделение мышью: тянут (якорь у handler'а), где мышь и шаг автопрокрутки.
    pub drag: bool,
    pub auto: Option<((f32, f32), Option<Instant>)>,
    /// Курсор двигался — мигание с начала.
    pub blink_from: Option<(u64, Instant)>,
}

/// Подсветка: цвета ролей.
#[derive(Clone, Copy)]
struct Paint {
    bg: Hsla,
    line: Hsla,
    sel: Hsla,
    hit: Hsla,
    cur: Hsla,
    caret: Hsla,
    skeleton: Hsla,
    added: Hsla,
    modified: Hsla,
    deleted: Hsla,
}

fn rt_hash(rt: &RowText) -> u64 {
    hash_of(&(
        &rt.text,
        rt.bad.as_slice(),
        rt.dim.as_slice(),
        rt.cut,
        rt.unit,
    ))
}

/// Байты строки `r`, покрытые диапазоном (пустая строка внутри — клетка).
fn span_in(s: &Range<u64>, r: &VRow) -> Option<(u64, u64)> {
    let a = s.start.max(r.start);
    let b = s.end.min(r.end);
    let empty_inside = r.start == r.end && s.start <= r.start && r.start < s.end;
    (a < b || empty_inside).then_some((a, b))
}

/// Колонки блока `[c0, c1)` на ряду, который начинается с колонки `base` строки (до
/// `end` — дальше следующий ряд переноса): `x` от начала текста; пустой блок — черта 2.
fn block_x(a: &Area, base: u32, end: u32, hscroll: u32, cw: f32) -> Option<(f32, f32)> {
    let c0 = a.c0.max(base);
    let c1 = a.c1.min(end);
    if c0 > c1 || (c0 == c1 && (a.c0 != a.c1 || c0 >= end)) {
        return None;
    }
    let from = base.saturating_add(hscroll);
    #[allow(clippy::cast_precision_loss, reason = "колонки — сотни")]
    let x = |c: u32| (i64::from(c) - i64::from(from)) as f32 * cw;
    let (x0, x1) = (x(c0).max(0.0), x(c1));
    if x1 < 0.0 {
        return None;
    }
    Some((x0, if c0 == c1 { x0 + 2.0 } else { x1.max(x0 + 1.0) }))
}

/// Подсказка кадра: что рисовать поверх строк.
pub(super) struct Over<'a> {
    pub font: &'a Font,
    pub font_px: f32,
    pub cw: f32,
    pub rows: u32,
    pub gutter: bool,
    /// Совпадения поиска на экране (строка «Найти» открыта).
    pub hits: bool,
    pub focused: bool,
    pub now: Instant,
}

impl EditView {
    pub(super) fn clear(&mut self) {
        self.map.clear();
        self.hits = None;
        self.changes = None;
        self.drag = false;
        self.auto = None;
    }

    /// Совпадения образца на экране: matcher по видимым байтам (не больше 64 КБ), без
    /// I/O — только кэши.
    fn hits(&mut self, d: &EditorDoc, from: u64, to: u64) -> Hits {
        let none = || Rc::from(Vec::new());
        let (Some(e), Some(m)) = (
            d.engine.as_deref(),
            d.search.as_ref().and_then(|s| s.st.matcher.clone()),
        ) else {
            return none();
        };
        if to <= from || to - from > HL_MAX {
            return none();
        }
        let key = (
            from,
            to,
            std::sync::Arc::as_ptr(&m).cast::<()>() as usize,
            e.text.edits(),
        );
        if let Some((k, h)) = &self.hits
            && *k == key
        {
            return h.clone();
        }
        let u = u64::from(m.unit().max(1));
        let edge = (m.max_len() as u64).min(4 << 10) / u * u;
        let wide = (from.saturating_sub(edge) / u * u)..(to + edge).min(e.len());
        let Some((from, buf)) = e
            .bytes(wide.clone())
            .map(|b| (wide.start, b))
            .or_else(|| e.bytes(from..to).map(|b| (from, b)))
        else {
            return none();
        };
        let p = m.prepare(&buf);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(r) = p.find_at(at) {
            if r.end <= r.start || out.len() >= HL_COUNT {
                break;
            }
            out.push((from + r.start as u64, from + r.end as u64));
            at = r.end;
        }
        let list: Hits = Rc::from(out);
        self.hits = Some((key, list.clone()));
        list
    }

    /// Метки полосы изменений видимых строк (кэш по правкам и диапазону).
    fn change_marks(&mut self, d: &EditorDoc, range: Range<u64>) -> Rc<[(u32, Change)]> {
        let Some(e) = d.engine.as_deref() else {
            return Rc::from(Vec::new());
        };
        let key = (e.text.edits(), range.start, range.end);
        if let Some((k, v)) = &self.changes
            && *k == key
        {
            return v.clone();
        }
        let v: Rc<[(u32, Change)]> = if d.dirty || !d.changes.is_empty() {
            match urfin_view::edit::changes::visible(&e.text, range, e.orig.clone()) {
                Ok(v) => Rc::from(v),
                Err(_) => return Rc::from(Vec::new()),
            }
        } else {
            Rc::from(Vec::new())
        };
        self.changes = Some((key, v.clone()));
        v
    }

    /// Кадр: видимые строки по кэшам документа, раскладка, номера, подсветка, курсор.
    #[allow(
        clippy::too_many_lines,
        reason = "строки, номера, полоса, подсветка и курсор — один проход"
    )]
    pub(super) fn build(
        &mut self,
        d: &EditorDoc,
        o: &Over<'_>,
        ime: &Ime,
        theme: &Theme,
        window: &mut Window,
    ) -> Option<Screen> {
        let e = d.engine.as_deref()?;
        self.frame += 1;
        let v = e.text.view();
        let spec = e.spec();
        let n = o.rows as usize;
        let pos = (e.top, e.top_sub);
        let rows = if let Ok(r) = rows_down(&v, pos, n, &spec) {
            r
        } else {
            // Сколько получится сверху.
            let (mut lo, mut hi) = (0usize, n);
            let mut best = SmallVec::new();
            while hi - lo > 1 {
                let mid = usize::midpoint(lo, hi);
                if let Ok(r) = rows_down(&v, pos, mid, &spec) {
                    lo = mid;
                    best = r;
                } else {
                    hi = mid;
                }
            }
            best
        };
        let style = hash_of(&(theme.generation(), o.font_px.to_bits(), &o.font.family));
        let caret = e.sel.caret;
        let caret_row = e.caret_row().ok();
        let caret_i = caret_row.and_then(|cr| rows.iter().position(|r| r.pos() == cr.pos()));
        // Строки курсора: ряд и его соседи-продолжения (перенос).
        let cur_rows = caret_i.map(|i| {
            let mut a = i;
            while a > 0 && rows[a].cont {
                a -= 1;
            }
            let mut b = i + 1;
            while b < rows.len() && rows[b].cont {
                b += 1;
            }
            a..b
        });
        // Номера: от первой целой строки, если её номер известен точно.
        let first_whole = rows.iter().position(|r| !r.cont);
        let first_line = first_whole.map(|j| e.text.line_of(rows[j].start));
        let mut line_no = match first_line {
            Some(LineGuess::Exact(k)) => Some(k),
            _ => None,
        };
        let from = rows.first().map_or(e.top, |r| r.start);
        let to = rows.last().map_or(e.top, |r| r.next);
        let hits = if o.hits {
            self.hits(d, from, to)
        } else {
            Rc::from(Vec::new())
        };
        let cur_hit = d
            .search
            .as_ref()
            .and_then(|s| s.st.hit.clone())
            .filter(|h| o.hits && e.sel.stream().as_ref() == Some(h));
        let sel = e.sel.stream();
        let changes = match first_whole {
            Some(j) => self.change_marks(d, rows[j].start..to),
            None => Rc::from(Vec::new()),
        };
        let (primary, secondary, tertiary, bad, on) = (
            theme.c(Role::TextPrimary),
            theme.c(Role::TextSecondary),
            theme.c(Role::TextTertiary),
            theme.c(Role::CriticalFg),
            theme.c(Role::AccentOn),
        );
        let mut lines = Vec::with_capacity(rows.len());
        let mut caret_x = None;
        let mut rel: Option<u32> = None;
        let mut hm = HitMap {
            len: e.len(),
            cw: o.cw,
            hscroll: if e.wrap { 0 } else { e.hscroll },
            ..HitMap::default()
        };
        // Блок: колонки рядов его строк (перенос — от начала логической строки).
        let block = e.block_area();
        let block_col = match e.sel.sel {
            Sel::Block(b) => Some(b.col),
            _ => None,
        };
        let seg_spec = e.seg_layout();
        let base_of = |r: &VRow| -> u32 {
            if r.start <= r.seg {
                return 0;
            }
            let seg = VRow {
                start: r.seg,
                end: r.start,
                next: r.start,
                cont: false,
                cut: false,
                seg: r.seg,
                sub: 0,
            };
            col_of(&v, &seg, r.start, &seg_spec).unwrap_or(0)
        };
        let hs = if e.wrap { 0 } else { e.hscroll };
        for (i, r) in rows.iter().enumerate() {
            let whole = Some(i) >= first_whole;
            if whole && !r.cont {
                rel = Some(rel.map_or(0, |k| k + 1));
            }
            let mut num_text = None;
            if whole
                && !r.cont
                && let Some(k) = line_no.as_mut()
            {
                if Some(i) != first_whole {
                    *k += 1;
                }
                num_text = Some((*k + 1).to_string());
            }
            let current = cur_rows.as_ref().is_some_and(|c| c.contains(&i));
            let shaped = if render_row(&v, r, e.hscroll, &spec, &mut self.scratch).is_ok() {
                let rt = &self.scratch;
                #[allow(clippy::cast_possible_truncation, reason = "символов — сотни")]
                let nch = rt.text.chars().count() as u32;
                let cur = cur_hit.as_ref().and_then(|h| span_in(h, r)).map(|(a, b)| {
                    let ca = char_of(rt, nch, a);
                    (ca, char_of(rt, nch, b).max(ca + 1))
                });
                let key = Key {
                    text: rt_hash(rt),
                    cur,
                    style,
                };
                let s = if let Some((s, used)) = self.map.get_mut(&key) {
                    *used = self.frame;
                    s.clone()
                } else {
                    let mut text = rt.text.clone();
                    if rt.cut {
                        text.push('↩');
                    }
                    let inside =
                        |list: &[Range<u32>], ci: u32| list.iter().any(|x| x.contains(&ci));
                    let runs = runs(&text, o.font, |ci| {
                        #[allow(clippy::cast_possible_truncation, reason = "символов — сотни")]
                        let ci = ci as u32;
                        if ci >= nch {
                            tertiary
                        } else if cur.is_some_and(|(a, b)| (a..b).contains(&ci)) {
                            on
                        } else if inside(&rt.bad, ci) {
                            bad
                        } else if inside(&rt.dim, ci) {
                            tertiary
                        } else {
                            primary
                        }
                    });
                    let line = window.text_system().shape_line(
                        SharedString::from(text),
                        px(o.font_px),
                        &runs,
                        None,
                    );
                    let s = Rc::new(Shaped {
                        line,
                        rt: rt.clone(),
                    });
                    self.map.insert(key, (s.clone(), self.frame));
                    s
                };
                Some(s)
            } else {
                None
            };
            let mut marks: SmallVec<[(f32, f32, Mark); 4]> = SmallVec::new();
            if let Some(s) = &shaped {
                #[allow(clippy::cast_possible_truncation, reason = "символов — сотни")]
                let nch = s.rt.text.chars().count() as u32;
                let x_of = |a: u64, b: u64| {
                    let (ca, cb) = (char_of(&s.rt, nch, a), char_of(&s.rt, nch, b));
                    let x0 = f32::from(s.line.x_for_index(byte_idx(&s.rt.text, ca)));
                    let x1 = if b >= r.end && cb >= nch {
                        f32::from(s.line.x_for_index(byte_idx(&s.rt.text, nch))).max(x0 + o.cw)
                    } else {
                        f32::from(s.line.x_for_index(byte_idx(&s.rt.text, cb)))
                    };
                    (x0, x1.max(x0 + 1.0))
                };
                if let Some(sel) = sel.as_ref().filter(|s| cur_hit.as_ref() != Some(*s))
                    && let Some((a, b)) = span_in(sel, r)
                {
                    // Перевод строки внутри выделения — клетка за концом строки.
                    let b = if sel.end > r.end && b >= r.end {
                        r.end
                    } else {
                        b
                    };
                    let (x0, x1) = x_of(a, b);
                    marks.push((x0, x1, Mark::Sel));
                }
                if let Some(a) = block
                    .as_ref()
                    .filter(|a| a.top <= r.seg && r.seg <= a.bottom)
                {
                    let more = rows.get(i + 1).is_some_and(|n| n.cont && n.seg == r.seg);
                    let end = if more {
                        base_of(&rows[i + 1])
                    } else {
                        u32::MAX
                    };
                    if let Some((x0, x1)) = block_x(a, base_of(r), end, hs, o.cw) {
                        marks.push((x0, x1, Mark::Block));
                    }
                }
                let k = hits.partition_point(|h| h.1 <= r.start);
                for &(a, b) in hits[k..]
                    .iter()
                    .take_while(|h| h.0 < r.end.max(r.start + 1))
                {
                    let (a, b) = (a.max(r.start), b.min(r.end));
                    if a >= b {
                        continue;
                    }
                    let is_cur = cur_hit.as_ref().is_some_and(|h| h.start <= a && b <= h.end);
                    let (x0, x1) = x_of(a, b);
                    marks.push((x0, x1, if is_cur { Mark::Cur } else { Mark::Hit }));
                }
                if let Some((a, b)) = cur_hit.as_ref().and_then(|h| span_in(h, r))
                    && !marks.iter().any(|m| m.2 == Mark::Cur)
                {
                    let (x0, x1) = x_of(a, b);
                    marks.push((x0, x1, Mark::Cur));
                }
                if caret_i == Some(i) {
                    let ci = char_of(&s.rt, nch, caret);
                    let x = f32::from(s.line.x_for_index(byte_idx(&s.rt.text, ci)));
                    // Угол блока — по колонке (и за концом строки).
                    #[allow(clippy::cast_precision_loss, reason = "колонки — сотни")]
                    let x = block_col.map_or(x, |c| {
                        c.saturating_sub(base_of(r)).saturating_sub(hs) as f32 * o.cw
                    });
                    caret_x = Some((i, x));
                }
            } else if caret_i == Some(i) {
                caret_x = Some((i, 0.0));
            }
            let (change, deleted) = match rel {
                Some(k) if whole => {
                    let c = changes
                        .iter()
                        .find(|(l, c)| *l == k && *c != Change::Deleted)
                        .map(|(_, c)| *c);
                    let del = !r.cont
                        && changes
                            .iter()
                            .any(|(l, c)| *l == k && *c == Change::Deleted);
                    (c, del)
                }
                _ => (None, false),
            };
            let num = num_text.filter(|_| o.gutter).map(|t| {
                let color = if current { primary } else { secondary };
                let mut f = o.font.clone();
                if current {
                    f.weight = gpui_kit::FontWeight::BOLD;
                }
                let run = [TextRun {
                    len: t.len(),
                    font: f,
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                window
                    .text_system()
                    .shape_line(SharedString::from(t), px(o.font_px), &run, None)
            });
            hm.rows.push((*r, shaped.clone()));
            lines.push(Line {
                row: *r,
                shaped,
                num,
                change,
                deleted,
                current,
                marks,
            });
        }
        // Кэш — на два экрана.
        if self.map.len() > 2 * n + 8 {
            let keep = self.frame.saturating_sub(1);
            self.map.retain(|_, (_, used)| *used >= keep);
        }
        {
            let mut m = self.hit.borrow_mut();
            m.rows = hm.rows;
            m.len = hm.len;
            m.cw = hm.cw;
            m.hscroll = hm.hscroll;
        }
        let marked = ime.marked.as_ref().map(|(t, _)| {
            let run = [TextRun {
                len: t.len(),
                font: o.font.clone(),
                color: primary,
                background_color: Some(theme.c(Role::LayerPanel)),
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(primary),
                    wavy: false,
                }),
                strikethrough: None,
            }];
            window.text_system().shape_line(
                SharedString::from(t.clone()),
                px(o.font_px),
                &run,
                None,
            )
        });
        // Мигание: курсор двигался — видим сразу.
        let since = match self.blink_from {
            Some((at, t)) if at == caret => t,
            _ => {
                self.blink_from = Some((caret, o.now));
                o.now
            }
        };
        let phase = o.now.saturating_duration_since(since).as_millis() / BLINK.as_millis();
        let show_caret = o.focused && (phase.is_multiple_of(2) || ime.marked.is_some());
        let line_count = lines.iter().filter(|l| !l.row.cont).count() as u64;
        Some(Screen {
            lines,
            caret: caret_x,
            marked,
            cw: o.cw,
            gutter: o.gutter,
            overwrite: e.overwrite,
            show_caret,
            first_line,
            line_count,
            from,
            to,
        })
    }
}

/// Тело элемента: `canvas` строк; `paint` ставит обработчик ввода (`EntityInputHandler`)
/// и запоминает геометрию (мышь, IME, окно текста).
pub(super) fn element(
    screen: Screen,
    theme: &Theme,
    view: &EditView,
    input: Option<(FocusHandle, gpui_kit::Entity<EditInput>)>,
    ime: ImeRc,
) -> impl gpui_kit::IntoElement {
    let p = Paint {
        bg: theme.c(Role::LayerPanel),
        line: theme.c(Role::CurrentLine),
        sel: theme.c(Role::AccentSubtle),
        hit: theme.c(Role::Highlight),
        cur: theme.c(Role::AccentFill),
        caret: theme.c(Role::TextPrimary),
        skeleton: theme.c(Role::FillSubtleHover),
        added: theme.c(Role::TypeExec),
        modified: theme.c(Role::AccentFill),
        deleted: theme.c(Role::Modified),
    };
    let hit = view.hit.clone();
    let size_cell = view.size.clone();
    canvas(
        |_, _, _| {},
        move |b, (), window, cx| {
            size_cell.set(Some((f32::from(b.size.width), f32::from(b.size.height))));
            let tx = b.origin.x + px(text_x(screen.gutter));
            {
                let mut m = hit.borrow_mut();
                m.x0 = f32::from(tx);
                m.y0 = f32::from(b.origin.y);
            }
            if let Some((focus, ent)) = &input {
                window.handle_input(focus, ElementInputHandler::new(b, ent.clone()), cx);
            }
            paint(&screen, p, b, tx, &ime, window, cx);
        },
    )
    .size_full()
}

#[allow(clippy::many_single_char_names, reason = "геометрия кадра")]
fn paint(
    s: &Screen,
    p: Paint,
    b: Bounds<Pixels>,
    tx: Pixels,
    ime: &ImeRc,
    window: &mut Window,
    cx: &mut App,
) {
    window.paint_quad(fill(b, p.bg));
    let rh = px(ROW_H);
    let gutter_w = if s.gutter {
        W_BAR + W_NUM + W_FOLD
    } else {
        W_BAR
    };
    window.with_content_mask(Some(gpui_kit::ContentMask { bounds: b }), |window| {
        #[allow(clippy::cast_precision_loss, reason = "строк на экране — десятки")]
        for (i, l) in s.lines.iter().enumerate() {
            let y = b.origin.y + rh * i as f32;
            if l.current {
                window.paint_quad(fill(
                    Bounds::new(
                        point(b.origin.x + px(gutter_w), y),
                        size(b.size.width - px(gutter_w), rh),
                    ),
                    p.line,
                ));
            }
            if let Some(c) = l.change {
                let color = if c == Change::Added {
                    p.added
                } else {
                    p.modified
                };
                window.paint_quad(fill(
                    Bounds::new(point(b.origin.x, y), size(px(W_BAR), rh)),
                    color,
                ));
            }
            if l.deleted {
                window.paint_quad(fill(
                    Bounds::new(
                        point(b.origin.x, y - px(1.0)),
                        size(px(W_BAR + 6.0), px(2.0)),
                    ),
                    p.deleted,
                ));
            }
            let Some(sh) = &l.shaped else {
                let w = (b.size.width - px(text_x(s.gutter) + PAD_RIGHT)) * skeleton_w(l.row.start);
                window.paint_quad(
                    fill(
                        Bounds::new(point(tx, y + px(5.0)), size(w, rh - px(10.0))),
                        p.skeleton,
                    )
                    .corner_radii(px(3.0)),
                );
                continue;
            };
            for &(x0, x1, mark) in &l.marks {
                let r = Bounds::new(point(tx + px(x0), y), size(px(x1 - x0), rh));
                let color = match mark {
                    Mark::Sel | Mark::Block => p.sel,
                    Mark::Hit => p.hit,
                    Mark::Cur => p.cur,
                };
                let radius = if mark == Mark::Block { 0.0 } else { 2.0 };
                window.paint_quad(fill(r, color).corner_radii(px(radius)));
            }
            let _ = sh
                .line
                .paint(point(tx, y), rh, TextAlign::Left, None, window, cx);
            if let Some(n) = &l.num {
                let x = b.origin.x + px(W_BAR + W_NUM - NUM_PAD_R) - n.width;
                let _ = n.paint(point(x, y), rh, TextAlign::Left, None, window, cx);
            }
        }
        if let Some((i, x)) = s.caret {
            #[allow(clippy::cast_precision_loss, reason = "строк на экране — десятки")]
            let y = b.origin.y + rh * i as f32;
            let at = point(tx + px(x), y);
            {
                let mut m = ime.borrow_mut();
                m.caret_at = Some(Bounds::new(at, size(px(s.cw), rh)));
                m.cw = s.cw;
            }
            if let Some(m) = &s.marked {
                window.paint_quad(fill(Bounds::new(at, size(m.width, rh)), p.bg));
                let _ = m.paint(at, rh, TextAlign::Left, None, window, cx);
            }
            if s.show_caret {
                let mx = s.marked.as_ref().map_or(px(0.0), |m| m.width);
                let r = if s.overwrite && s.marked.is_none() {
                    Bounds::new(point(at.x, y + rh - px(3.0)), size(px(s.cw), px(2.0)))
                } else {
                    Bounds::new(point(at.x + mx, y + px(1.0)), size(px(2.0), rh - px(2.0)))
                };
                window.paint_quad(fill(r, p.caret));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Composition не трогает документ; commit — одно `Input` с текстом, composition
    /// снята; без документа и пустой commit — ничего.
    #[test]
    fn ime_commit_and_composition() {
        let mut ime = Ime::default();
        ime.commit("x");
        assert!(ime.out.is_empty(), "без документа");
        ime.doc = Some(DocId(7));
        ime.mark("にほ", Some(2..2));
        ime.mark("日本", None);
        assert!(ime.out.is_empty(), "composition — не в документ");
        assert_eq!(ime.marked.as_ref().map(|m| m.0.as_str()), Some("日本"));
        ime.commit("日本");
        assert!(ime.marked.is_none());
        assert_eq!(ime.out.len(), 1);
        assert!(matches!(
            &ime.out[0],
            InboxMsg::Input { doc: DocId(7), text } if text.as_str() == "日本"
        ));
        ime.mark("", None);
        assert!(ime.marked.is_none());
        ime.commit("");
        assert_eq!(ime.out.len(), 1);
    }

    #[test]
    fn utf16_offsets() {
        let s = "aб𝄞c";
        assert_eq!(utf16_len(s), 5);
        assert_eq!(byte_of16(s, 0), 0);
        assert_eq!(byte_of16(s, 2), 3);
        assert_eq!(byte_of16(s, 4), 7);
        assert_eq!(byte_of16(s, 99), s.len());
    }

    #[test]
    fn spans_and_grid() {
        let r = VRow {
            start: 10,
            end: 20,
            next: 21,
            cont: false,
            cut: false,
            seg: 10,
            sub: 0,
        };
        assert_eq!(span_in(&(12..30), &r), Some((12, 20)));
        assert_eq!(span_in(&(0..5), &r), None);
        let empty = VRow {
            start: 5,
            end: 5,
            next: 6,
            ..r
        };
        assert_eq!(span_in(&(0..9), &empty), Some((5, 5)));
        assert_eq!(text_x(true), 70.0);
        let (cols, rows) = grid(870.0, 400.0, 8.0, true);
        assert_eq!((cols, rows), (99, 20));
    }

    /// Мышь: строка и байт под точкой, за концом строки — её конец, ниже текста —
    /// конец документа.
    #[test]
    fn hit_map_bytes() {
        let r = VRow {
            start: 0,
            end: 3,
            next: 4,
            cont: false,
            cut: false,
            seg: 0,
            sub: 0,
        };
        let m = HitMap {
            x0: 10.0,
            y0: 100.0,
            rows: vec![(r, None)],
            len: 4,
            cw: 8.0,
            hscroll: 2,
        };
        assert_eq!(m.byte_at(50.0, 90.0), Some(0));
        assert_eq!(m.byte_at(50.0, 105.0), Some(0), "без раскладки — начало");
        assert_eq!(m.byte_at(50.0, 200.0), Some(4), "ниже — конец документа");
        assert_eq!(m.past(130.0), Some(0.5));
        assert_eq!(m.past(110.0), None);
        assert_eq!(m.col_at(10.0 + 8.0 * 3.4), Some(5), "колонка с hscroll");
        assert_eq!(m.col_at(0.0), Some(2));
    }

    /// Блок на ряду: колонки → `x`; за `hscroll` слева — обрезан; ряд переноса — своя
    /// часть; пустой блок — черта.
    #[test]
    fn block_columns_to_x() {
        let a = Area {
            top: 0,
            bottom: 10,
            c0: 2,
            c1: 6,
        };
        assert_eq!(block_x(&a, 0, u32::MAX, 0, 8.0), Some((16.0, 48.0)));
        assert_eq!(block_x(&a, 0, u32::MAX, 4, 8.0), Some((0.0, 16.0)));
        assert_eq!(block_x(&a, 0, u32::MAX, 7, 8.0), None);
        assert_eq!(
            block_x(&a, 4, u32::MAX, 0, 8.0),
            Some((0.0, 16.0)),
            "ряд переноса"
        );
        assert_eq!(block_x(&a, 0, 4, 0, 8.0), Some((16.0, 32.0)));
        let thin = Area { c1: 2, ..a };
        assert_eq!(block_x(&thin, 0, u32::MAX, 0, 8.0), Some((16.0, 18.0)));
    }
}
