//! Правка на движке — в handler `editor` (ADR-0023 §7, §9; S7 §5.2). Карточка S7-29.
//!
//! [`Engine`] — текст документа из кусков, курсор и выделение ([`Selections`]: курсор и
//! потоковое или блочное выделение; операции блока — S7-40, мультикурсора нет), верх
//! экрана и журнал отмены. Движения — по раскладке S7-06 (`rows_down`, `row_up` по
//! `TextView`): вверх/вниз — по визуальным рядам с памятью колонки, слова —
//! `unicode-segmentation` по сегменту строки (≤ `max_line`, 64 КБ). Байты — только из
//! кэшей страниц: движение, упёршееся в `Missing`, ждёт страниц ([`Engine::wait`]) и
//! завершается по их приходу. Ввод кодируется в кодировку документа; символа нет —
//! [`EdAsk::NoChar`].

use std::borrow::Cow;
use std::ops::Range;
use std::time::Instant;

use compact_str::CompactString;
use encoding_rs::{EncoderResult, UTF_8, UTF_16BE, UTF_16LE};
use unicode_segmentation::UnicodeSegmentation;

use super::EdAsk;
use super::fold;
use super::open::Guard;
use super::piece::TextDoc;
use super::undo::{Journal, Kind};
use crate::cache::{ByteView, Missing, PAGE, Prio};
use crate::detect::Eol;
use crate::enc::{CharDecoder, Enc, Step};
use crate::layout::{
    LayoutSpec, Mode, VRow, WrapKind, col_of, line_start_before, off_at_col, rows_down, shown_from,
};
use crate::viewer::Scroll;

#[path = "block.rs"]
pub mod block;

pub use block::{Area, BlockOut};

#[path = "md.rs"]
pub mod md;

pub use md::{MdAct, MdCtx};

/// Блочное выделение (Alt+Shift+стрелки, S7-40): угол `anchor` (смещение) и колонки
/// от `anchor_col` до `col`; второй угол — курсор.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub anchor: u64,
    pub anchor_col: u32,
    pub col: u32,
}

/// Выделение при курсоре.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Sel {
    #[default]
    None,
    /// Потоковое: от `anchor` до курсора.
    Stream {
        anchor: u64,
    },
    Block(Block),
}

/// Курсор и выделение документа (набор — заготовка мультикурсора после M4).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Selections {
    pub caret: u64,
    pub sel: Sel,
}

impl Selections {
    pub fn at(caret: u64) -> Selections {
        Selections {
            caret,
            sel: Sel::None,
        }
    }

    /// Непустое потоковое выделение.
    pub fn stream(&self) -> Option<Range<u64>> {
        match self.sel {
            Sel::Stream { anchor } if anchor != self.caret => {
                Some(anchor.min(self.caret)..anchor.max(self.caret))
            }
            _ => None,
        }
    }
}

/// Куда двигаться (`editor.move to=`, `editor.delete to=`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

impl Move {
    pub fn parse(s: &str) -> Option<Move> {
        Some(match s {
            "left" => Move::Left,
            "right" => Move::Right,
            "word_left" => Move::WordLeft,
            "word_right" => Move::WordRight,
            "up" => Move::Up,
            "down" => Move::Down,
            "home" => Move::Home,
            "end" => Move::End,
            "page_up" => Move::PageUp,
            "page_down" => Move::PageDown,
            "doc_start" => Move::DocStart,
            "doc_end" => Move::DocEnd,
            _ => return None,
        })
    }
}

/// Расширение выделения движением (`editor.move select=`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Extend {
    #[default]
    No,
    /// Shift.
    Stream,
    /// Alt+Shift.
    Block,
}

/// Мышь: клик, двойной, тройной (`InboxMsg::Caret`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Unit {
    #[default]
    Char,
    Word,
    Line,
}

/// Действие над документом (команда или событие UI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Move(Move, Extend),
    Delete(Move),
    /// Набранный текст, commit IME (UTF-8); `replace` — непредставимые как `?`.
    Input {
        text: CompactString,
        replace: bool,
    },
    /// Вставка из буфера обмена — одна группа отмены.
    Paste {
        text: String,
        replace: bool,
        /// В буфере — блок (`MSDEVColumnSelect`): вставка колонкой.
        column: bool,
    },
    /// `editor.copy` / `editor.cut` при блоке: текст — в [`Engine::block_out`];
    /// `limit` — лимит копирования, байт.
    BlockCopy {
        cut: bool,
        limit: u64,
    },
    Newline,
    Tab,
    ShiftLeft,
    ShiftRight,
    DeleteLine,
    SelectAll,
    Caret {
        off: u64,
        extend: bool,
        unit: Unit,
        /// Alt+мышь: блок до экранной колонки ряда `off` (за концом строки — тоже).
        block: Option<u32>,
    },
    /// Команда `editor.md.*`, флажок задачи предпросмотра (S7-24).
    Md(MdAct),
    /// `editor.goto`: курсор на начало строки `off` и экранную колонку `col` (с 0).
    Goto {
        off: u64,
        col: u32,
    },
}

impl Act {
    /// Меняет текст (только для чтения — нельзя).
    pub fn edits(&self) -> bool {
        !matches!(
            self,
            Act::Move(..)
                | Act::SelectAll
                | Act::Caret { .. }
                | Act::Goto { .. }
                | Act::BlockCopy { cut: false, .. }
        )
    }

    /// То же действие, но непредставимые символы — `?` («Вставить «?»»).
    #[must_use]
    pub fn with_replace(self) -> Act {
        match self {
            Act::Input { text, .. } => Act::Input {
                text,
                replace: true,
            },
            Act::Paste { text, column, .. } => Act::Paste {
                text,
                replace: true,
                column,
            },
            a => a,
        }
    }
}

/// Итог действия.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    /// Ничего не изменилось.
    Same,
    /// Курсор, выделение или вид.
    Moved,
    /// Текст изменён.
    Edited,
    /// Нужен ответ (действие держится в [`Engine::held`], вопрос — `EditorDoc::ask`).
    Ask(EdAsk),
}

/// Что документ даёт действию.
#[derive(Copy, Clone, Debug)]
pub struct Env {
    /// Конец новых строк.
    pub eol_new: Eol,
    /// Правка запрещена (атрибут, сохранение, вопрос, задача).
    pub readonly: bool,
    pub now: Instant,
}

/// Окно до первого `Viewport`.
const DEFAULT_VIEW: (u32, u32) = (80, 25);
/// Слов ищется в окне не длиннее (ADR-0023 §7).
const WORD_WINDOW: u64 = 64 << 10;
/// Страницы вокруг видимой части: мини-карта и прокрутка без скелетонов.
pub const NEAR: u64 = 128 << 10;

/// Документ движка: текст, курсор и выделение, экран, отмена.
#[allow(
    clippy::struct_excessive_bools,
    clippy::many_single_char_names,
    reason = "независимые признаки документа; v — вид, s — раскладка, r — ряд"
)]
#[derive(Debug)]
pub struct Engine {
    pub text: TextDoc,
    /// Кодировка байт документа (`bom_len` = 0: куски — после BOM).
    pub enc: Enc,
    /// Концы строк для раскладки (`Mixed` — одиночный CR рисуется `␍`).
    pub eol: Eol,
    pub sel: Selections,
    /// Верх экрана: начало сегмента и подстрока переноса; `hscroll` — колонка.
    pub top: u64,
    pub top_sub: u32,
    pub hscroll: u32,
    /// Ins: режим замены.
    pub overwrite: bool,
    pub undo: Journal<Selections>,
    pub guard: Guard,
    /// Окно текста из `Viewport` (0 — ещё нет).
    pub cols: u32,
    pub rows: u32,
    pub tab: u8,
    pub expand_tabs: bool,
    pub wrap: bool,
    pub max_line: u32,
    /// Байты исходника в документе (после BOM): полоса изменений.
    pub orig: Range<u64>,
    /// Чтение исходника не удалось (носитель пропал): документ только для чтения.
    pub read_failed: bool,
    /// Фоновая задача над документом (перевод, концы строк, копия): только для чтения.
    pub busy: bool,
    /// Действие, ждущее ответа на вопрос документа.
    pub held: Option<Act>,
    /// Действие, ждущее страниц.
    pub wait: Option<Act>,
    /// Вывод курсора ждёт страниц.
    pub need_reveal: bool,
    /// Байты документа, которых ждут действие или вывод курсора.
    pub need: Option<Range<u64>>,
    /// Колонка, которую помнят вверх/вниз.
    goal: Option<u32>,
    /// Итог блочной операции для документа: текст в буфер обмена, тост, задача.
    pub block_out: Option<BlockOut>,
    /// Строк блока правится сразу не больше (выше — задачей, S7-42); тесты меняют.
    pub block_lines: usize,
}

/// Текст в кодировке документа; непредставимый символ — `Err` (или `?` при `replace`).
pub fn encode(enc: &Enc, s: &str, replace: bool) -> Result<Vec<u8>, char> {
    if enc.enc == UTF_8 {
        return Ok(s.as_bytes().to_vec());
    }
    if enc.enc == UTF_16LE || enc.enc == UTF_16BE {
        let be = enc.enc == UTF_16BE;
        return Ok(s
            .encode_utf16()
            .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
            .collect());
    }
    let mut e = enc.enc.new_encoder();
    let mut out = Vec::with_capacity(s.len() + 8);
    let mut rest = s;
    loop {
        let need = e
            .max_buffer_length_from_utf8_without_replacement(rest.len())
            .unwrap_or(rest.len() * 4 + 16);
        out.reserve(need);
        let (r, read) = e.encode_from_utf8_to_vec_without_replacement(rest, &mut out, true);
        rest = &rest[read..];
        match r {
            EncoderResult::InputEmpty => return Ok(out),
            EncoderResult::OutputFull => {}
            EncoderResult::Unmappable(c) if replace => {
                let _ = c;
                out.push(b'?');
            }
            EncoderResult::Unmappable(c) => return Err(c),
        }
    }
}

/// Концы строк текста — `eol` (`\r\n`, `\r`, `\n` → один вид).
pub fn norm_eol(s: &str, eol: Eol) -> Cow<'_, str> {
    if !s.contains(['\r', '\n']) {
        return Cow::Borrowed(s);
    }
    let to = match eol {
        Eol::CrLf => "\r\n",
        Eol::Cr => "\r",
        Eol::Lf | Eol::Mixed => "\n",
    };
    let mut out = String::with_capacity(s.len() + 16);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str(to);
            }
            '\n' => out.push_str(to),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn eol_str(eol: Eol) -> &'static str {
    match eol {
        Eol::CrLf => "\r\n",
        Eol::Cr => "\r",
        Eol::Lf | Eol::Mixed => "\n",
    }
}

/// Байты `[off, off + buf.len())` из кэшей; меньше — конец документа.
fn peek(v: &dyn ByteView, off: u64, buf: &mut [u8]) -> Result<usize, Missing> {
    let mut done = 0;
    while done < buf.len() {
        let c = v.chunk(off + done as u64)?;
        if c.is_empty() {
            break;
        }
        let k = c.len().min(buf.len() - done);
        buf[done..done + k].copy_from_slice(&c[..k]);
        done += k;
    }
    Ok(done)
}

fn read_range(v: &dyn ByteView, r: Range<u64>) -> Result<Vec<u8>, Missing> {
    let mut out = vec![0u8; (r.end - r.start) as usize];
    let n = peek(v, r.start, &mut out)?;
    out.truncate(n);
    Ok(out)
}

/// Байты пропущенных страниц документа (просьба кэшу).
pub(crate) fn missing_range(m: &Missing) -> Range<u64> {
    m.pages.start * PAGE..m.pages.end * PAGE
}

#[allow(
    clippy::many_single_char_names,
    reason = "v — вид, s — раскладка, r — ряд, b — байты, p — позиция"
)]
impl Engine {
    pub fn new(text: TextDoc, enc: Enc, eol: Eol, guard: Guard, orig: Range<u64>) -> Engine {
        Engine {
            text,
            enc: Enc { bom_len: 0, ..enc },
            eol,
            sel: Selections::default(),
            top: 0,
            top_sub: 0,
            hscroll: 0,
            overwrite: false,
            undo: Journal::new(),
            guard,
            cols: 0,
            rows: 0,
            tab: 4,
            expand_tabs: true,
            wrap: false,
            max_line: 65_536,
            orig,
            read_failed: false,
            busy: false,
            held: None,
            wait: None,
            need_reveal: false,
            need: None,
            goal: None,
            block_out: None,
            block_lines: block::LINES_MAX,
        }
    }

    /// Окно текста (до первого `Viewport` — 80×25).
    pub fn viewport(&self) -> (u32, u32) {
        if self.cols == 0 || self.rows == 0 {
            DEFAULT_VIEW
        } else {
            (self.cols, self.rows)
        }
    }

    /// Раскладка экрана.
    pub fn spec(&self) -> LayoutSpec {
        LayoutSpec {
            mode: Mode::Text,
            wrap: self.wrap.then_some(WrapKind::Words),
            width_cols: self.viewport().0,
            tab: self.tab,
            max_line: self.max_line,
            hex_cols: 16,
            enc: self.enc,
            eol: self.eol,
        }
    }

    /// Раскладка логических сегментов (без переноса): слова, строки, отступ, блок.
    pub fn seg_spec(&self) -> LayoutSpec {
        LayoutSpec {
            wrap: None,
            ..self.spec()
        }
    }

    fn unit(&self) -> u64 {
        u64::from(self.enc.unit())
    }

    pub fn len(&self) -> u64 {
        self.text.len()
    }

    /// Временные файлы `Rewrite(n)`, отпущенные журналом (обрезка, отрезанный повтор),
    /// — прочь, если на них не ссылается и текст (S7-44); освобождено.
    pub fn release_rewrites(&mut self) -> usize {
        let gone = self.undo.take_gone();
        if gone.is_empty() {
            return 0;
        }
        let undo = &self.undo;
        self.text.release_rewrites(&gone, |n| undo.uses_rewrite(n))
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Правка разрешена: нет задачи и ошибки чтения (вопрос документа — `Env::readonly`).
    pub fn editable(&self) -> bool {
        !self.busy && !self.read_failed
    }

    // ------------------------------------------------------------ символы

    /// Начало символа после `off` (CRLF — один шаг).
    fn next_char(&self, off: u64) -> Result<u64, Missing> {
        let v = self.text.view();
        if off >= v.len() {
            return Ok(v.len());
        }
        let mut b = [0u8; 16];
        let n = peek(&v, off, &mut b)?;
        let mut dec = CharDecoder::new(self.enc);
        let s = dec.next(&b[..n]);
        let k = s.bytes();
        if matches!(s, Step::Char('\r', _)) && k < n {
            let s2 = dec.next(&b[k..n]);
            if matches!(s2, Step::Char('\n', _)) {
                return Ok(off + (k + s2.bytes()) as u64);
            }
        }
        Ok(off + k.max(1) as u64)
    }

    /// Символ, начинающийся на `p`.
    fn char_at(&self, p: u64) -> Result<Option<char>, Missing> {
        let mut b = [0u8; 16];
        let n = peek(&self.text.view(), p, &mut b)?;
        if n == 0 {
            return Ok(None);
        }
        Ok(match CharDecoder::new(self.enc).next(&b[..n]) {
            Step::Char(c, _) => Some(c),
            Step::Bad(_) => None,
        })
    }

    /// Начало символа перед `off` (CRLF — один шаг).
    fn prev_char(&self, off: u64) -> Result<u64, Missing> {
        if off == 0 {
            return Ok(0);
        }
        let v = self.text.view();
        let u = self.unit();
        let p = if self.enc.is_utf16() {
            let lo = off.saturating_sub(4);
            let b = read_range(&v, lo..off)?;
            let unit = |i: usize| -> u16 {
                if self.enc.is_utf16be() {
                    u16::from_be_bytes([b[i], b[i + 1]])
                } else {
                    u16::from_le_bytes([b[i], b[i + 1]])
                }
            };
            let k = b.len();
            if k >= 4
                && (0xDC00..=0xDFFF).contains(&unit(k - 2))
                && (0xD800..=0xDBFF).contains(&unit(k - 4))
            {
                off - 4
            } else {
                off.saturating_sub(2)
            }
        } else if self.enc.is_utf8() {
            let lo = off.saturating_sub(4);
            let b = read_range(&v, lo..off)?;
            let mut i = b.len() - 1;
            while i > 0 && !crate::enc::is_utf8_boundary(b[i]) && b.len() - i < 4 {
                i -= 1;
            }
            let cand = lo + i as u64;
            match crate::enc::CharDecoder::new(self.enc).next(&b[i..]) {
                Step::Char(_, n) if cand + u64::from(n) == off => cand,
                _ => off - 1,
            }
        } else if self.enc.is_single_byte() {
            off - 1
        } else {
            // Многобайтовые без обратного разбора: от начала сегмента вперёд.
            let start = line_start_before(&v, off - 1, &self.seg_spec())?;
            let b = read_range(&v, start..off)?;
            let mut dec = CharDecoder::new(self.enc);
            let (mut i, mut last) = (0usize, 0usize);
            while i < b.len() {
                last = i;
                i += dec.next(&b[i..]).bytes().max(1);
            }
            start + last as u64
        };
        if p >= u && self.char_at(p)? == Some('\n') && self.char_at(p - u)? == Some('\r') {
            return Ok(p - u);
        }
        Ok(p)
    }

    // ------------------------------------------------------------ ряды

    /// Документ кончается разделителем строк (после него — пустая строка).
    fn ends_with_eol(&self) -> Result<bool, Missing> {
        let len = self.len();
        if len == 0 {
            return Ok(true);
        }
        let u = self.unit();
        let p = len.saturating_sub(u);
        Ok(matches!(self.char_at(p)?, Some('\n'))
            || (self.eol == Eol::Cr && matches!(self.char_at(p)?, Some('\r'))))
    }

    /// Пустая строка после последнего разделителя.
    fn tail_row(&self) -> VRow {
        let len = self.len();
        VRow {
            start: len,
            end: len,
            next: len,
            cont: false,
            cut: false,
            seg: len,
            sub: 0,
        }
    }

    /// Ряд раскладки `s`, в котором стоит `off`.
    fn row_in(&self, off: u64, s: &LayoutSpec) -> Result<VRow, Missing> {
        let v = self.text.view();
        let len = v.len();
        if off >= len && self.ends_with_eol()? {
            return Ok(self.tail_row());
        }
        let seg = line_start_before(&v, off, s)?;
        let mut n = 8usize;
        loop {
            let rows = rows_down(&v, (seg, 0), n, s)?;
            for r in &rows {
                if off < r.next || r.next >= len {
                    return Ok(*r);
                }
            }
            match rows.last() {
                Some(r) if rows.len() == n && r.seg == seg => n *= 4,
                Some(r) => return Ok(*r),
                None => return Ok(self.tail_row()),
            }
        }
    }

    /// Визуальный ряд курсора.
    fn row_of(&self, off: u64) -> Result<VRow, Missing> {
        self.row_in(off, &self.spec())
    }

    /// Логический сегмент строки (без переноса).
    fn seg_of(&self, off: u64) -> Result<VRow, Missing> {
        self.row_in(off, &self.seg_spec())
    }

    /// Ряд, начинающийся с позиции `pos`.
    fn row_at(&self, pos: (u64, u32), s: &LayoutSpec) -> Result<Option<VRow>, Missing> {
        Ok(rows_down(&self.text.view(), pos, 1, s)?.first().copied())
    }

    /// Ряд выше `r` (`None` — `r` первый).
    fn row_above(&self, r: &VRow) -> Result<Option<VRow>, Missing> {
        if r.pos() == (0, 0) {
            return Ok(None);
        }
        let s = self.spec();
        let p = fold::row_up(&self.text.view(), r.pos(), &s, &self.text.folds)?;
        self.row_at(p, &s)
    }

    /// Ряд ниже `r` (`None` — `r` последний).
    fn row_below(&self, r: &VRow) -> Result<Option<VRow>, Missing> {
        let len = self.len();
        if r.seg >= len {
            return Ok(None);
        }
        let s = self.spec();
        let rows = fold::rows_down(&self.text.view(), r.pos(), 2, &s, &self.text.folds)?;
        if let Some(n) = rows.get(1) {
            return Ok(Some(*n));
        }
        Ok((r.next >= len && r.next > r.end && self.ends_with_eol()?).then(|| self.tail_row()))
    }

    /// Колонка `off` в ряду `r`.
    fn col_in(&self, r: &VRow, off: u64) -> Result<u32, Missing> {
        col_of(&self.text.view(), r, off, &self.spec())
    }

    /// Смещение колонки `col` в ряду `r`.
    fn off_in(&self, r: &VRow, col: u32) -> Result<u64, Missing> {
        off_at_col(&self.text.view(), r, col, &self.spec())
    }

    // ------------------------------------------------------------ слова

    /// Текст сегмента (окно ≤ 64 КБ вокруг `off`) и смещения начал символов.
    fn words_of(&self, seg: &VRow, off: u64) -> Result<(String, Vec<(usize, u64)>), Missing> {
        let lo = seg.start.max(off.saturating_sub(WORD_WINDOW));
        let lo = if lo == seg.start {
            lo
        } else {
            // Граница символа: от начала сегмента вперёд не читаем — шаг назад до неё.
            self.prev_char(self.next_char(lo)?)?
        };
        let hi = seg.end.min(off.saturating_add(WORD_WINDOW));
        let b = read_range(&self.text.view(), lo..hi)?;
        let mut dec = CharDecoder::new(self.enc);
        let mut text = String::with_capacity(b.len());
        let mut map = Vec::with_capacity(b.len());
        let mut i = 0usize;
        while i < b.len() {
            let s = dec.next(&b[i..]);
            map.push((text.len(), lo + i as u64));
            text.push(match s {
                Step::Char(c, _) => c,
                Step::Bad(_) => '\u{FFFD}',
            });
            i += s.bytes().max(1);
        }
        map.push((text.len(), lo + i as u64));
        Ok((text, map))
    }

    /// Слова UAX#29 окна: (начало, конец, не пробел).
    fn word_spans(text: &str, map: &[(usize, u64)]) -> Vec<(u64, u64, bool)> {
        let at = |i: usize| {
            let k = map.partition_point(|m| m.0 < i);
            map.get(k).map_or(map.last().map_or(0, |m| m.1), |m| m.1)
        };
        text.split_word_bound_indices()
            .map(|(i, w)| (at(i), at(i + w.len()), !w.chars().all(char::is_whitespace)))
            .collect()
    }

    /// Начало следующего слова (Ctrl+→); в конце строки — начало следующей.
    fn word_right(&self, off: u64) -> Result<u64, Missing> {
        let seg = self.seg_of(off)?;
        if off >= seg.end {
            return Ok(seg.next.max(off).min(self.len()));
        }
        let (text, map) = self.words_of(&seg, off)?;
        Ok(Self::word_spans(&text, &map)
            .into_iter()
            .find(|&(a, _, w)| a > off && w)
            .map_or(seg.end, |(a, _, _)| a))
    }

    /// Начало слова слева (Ctrl+←); в начале строки — конец предыдущей.
    fn word_left(&self, off: u64) -> Result<u64, Missing> {
        let seg = self.seg_of(off)?;
        if off <= seg.start {
            return self.prev_char(off);
        }
        let (text, map) = self.words_of(&seg, off)?;
        Ok(Self::word_spans(&text, &map)
            .into_iter()
            .rev()
            .find(|&(a, _, w)| a < off && w)
            .map_or(seg.start, |(a, _, _)| a))
    }

    /// Слово под `off` (двойной клик).
    fn word_at(&self, off: u64) -> Result<Range<u64>, Missing> {
        let seg = self.seg_of(off)?;
        if seg.start == seg.end {
            return Ok(off..off);
        }
        let (text, map) = self.words_of(&seg, off)?;
        Ok(Self::word_spans(&text, &map)
            .into_iter()
            .find(|&(a, b, _)| a <= off && off < b)
            .map_or(off..off, |(a, b, _)| a..b))
    }

    // ------------------------------------------------------------ движения

    /// Курсор после движения `m` с `off`; `goal` — колонка вверх/вниз.
    fn target(&mut self, m: Move, off: u64) -> Result<u64, Missing> {
        Ok(match m {
            Move::Left => self.prev_char(off)?,
            Move::Right => self.next_char(off)?,
            Move::WordLeft => self.word_left(off)?,
            Move::WordRight => self.word_right(off)?,
            Move::Home => self.row_of(off)?.start,
            Move::End => self.row_of(off)?.end,
            Move::DocStart => 0,
            Move::DocEnd => self.len(),
            Move::Up | Move::Down | Move::PageUp | Move::PageDown => {
                let n = match m {
                    Move::Up | Move::Down => 1,
                    _ => self.viewport().1.saturating_sub(1).max(1),
                };
                let down = matches!(m, Move::Down | Move::PageDown);
                let mut r = self.row_of(off)?;
                let goal = match self.goal {
                    Some(g) => g,
                    None => self.col_in(&r, off)?,
                };
                let mut moved = false;
                for _ in 0..n {
                    let next = if down {
                        self.row_below(&r)?
                    } else {
                        self.row_above(&r)?
                    };
                    match next {
                        Some(x) => {
                            r = x;
                            moved = true;
                        }
                        None => break,
                    }
                }
                let to = if moved {
                    self.off_in(&r, goal)?
                } else if down {
                    self.len()
                } else {
                    0
                };
                self.goal = Some(goal);
                return Ok(to);
            }
        })
    }

    fn do_move(&mut self, m: Move, ext: Extend) -> Result<Done, Missing> {
        if ext == Extend::Block {
            return self.block_move(m);
        }
        let before = self.sel;
        if ext == Extend::No
            && let Some(r) = before.stream()
            && matches!(m, Move::Left | Move::Right)
        {
            // Стрелка при выделении — к его краю.
            self.goal = None;
            self.sel = Selections::at(if m == Move::Left { r.start } else { r.end });
            return Ok(self.after_move(before));
        }
        let keep = matches!(m, Move::Up | Move::Down | Move::PageUp | Move::PageDown);
        if !keep {
            self.goal = None;
        }
        let to = self.target(m, before.caret)?;
        self.sel = match ext {
            Extend::No => Selections::at(to),
            Extend::Stream => Selections {
                caret: to,
                sel: Sel::Stream {
                    anchor: match before.sel {
                        Sel::Stream { anchor } => anchor,
                        _ => before.caret,
                    },
                },
            },
            Extend::Block => unreachable!("блок — block_move"),
        };
        if matches!(m, Move::PageUp | Move::PageDown) {
            // Экран — на ту же страницу (страниц нет — только вывод курсора).
            let n = self.viewport().1.saturating_sub(1).max(1);
            let _ = self.scroll_rows(m == Move::PageDown, n);
        }
        Ok(self.after_move(before))
    }

    /// После смены курсора (без `?`: повтор действия сдвинул бы курсор дважды).
    fn after_move(&mut self, before: Selections) -> Done {
        self.undo.break_group();
        self.reveal_soft();
        if before == self.sel {
            Done::Same
        } else {
            Done::Moved
        }
    }

    /// Вывод курсора; страниц нет — по их приходу ([`Engine::need_reveal`]).
    fn reveal_soft(&mut self) {
        match self.reveal() {
            Ok(()) => self.need_reveal = false,
            Err(m) => {
                self.need_reveal = true;
                self.need = Some(missing_range(&m));
            }
        }
    }

    /// Курсор — в видимую часть: верх экрана и горизонтальный сдвиг.
    pub fn reveal(&mut self) -> Result<(), Missing> {
        // Курсор в свёрнутом — разворачивает (S7-35); в конце документа без перевода
        // строки он на последней скрытой строке.
        let caret = self.sel.caret;
        if !self.text.folds.reveal(caret)
            && caret == self.len()
            && caret > 0
            && self.text.folds.hidden(caret - 1).is_some()
            && !self.ends_with_eol()?
        {
            self.text.folds.reveal(caret - 1);
        }
        let s = self.spec();
        let rows = self.viewport().1 as usize;
        let r = self.row_of(self.sel.caret)?;
        let v = self.text.view();
        if r.pos() < (self.top, self.top_sub) {
            (self.top, self.top_sub) = r.pos();
        } else {
            let f = &self.text.folds;
            let shown = fold::rows_down(&v, (self.top, self.top_sub), rows, &s, f)?;
            let tail_shown = r.seg >= self.len() && shown.len() < rows;
            if !tail_shown && !shown.iter().any(|x| x.pos() == r.pos()) {
                let mut p = r.pos();
                if r.seg >= self.len() {
                    p = fold::row_up(&v, p, &s, f)?;
                }
                let k = if r.seg >= self.len() { 2 } else { 1 };
                for _ in k..rows {
                    if p == (0, 0) {
                        break;
                    }
                    p = fold::row_up(&v, p, &s, f)?;
                }
                (self.top, self.top_sub) = p;
            }
        }
        if !self.wrap {
            let cols = self.viewport().0;
            // Угол блока — и за концом строки.
            let col = match self.sel.sel {
                Sel::Block(b) => b.col,
                _ => self.col_in(&r, self.sel.caret)?,
            };
            if col < self.hscroll {
                self.hscroll = col;
            } else if col >= self.hscroll + cols {
                self.hscroll = col + 1 - cols;
            }
        }
        Ok(())
    }

    /// Экран на `n` рядов вниз или вверх (курсор не трогается).
    fn scroll_rows(&mut self, down: bool, n: u32) -> Result<bool, Missing> {
        let s = self.spec();
        let v = self.text.view();
        let before = (self.top, self.top_sub);
        let f = &self.text.folds;
        if down {
            let rows = self.viewport().1 as usize;
            let r = fold::rows_down(&v, before, (n as usize).saturating_add(rows), &s, f)?;
            if r.len() > rows {
                let k = (n as usize).min(r.len() - rows);
                (self.top, self.top_sub) = r[k].pos();
            }
        } else {
            let mut p = before;
            for _ in 0..n {
                if p == (0, 0) {
                    break;
                }
                p = fold::row_up(&v, p, &s, f)?;
            }
            (self.top, self.top_sub) = p;
        }
        Ok(before != (self.top, self.top_sub))
    }

    /// Прокрутка колесом или полосой (`InboxMsg::Scroll`): курсор остаётся.
    pub fn scroll(&mut self, sc: Scroll) -> Result<bool, Missing> {
        let rows = self.viewport().1;
        match sc {
            Scroll::Up(n) => self.scroll_rows(false, n),
            Scroll::Down(n) => self.scroll_rows(true, n),
            Scroll::PageUp(n) => self.scroll_rows(false, n.saturating_mul(rows)),
            Scroll::PageDown(n) => self.scroll_rows(true, n.saturating_mul(rows)),
            Scroll::Home => {
                let changed = (self.top, self.top_sub) != (0, 0);
                (self.top, self.top_sub) = (0, 0);
                Ok(changed)
            }
            Scroll::End => self.scroll_rows(true, u32::MAX / 2),
            Scroll::Left(n) => {
                let h = self.hscroll;
                self.hscroll = h.saturating_sub(n);
                Ok(h != self.hscroll)
            }
            Scroll::Right(n) => {
                let h = self.hscroll;
                if !self.wrap {
                    self.hscroll = h.saturating_add(n).min(self.max_line);
                }
                Ok(h != self.hscroll)
            }
        }
    }

    /// Бегунок полосы: верх экрана — к началу строки `off`.
    pub fn scroll_to(&mut self, off: u64) -> Result<bool, Missing> {
        let off = off.min(self.len());
        let s = self.spec();
        let start = line_start_before(&self.text.view(), off, &s)?;
        let changed = (self.top, self.top_sub) != (start, 0);
        (self.top, self.top_sub) = (start, 0);
        Ok(changed)
    }

    /// Байты видимых рядов (просьба кэшам); страниц нет — их диапазон.
    pub fn visible_range(&self) -> Range<u64> {
        let s = self.spec();
        let rows = self.viewport().1 as usize;
        let pos = (self.top, self.top_sub);
        match fold::rows_down(&self.text.view(), pos, rows, &s, &self.text.folds) {
            Ok(r) => match (r.first(), r.last()) {
                (Some(a), Some(z)) => a.start.min(self.top)..z.next.max(a.start + 1),
                _ => self.top..self.top + 1,
            },
            Err(m) => missing_range(&m),
        }
    }

    /// Куски экрана, где текст виден (ряды длинных строк показывают только начало): только
    /// им нужны цвета, когда видимое окно — мегабайты одной строки.
    pub(crate) fn shown_slices(&self) -> Option<Vec<Range<u64>>> {
        let s = self.spec();
        let (cols, rows) = self.viewport();
        let pos = (self.top, self.top_sub);
        let r =
            fold::rows_down(&self.text.view(), pos, rows as usize, &s, &self.text.folds).ok()?;
        let h = if self.wrap { 0 } else { self.hscroll };
        Some(
            r.iter()
                .map(|x| {
                    let a = shown_from(&self.text.view(), x, h, &s).unwrap_or((x.start, 0));
                    // Символ — до 8 байт (таб, UTF-16 с заменой): запас вместо точного счёта.
                    let bytes = (u64::from(h - a.1) + u64::from(cols)) * 8 + 256;
                    a.0..a.0.saturating_add(bytes).min(x.end.max(a.0 + 1))
                })
                .collect(),
        )
    }

    /// Страницы от loader'а → кэш. Ошибка чтения (носитель пропал) — строки остаются
    /// непрочитанными, документ — только для чтения ([`Engine::read_failed`]).
    pub fn on_pages(&mut self, src: super::piece::Src, loaded: crate::cache::Loaded) {
        if let Some(err) = &loaded.error {
            tracing::warn!(error = %err, "editor: source read failed, read-only");
            self.read_failed = true;
        }
        self.text.insert_pages(src, loaded);
    }

    /// Просьба кэшам: страницы под ждущее действие, иначе видимые с запасом
    /// [`NEAR`] по краям (мини-карта — окно строк вокруг видимой части, ADR-0023 §7).
    pub fn want_visible(&mut self) {
        let r = if let Some(r) = &self.need {
            r.clone()
        } else {
            let v = self.visible_range();
            // Страницы экрана — одним заданием (`viewer::ahead`), а не по одной.
            let r = v.start.saturating_sub(NEAR)..v.end.saturating_add(NEAR);
            let r = crate::viewer::ahead(r, self.viewport().1);
            r.start..r.end.min(self.len())
        };
        self.text.want(r, Prio::Visible);
    }

    // ------------------------------------------------------------ правка

    /// Заменяет `range` байтами `bytes`: курсор — за вставкой, запись в журнал.
    fn splice(&mut self, range: Range<u64>, bytes: &[u8], kind: Kind, now: Instant) -> Done {
        let before = self.sel;
        let res = if bytes.is_empty() {
            Ok(self.text.remove(range.clone()))
        } else {
            self.text.replace(range.clone(), bytes)
        };
        match res {
            Ok(sp) => {
                self.sel = Selections::at(range.start + bytes.len() as u64);
                self.goal = None;
                self.undo.record(sp.into(), kind, &before, &self.sel, now);
                Done::Edited
            }
            Err(e) => {
                tracing::warn!(error = %e, "editor: edit buffer write failed");
                Done::Same
            }
        }
    }

    /// Что заменяет ввод: выделение, в режиме замены — символ под курсором (не конец
    /// строки).
    fn input_range(&self) -> Result<Range<u64>, Missing> {
        if let Some(r) = self.sel.stream() {
            return Ok(r);
        }
        let c = self.sel.caret;
        if self.overwrite && c < self.len() && !matches!(self.char_at(c)?, Some('\r' | '\n')) {
            return Ok(c..self.next_char(c)?);
        }
        Ok(c..c)
    }

    /// Большая вставка готова (`paste`): текст уже во временном файле, здесь — один кусок
    /// в дереве, вместо выделения или в курсор, одна группа отмены.
    pub(super) fn paste_rewrite(&mut self, rw: super::write::Rewritten, now: Instant) -> Done {
        let src = self.text.push_rewrite(rw.src);
        let inserted: super::piece::Pieces = rw
            .pieces
            .into_iter()
            .map(|p| super::piece::Piece { src, ..p })
            .collect();
        let r = self.sel.stream().unwrap_or(self.sel.caret..self.sel.caret);
        let before = self.sel;
        self.undo.begin_group(Kind::Command);
        let removed = self.text.splice(r.start, r.end - r.start, &inserted);
        self.sel = Selections::at(r.start + super::piece::bytes_of(&inserted));
        self.goal = None;
        let sp = super::piece::Splice {
            at: r.start,
            removed,
            inserted,
        };
        self.undo
            .record(sp.into(), Kind::Command, &before, &self.sel, now);
        self.reveal_soft();
        Done::Edited
    }

    fn insert_text(
        &mut self,
        text: &str,
        replace: bool,
        kind: Kind,
        env: &Env,
    ) -> Result<Done, Missing> {
        let s = norm_eol(text, env.eol_new);
        let bytes = match encode(&self.enc, &s, replace) {
            Ok(b) => b,
            Err(ch) => return Ok(Done::Ask(EdAsk::NoChar { ch })),
        };
        let r = self.input_range()?;
        if kind == Kind::Command {
            self.undo.begin_group(Kind::Command);
        }
        let d = self.splice(r, &bytes, kind, env.now);
        self.reveal_soft();
        Ok(d)
    }

    /// Ведущие пробелы и табы строки `off` до курсора (автоотступ).
    fn indent_of(&self, off: u64) -> Result<String, Missing> {
        let seg = self.seg_of(off)?;
        if seg.cont {
            // Начало строки дальше `max_line` — без отступа.
            return Ok(String::new());
        }
        let b = read_range(&self.text.view(), seg.start..off.min(seg.end))?;
        let mut dec = CharDecoder::new(self.enc);
        let mut out = String::new();
        let mut i = 0;
        while i < b.len() {
            match dec.next(&b[i..]) {
                Step::Char(c @ (' ' | '\t'), n) => {
                    out.push(c);
                    i += usize::from(n);
                }
                _ => break,
            }
        }
        Ok(out)
    }

    /// Начала логических строк, задетых выделением (или строкой курсора); строка, перед
    /// которой выделение кончается, — нет.
    fn lines_of_sel(&self) -> Result<Vec<u64>, Missing> {
        let (a, b) = match (self.sel.stream(), self.area()?) {
            (Some(r), _) => (r.start, r.end),
            // Блок: строки от верхней до нижней (последняя — включая).
            (None, Some(x)) => (x.top, x.bottom + 1),
            (None, None) => (self.sel.caret, self.sel.caret),
        };
        let mut out = Vec::new();
        let mut seg = self.seg_of(a)?;
        loop {
            if !seg.cont {
                out.push(seg.start);
            }
            if seg.next >= b || seg.next >= self.len() || seg.next <= seg.start {
                break;
            }
            seg = self.seg_of(seg.next)?;
        }
        Ok(out)
    }

    /// Сдвиг строк выделения на отступ вправо или влево (одна группа).
    fn shift(&mut self, right: bool, env: &Env) -> Result<Done, Missing> {
        let lines = self.lines_of_sel()?;
        let unit = self.unit();
        let indent = if self.expand_tabs {
            " ".repeat(usize::from(self.tab.max(1)))
        } else {
            "\t".to_owned()
        };
        let ind = encode(&self.enc, &indent, true).unwrap_or_default();
        // Что убрать слева у каждой строки — до правки (байты из кэшей).
        let mut cuts = Vec::with_capacity(lines.len());
        if !right {
            for &l in &lines {
                let mut k = 0u64;
                while k < u64::from(self.tab.max(1)) {
                    match self.char_at(l + k * unit)? {
                        Some(' ') => k += 1,
                        Some('\t') if k == 0 => {
                            k = 1;
                            break;
                        }
                        _ => break,
                    }
                }
                cuts.push(k * unit);
            }
        }
        let before = self.sel;
        self.undo.begin_group(Kind::Command);
        let mut changed = false;
        for (i, &l) in lines.iter().enumerate().rev() {
            let res = if right {
                self.text.insert(l, &ind)
            } else if cuts[i] > 0 {
                Ok(self.text.remove(l..l + cuts[i]))
            } else {
                continue;
            };
            let Ok(sp) = res else {
                continue;
            };
            changed = true;
            self.undo
                .record(sp.into(), Kind::Command, &before, &before, env.now);
        }
        if !changed {
            return Ok(Done::Same);
        }
        let n = ind.len() as u64;
        let shift = |x: u64| {
            let mut y = x;
            for (i, &l) in lines.iter().enumerate() {
                if x > l {
                    if right {
                        y += n;
                    } else {
                        y -= cuts[i].min(x - l);
                    }
                }
            }
            y
        };
        self.sel = match before.sel {
            Sel::Stream { anchor } => Selections {
                caret: shift(before.caret),
                sel: Sel::Stream {
                    anchor: shift(anchor),
                },
            },
            _ => Selections::at(shift(before.caret)),
        };
        self.reveal_soft();
        Ok(Done::Edited)
    }

    /// Выполняет действие. `Err(Missing)` — нужны страницы: действие ждёт их
    /// (повтор — [`Engine::retry`]).
    pub fn act(&mut self, a: Act, env: &Env) -> Result<Done, Missing> {
        let res = self.run(&a, env);
        match &res {
            Err(m) => {
                let whole = a.edits() || matches!(a, Act::BlockCopy { .. });
                self.need = Some(if matches!(a, Act::Md(_)) {
                    self.md_need(m)
                } else {
                    whole
                        .then(|| self.block_need(m))
                        .flatten()
                        .unwrap_or_else(|| missing_range(m))
                });
                self.wait = Some(a);
            }
            Ok(Done::Ask(_)) => self.held = Some(a),
            Ok(_) => {}
        }
        res
    }

    /// Ждавшее страниц действие или вывод курсора — снова (после страниц); `None` —
    /// ждать нечего.
    pub fn retry(&mut self, env: &Env) -> Option<Result<Done, Missing>> {
        self.need = None;
        if let Some(a) = self.wait.take() {
            return Some(self.act(a, env));
        }
        if self.need_reveal {
            self.reveal_soft();
            return Some(Ok(Done::Moved));
        }
        None
    }

    #[allow(clippy::too_many_lines, reason = "таблица действий")]
    fn run(&mut self, a: &Act, env: &Env) -> Result<Done, Missing> {
        // Вырезание блока при запрете правки — только копия (`block_copy`).
        if a.edits() && !matches!(a, Act::BlockCopy { .. }) {
            if env.readonly || !self.editable() {
                return Ok(Done::Same);
            }
            if self.guard == Guard::Unlocked {
                return Ok(Done::Ask(EdAsk::Writer));
            }
        }
        let now = env.now;
        match a {
            Act::Move(m, ext) => self.do_move(*m, *ext),
            Act::SelectAll => {
                let before = self.sel;
                self.goal = None;
                self.sel = Selections {
                    caret: self.len(),
                    sel: Sel::Stream { anchor: 0 },
                };
                Ok(self.after_move(before))
            }
            Act::Goto { off, col } => {
                let before = self.sel;
                let mut at = (*off).min(self.len());
                if *col > 0 {
                    let seg = self.seg_of(at)?;
                    at = off_at_col(&self.text.view(), &seg, *col, &self.seg_spec())?;
                }
                self.goal = None;
                self.sel = Selections::at(at);
                Ok(self.after_move(before))
            }
            Act::Caret {
                off,
                extend,
                block: Some(col),
                ..
            } => self.block_caret(*off, *col, *extend),
            Act::Caret {
                off, extend, unit, ..
            } => {
                let before = self.sel;
                let off = (*off).min(self.len());
                self.goal = None;
                self.sel = match unit {
                    Unit::Char if *extend => Selections {
                        caret: off,
                        sel: Sel::Stream {
                            anchor: match before.sel {
                                Sel::Stream { anchor } => anchor,
                                _ => before.caret,
                            },
                        },
                    },
                    Unit::Char => Selections::at(off),
                    Unit::Word => {
                        let w = self.word_at(off)?;
                        Selections {
                            caret: w.end,
                            sel: Sel::Stream { anchor: w.start },
                        }
                    }
                    Unit::Line => {
                        let s = self.seg_of(off)?;
                        Selections {
                            caret: s.next,
                            sel: Sel::Stream { anchor: s.start },
                        }
                    }
                };
                Ok(self.after_move(before))
            }
            Act::BlockCopy { cut, limit } => self.block_copy(*cut, *limit, env),
            Act::Paste {
                text,
                replace,
                column: true,
            } => self.paste_column(text, *replace, env),
            Act::Input { .. } | Act::Paste { .. }
                if self.sel.stream().is_none() && self.area()?.is_some() =>
            {
                let (text, replace) = match a {
                    Act::Input { text, replace } => (text.as_str(), replace),
                    Act::Paste { text, replace, .. } => (text.as_str(), replace),
                    _ => unreachable!("ввод или вставка"),
                };
                let s = norm_eol(text, env.eol_new);
                let bytes = match encode(&self.enc, &s, *replace) {
                    Ok(b) => b,
                    Err(ch) => return Ok(Done::Ask(EdAsk::NoChar { ch })),
                };
                let a = self.area()?.expect("блок");
                let d = self.block_replace(&a, &bytes, now)?;
                self.reveal_soft();
                Ok(d)
            }
            Act::Input { text, replace } => self.insert_text(text, *replace, Kind::Input, env),
            Act::Paste { text, replace, .. } => {
                self.insert_text(text, *replace, Kind::Command, env)
            }
            Act::Newline if self.area()?.is_some() => {
                let ind = self.indent_of(self.sel.caret)?;
                let text = format!("{}{ind}", eol_str(env.eol_new));
                let bytes =
                    encode(&self.enc, &norm_eol(&text, env.eol_new), true).unwrap_or_default();
                let a = self.area()?.expect("блок");
                let d = self.block_replace(&a, &bytes, now)?;
                self.reveal_soft();
                Ok(d)
            }
            Act::Tab if self.area()?.is_some() => self.shift(true, env),
            Act::Delete(_) if self.sel.stream().is_none() && self.area()?.is_some() => {
                let a = self.area()?.expect("блок");
                let d = self.block_replace(&a, &[], now)?;
                self.reveal_soft();
                Ok(d)
            }
            Act::Newline => {
                let ind = self.indent_of(self.sel.caret)?;
                let text = format!("{}{ind}", eol_str(env.eol_new));
                let s = norm_eol(&text, env.eol_new);
                let bytes = encode(&self.enc, &s, true).unwrap_or_default();
                let r = self.sel.stream().unwrap_or(self.sel.caret..self.sel.caret);
                let d = self.splice(r, &bytes, Kind::Input, now);
                self.reveal_soft();
                Ok(d)
            }
            Act::Tab => {
                if let Some(r) = self.sel.stream()
                    && self.seg_of(r.start)?.next < r.end
                {
                    return self.shift(true, env);
                }
                let text = if self.expand_tabs {
                    let r = self.seg_of(self.sel.caret)?;
                    let col = col_of(&self.text.view(), &r, self.sel.caret, &self.seg_spec())?;
                    let t = u32::from(self.tab.max(1));
                    " ".repeat((t - col % t) as usize)
                } else {
                    "\t".to_owned()
                };
                self.insert_text(&text, true, Kind::Input, env)
            }
            Act::Md(m) => self.md(m, env),
            Act::ShiftLeft => self.shift(false, env),
            Act::ShiftRight => self.shift(true, env),
            Act::DeleteLine => {
                let s = self.seg_of(self.sel.caret)?;
                if s.start == s.next {
                    return Ok(Done::Same);
                }
                self.undo.begin_group(Kind::Command);
                let d = self.splice(s.start..s.next, &[], Kind::Command, now);
                self.reveal_soft();
                Ok(d)
            }
            Act::Delete(m) => {
                let r = if let Some(r) = self.sel.stream() {
                    r
                } else {
                    {
                        let c = self.sel.caret;
                        let to = match m {
                            Move::Left => self.prev_char(c)?,
                            Move::Right => self.next_char(c)?,
                            Move::WordLeft => self.word_left(c)?,
                            Move::WordRight => self.word_right(c)?,
                            other => {
                                let t = self.target(*other, c)?;
                                self.goal = None;
                                t
                            }
                        };
                        c.min(to)..c.max(to)
                    }
                };
                if r.is_empty() {
                    return Ok(Done::Same);
                }
                let d = self.splice(r, &[], Kind::Delete, now);
                self.reveal_soft();
                Ok(d)
            }
        }
    }

    /// Заменяет `range` байтами `bytes` одной группой («Заменить», S7-30): курсор — за
    /// вставкой.
    pub fn replace_range(&mut self, range: Range<u64>, bytes: &[u8], env: &Env) -> Done {
        if env.readonly || !self.editable() {
            return Done::Same;
        }
        self.undo.begin_group(Kind::Command);
        let d = self.splice(range, bytes, Kind::Command, env.now);
        self.reveal_soft();
        d
    }

    /// Байты документа `range` из кэшей (UI-поток, без I/O); страниц нет — `None`.
    pub fn bytes(&self, range: Range<u64>) -> Option<Vec<u8>> {
        let v = self.text.view();
        let mut out = Vec::with_capacity((range.end - range.start) as usize);
        let mut at = range.start;
        while at < range.end {
            let c = crate::cache::ByteView::chunk(&v, at).ok()?;
            if c.is_empty() {
                break;
            }
            let k = c.len().min((range.end - at) as usize);
            out.extend_from_slice(&c[..k]);
            at += k as u64;
        }
        Some(out)
    }

    /// Удаляет выделение одной группой (вырезание после копирования).
    pub fn cut(&mut self, env: &Env) -> Result<Done, Missing> {
        let Some(r) = self.sel.stream() else {
            return Ok(Done::Same);
        };
        if env.readonly || !self.editable() {
            return Ok(Done::Same);
        }
        self.undo.begin_group(Kind::Command);
        let d = self.splice(r, &[], Kind::Command, env.now);
        self.reveal_soft();
        Ok(d)
    }

    /// Отмена (`redo` — повтор): документ и выделение — как были; `Some((enc, eol))` —
    /// массовая правка меняла их.
    pub fn undo(&mut self, redo: bool) -> Option<(Option<Enc>, Option<Eol>)> {
        if self.busy {
            return None;
        }
        let step = if redo {
            self.undo.redo(&mut self.text)
        } else {
            self.undo.undo(&mut self.text)
        }?;
        self.sel = step.sel;
        self.goal = None;
        if let Some(e) = step.enc {
            self.enc = Enc { bom_len: 0, ..e };
        }
        if let Some(l) = step.eol {
            self.eol = l;
        }
        if step.enc.is_some() || step.eol.is_some() {
            self.text
                .set_sep(crate::index::Sep::of(&self.enc, self.eol));
        }
        self.reveal_soft();
        Some((step.enc, step.eol))
    }

    /// Ряд курсора по раскладке экрана (рендер курсора в UI).
    pub fn caret_row(&self) -> Result<VRow, Missing> {
        self.row_of(self.sel.caret)
    }

    /// Логический сегмент курсора (без переноса, не длиннее `max_line`): окно IME;
    /// `cont` — курсор дальше первого сегмента длинной строки («Кол ≈N»).
    pub fn caret_seg(&self) -> Result<VRow, Missing> {
        self.seg_of(self.sel.caret)
    }

    /// Раскладка логических сегментов (без переноса): мини-карта, окно IME.
    pub fn seg_layout(&self) -> LayoutSpec {
        self.seg_spec()
    }

    /// Строка и колонка курсора (с 0): история правок, F6.
    pub fn cursor(&self) -> (u32, u32) {
        let line = u32::try_from(self.text.line_of(self.sel.caret).get()).unwrap_or(u32::MAX);
        let col = self
            .seg_of(self.sel.caret)
            .and_then(|r| col_of(&self.text.view(), &r, self.sel.caret, &self.seg_spec()))
            .unwrap_or(0);
        (line, col)
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
pub(crate) mod tests;
