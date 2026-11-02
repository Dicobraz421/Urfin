//! Полоса изменений по кускам, без diff текста (ADR-0023 §7, S7 §5.3). Карточка S7-27.
//!
//! Строка, все байты которой (с её переводом строки) из правок (`Add`, `Rewrite`), —
//! добавлена; правки вместе с исходником — изменена; шов удаления (кусок исходника не
//! продолжает предыдущий кусок исходника) в начале строки — метка удаления перед ней,
//! внутри строки — строка изменена. Строки внутри куска исходника не меняются — их не
//! читают: на кусок нужны только число переводов строк и стоит ли перевод строки в его
//! конце. Счётчик шапки — фоном по снимку ([`count`], O(кусков)); метки — для видимого
//! диапазона по запросу ([`visible`], по байтам из кэшей).

use std::io;
use std::ops::Range;
use std::sync::Arc;

use urfin_core::CancelToken;

use super::piece::{Lf, Piece, Src, TextDoc, TextSnap};
use crate::cache::{ByteView, Missing};
use crate::index::{Sep, count_seps};

/// Изменение строки текущего текста.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Новая строка (`type.exec` на полосе).
    Added,
    /// Строка изменена (`accent`).
    Modified,
    /// Перед этой строкой удалены строки сохранённого текста (номер может быть равен
    /// числу строк — удалено в конце).
    Deleted,
}

/// Счётчик в шапке («+2 добавлено · 1 изменена») и метки полосы по строкам текущего
/// текста (с 0, по возрастанию).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineChanges {
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
    pub lines: Arc<[(u32, Change)]>,
}

impl LineChanges {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.modified == 0 && self.deleted == 0
    }
}

/// Что попало в текущую строку.
#[allow(clippy::struct_excessive_bools, reason = "независимые признаки строки")]
#[derive(Clone, Copy, Debug, Default)]
struct Line {
    any: bool,
    orig: bool,
    new: bool,
    seam: bool,
}

/// Проход по кускам: строки и их метки.
#[derive(Debug)]
struct Walk {
    line: u64,
    cur: Line,
    /// Конец предыдущего куска исходника (в координатах исходника).
    orig_end: u64,
    out: Option<Vec<(u32, Change)>>,
    res: LineChanges,
}

impl Walk {
    fn new(orig_end: u64, collect: bool) -> Walk {
        Walk {
            line: 0,
            cur: Line::default(),
            orig_end,
            out: collect.then(Vec::new),
            res: LineChanges::default(),
        }
    }

    fn mark(&mut self, line: u64, c: Change) {
        match c {
            Change::Added => self.res.added += 1,
            Change::Modified => self.res.modified += 1,
            Change::Deleted => self.res.deleted += 1,
        }
        if let Some(out) = &mut self.out {
            out.push((u32::try_from(line).unwrap_or(u32::MAX), c));
        }
    }

    /// Строка кончилась: метка по тому, что в неё попало.
    fn close(&mut self) {
        let l = self.cur;
        if l.new || l.seam {
            let c = if l.orig {
                Change::Modified
            } else {
                Change::Added
            };
            self.mark(self.line, c);
        }
        self.line += 1;
        self.cur = Line::default();
    }

    /// Шов удаления на текущем месте.
    fn seam(&mut self) {
        if self.cur.any {
            self.cur.seam = true;
        } else {
            self.mark(self.line, Change::Deleted);
        }
    }

    /// Кусок (или его часть): источник, диапазон в источнике, переводов строк и стоит ли
    /// перевод строки в конце.
    fn piece(&mut self, src: Src, r: Range<u64>, lf: u64, ends_sep: bool) {
        let orig = src == Src::Original;
        if orig {
            if r.start != self.orig_end {
                self.seam();
            }
            self.orig_end = r.end;
        }
        self.cur.any = true;
        if orig {
            self.cur.orig = true;
        } else {
            self.cur.new = true;
        }
        if lf == 0 {
            return;
        }
        self.close();
        let inner = lf - 1;
        if orig {
            self.line += inner;
        } else if self.out.is_some() {
            for _ in 0..inner {
                self.mark(self.line, Change::Added);
                self.line += 1;
            }
        } else {
            self.res.added += u32::try_from(inner).unwrap_or(u32::MAX);
            self.line += inner;
        }
        if !ends_sep {
            self.cur = Line {
                any: true,
                orig,
                new: !orig,
                seam: false,
            };
        }
    }

    /// Конец документа: исходник не дочитан до конца — удалено в конце.
    fn finish(mut self, orig_end: u64) -> (LineChanges, Option<Vec<(u32, Change)>>) {
        if self.orig_end != orig_end {
            self.seam();
        }
        if self.cur.any {
            self.close();
        }
        (self.res, self.out)
    }
}

/// Последняя единица `tail` — разделитель.
fn ends_with_sep(tail: &[u8], sep: Sep) -> bool {
    let b = sep.byte();
    match sep {
        Sep::Byte(_) => tail.last() == Some(&b),
        Sep::Le(_) => tail.len() >= 2 && tail[tail.len() - 2..] == [b, 0],
        Sep::Be(_) => tail.len() >= 2 && tail[tail.len() - 2..] == [0, b],
    }
}

/// Счётчик шапки по снимку (вне UI): добавлено, изменено, швов удаления; `lines` пуст.
/// `orig` — байты исходника в документе (после BOM). Читает по единице на кусок с
/// переводами строк; у куска исходника с неизвестными — до первого разделителя.
pub fn count(snap: &TextSnap, orig: Range<u64>, cancel: &CancelToken) -> io::Result<LineChanges> {
    let sep = snap.sep();
    let u = sep.unit() as usize;
    let mut w = Walk::new(orig.start, false);
    let mut tail = [0u8; 2];
    let mut buf = Vec::new();
    for (i, (_, p)) in snap.tree().iter().enumerate() {
        if i % 4096 == 0 && cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let lf = match p.lf {
            Lf::Known(n) => u64::from(n),
            Lf::Unknown => unknown_lf(snap, p, &mut buf)?,
        };
        let ends = lf > 0 && {
            let t = &mut tail[..u.min(p.len as usize)];
            snap.read_src_full(p.src, p.end() - t.len() as u64, t)?;
            ends_with_sep(t, sep)
        };
        w.piece(p.src, p.start..p.end(), lf, ends);
    }
    Ok(w.finish(orig.end).0)
}

/// Переводов строк у куска с неизвестными: у исходника хватает «есть ли хоть один»
/// (строки внутри не меняются), у правок — точно.
fn unknown_lf(snap: &TextSnap, p: &Piece, buf: &mut Vec<u8>) -> io::Result<u64> {
    const STEP: usize = 64 << 10;
    let sep = snap.sep();
    let mut n = 0;
    let mut pos = p.start;
    while pos < p.end() {
        let k = STEP.min((p.end() - pos) as usize);
        buf.resize(k, 0);
        let got = snap.read_src_full(p.src, pos, buf)?;
        if got == 0 {
            break;
        }
        crate::index::scan(&buf[..got], pos, sep, |_| n += 1);
        if n > 0 && p.src == Src::Original {
            return Ok(n);
        }
        pos += got as u64;
    }
    Ok(n)
}

/// Конец предыдущего куска исходника перед `off` документа (шов у первого видимого).
fn orig_before(doc: &TextDoc, mut off: u64, orig_start: u64) -> u64 {
    while off > 0 {
        let Some((ps, p)) = doc.tree().find(off - 1) else {
            break;
        };
        if p.src == Src::Original {
            return p.start + (off - ps);
        }
        off = ps;
    }
    orig_start
}

/// Метки видимых строк: `range.start` — начало строки, номера — от неё (с 0). Байты —
/// из кэшей (`Missing` — страниц нет). `orig` — байты исходника в документе.
pub fn visible(
    doc: &TextDoc,
    range: Range<u64>,
    orig: Range<u64>,
) -> Result<Vec<(u32, Change)>, Missing> {
    let view = doc.view();
    let sep = doc.sep();
    let end = range.end.min(doc.len());
    let mut w = Walk::new(orig_before(doc, range.start, orig.start), true);
    for (ps, p) in doc.tree().iter_from(range.start) {
        if ps >= end {
            break;
        }
        let a = ps.max(range.start);
        let b = (ps + p.len).min(end);
        if a >= b {
            break;
        }
        let lf = count_seps(&view, a..b, sep)?;
        let ends = lf > 0 && {
            let c = view.chunk_before(b)?;
            ends_with_sep(&c[c.len().saturating_sub(sep.unit() as usize)..], sep)
        };
        let s = p.start + (a - ps);
        w.piece(p.src, s..s + (b - a), lf, ends);
    }
    let out = if end == doc.len() {
        w.finish(orig.end).1
    } else {
        if w.cur.any {
            w.close();
        }
        w.out
    };
    Ok(out.unwrap_or_default())
}

#[cfg(test)]
#[path = "changes_tests.rs"]
mod tests;
