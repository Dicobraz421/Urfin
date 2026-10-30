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

use std::ops::Range;

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
    match find_sep(b, start, limit, sep)? {
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

/// Символы байтов `bytes` (с абсолютного `base`); BOM пропускается. `f` → `false` — стоп.
fn cells(bytes: &[u8], base: u64, s: &LayoutSpec, mut f: impl FnMut(Cell) -> bool) {
    let mut dec = CharDecoder::new(s.enc);
    let skip = u64::from(s.enc.bom_len)
        .saturating_sub(base)
        .min(bytes.len() as u64) as usize;
    let mut i = skip;
    while i < bytes.len() {
        let step = dec.next(&bytes[i..]);
        let off = base + i as u64;
        let cell = match step {
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
        };
        i += step.bytes();
        if !f(cell) {
            break;
        }
    }
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

/// Подстроки переноса сегмента: диапазоны байтов.
fn wrap_seg(
    b: &(impl ByteView + ?Sized),
    sg: &Seg,
    s: &LayoutSpec,
    kind: WrapKind,
) -> Result<SmallVec<[Range<u64>; 8]>, Missing> {
    let mut out = SmallVec::new();
    if sg.end <= sg.start {
        out.push(sg.start..sg.start);
        return Ok(out);
    }
    let mut bytes = Vec::new();
    read_range(b, sg.start..sg.end, &mut bytes)?;
    let mut cs: Vec<Cell> = Vec::with_capacity(bytes.len());
    cells(&bytes, sg.start, s, |c| {
        cs.push(c);
        true
    });
    if cs.is_empty() {
        out.push(sg.start..sg.end);
        return Ok(out);
    }
    // Начала слов UAX#29 (по исходным символам).
    let mut word = vec![false; cs.len()];
    if kind == WrapKind::Words {
        let text: String = cs.iter().map(|c| c.src).collect();
        let mut ci = 0usize;
        let mut bi = 0usize;
        for (at, _) in text.split_word_bound_indices() {
            while bi < at {
                bi += cs[ci].src.len_utf8();
                ci += 1;
            }
            if ci < word.len() {
                word[ci] = true;
            }
        }
    }
    let width = s.width();
    let (mut rs, mut col, mut brk, mut i) = (0usize, 0u32, None::<usize>, 0usize);
    while i < cs.len() {
        if word[i] && i > rs {
            brk = Some(i);
        }
        let w = cell_width(&cs[i], col, s);
        if col + w > width && i > rs {
            let cut = brk.filter(|&k| k > rs).unwrap_or(i);
            out.push(cs[rs].off..cs[cut].off);
            (rs, col, brk, i) = (cut, 0, None, cut);
            continue;
        }
        col += w;
        i += 1;
    }
    out.push(cs[rs].off..sg.end);
    // Байты BOM до первого символа — в первой подстроке.
    out[0].start = sg.start;
    Ok(out)
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
    out.cut = r.cut;
    out.unit = s.enc.unit();
    let mut bytes = Vec::new();
    read_range(b, r.start..r.end, &mut bytes)?;
    let h = if s.wrap().is_some() { 0 } else { hscroll };
    let right = h + s.width();
    let mut col = 0u32;
    let mut ci = 0u32;
    cells(&bytes, r.start, s, |c| {
        if col >= right {
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
    Ok(())
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
