//! Чанковый проход по файлу (S7 §3.6): окна по 4 МБ с перекрытием `min(max_len, 64 КБ)`
//! и контекстом по краям (границы слов, `^`/`$`), вперёд и назад, перенос через конец,
//! прогресс в атомике, отмена между окнами. Результат совпадает с поиском по целому буферу
//! (proptest), если длина совпадения не больше перекрытия.

#![allow(
    clippy::many_single_char_names,
    reason = "m — образец, o — параметры, w — окно, p — подготовленное окно"
)]

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use urfin_core::{CancelToken, Cancelled};

use super::Matcher;
use crate::source::{ByteSource, read_full};

/// Окно поиска.
pub const CHUNK: u64 = 4 << 20;
/// Контекст по краям окна: символ до и после совпадения (UTF-8 — до 4 байт).
const CTX: u64 = 8;

/// Байты, по которым идёт поиск.
#[allow(clippy::len_without_is_empty, reason = "длина файла, пустота не нужна")]
pub trait Hay {
    fn len(&self) -> u64;
    /// Байты `r` (обрезано по длине).
    fn window(&mut self, r: Range<u64>) -> io::Result<&[u8]>;
}

/// Срез в памяти (тесты, mmap).
#[derive(Debug)]
pub struct SliceHay<'a>(pub &'a [u8]);

impl Hay for SliceHay<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn window(&mut self, r: Range<u64>) -> io::Result<&[u8]> {
        let end = (r.end as usize).min(self.0.len());
        Ok(&self.0[(r.start as usize).min(end)..end])
    }
}

/// Источник документа: `Mapped`/`Owned` — прямо по срезу, иначе `read_at` в буфер.
#[derive(Debug)]
pub struct SourceHay<'a> {
    src: &'a dyn ByteSource,
    len: u64,
    buf: Vec<u8>,
}

impl<'a> SourceHay<'a> {
    /// Длина фиксируется на старте поиска.
    pub fn new(src: &'a dyn ByteSource) -> SourceHay<'a> {
        SourceHay {
            src,
            len: src.len(),
            buf: Vec::new(),
        }
    }
}

impl Hay for SourceHay<'_> {
    fn len(&self) -> u64 {
        self.len
    }

    fn window(&mut self, r: Range<u64>) -> io::Result<&[u8]> {
        let end = r.end.min(self.len);
        let start = r.start.min(end);
        if let Some(m) = self.src.mapped() {
            let end = (end as usize).min(m.len());
            return Ok(&m[(start as usize).min(end)..end]);
        }
        let n = (end - start) as usize;
        self.buf.resize(n, 0);
        let got = read_full(self.src, start, &mut self.buf)?;
        Ok(&self.buf[..got])
    }
}

/// Ошибка прохода.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("cancelled")]
    Cancelled,
    #[error("search read: {0}")]
    Io(#[from] io::Error),
}

impl From<Cancelled> for ScanError {
    fn from(_: Cancelled) -> Self {
        ScanError::Cancelled
    }
}

/// Параметры прохода.
#[derive(Debug)]
pub struct Scan<'a> {
    pub chunk: u64,
    pub cancel: &'a CancelToken,
    /// Просмотрено байт (прогресс; растёт и при переносе).
    pub scanned: &'a AtomicU64,
}

/// Найденное: диапазон и был ли перенос через конец (начало).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub range: Range<u64>,
    pub wrapped: bool,
}

fn down(x: u64, unit: u8) -> u64 {
    x / u64::from(unit) * u64::from(unit)
}

/// Окно для совпадений, начинающихся в `[cs, ce)`.
fn window(m: &Matcher, len: u64, cs: u64, ce: u64) -> Range<u64> {
    let lo = down(cs.saturating_sub(CTX), m.unit());
    let hi = ce
        .saturating_add(m.max_len() as u64)
        .saturating_add(CTX)
        .min(len);
    lo..hi
}

/// Первое совпадение, начинающееся в `[from, to)`.
pub fn forward(
    hay: &mut dyn Hay,
    m: &Matcher,
    from: u64,
    to: u64,
    o: &Scan<'_>,
) -> Result<Option<Range<u64>>, ScanError> {
    let len = hay.len();
    let to = to.min(len);
    let mut cs = from;
    let chunk = o.chunk.max(1);
    while cs < to {
        o.cancel.check()?;
        let ce = cs.saturating_add(chunk).min(to);
        let w = window(m, len, cs, ce);
        let bytes = hay.window(w.clone())?;
        let p = m.prepare(bytes);
        if let Some(r) = p.find_at((cs - w.start) as usize) {
            let s = w.start + r.start as u64;
            if s < ce {
                o.scanned.fetch_add(s - cs, Ordering::Relaxed);
                return Ok(Some(s..w.start + r.end as u64));
            }
        }
        o.scanned.fetch_add(ce - cs, Ordering::Relaxed);
        cs = ce;
    }
    Ok(None)
}

/// Последнее совпадение, начинающееся в `[to, from)`.
pub fn backward(
    hay: &mut dyn Hay,
    m: &Matcher,
    from: u64,
    to: u64,
    o: &Scan<'_>,
) -> Result<Option<Range<u64>>, ScanError> {
    let len = hay.len();
    let mut ce = from.min(len);
    let chunk = o.chunk.max(1);
    while ce > to {
        o.cancel.check()?;
        let cs = down(ce.saturating_sub(chunk).max(to), m.unit());
        let w = window(m, len, cs, ce);
        let bytes = hay.window(w.clone())?;
        let p = m.prepare(bytes);
        let lo = (cs.max(to) - w.start) as usize;
        if let Some(r) = p.rfind(lo, (ce - w.start) as usize) {
            let s = w.start + r.start as u64;
            o.scanned.fetch_add(ce - s, Ordering::Relaxed);
            return Ok(Some(s..w.start + r.end as u64));
        }
        o.scanned.fetch_add(ce - cs, Ordering::Relaxed);
        ce = cs;
    }
    Ok(None)
}

/// Следующее совпадение от `from`: вперёд — начинающееся не раньше `from`, назад — раньше
/// `from`; `wrap` — продолжить с другого конца файла до `from`.
pub fn find_next(
    hay: &mut dyn Hay,
    m: &Matcher,
    from: u64,
    back: bool,
    wrap: bool,
    o: &Scan<'_>,
) -> Result<Option<Hit>, ScanError> {
    let len = hay.len();
    let from = from.min(len);
    let hit = |r: Option<Range<u64>>, wrapped| r.map(|range| Hit { range, wrapped });
    if back {
        if let Some(h) = hit(backward(hay, m, from, 0, o)?, false) {
            return Ok(Some(h));
        }
        if wrap {
            return Ok(hit(backward(hay, m, len, from, o)?, true));
        }
    } else {
        if let Some(h) = hit(forward(hay, m, from, len, o)?, false) {
            return Ok(Some(h));
        }
        if wrap {
            return Ok(hit(forward(hay, m, 0, from, o)?, true));
        }
    }
    Ok(None)
}

/// Итог счёта: начала совпадений (до `limit`) и есть ли ещё.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counted {
    pub starts: Vec<u64>,
    pub more: bool,
}

/// Считает совпадения без перекрытий от начала файла до `limit` (и одно сверх — для
/// «1 000+»).
pub fn count(
    hay: &mut dyn Hay,
    m: &Matcher,
    limit: usize,
    o: &Scan<'_>,
) -> Result<Counted, ScanError> {
    let len = hay.len();
    let mut out = Counted::default();
    let chunk = o.chunk.max(1);
    let (mut cs, mut pos) = (0u64, 0u64);
    while cs < len {
        o.cancel.check()?;
        let ce = cs.saturating_add(chunk).min(len);
        let w = window(m, len, cs, ce);
        let bytes = hay.window(w.clone())?;
        let p = m.prepare(bytes);
        while pos < ce {
            let Some(r) = p.find_at((pos - w.start) as usize) else {
                break;
            };
            let s = w.start + r.start as u64;
            if s >= ce {
                break;
            }
            if out.starts.len() == limit {
                out.more = true;
                return Ok(out);
            }
            out.starts.push(s);
            pos = (w.start + r.end as u64).max(s + 1);
        }
        o.scanned.fetch_add(ce - cs, Ordering::Relaxed);
        cs = ce;
        pos = pos.max(cs);
    }
    Ok(out)
}
