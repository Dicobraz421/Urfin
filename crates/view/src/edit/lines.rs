//! Строки документа поверх правок (ADR-0023 §1, §7; S7 §3.3). Карточка S7-27.
//!
//! Номер строки — суммы известных переводов строк в дереве до куска и досчёт внутри
//! него: в `Original` — метка `LineIndex` файла и не больше 1024 строк по байтам, в `Add`
//! — `memchr` по блоку. Пока перед смещением есть куски с `Lf::Unknown` (индекс файла не
//! дошёл), номер — оценка `Approx` по средней длине строки, как у вьювера. Индексатор
//! вьювера идёт по файлу; по его продвижению фоновый [`lf_patch`] по снимку считает
//! переводы строк неизвестных кусков (края разрезанных — досчётом от метки), а
//! [`TextDoc::apply_lf`] ставит их в дерево (UI-поток, `Msg` — S7-29).

use std::io;

use rustc_hash::FxHashMap;
use urfin_core::CancelToken;

use super::piece::{Lf, Piece, Src, TextDoc, TextSnap};
use crate::index::{IndexDelta, IndexError, IndexMode, Indexer, LineGuess, LineIndex, Sep, scan};
use crate::source::ByteSource;

/// Индекс строк источника целиком здесь же (файл до 16 МБ в памяти — на открытии,
/// Interactive lane): строки документа сразу точные.
pub fn index_now(src: &dyn ByteSource, sep: Sep) -> io::Result<LineIndex> {
    let mut ix = LineIndex::default();
    Indexer::new(sep)
        .run(src, IndexMode::Parallel, &CancelToken::new(), &mut |d| {
            ix.apply(d);
        })
        .map_err(|e| match e {
            IndexError::Io(e) => e,
            IndexError::Cancelled => io::ErrorKind::Interrupted.into(),
        })?;
    Ok(ix)
}

impl TextDoc {
    /// Индекс строк исходника.
    pub fn index(&self) -> &LineIndex {
        &self.store.index
    }

    /// Дельта фонового индексатора исходника (`IndexMsg`); неизвестные куски — потом
    /// [`lf_patch`] по снимку.
    pub fn on_index(&mut self, delta: IndexDelta) {
        self.store.index.apply(delta);
    }

    /// Средняя длина строки для оценок: по индексу файла, иначе по известным кускам;
    /// `u64::MAX` — ничего не известно (оценка не добавляет строк).
    fn avg_line(&self) -> u64 {
        let ix = &self.store.index;
        let s = self.tree.sums();
        ix.upto
            .checked_div(ix.lines)
            .or_else(|| (s.bytes - s.unknown).checked_div(s.lf))
            .map_or(u64::MAX, |a| a.max(1))
    }

    /// Номер строки (с 0), содержащей `off`: точный, если до `off` все переводы строк
    /// известны и досчёт внутри куска сделан по байтам из кэшей; иначе оценка.
    pub fn line_of(&self, off: u64) -> LineGuess {
        let off = off.min(self.len());
        let avg = self.avg_line();
        let (lf, unknown) = match self.tree.prefix(off) {
            None => {
                let s = self.tree.sums();
                (s.lf, s.unknown)
            }
            Some((before, ps, p)) => {
                let rel = off - ps;
                let inner = if rel == 0 || p.lf == Lf::Known(0) {
                    Some(0)
                } else {
                    self.store.count_lf(p.src, p.start..p.start + rel)
                };
                match inner {
                    Some(k) => (before.lf + u64::from(k), before.unknown),
                    None => (before.lf, before.unknown + rel),
                }
            }
        };
        if unknown == 0 {
            LineGuess::Exact(lf)
        } else {
            LineGuess::Approx(lf + unknown / avg)
        }
    }

    /// Начало строки `n` (с 0): точно или оценкой (как [`TextDoc::line_of`]). `None` —
    /// строки нет (известно точно).
    pub fn offset_of_line(&self, n: u64) -> Option<LineGuess> {
        if n == 0 {
            return Some(LineGuess::Exact(0));
        }
        let avg = self.avg_line();
        let Some((before, ps, p, need)) = self.tree.find_line(n, avg) else {
            return (self.tree.sums().unknown > 0).then(|| LineGuess::Approx(self.len()));
        };
        let exact = match p.lf {
            Lf::Known(_) => self.store.nth_sep(p.src, p.start..p.end(), need),
            Lf::Unknown => None,
        };
        Some(match exact {
            Some(at) if before.unknown == 0 => LineGuess::Exact(ps + (at - p.start)),
            Some(at) => LineGuess::Approx(ps + (at - p.start)),
            None => {
                let per = match p.lf {
                    Lf::Known(k) if k > 0 => p.len / u64::from(k),
                    _ => avg,
                };
                LineGuess::Approx(ps + need.saturating_mul(per).min(p.len))
            }
        })
    }

    /// Переводы строк, посчитанные [`lf_patch`]: куски, которые с тех пор не менялись,
    /// становятся `Known`. Возвращает их число.
    pub fn apply_lf(&mut self, patch: &LfPatch) -> usize {
        for &(off, line) in &patch.lines {
            self.store.remember(off, line);
        }
        if patch.items.is_empty() {
            return 0;
        }
        let map: FxHashMap<(Src, u64, u64), u32> = patch
            .items
            .iter()
            .map(|(p, n)| ((p.src, p.start, p.len), *n))
            .collect();
        self.tree
            .patch_lf(&mut |p| map.get(&(p.src, p.start, p.len)).copied())
    }
}

/// Переводы строк кусков с `Lf::Unknown`, посчитанные по снимку, и номера строк
/// исходника на краях его кусков.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LfPatch {
    pub items: Vec<(Piece, u32)>,
    pub lines: Vec<(u64, u64)>,
}

/// Блок чтения при счёте.
const READ: usize = 1 << 20;

/// Разделителей в `[r)` источника (чтение блоками, отмена между ними).
fn count_read(
    snap: &TextSnap,
    src: Src,
    r: std::ops::Range<u64>,
    buf: &mut Vec<u8>,
    cancel: &CancelToken,
) -> io::Result<u64> {
    let mut n = 0;
    let mut pos = r.start;
    while pos < r.end {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let k = READ.min((r.end - pos) as usize);
        buf.resize(k, 0);
        let got = snap.read_src_full(src, pos, buf)?;
        if got == 0 {
            break;
        }
        scan(&buf[..got], pos, snap.sep(), |_| n += 1);
        pos += got as u64;
    }
    Ok(n)
}

/// Номер строки исходника в `off` по индексу и чтению от метки (вне UI).
fn line_read(
    snap: &TextSnap,
    ix: &LineIndex,
    off: u64,
    buf: &mut Vec<u8>,
    cancel: &CancelToken,
) -> io::Result<u64> {
    let (mo, ml) = ix.mark_before(off);
    Ok(ml + count_read(snap, Src::Original, mo..off, buf, cancel)?)
}

/// Считает переводы строк неизвестных кусков снимка (вне UI-потока): куски исходника —
/// там, куда дошёл индекс `ix` (метки и досчёт краёв), прочие — по байтам.
pub fn lf_patch(snap: &TextSnap, ix: &LineIndex, cancel: &CancelToken) -> io::Result<LfPatch> {
    let mut out = LfPatch::default();
    let mut buf = Vec::new();
    for (_, p) in snap.tree().unknown() {
        let n = match p.src {
            Src::Original => {
                if p.end() > ix.upto {
                    continue;
                }
                let a = line_read(snap, ix, p.start, &mut buf, cancel)?;
                let b = line_read(snap, ix, p.end(), &mut buf, cancel)?;
                out.lines.extend([(p.start, a), (p.end(), b)]);
                b - a
            }
            Src::Add | Src::Rewrite(_) => {
                count_read(snap, p.src, p.start..p.end(), &mut buf, cancel)?
            }
        };
        if let Ok(n) = u32::try_from(n) {
            out.items.push((p, n));
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "lines_tests.rs"]
mod tests;
