//! Чистая раскладка Text/Wrap/Hex без gpui и I/O (S7 §3.4). Карточка S7-06.
//!
//! Позиция экрана — `(начало сегмента, номер подстроки при переносе)`. Сегмент — логическая
//! строка до разделителя; строка длиннее `max_line` режется на сегменты по абсолютным
//! смещениям, кратным `max_line` (подвинутым к границе символа). Так начало строки назад
//! ищется не дальше `max_line` байт, а вверх и вниз раскладка сходится (proptest).

#![allow(
    clippy::many_single_char_names,
    reason = "b — байты, s — раскладка, r — ряд: по всему модулю"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::hash::BuildHasher;
use std::ops::Range;
use std::rc::Rc;

use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

pub use crate::cache::{ByteView, Missing};
use crate::detect::Eol;
use crate::enc::{CharDecoder, Enc, Step, is_utf8_boundary};
use crate::index::Sep;

/// Режим вьювера.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Text,
    /// Текст + подсветка tree-sitter (S7-21); раскладка — как у `Text`.
    Markup,
    Hex,
    /// Запись сеанса `.cast` (F03 §3.6): плеер; строки текста не показываются, источник и
    /// раскладка — как у `Text` (вывод без VT), по ним идёт поиск.
    Cast,
}

/// Перенос строк.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WrapKind {
    /// По ширине в колонках.
    Chars,
    /// По границам слов UAX#29; слово длиннее строки рвётся.
    Words,
}

/// Параметры раскладки.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LayoutSpec {
    pub mode: Mode,
    pub wrap: Option<WrapKind>,
    /// Ширина окна текста в колонках.
    pub width_cols: u32,
    pub tab: u8,
    /// Байт в сегменте длинной строки (`[viewer] max_line`).
    pub max_line: u32,
    /// Байт в строке hex (8, 16, 32; [`hex_cols_auto`]).
    pub hex_cols: u8,
    pub enc: Enc,
    pub eol: Eol,
}

impl Default for LayoutSpec {
    fn default() -> Self {
        LayoutSpec {
            mode: Mode::Text,
            wrap: None,
            width_cols: 80,
            tab: 8,
            max_line: 65_536,
            hex_cols: 16,
            enc: Enc::utf8(),
            eol: Eol::default(),
        }
    }
}

impl LayoutSpec {
    fn sep(&self) -> Sep {
        Sep::of(&self.enc, self.eol)
    }

    fn wrap(&self) -> Option<WrapKind> {
        if self.mode == Mode::Hex {
            None
        } else {
            self.wrap
        }
    }

    /// Длина сегмента: не меньше 64, чётная (UTF-16).
    fn ml(&self) -> u64 {
        u64::from(self.max_line.max(64)) & !1
    }

    fn cols(&self) -> u64 {
        u64::from(self.hex_cols.max(1))
    }

    fn width(&self) -> u32 {
        self.width_cols.max(1)
    }

    fn tab(&self) -> u32 {
        u32::from(self.tab.max(1))
    }
}

/// Колонок hex по ширине окна: 32 при ≥ 150, 16 при ≥ 76, иначе 8.
pub fn hex_cols_auto(width_cols: u32) -> u8 {
    match width_cols {
        150.. => 32,
        76.. => 16,
        _ => 8,
    }
}

/// Визуальная строка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VRow {
    /// Рисуемые байты `start..end` (без разделителя и CR перед LF).
    pub start: u64,
    pub end: u64,
    /// Начало следующей визуальной строки.
    pub next: u64,
    /// Продолжение: подстрока переноса или сегмент после разреза.
    pub cont: bool,
    /// Кончается разрезом длинной строки (маркер `↩`).
    pub cut: bool,
    /// Позиция для `rows_down`/`row_up`: начало сегмента и номер подстроки.
    pub seg: u64,
    pub sub: u32,
}

impl VRow {
    pub fn pos(&self) -> (u64, u32) {
        (self.seg, self.sub)
    }
}

/// Текст визуальной строки для рендера.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowText {
    pub text: String,
    /// Опорные точки `(символ, байт)`: между ними символ `i` начинается с байта
    /// `опора.байт + (i − опора.символ) × unit` ([`RowText::byte_at`]).
    pub map: SmallVec<[(u32, u64); 8]>,
    /// Символы U+FFFD (недекодируемые байты) — цвет `critical.fg`.
    pub bad: SmallVec<[Range<u32>; 2]>,
    /// Символы-заменители (`·` управляющих, `␍`, раскрытые табы не в счёт) — `text.tertiary`.
    pub dim: SmallVec<[Range<u32>; 4]>,
    /// В конце строки — маркер разреза `↩`.
    pub cut: bool,
    pub unit: u8,
}

impl RowText {
    pub fn clear(&mut self) {
        self.text.clear();
        self.map.clear();
        self.bad.clear();
        self.dim.clear();
        self.cut = false;
        self.unit = 1;
    }

    /// Байт начала символа `ci` (hit-test).
    pub fn byte_at(&self, ci: u32) -> Option<u64> {
        let i = self.map.partition_point(|m| m.0 <= ci);
        let (c, b) = *self.map.get(i.checked_sub(1)?)?;
        Some(b + u64::from(ci - c) * u64::from(self.unit))
    }

    fn anchor(&mut self, ci: u32, off: u64) {
        let predicted = self
            .map
            .last()
            .map(|&(c, b)| b + u64::from(ci - c) * u64::from(self.unit));
        if predicted != Some(off) {
            self.map.push((ci, off));
        }
    }

    fn mark(list: &mut SmallVec<[Range<u32>; 2]>, ci: u32) {
        match list.last_mut() {
            Some(r) if r.end == ci => r.end = ci + 1,
            _ => list.push(ci..ci + 1),
        }
    }
}

// ---------------------------------------------------------------- байты

/// Копирует до `buf.len()` байт с `off`.
fn read_into(b: &(impl ByteView + ?Sized), off: u64, buf: &mut [u8]) -> Result<usize, Missing> {
    let mut done = 0;
    while done < buf.len() {
        let c = b.chunk(off + done as u64)?;
        if c.is_empty() {
            break;
        }
        let k = c.len().min(buf.len() - done);
        buf[done..done + k].copy_from_slice(&c[..k]);
        done += k;
    }
    Ok(done)
}

/// Копирует `r` в `out`.
fn read_range(
    b: &(impl ByteView + ?Sized),
    r: Range<u64>,
    out: &mut Vec<u8>,
) -> Result<(), Missing> {
    out.clear();
    let mut pos = r.start;
    while pos < r.end {
        let c = b.chunk(pos)?;
        if c.is_empty() {
            break;
        }
        let k = c.len().min((r.end - pos) as usize);
        out.extend_from_slice(&c[..k]);
        pos += k as u64;
    }
    Ok(())
}

/// Байты `r`: без копии, если они лежат одним куском, иначе — в `buf`.
fn row_bytes<'a>(
    b: &'a (impl ByteView + ?Sized),
    r: Range<u64>,
    buf: &'a mut Vec<u8>,
) -> Result<&'a [u8], Missing> {
    let n = (r.end - r.start) as usize;
    if n == 0 {
        return Ok(&[]);
    }
    let c = b.chunk(r.start)?;
    if c.len() >= n {
        return Ok(&c[..n]);
    }
    read_range(b, r, buf)?;
    Ok(buf)
}

/// Первый разделитель в `[from, limit)` целиком: начало его единицы.
fn find_sep(
    b: &(impl ByteView + ?Sized),
    from: u64,
    limit: u64,
    sep: Sep,
) -> Result<Option<u64>, Missing> {
    let mut pos = from;
    let x = sep.byte();
    while pos < limit {
        let c = b.chunk(pos)?;
        if c.is_empty() {
            break;
        }
        let c = &c[..c.len().min((limit - pos) as usize)];
        match sep {
            Sep::Byte(_) => {
                if let Some(i) = memchr::memchr(x, c) {
                    return Ok(Some(pos + i as u64));
                }
            }
            Sep::Le(_) => {
                for i in memchr::memchr_iter(x, c) {
                    if (pos + i as u64).is_multiple_of(2) && c.get(i + 1) == Some(&0) {
                        return Ok(Some(pos + i as u64));
                    }
                }
            }
            Sep::Be(_) => {
                for i in memchr::memchr_iter(x, c) {
                    if !(pos + i as u64).is_multiple_of(2) && i > 0 && c[i - 1] == 0 {
                        return Ok(Some(pos + i as u64 - 1));
                    }
                }
            }
        }
        pos += c.len() as u64;
    }
    Ok(None)
}

/// Последний разделитель в `[floor, before)` целиком: начало его единицы.
fn rfind_sep(
    b: &(impl ByteView + ?Sized),
    floor: u64,
    before: u64,
    sep: Sep,
) -> Result<Option<u64>, Missing> {
    let mut end = before;
    let x = sep.byte();
    while end > floor {
        let c = b.chunk_before(end)?;
        if c.is_empty() {
            break;
        }
        let cstart = end - c.len() as u64;
        let lo = cstart.max(floor);
        let s = &c[(lo - cstart) as usize..];
        match sep {
            Sep::Byte(_) => {
                if let Some(i) = memchr::memrchr(x, s) {
                    return Ok(Some(lo + i as u64));
                }
            }
            Sep::Le(_) => {
                for i in memchr::memrchr_iter(x, s) {
                    if (lo + i as u64).is_multiple_of(2) && s.get(i + 1) == Some(&0) {
                        return Ok(Some(lo + i as u64));
                    }
                }
            }
            Sep::Be(_) => {
                for i in memchr::memrchr_iter(x, s) {
                    if !(lo + i as u64).is_multiple_of(2) && i > 0 && s[i - 1] == 0 {
                        return Ok(Some(lo + i as u64 - 1));
                    }
                }
            }
        }
        end = lo;
    }
    Ok(None)
}

/// Разрез у кратного `m`, подвинутый к границе символа (UTF-8 — за продолжения, UTF-16 —
/// за младший суррогат).
fn cut_at(b: &(impl ByteView + ?Sized), m: u64, s: &LayoutSpec) -> Result<u64, Missing> {
    if m >= b.len() {
        return Ok(b.len());
    }
    if s.enc.is_utf8() {
        let mut buf = [0u8; 3];
        let n = read_into(b, m, &mut buf)?;
        let k = buf[..n]
            .iter()
            .take_while(|&&x| !is_utf8_boundary(x))
            .count();
        return Ok(m + k as u64);
    }
    if s.enc.is_utf16() {
        let mut buf = [0u8; 2];
        if read_into(b, m, &mut buf)? == 2 {
            let u = if s.enc.is_utf16be() {
                u16::from_be_bytes(buf)
            } else {
                u16::from_le_bytes(buf)
            };
            if (0xDC00..=0xDFFF).contains(&u) {
                return Ok(m + 2);
            }
        }
    }
    Ok(m)
}

fn next_cut(b: &(impl ByteView + ?Sized), from: u64, s: &LayoutSpec) -> Result<u64, Missing> {
    let ml = s.ml();
    cut_at(b, (from / ml + 1) * ml, s)
}

/// `at` — начало логической строки (начало файла или сразу за разделителем).
fn is_line_start(b: &(impl ByteView + ?Sized), at: u64, sep: Sep) -> Result<bool, Missing> {
    let u = sep.unit();
    if at == 0 {
        return Ok(true);
    }
    if at < u {
        return Ok(false);
    }
    let mut buf = [0u8; 2];
    read_into(b, at - u, &mut buf[..u as usize])?;
    Ok(match sep {
        Sep::Byte(x) => buf[0] == x,
        Sep::Le(x) => (at - u).is_multiple_of(2) && buf == [x, 0],
        Sep::Be(x) => (at - u).is_multiple_of(2) && buf == [0, x],
    })
}

/// Сегмент: рисуемые байты, начало следующего, разрез.
#[derive(Copy, Clone, Debug)]
struct Seg {
    start: u64,
    end: u64,
    next: u64,
    cut: bool,
    line_start: bool,
}

/// Сегмент с начала `start` (начало строки или разрез).
fn segment(b: &(impl ByteView + ?Sized), start: u64, s: &LayoutSpec) -> Result<Seg, Missing> {
    let len = b.len();
    let sep = s.sep();
    let u = sep.unit();
    let line_start = is_line_start(b, start, sep)?;
    let limit = if line_start {
        // Короткая строка (≤ max_line) — целиком; длинная — до первого разреза.
        let end = (start + s.ml() + u).min(len);
        match find_sep(b, start, end, sep)? {
            Some(p) => return finish(b, start, p, u, s, line_start),
            None if end == len && len - start <= s.ml() => len,
            None => next_cut(b, start, s)?.min(len),
        }
    } else {
        next_cut(b, start, s)?.min(len)
    };
    // У начала строки до `start + ml + u` уже смотрели.
    let from = if line_start {
        (start + s.ml() + u).min(limit)
    } else {
        start
    };
    match find_sep(b, from, limit, sep)? {
        Some(p) => finish(b, start, p, u, s, line_start),
        None => Ok(Seg {
            start,
            end: limit,
            next: limit,
            cut: limit < len,
            line_start,
        }),
    }
}

/// Сегмент, кончающийся разделителем в `p`: CR перед LF не рисуется.
fn finish(
    b: &(impl ByteView + ?Sized),
    start: u64,
    p: u64,
    u: u64,
    s: &LayoutSpec,
    line_start: bool,
) -> Result<Seg, Missing> {
    let mut end = p;
    if s.eol != Eol::Cr && p >= start + u {
        let mut buf = [0u8; 2];
        read_into(b, p - u, &mut buf[..u as usize])?;
        let cr = match u {
            1 => buf[0] == b'\r',
            _ if s.enc.is_utf16be() => buf == [0, b'\r'],
            _ => buf == [b'\r', 0],
        };
        if cr {
            end = p - u;
        }
    }
    Ok(Seg {
        start,
        end,
        next: p + u,
        cut: false,
        line_start,
    })
}

/// Начало сегмента, содержащего байт `off` (назад не дальше `max_line`).
pub fn line_start_before(
    b: &(impl ByteView + ?Sized),
    off: u64,
    s: &LayoutSpec,
) -> Result<u64, Missing> {
    let len = b.len();
    if len == 0 {
        return Ok(0);
    }
    let off = off.min(len - 1);
    if s.mode == Mode::Hex {
        return Ok(off / s.cols() * s.cols());
    }
    let sep = s.sep();
    let ml = s.ml();
    let lo = off.saturating_sub(ml + 4);
    let line = match rfind_sep(b, lo, off, sep)? {
        Some(p) => Some(p + sep.unit()),
        None if lo == 0 => Some(0),
        None => None,
    };
    let Some(l) = line else {
        // Строка длиннее max_line: последний разрез не дальше `off`.
        let mut m = off / ml * ml;
        loop {
            let c = cut_at(b, m, s)?;
            if c <= off || m == 0 {
                return Ok(c.min(off));
            }
            m -= ml;
        }
    };
    if l == off {
        return Ok(off);
    }
    // Короткая строка — её начало; длинная — последний разрез после начала.
    let end = (l + ml + sep.unit()).min(len);
    let short = find_sep(b, l, end, sep)?.is_some() || (end == len && len - l <= ml);
    if short {
        return Ok(l);
    }
    let mut m = off / ml * ml;
    while m > l {
        let c = cut_at(b, m, s)?;
        if c <= off {
            return Ok(c);
        }
        m -= ml;
    }
    Ok(l)
}

// ---------------------------------------------------------------- символы

/// Ячейка: символ строки на экране.
#[derive(Copy, Clone, Debug)]
struct Cell {
    /// Что рисовать.
    ch: char,
    /// Исходный символ (границы слов).
    src: char,
    off: u64,
    width: u8,
    tab: bool,
    bad: bool,
    dim: bool,
}

/// Байт BOM в начале `bytes` (с абсолютного `base`).
fn after_bom(bytes: &[u8], base: u64, s: &LayoutSpec) -> usize {
    u64::from(s.enc.bom_len)
        .saturating_sub(base)
        .min(bytes.len() as u64) as usize
}

/// Ячейка символа `step` со смещения `off`.
fn cell_of(step: Step, off: u64, s: &LayoutSpec) -> Cell {
    match step {
        Step::Bad(_) => Cell {
            ch: '\u{FFFD}',
            src: '\u{FFFD}',
            off,
            width: 1,
            tab: false,
            bad: true,
            dim: false,
        },
        Step::Char(c, _) => display(c, off, s),
    }
}

/// Символы байтов `bytes` (с абсолютного `base`) с индекса `from`. `f` → `false` — стоп.
fn cells_from(
    bytes: &[u8],
    base: u64,
    from: usize,
    s: &LayoutSpec,
    mut f: impl FnMut(Cell) -> bool,
) {
    let mut dec = CharDecoder::new(s.enc);
    let mut i = from;
    while i < bytes.len() {
        let step = dec.next(&bytes[i..]);
        let cell = cell_of(step, base + i as u64, s);
        i += step.bytes();
        if !f(cell) {
            break;
        }
    }
}

/// Символы байтов `bytes` (с абсолютного `base`); BOM пропускается. `f` → `false` — стоп.
fn cells(bytes: &[u8], base: u64, s: &LayoutSpec, f: impl FnMut(Cell) -> bool) {
    cells_from(bytes, base, after_bom(bytes, base, s), s, f);
}

/// В слове из восьми байт только ASCII от пробела и выше: каждый — одна колонка.
fn narrow8(x: u64) -> bool {
    const HI: u64 = 0x8080_8080_8080_8080;
    const LOW: u64 = 0x2020_2020_2020_2020;
    (x | x.wrapping_sub(LOW)) & HI == 0
}

/// Колонки строки слева от `h` без разбора по ячейкам: индекс в `bytes` и колонка, откуда
/// идти обычным проходом (не правее `h`). В длинной строке слева от окна — десятки тысяч
/// колонок на каждый ряд каждого кадра. Только UTF-8: ASCII — словами, остальное — по
/// символу; итог тот же, что у прохода по ячейкам.
fn skip_cols(bytes: &[u8], base: u64, s: &LayoutSpec, h: u32) -> (usize, u32) {
    let mut i = after_bom(bytes, base, s);
    let mut col = 0u32;
    if !s.enc.is_utf8() {
        return (i, col);
    }
    let mut dec = CharDecoder::new(s.enc);
    while h - col >= 8
        && let Some(word) = bytes.get(i..i + 8)
    {
        let mut x = [0u8; 8];
        x.copy_from_slice(word);
        if narrow8(u64::from_le_bytes(x)) {
            i += 8;
            col += 8;
            continue;
        }
        let step = dec.next(&bytes[i..]);
        let w = cell_width(&cell_of(step, base + i as u64, s), col, s);
        if w > h - col {
            break;
        }
        i += step.bytes();
        col += w;
    }
    (i, col)
}

fn display(c: char, off: u64, s: &LayoutSpec) -> Cell {
    let (ch, width, tab, dim) = match c {
        '\t' => (' ', 0, true, false),
        '\r' if s.eol == Eol::Mixed => ('\u{240D}', 1, false, true),
        c if c.is_control() => ('\u{B7}', 1, false, true),
        c => (c, c.width().unwrap_or(1).min(2) as u8, false, false),
    };
    Cell {
        ch,
        src: c,
        off,
        width,
        tab,
        bad: false,
        dim,
    }
}

/// Ширина ячейки в колонке `col` (таб — до следующей остановки).
fn cell_width(c: &Cell, col: u32, s: &LayoutSpec) -> u32 {
    if c.tab {
        s.tab() - col % s.tab()
    } else {
        c.width.into()
    }
}

/// Символ при переносе: смещение от начала сегмента, ширина ([`WRAP_TAB`] — таб) и
/// начало слова.
#[derive(Copy, Clone)]
struct WrapCell {
    off: u32,
    width: u8,
    word: bool,
}

const WRAP_TAB: u8 = u8::MAX;

/// Символы сегмента для переноса и его текст (исходные символы, границы слов).
fn wrap_cells(bytes: &[u8], base: u64, s: &LayoutSpec, text: &mut String) -> Vec<WrapCell> {
    let mut cs = Vec::with_capacity(bytes.len());
    text.reserve(bytes.len());
    cells(bytes, base, s, |c| {
        text.push(c.src);
        cs.push(WrapCell {
            off: (c.off - base) as u32,
            width: if c.tab { WRAP_TAB } else { c.width },
            word: false,
        });
        true
    });
    cs
}

/// Начала слов UAX#29 по исходным символам.
fn mark_words(text: &str, cs: &mut [WrapCell]) {
    let b = text.as_bytes();
    let (mut ci, mut bi) = (0usize, 0usize);
    for (at, _) in text.split_word_bound_indices() {
        while bi < at {
            bi += match b[bi] {
                0..0xC0 => 1,
                0xC0..0xE0 => 2,
                0xE0..0xF0 => 3,
                _ => 4,
            };
            ci += 1;
        }
        if let Some(c) = cs.get_mut(ci) {
            c.word = true;
        }
    }
}

/// Подстроки байтов `bytes` сегмента `sg` (не пустых).
fn wrap_bytes(bytes: &[u8], sg: &Seg, s: &LayoutSpec, kind: WrapKind) -> Vec<Range<u64>> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut cs = wrap_cells(bytes, sg.start, s, &mut text);
    if cs.is_empty() {
        out.push(sg.start..sg.end);
        return out;
    }
    if kind == WrapKind::Words {
        mark_words(&text, &mut cs);
    }
    drop(text);
    let at = |i: usize| sg.start + u64::from(cs[i].off);
    let width = s.width();
    let (mut rs, mut col, mut brk, mut i) = (0usize, 0u32, None::<usize>, 0usize);
    while i < cs.len() {
        let c = cs[i];
        if c.word && i > rs {
            brk = Some(i);
        }
        let w = if c.width == WRAP_TAB {
            s.tab() - col % s.tab()
        } else {
            c.width.into()
        };
        if col + w > width && i > rs {
            let cut = brk.filter(|&k| k > rs).unwrap_or(i);
            out.push(at(rs)..at(cut));
            (rs, col, brk, i) = (cut, 0, None, cut);
            continue;
        }
        col += w;
        i += 1;
    }
    out.push(at(rs)..sg.end);
    // Байты BOM до первого символа — в первой подстроке.
    out[0].start = sg.start;
    out
}

/// Разбиений на подстроки помнится не больше: сегменты экрана и строки курсора.
const WRAPS: usize = 6;

/// Что определяет подстроки сегмента.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WrapKey {
    hash: u64,
    start: u64,
    end: u64,
    spec: LayoutSpec,
}

type Wrapped = Rc<[Range<u64>]>;

thread_local! {
    /// Сегмент длинной строки — 64 КиБ: перенос (UAX#29) не укладывается в кадр, а нужен
    /// несколько раз на кадр. Память — по содержимому, сброс не нужен; сверка байтов дешевле
    /// переноса на порядки.
    static WRAP_MEMO: RefCell<VecDeque<(WrapKey, Wrapped)>> =
        RefCell::new(VecDeque::with_capacity(WRAPS));
}

/// Подстроки переноса сегмента: диапазоны байтов.
fn wrap_seg(
    b: &(impl ByteView + ?Sized),
    sg: &Seg,
    s: &LayoutSpec,
    kind: WrapKind,
) -> Result<Wrapped, Missing> {
    if sg.end <= sg.start {
        return Ok(Rc::from([sg.start..sg.start]));
    }
    let mut buf = Vec::new();
    let bytes = row_bytes(b, sg.start..sg.end, &mut buf)?;
    let key = WrapKey {
        hash: rustc_hash::FxBuildHasher.hash_one(bytes),
        start: sg.start,
        end: sg.end,
        spec: *s,
    };
    let known =
        WRAP_MEMO.with_borrow(|m| m.iter().find(|(k, _)| *k == key).map(|(_, v)| Rc::clone(v)));
    if let Some(v) = known {
        return Ok(v);
    }
    let v: Wrapped = wrap_bytes(bytes, sg, s, kind).into();
    WRAP_MEMO.with_borrow_mut(|m| {
        if m.len() == WRAPS {
            m.pop_front();
        }
        m.push_back((key, Rc::clone(&v)));
    });
    Ok(v)
}

// ---------------------------------------------------------------- API

/// `n` визуальных строк вниз с позиции `from`.
pub fn rows_down(
    b: &(impl ByteView + ?Sized),
    from: (u64, u32),
    n: usize,
    s: &LayoutSpec,
) -> Result<SmallVec<[VRow; 64]>, Missing> {
    let mut rows = SmallVec::new();
    let len = b.len();
    if s.mode == Mode::Hex {
        let cols = s.cols();
        let mut p = from.0;
        while rows.len() < n && p < len {
            let e = (p + cols).min(len);
            rows.push(VRow {
                start: p,
                end: e,
                next: e,
                cont: false,
                cut: false,
                seg: p,
                sub: 0,
            });
            p = e;
        }
        return Ok(rows);
    }
    let (mut p, mut sub) = from;
    while rows.len() < n && p < len {
        let sg = segment(b, p, s)?;
        match s.wrap() {
            None => rows.push(VRow {
                start: p,
                end: sg.end,
                next: sg.next,
                cont: !sg.line_start,
                cut: sg.cut,
                seg: p,
                sub: 0,
            }),
            Some(kind) => {
                let subs = wrap_seg(b, &sg, s, kind)?;
                let last = subs.len() - 1;
                for (i, r) in subs.iter().enumerate().skip(sub as usize) {
                    if rows.len() == n {
                        break;
                    }
                    rows.push(VRow {
                        start: r.start,
                        end: r.end,
                        next: if i < last { subs[i + 1].start } else { sg.next },
                        cont: i > 0 || !sg.line_start,
                        cut: i == last && sg.cut,
                        seg: p,
                        sub: i as u32,
                    });
                }
            }
        }
        p = sg.next;
        sub = 0;
    }
    Ok(rows)
}

/// Позиция визуальной строки над `at`; в начале файла — `(0, 0)`.
pub fn row_up(
    b: &(impl ByteView + ?Sized),
    at: (u64, u32),
    s: &LayoutSpec,
) -> Result<(u64, u32), Missing> {
    if s.mode == Mode::Hex {
        let cols = s.cols();
        let p = at.0 / cols * cols;
        return Ok((p.saturating_sub(cols), 0));
    }
    if at.1 > 0 && s.wrap().is_some() {
        return Ok((at.0, at.1 - 1));
    }
    if at.0 == 0 {
        return Ok((0, 0));
    }
    let prev = line_start_before(b, at.0 - 1, s)?;
    match s.wrap() {
        None => Ok((prev, 0)),
        Some(kind) => {
            let sg = segment(b, prev, s)?;
            let n = wrap_seg(b, &sg, s, kind)?.len();
            Ok((prev, (n - 1) as u32))
        }
    }
}

/// Текст строки `r`: окно колонок `hscroll..hscroll + width_cols` (без переноса), табы,
/// управляющие `·`, U+FFFD, `␍` в `Mixed`; hex — смещение, байты, текст.
pub fn render_row(
    b: &(impl ByteView + ?Sized),
    r: &VRow,
    hscroll: u32,
    s: &LayoutSpec,
    out: &mut RowText,
) -> Result<(), Missing> {
    out.clear();
    if s.mode == Mode::Hex {
        return render_hex(b, r, s, out);
    }
    let h = if s.wrap().is_some() { 0 } else { hscroll };
    let right = h.saturating_add(s.width());
    let len = r.end - r.start;
    // Ряд длинной строки — десятки КиБ, на экране — сотни символов: читается окно с
    // запасом на два байта колонки; не хватило — вдвое больше.
    let mut buf = Vec::new();
    let mut want = (u64::from(right) * 2 + 256).min(len);
    loop {
        out.clear();
        out.cut = r.cut;
        out.unit = s.enc.unit();
        let bytes = row_bytes(b, r.start..r.start + want, &mut buf)?;
        let stop = fill_row(bytes, r, h, right, s, out);
        // Символы до `stop` целы: конец окна режет только последний символ.
        if want == len || stop.is_some_and(|off| off + MAX_CHAR <= r.start + want) {
            return Ok(());
        }
        want = (want * 2).min(len);
    }
}

/// Байт на символ не больше (обрезанный концом окна символ — только в стольких байтах).
const MAX_CHAR: u64 = 8;

/// Окно колонок `[h, right)` из байтов `bytes` с начала ряда `r`. Итог — первый символ
/// за правым краем; `None` — байты кончились раньше.
fn fill_row(
    bytes: &[u8],
    r: &VRow,
    h: u32,
    right: u32,
    s: &LayoutSpec,
    out: &mut RowText,
) -> Option<u64> {
    let (from, col) = skip_cols(bytes, r.start, s, h);
    fill_from(bytes, r, (from, col), h, right, s, out)
}

/// [`fill_row`] с места `at`: индекс в `bytes` и его колонка.
fn fill_from(
    bytes: &[u8],
    r: &VRow,
    at: (usize, u32),
    h: u32,
    right: u32,
    s: &LayoutSpec,
    out: &mut RowText,
) -> Option<u64> {
    let (from, mut col) = at;
    let mut ci = 0u32;
    let mut stop = None;
    cells_from(bytes, r.start, from, s, |c| {
        if col >= right {
            stop = Some(c.off);
            return false;
        }
        let w = cell_width(&c, col, s);
        let (c0, c1) = (col, col + w);
        col = c1;
        if w == 0 {
            // Нулевой ширины (комбинирующие) — к видимому соседу.
            if c0 >= h {
                out.anchor(ci, c.off);
                out.text.push(c.ch);
                ci += 1;
            }
            return true;
        }
        if c1 <= h {
            return true;
        }
        let (a, z) = (c0.max(h), c1.min(right));
        if c.tab || a > c0 || z < c1 {
            // Таб и обрезанный край широкого символа — пробелы.
            for _ in a..z {
                out.anchor(ci, c.off);
                out.text.push(' ');
                ci += 1;
            }
            return true;
        }
        out.anchor(ci, c.off);
        out.text.push(c.ch);
        if c.bad {
            RowText::mark(&mut out.bad, ci);
        } else if c.dim {
            mark4(&mut out.dim, ci);
        }
        ci += 1;
        true
    });
    out.anchor(ci, r.end);
    stop
}

/// Экранная колонка байта `off` в строке `r` от её начала (табы раскрыты, широкие — 2):
/// курсор редактора, память колонки вверх/вниз (ADR-0023 §7).
pub fn col_of(
    b: &(impl ByteView + ?Sized),
    r: &VRow,
    off: u64,
    s: &LayoutSpec,
) -> Result<u32, Missing> {
    let mut buf = Vec::new();
    let bytes = row_bytes(b, r.start..off.clamp(r.start, r.end), &mut buf)?;
    let (from, mut col) = skip_cols(bytes, r.start, s, u32::MAX);
    cells_from(bytes, r.start, from, s, |c| {
        col += cell_width(&c, col, s);
        true
    });
    Ok(col)
}

/// Колонки `[c0, c1)` строки `r` в байтах (блочное выделение, S7-40).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColSpan {
    /// Байты символов, задевающих колонки; ничего не задето (короткая строка, пустой
    /// блок) — пусто в месте вставки: у первого символа с колонки `c1` или у `r.end`.
    pub start: u64,
    pub end: u64,
    /// Колонка начала `start` (≤ `c0`, если символ задет; иначе — колонка места).
    pub col: u32,
    /// Ширина строки в колонках.
    pub width: u32,
}

/// Байты символов строки `r`, задевающих колонки `[c0, c1)`: символ, который колонки
/// задевает хоть частью (широкий, таб), входит целиком; нулевой ширины — со своим
/// видимым соседом.
pub fn col_span(
    b: &(impl ByteView + ?Sized),
    r: &VRow,
    c0: u32,
    c1: u32,
    s: &LayoutSpec,
) -> Result<ColSpan, Missing> {
    let mut bytes = Vec::new();
    read_range(b, r.start..r.end, &mut bytes)?;
    let mut out = ColSpan {
        start: r.end,
        end: r.end,
        col: 0,
        width: 0,
    };
    let (mut started, mut ended) = (false, false);
    let mut end_col = None;
    let mut col = 0u32;
    cells(&bytes, r.start, s, |c| {
        let w = cell_width(&c, col, s);
        if w > 0 {
            if !started && c0 < c1 && col + w > c0 && col < c1 {
                started = true;
                out.start = c.off;
                out.col = col;
            }
            if !ended && col >= c1 {
                ended = true;
                out.end = c.off;
                end_col = Some(col);
            }
        }
        col += w;
        true
    });
    out.width = col;
    if !started {
        out.start = out.end;
        out.col = end_col.unwrap_or(col);
    }
    Ok(out)
}

/// Начало символа строки `r`, который покрывает колонку `col` (дальше конца — `r.end`):
/// курсор на ряд выше или ниже, клик мышью.
pub fn off_at_col(
    b: &(impl ByteView + ?Sized),
    r: &VRow,
    col: u32,
    s: &LayoutSpec,
) -> Result<u64, Missing> {
    let mut bytes = Vec::new();
    read_range(b, r.start..r.end, &mut bytes)?;
    let mut at = r.end;
    let mut c0 = 0u32;
    cells(&bytes, r.start, s, |c| {
        let w = cell_width(&c, c0, s);
        if c0 + w > col {
            at = c.off;
            return false;
        }
        c0 += w;
        true
    });
    Ok(at)
}

fn mark4(list: &mut SmallVec<[Range<u32>; 4]>, ci: u32) {
    match list.last_mut() {
        Some(r) if r.end == ci => r.end = ci + 1,
        _ => list.push(ci..ci + 1),
    }
}

/// Колонки строки hex в символах.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HexGeom {
    /// Цифр смещения: 8, при длине > 4 ГБ — 12.
    pub off_w: u32,
    /// Начало байтов.
    pub bytes_at: u32,
    /// Начало текста.
    pub text_at: u32,
}

/// Раскладка строки hex: смещение, два пробела, байты (группы по 8 через два пробела),
/// два пробела, текст.
pub fn hex_geom(cols: u8, len: u64) -> HexGeom {
    let off_w = if len > 1 << 32 { 12 } else { 8 };
    let bytes_at = off_w + 2;
    let cols = u32::from(cols.max(1));
    let groups = cols.div_ceil(8);
    let bytes_w = cols * 3 - 1 + (groups - 1);
    HexGeom {
        off_w,
        bytes_at,
        text_at: bytes_at + bytes_w + 2,
    }
}

fn render_hex(
    b: &(impl ByteView + ?Sized),
    r: &VRow,
    s: &LayoutSpec,
    out: &mut RowText,
) -> Result<(), Missing> {
    use std::fmt::Write;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let cols = s.cols() as usize;
    let g = hex_geom(s.hex_cols, b.len());
    let mut bytes = Vec::new();
    // Хвост символа UTF-8 / суррогата за краем строки — для текста последнего байта.
    read_range(b, r.start..(r.end + 3).min(b.len()), &mut bytes)?;
    let n = (r.end - r.start) as usize;
    let _ = write!(out.text, "{:0w$X}  ", r.start, w = g.off_w as usize);
    for i in 0..cols {
        if i > 0 {
            out.text.push(' ');
            if i % 8 == 0 {
                out.text.push(' ');
            }
        }
        match bytes.get(i).filter(|_| i < n) {
            Some(&x) => {
                out.text.push(HEX[(x >> 4) as usize] as char);
                out.text.push(HEX[(x & 15) as usize] as char);
            }
            None => out.text.push_str("  "),
        }
    }
    out.text.push_str("  ");
    out.unit = 1;
    let base = out.text.chars().count() as u32;
    out.map.push((base, r.start));
    let dot = '\u{B7}';
    let shown = |c: char| {
        if c.is_control() || c == '\u{FFFD}' {
            dot
        } else {
            c
        }
    };
    let enc = s.enc;
    if let Some(t) = enc.table() {
        for &x in &bytes[..n] {
            out.text.push(shown(t.0[x as usize]));
        }
    } else if enc.is_utf8() {
        for i in 0..n {
            if !is_utf8_boundary(bytes[i]) {
                out.text.push(dot);
                continue;
            }
            match CharDecoder::new(enc).next(&bytes[i..]) {
                Step::Char(c, _) => out.text.push(shown(c)),
                Step::Bad(_) => out.text.push(dot),
            }
        }
    } else if enc.is_utf16() {
        let mut dec = CharDecoder::new(enc);
        for i in 0..n {
            if !(r.start + i as u64).is_multiple_of(2) {
                out.text.push(' ');
                continue;
            }
            match dec.next(&bytes[i..]) {
                Step::Char(c, _) => out.text.push(shown(c)),
                Step::Bad(_) => out.text.push(dot),
            }
        }
    } else {
        // Прочие многобайтовые: символ на первом байте, продолжения — `·`.
        let mut dec = CharDecoder::new(enc);
        let mut i = 0;
        while i < n {
            let step = dec.next(&bytes[i..]);
            out.text.push(match step {
                Step::Char(c, _) => shown(c),
                Step::Bad(_) => dot,
            });
            for _ in 1..step.bytes().min(n - i) {
                out.text.push(dot);
            }
            i += step.bytes();
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
