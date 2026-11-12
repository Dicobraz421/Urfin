//! Блочное выделение (S7-40; ADR-0023 §7, S7 §5.2): прямоугольник — логические строки
//! (сегменты раскладки без переноса) × экранные колонки (табы раскрыты, `unicode-width`,
//! широкие символы — целиком, [`col_span`]). Углы — якорь [`Block::anchor`] с колонкой
//! `anchor_col` и курсор с колонкой `col`; колонки «виртуальные» — за концом короткой
//! строки тоже (как в Far).
//!
//! Операции: копирование — строки блока через `eol_new` + признак `MSDEVColumnSelect`
//! (`sys::clip_write`); вырезание и Del/Backspace — удаление колонок в каждой строке
//! (короткие строки не трогаются); ввод при блоке — блок прочь, текст — у курсора;
//! вставка блочного буфера — колонкой от курсора (недостающие строки дописываются,
//! короткие — добиваются пробелами). Каждая операция — одна группа отмены. Сначала всё
//! читается из кэшей (нет страниц — `Missing`, действие ждёт их целиком), затем правки
//! снизу вверх — смещения выше не сдвигаются.
//!
//! Блок выше [`LINES_MAX`] строк (или шире [`BYTES_MAX`] байт) сразу не правится и не
//! копируется: операция уходит фоновой задачей ([`super::super::bigblock`], S7-42) —
//! [`BlockOut::Job`], документ запускает её с прогрессом и отменой; итог —
//! [`Engine::apply_block`], одна группа отмены. Страницы под такой блок не просятся
//! целиком: только под углы.

use std::ops::Range;
use std::time::Instant;

use super::{Block, Done, Engine, Env, Guard, Move, Sel, Selections, encode, eol_str, norm_eol};
use crate::cache::Missing;
use crate::edit::EdAsk;
use crate::edit::bigblock::{BlockDone, BlockJob, CopyOpts};
use crate::edit::piece::{Piece, Pieces};
use crate::edit::undo::{Kind, Op};
use crate::layout::{ColSpan, VRow, col_of, col_span, off_at_col};

/// Строк блока правится и копируется сразу не больше.
pub const LINES_MAX: usize = 64 << 10;

/// Байт от угла до угла блока сразу не больше (половина бюджета страниц документа):
/// шире — задачей, страницы под такой блок кэш не держит.
pub const BYTES_MAX: u64 = 8 << 20;

/// Прямоугольник блока: начала (сегментов) первой и последней строки, колонки
/// `[c0, c1)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Area {
    pub top: u64,
    pub bottom: u64,
    pub c0: u32,
    pub c1: u32,
}

/// Что блочная операция оставила документу (буфер обмена, тост).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockOut {
    /// Текст блока — в буфер обмена колонкой.
    Copy(String),
    /// Блок больше лимита копирования: байт.
    TooBig(u64),
    /// Выделение больше лимита команды Markdown.
    MdTooBig,
    /// Блок выше [`LINES_MAX`] строк или шире [`BYTES_MAX`]: фоновая задача.
    Job(Box<BlockJob>),
}

/// Правка строки плана: заменить `range` байтами `bytes`.
struct RowEdit {
    range: Range<u64>,
    bytes: Vec<u8>,
}

/// Строки текста вставки колонкой: концы строк любые, один конечный — не строка.
fn clip_lines(text: &str) -> Vec<&str> {
    let t = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text);
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(i) = rest.find(['\r', '\n']) {
        out.push(&rest[..i]);
        let k = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
        rest = &rest[i + k..];
    }
    out.push(rest);
    out
}

#[allow(
    clippy::many_single_char_names,
    reason = "a — область, r — строка, s — колонки"
)]
impl Engine {
    /// Колонка `off` от начала его логической строки (сегмента).
    fn seg_col(&self, off: u64) -> Result<u32, Missing> {
        let r = self.seg_of(off)?;
        col_of(&self.text.view(), &r, off, &self.seg_spec())
    }

    /// Логическая строка выше (`None` — первая).
    fn seg_above(&self, r: &VRow) -> Result<Option<VRow>, Missing> {
        if r.start == 0 {
            return Ok(None);
        }
        let p = self.prev_char(r.start)?;
        let up = self.seg_of(p)?;
        Ok((up.start < r.start).then_some(up))
    }

    /// Логическая строка ниже (`None` — последняя).
    fn seg_below(&self, r: &VRow) -> Result<Option<VRow>, Missing> {
        if r.next <= r.start || r.next > self.len() {
            return Ok(None);
        }
        let down = self.seg_of(r.next)?;
        Ok((down.start > r.start).then_some(down))
    }

    /// Колонки `[c0, c1)` строки `r`.
    fn span(&self, r: &VRow, c0: u32, c1: u32) -> Result<ColSpan, Missing> {
        col_span(&self.text.view(), r, c0, c1, &self.seg_spec())
    }

    /// Прямоугольник блока; пустой (угол в угол) — `None`.
    pub(crate) fn area(&self) -> Result<Option<Area>, Missing> {
        let Sel::Block(b) = self.sel.sel else {
            return Ok(None);
        };
        let ra = self.seg_of(b.anchor)?;
        let rc = self.seg_of(self.sel.caret)?;
        if ra.start == rc.start && b.anchor_col == b.col {
            return Ok(None);
        }
        Ok(Some(Area {
            top: ra.start.min(rc.start),
            bottom: ra.start.max(rc.start),
            c0: b.anchor_col.min(b.col),
            c1: b.anchor_col.max(b.col),
        }))
    }

    /// Прямоугольник блока для кадра и статуса (страниц нет — `None`).
    pub fn block_area(&self) -> Option<Area> {
        self.area().ok().flatten()
    }

    /// Строки блока сверху вниз; выше [`Engine::block_lines`] — `Err(строк)` (сколько
    /// насчитано).
    fn rows_of(&self, a: &Area) -> Result<Result<Vec<VRow>, u64>, Missing> {
        let mut out = Vec::new();
        let mut r = self.seg_of(a.top)?;
        loop {
            if out.len() >= self.block_lines {
                return Ok(Err(self.block_lines as u64));
            }
            out.push(r);
            if r.start >= a.bottom {
                return Ok(Ok(out));
            }
            match self.seg_below(&r)? {
                Some(n) => r = n,
                None => return Ok(Ok(out)),
            }
        }
    }

    /// Блок между смещениями `a` и `b` — для задачи: строк (по счёту переводов строк,
    /// оценка годится) не меньше [`Engine::block_lines`] или байт больше [`BYTES_MAX`].
    fn big_between(&self, a: u64, b: u64) -> bool {
        let (lo, hi) = (a.min(b), a.max(b));
        let lines = self.text.line_of(hi).get() - self.text.line_of(lo).get();
        hi - lo > BYTES_MAX || lines >= self.block_lines as u64
    }

    /// Страницы под блок целиком (действие ждёт их одним запросом, а не по странице);
    /// блок для задачи — только недостающие (углы).
    pub(crate) fn block_need(&self, m: &Missing) -> Option<Range<u64>> {
        let Sel::Block(b) = self.sel.sel else {
            return None;
        };
        if self.big_between(b.anchor, self.sel.caret) {
            return None;
        }
        let ml = u64::from(self.max_line);
        let lo = b.anchor.min(self.sel.caret).saturating_sub(ml);
        let hi = (b.anchor.max(self.sel.caret) + ml).min(self.len());
        let r = super::missing_range(m);
        Some(lo.min(r.start)..hi.max(r.end))
    }

    // ------------------------------------------------------------ движения

    /// Движение с Alt+Shift (`editor.move select=block`): угол-курсор — по логическим
    /// строкам и колонкам; колонка за концом строки сохраняется.
    pub(super) fn block_move(&mut self, m: Move) -> Result<Done, Missing> {
        let before = self.sel;
        let (anchor, anchor_col, col) = if let Sel::Block(b) = before.sel {
            (b.anchor, b.anchor_col, b.col)
        } else {
            let c = self.seg_col(before.caret)?;
            (before.caret, c, c)
        };
        let seg = self.seg_of(before.caret)?;
        let width = self.span(&seg, 0, 0)?.width;
        let caret = before.caret;
        let pages = self.viewport().1.saturating_sub(1).max(1);
        let (line, ncol) = match m {
            Move::Left if col > width => (seg, col - 1),
            Move::Left if caret <= seg.start => (seg, 0),
            Move::Left => (seg, self.seg_col(self.prev_char(caret)?)?),
            Move::Right if col >= width => (seg, col.saturating_add(1).min(self.max_line)),
            Move::Right => (seg, self.seg_col(self.next_char(caret)?)?),
            Move::Home => (seg, 0),
            Move::End => (seg, width),
            Move::WordLeft => {
                let t = self.word_left(caret)?;
                (seg, if t < seg.start { 0 } else { self.seg_col(t)? })
            }
            Move::WordRight => {
                let t = self.word_right(caret)?;
                (seg, if t > seg.end { width } else { self.seg_col(t)? })
            }
            Move::Up | Move::Down | Move::PageUp | Move::PageDown => {
                let n = if matches!(m, Move::Up | Move::Down) {
                    1
                } else {
                    pages
                };
                let down = matches!(m, Move::Down | Move::PageDown);
                let mut r = seg;
                for _ in 0..n {
                    let next = if down {
                        self.seg_below(&r)?
                    } else {
                        self.seg_above(&r)?
                    };
                    match next {
                        Some(x) => r = x,
                        None => break,
                    }
                }
                (r, col)
            }
            Move::DocStart => (self.seg_of(0)?, 0),
            Move::DocEnd => {
                let r = self.seg_of(self.len())?;
                let w = self.span(&r, 0, 0)?.width;
                (r, w)
            }
        };
        let to = off_at_col(&self.text.view(), &line, ncol, &self.seg_spec())?;
        self.goal = None;
        self.sel = Selections {
            caret: to,
            sel: Sel::Block(Block {
                anchor,
                anchor_col,
                col: ncol,
            }),
        };
        if matches!(m, Move::PageUp | Move::PageDown) {
            let _ = self.scroll_rows(m == Move::PageDown, pages);
        }
        Ok(self.after_move(before))
    }

    /// Alt+мышь (`InboxMsg::Caret` с колонкой): `col` — экранная колонка в визуальном
    /// ряду `off` (с `hscroll`); `extend` — тянут от прежнего угла.
    pub(super) fn block_caret(
        &mut self,
        off: u64,
        col: u32,
        extend: bool,
    ) -> Result<Done, Missing> {
        let before = self.sel;
        let off = off.min(self.len());
        let seg = self.seg_of(off)?;
        // Перенос: колонка ряда — от начала его логической строки.
        let base = if self.wrap {
            let r = self.row_of(off)?;
            col_of(&self.text.view(), &seg, r.start, &self.seg_spec())?
        } else {
            0
        };
        let col = base.saturating_add(col).min(self.max_line);
        let caret = off_at_col(&self.text.view(), &seg, col, &self.seg_spec())?;
        let (anchor, anchor_col) = match (extend, before.sel) {
            (true, Sel::Block(b)) => (b.anchor, b.anchor_col),
            (true, _) => (before.caret, self.seg_col(before.caret)?),
            (false, _) => (caret, col),
        };
        self.goal = None;
        self.sel = Selections {
            caret,
            sel: Sel::Block(Block {
                anchor,
                anchor_col,
                col,
            }),
        };
        Ok(self.after_move(before))
    }

    // ------------------------------------------------------------ правка

    /// Правки плана снизу вверх одной группой: смещения выше правки не сдвигаются;
    /// `tail` — дописать в конец документа.
    fn apply_plan(&mut self, mut plan: Vec<RowEdit>, after: Selections, now: Instant) -> Done {
        plan.retain(|e| !(e.range.is_empty() && e.bytes.is_empty()));
        if plan.is_empty() {
            let before = self.sel;
            self.sel = after;
            return if before == after {
                Done::Same
            } else {
                Done::Moved
            };
        }
        plan.sort_by_key(|e| e.range.start);
        let before = self.sel;
        self.undo.begin_group(Kind::Command);
        let mut changed = false;
        for e in plan.into_iter().rev() {
            let res = if e.bytes.is_empty() {
                Ok(self.text.remove(e.range))
            } else if e.range.is_empty() {
                self.text.insert(e.range.start, &e.bytes)
            } else {
                self.text.replace(e.range, &e.bytes)
            };
            match res {
                Ok(sp) => {
                    changed = true;
                    self.undo
                        .record(sp.into(), Kind::Command, &before, &after, now);
                }
                Err(err) => tracing::error!(error = %err, "editor: block edit failed"),
            }
        }
        self.sel = after;
        self.goal = None;
        if changed { Done::Edited } else { Done::Same }
    }

    /// Строки блока с колонками; блок для задачи — `None`.
    fn block_spans(&mut self, a: &Area) -> Result<Option<Vec<(VRow, ColSpan)>>, Missing> {
        if self.big_between(a.top, a.bottom) {
            return Ok(None);
        }
        let Ok(rows) = self.rows_of(a)? else {
            return Ok(None);
        };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let s = self.span(&r, a.c0, a.c1)?;
            out.push((r, s));
        }
        Ok(Some(out))
    }

    /// Блок прочь, на строке курсора у левого края блока — `bytes` (ввод, вставка,
    /// Enter); пусто — только удаление (Del, Backspace, вырезание).
    pub(super) fn block_replace(
        &mut self,
        a: &Area,
        bytes: &[u8],
        now: Instant,
    ) -> Result<Done, Missing> {
        let caret_line = self.seg_of(self.sel.caret)?.start;
        let Some(spans) = self.block_spans(a)? else {
            return Ok(self.block_job(BlockJob::Replace {
                area: *a,
                caret_line,
                bytes: bytes.to_vec(),
                copy: None,
            }));
        };
        let mut at = spans.first().map_or(self.sel.caret, |(_, s)| s.start);
        let mut plan = Vec::with_capacity(spans.len());
        for (r, s) in &spans {
            let here = r.start == caret_line;
            if here {
                at = s.start;
            }
            plan.push(RowEdit {
                range: s.start..s.end,
                bytes: if here { bytes.to_vec() } else { Vec::new() },
            });
        }
        // Сдвиг места курсора удалениями выше него.
        let above: u64 = spans
            .iter()
            .filter(|(r, _)| r.start < caret_line)
            .map(|(_, s)| s.end - s.start)
            .sum();
        let after = Selections::at(at - above + bytes.len() as u64);
        Ok(self.apply_plan(plan, after, now))
    }

    /// Текст блока: строки через `eol_new` (недекодируемые байты — U+FFFD).
    fn block_text(&self, spans: &[(VRow, ColSpan)], eol: &str) -> Option<String> {
        let mut out = String::new();
        for (i, (_, s)) in spans.iter().enumerate() {
            if i > 0 {
                out.push_str(eol);
            }
            let b = self.bytes(s.start..s.end)?;
            out.push_str(&self.enc.enc.decode_without_bom_handling(&b).0);
        }
        Some(out)
    }

    /// `editor.copy` / `editor.cut` при блоке: текст — в [`Engine::block_out`] (буфер
    /// обмена пишет документ), вырезание — удаление колонок той же группой.
    pub(super) fn block_copy(&mut self, cut: bool, limit: u64, env: &Env) -> Result<Done, Missing> {
        let Some(a) = self.area()? else {
            return Ok(Done::Same);
        };
        let cuts = cut && !env.readonly && self.editable() && self.guard != Guard::Unlocked;
        let Some(spans) = self.block_spans(&a)? else {
            let copy = CopyOpts {
                limit,
                eol: eol_str(env.eol_new),
            };
            let job = if cuts {
                BlockJob::Replace {
                    area: a,
                    caret_line: self.seg_of(self.sel.caret)?.start,
                    bytes: Vec::new(),
                    copy: Some(copy),
                }
            } else {
                BlockJob::Copy { area: a, copy }
            };
            return Ok(self.block_job(job));
        };
        let size: u64 = spans.iter().map(|(_, s)| s.end - s.start).sum();
        if size > limit {
            self.block_out = Some(BlockOut::TooBig(size));
            return Ok(Done::Same);
        }
        // Байты только что прочитаны раскладкой — в кэше.
        let Some(text) = self.block_text(&spans, eol_str(env.eol_new)) else {
            return Ok(Done::Same);
        };
        self.block_out = Some(BlockOut::Copy(text));
        if !cuts {
            return Ok(Done::Same);
        }
        let caret_line = self.seg_of(self.sel.caret)?.start;
        let at = spans
            .iter()
            .find(|(r, _)| r.start == caret_line)
            .or(spans.first())
            .map_or(self.sel.caret, |(_, s)| s.start);
        let above: u64 = spans
            .iter()
            .filter(|(r, _)| r.start < caret_line)
            .map(|(_, s)| s.end - s.start)
            .sum();
        let plan = spans
            .iter()
            .map(|(_, s)| RowEdit {
                range: s.start..s.end,
                bytes: Vec::new(),
            })
            .collect();
        Ok(self.apply_plan(plan, Selections::at(at - above), env.now))
    }

    /// Вставка блочного буфера колонкой: от курсора (при блоке — блок прочь, от его
    /// левого верхнего угла) строка за строкой; короткие строки добиваются пробелами,
    /// недостающие — дописываются в конец документа.
    pub(super) fn paste_column(
        &mut self,
        text: &str,
        replace: bool,
        env: &Env,
    ) -> Result<Done, Missing> {
        let lines = clip_lines(text);
        let mut enc_lines = Vec::with_capacity(lines.len());
        for l in &lines {
            match encode(&self.enc, l, replace) {
                Ok(b) => enc_lines.push(b),
                Err(ch) => return Ok(Done::Ask(EdAsk::NoChar { ch })),
            }
        }
        let space = encode(&self.enc, " ", true).unwrap_or_default();
        let eol = encode(&self.enc, &norm_eol("\n", env.eol_new), true).unwrap_or_default();
        let area = self.area()?;
        // Строки блока (удаляемые колонки) и строка начала; много строк — задачей.
        let (mut spans, col, first) = if let Some(a) = &area {
            let Some(s) = self.block_spans(a)? else {
                return Ok(self.block_job(BlockJob::Paste {
                    first: a.top,
                    col: a.c0,
                    block: area,
                    lines: enc_lines,
                    space,
                    eol,
                }));
            };
            let first = s[0].0;
            (s, a.c0, first)
        } else {
            let c = self.seg_col(self.sel.caret)?;
            (Vec::new(), c, self.seg_of(self.sel.caret)?)
        };
        if lines.len() > self.block_lines {
            return Ok(self.block_job(BlockJob::Paste {
                first: first.start,
                col,
                block: area,
                lines: enc_lines,
                space,
                eol,
            }));
        }
        // Строки под вставку ниже блока.
        let mut last = spans.last().map_or(first, |x| x.0);
        if spans.is_empty() {
            spans.push((first, self.span(&first, col, col)?));
        }
        while spans.len() < lines.len() {
            match self.seg_below(&last)? {
                Some(r) => {
                    let s = self.span(&r, col, col)?;
                    spans.push((r, s));
                    last = r;
                }
                None => break,
            }
        }
        let pad = |n: u32| space.repeat(n as usize);
        let mut plan = Vec::with_capacity(lines.len() + 1);
        let mut caret = None;
        for (i, (_, s)) in spans.iter().enumerate() {
            let mut bytes = Vec::new();
            if let Some(l) = enc_lines.get(i) {
                if s.width < col {
                    bytes.extend(pad(col - s.width));
                }
                bytes.extend_from_slice(l);
            }
            if i == 0 {
                caret = Some(s.start + bytes.len() as u64);
            }
            plan.push(RowEdit {
                range: s.start..s.end,
                bytes,
            });
        }
        // Строк документа не хватило — дописать.
        if enc_lines.len() > spans.len() {
            let mut tail = Vec::new();
            for l in &enc_lines[spans.len()..] {
                tail.extend_from_slice(&eol);
                tail.extend(pad(col));
                tail.extend_from_slice(l);
            }
            let len = self.len();
            plan.push(RowEdit {
                range: len..len,
                bytes: tail,
            });
        }
        let after = Selections::at(caret.unwrap_or(self.sel.caret));
        let d = self.apply_plan(plan, after, env.now);
        self.reveal_soft();
        Ok(d)
    }

    // ------------------------------------------------------------ задача

    /// Операция — задачей документа (S7-42): документ её запускает (`settle`).
    fn block_job(&mut self, job: BlockJob) -> Done {
        self.block_out = Some(BlockOut::Job(Box::new(job)));
        Done::Same
    }

    /// Итог задачи (UI-поток, без I/O: байты — готовый источник `Rewrite`): текст — в
    /// [`Engine::block_out`], правка — новыми кусками диапазона одной группой отмены.
    pub fn apply_block(&mut self, done: BlockDone, now: Instant) -> Done {
        if let Some(t) = done.text {
            self.block_out = Some(BlockOut::Copy(t));
        }
        let before = self.sel;
        let Some((range, rw)) = done.edit else {
            let after = done.caret.map_or(before, Selections::at);
            self.sel = after;
            self.goal = None;
            return if before == after {
                Done::Same
            } else {
                Done::Moved
            };
        };
        let slot = (!rw.pieces.is_empty()).then(|| self.text.push_rewrite(rw.src));
        let inserted: Pieces = rw
            .pieces
            .into_iter()
            .map(|p| Piece {
                src: slot.unwrap_or(p.src),
                ..p
            })
            .collect();
        let at = range.start;
        let removed = self.text.splice(at, range.end - range.start, &inserted);
        let after = done
            .caret
            .map_or(before, |c| Selections::at(c.min(self.len())));
        self.sel = after;
        self.goal = None;
        self.undo.begin_group(Kind::Command);
        self.undo.record(
            Op::Splice {
                at,
                removed,
                inserted,
            },
            Kind::Command,
            &before,
            &after,
            now,
        );
        self.reveal_soft();
        Done::Edited
    }
}

#[cfg(test)]
#[path = "block_tests.rs"]
mod tests;
