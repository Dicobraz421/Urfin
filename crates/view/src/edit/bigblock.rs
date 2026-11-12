//! Блочная операция выше [`LINES_MAX`](super::input::block::LINES_MAX) строк (или шире бюджета страниц) — фоновой задачей
//! (S7-42; S7 §5.2, ADR-0023 §5, §7). Карточка S7-42.
//!
//! Задача идёт по снимку (Bulk) окном байт [`WIN_MIN`]+ (не кэшем страниц документа):
//! логические строки блока сверху вниз — та же раскладка, что у синхронного пути
//! (`rows_down`, `col_span`), правка каждой строки — сразу в поток результата
//! (`RewriteOut`: память до 16 мебибайт, дальше — временный файл). Итог — новые байты
//! диапазона `[начало первой строки, конец последней правки)`: документ ставит их одним
//! куском `Rewrite` одной группой отмены. Память задачи постоянна: окно чтения и хвост
//! записи. Отмена — между строками; документ на время задачи только для чтения
//! (`Engine::busy`), отменённая задача его не трогает.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use super::input::block::Area;
use super::piece::TextSnap;
use super::replace::TempAt;
use super::write::{RewriteOut, Rewritten};
use crate::cache::{ByteView, Missing};
use crate::layout::{ColSpan, LayoutSpec, VRow, col_span, rows_down};

/// Окно чтения не меньше (и не меньше четырёх сегментов `max_line`).
pub const WIN_MIN: usize = 4 << 20;

/// Текст блока в буфер обмена: лимит копирования (байт документа) и конец строк.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyOpts {
    pub limit: u64,
    pub eol: &'static str,
}

/// Что сделать с блоком.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockJob {
    /// Блок прочь (Del, Backspace, вырезание, ввод и Enter при блоке): на строке
    /// `caret_line` (начало её сегмента) у левого края блока — `bytes`; `copy` — текст
    /// блока в буфер обмена (вырезание).
    Replace {
        area: Area,
        caret_line: u64,
        bytes: Vec<u8>,
        copy: Option<CopyOpts>,
    },
    /// Только текст блока (копирование).
    Copy { area: Area, copy: CopyOpts },
    /// Вставка колонкой `col` от строки `first`: строки `lines` (в кодировке документа),
    /// короткие строки добиваются `space`, недостающие дописываются через `eol`; `block`
    /// — блок, колонки которого уходят.
    Paste {
        first: u64,
        col: u32,
        block: Option<Area>,
        lines: Vec<Vec<u8>>,
        space: Vec<u8>,
        eol: Vec<u8>,
    },
}

impl BlockJob {
    /// Объём для прогресса: байты блока (вставка — строки).
    pub fn total(&self) -> u64 {
        match self {
            BlockJob::Replace { area, .. } | BlockJob::Copy { area, .. } => {
                area.bottom - area.top + 1
            }
            BlockJob::Paste { lines, block, .. } => {
                (lines.len() as u64).max(block.map_or(0, |b| b.bottom - b.top + 1))
            }
        }
    }

    /// Строка начала обхода.
    fn first(&self) -> u64 {
        match self {
            BlockJob::Replace { area, .. } | BlockJob::Copy { area, .. } => area.top,
            BlockJob::Paste { first, .. } => *first,
        }
    }
}

/// Итог задачи.
#[derive(Debug)]
pub struct BlockDone {
    /// Текст блока в буфер обмена (копирование, вырезание).
    pub text: Option<String>,
    /// Правка: диапазон документа и его новые байты (куски над `Src::Rewrite(0)`).
    pub edit: Option<(Range<u64>, Rewritten)>,
    /// Курсор после операции (`None` — не трогать: копирование).
    pub caret: Option<u64>,
    /// Пик памяти задачи: окно чтения и буфер результата, байт.
    pub peak: usize,
}

/// Итог задачи или отказ.
#[derive(Debug)]
pub enum BlockRes {
    Done(BlockDone),
    /// Текст блока больше лимита копирования (байт): ничего не сделано.
    TooBig(u64),
}

#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Окно байт снимка `[base, base + buf.len())` как `ByteView` (вне окна — `Missing`).
struct Win<'a> {
    snap: &'a TextSnap,
    base: u64,
    buf: Vec<u8>,
    len: u64,
    size: usize,
}

impl ByteView for Win<'_> {
    fn len(&self) -> u64 {
        self.len
    }

    fn chunk(&self, off: u64) -> Result<&[u8], Missing> {
        if off >= self.len {
            return Ok(&[]);
        }
        let end = self.base + self.buf.len() as u64;
        if off >= self.base && off < end {
            Ok(&self.buf[(off - self.base) as usize..])
        } else {
            Err(Missing { pages: 0..0 })
        }
    }

    fn chunk_before(&self, end: u64) -> Result<&[u8], Missing> {
        if end == 0 {
            return Ok(&[]);
        }
        if end > self.base && end <= self.base + self.buf.len() as u64 {
            Ok(&self.buf[..(end - self.base) as usize])
        } else {
            Err(Missing { pages: 0..0 })
        }
    }
}

impl Win<'_> {
    /// Окно — с `keep` (ещё не выведенные байты) и не позже чем за 16 байт до `at`.
    fn load(&mut self, at: u64, keep: u64) -> io::Result<()> {
        let base = keep.min(at.saturating_sub(16));
        let n = (self.size as u64).min(self.len - base.min(self.len)) as usize;
        self.buf.resize(n, 0);
        let got = self.snap.read_at(base, &mut self.buf)?;
        self.buf.truncate(got);
        self.base = base;
        Ok(())
    }

    fn bytes(&self, r: Range<u64>) -> &[u8] {
        &self.buf[(r.start - self.base) as usize..(r.end - self.base) as usize]
    }
}

/// Строка блока: логический сегмент и его колонки.
type Row = (VRow, ColSpan);

/// Обход строк блока по окну.
struct Walk<'a> {
    win: Win<'a>,
    spec: LayoutSpec,
}

impl Walk<'_> {
    /// Сегмент с `p` и колонки `[c0, c1)`; `p` — конец документа — пустая строка после
    /// последнего перевода строки (`tail`), иначе `None`.
    fn row(&mut self, p: u64, keep: u64, c: (u32, u32), tail: bool) -> io::Result<Option<Row>> {
        let len = self.win.len;
        if p >= len {
            if !tail {
                return Ok(None);
            }
            let r = VRow {
                start: len,
                end: len,
                next: len,
                cont: false,
                cut: false,
                seg: len,
                sub: 0,
            };
            let s = ColSpan {
                start: len,
                end: len,
                col: 0,
                width: 0,
            };
            return Ok(Some((r, s)));
        }
        let spec = self.spec;
        let get = |w: &Win<'_>| -> Result<Option<Row>, Missing> {
            let Some(r) = rows_down(w, (p, 0), 1, &spec)?.first().copied() else {
                return Ok(None);
            };
            // Колонки — и по байтам перевода строки (окно держит строку целиком).
            w.chunk(r.next.saturating_sub(1).max(r.start))?;
            Ok(Some((r, col_span(w, &r, c.0, c.1, &spec)?)))
        };
        if let Ok(v) = get(&self.win) {
            return Ok(v);
        }
        self.win.load(p, keep)?;
        get(&self.win).map_err(|_| io::Error::other("block row is longer than the read window"))
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "обход строк — один путь сверху вниз; w — окно, r — строка, s — колонки, p — позиция"
)]
/// Задача над снимком `snap` (вне UI-потока); `spec` — раскладка логических строк
/// документа (без переноса), `progress` — сделано из [`BlockJob::total`].
pub fn run(
    snap: &TextSnap,
    job: &BlockJob,
    spec: &LayoutSpec,
    temp: &TempAt,
    progress: &AtomicU64,
    cancelled: &dyn Fn() -> bool,
) -> Result<BlockRes, BlockError> {
    let _span = tracing::debug_span!("edit.block", len = snap.len()).entered();
    let len = snap.len();
    let ml = usize::try_from(spec.max_line).unwrap_or(usize::MAX);
    let size = WIN_MIN.max(ml.saturating_mul(4)).saturating_add(64 << 10);
    let mut w = Walk {
        win: Win {
            snap,
            base: 0,
            buf: Vec::new(),
            len,
            size,
        },
        spec: *spec,
    };
    let edits = !matches!(job, BlockJob::Copy { .. });
    let copy = match job {
        BlockJob::Replace { copy, .. } => copy.clone(),
        BlockJob::Copy { copy, .. } => Some(copy.clone()),
        BlockJob::Paste { .. } => None,
    };
    let mut out = if edits {
        Some(RewriteOut::new(snap.sep(), 0, &temp.dir, &temp.name)?)
    } else {
        None
    };
    let mut text = copy.as_ref().map(|_| String::new());
    let mut size_copied = 0u64;
    let mut peak = 0usize;
    let start = job.first();
    // Вход до `cur` уже выведен; курсор — место в выходе.
    let mut cur = start;
    let mut caret = None;
    let mut changed = false;
    let mut i = 0usize;
    let mut p = start;
    let mut prev: Option<VRow> = None;
    let mut buf = Vec::new();
    loop {
        if cancelled() {
            return Err(BlockError::Cancelled);
        }
        let (in_block, c) = match job {
            BlockJob::Replace { area, .. } | BlockJob::Copy { area, .. } => {
                (true, (area.c0, area.c1))
            }
            BlockJob::Paste { col, block, .. } => match block {
                Some(b) if prev.is_none_or(|r| r.start < b.bottom) => (true, (b.c0, b.c1)),
                _ => (false, (*col, *col)),
            },
        };
        if let BlockJob::Paste { lines, .. } = job
            && !in_block
            && i >= lines.len()
        {
            break;
        }
        // Пустая строка после последнего перевода строки — у первой строки (угол блока на
        // ней) и после строки, кончившейся переводом строки в конце документа.
        let tail = prev.is_none_or(|r| r.next == len && r.end < r.next);
        let Some((r, s)) = w.row(p, cur, c, tail)? else {
            break;
        };
        // Байты строки, задевающие колонки, — в окне: строка только что разложена.
        // Счёт байт — и после лимита: тост называет размер всего блока.
        if copy.is_some() {
            size_copied += s.end - s.start;
        }
        if let Some(t) = &mut text {
            let lim = copy.as_ref().map_or(u64::MAX, |c| c.limit);
            if size_copied > lim {
                // Дальше — только счёт байт для тоста.
                text = None;
                out = None;
            } else {
                if i > 0 {
                    t.push_str(copy.as_ref().map_or("\n", |c| c.eol));
                }
                let b = w.win.bytes(s.start..s.end);
                t.push_str(&spec.enc.enc.decode_without_bom_handling(b).0);
            }
        }
        if let Some(o) = &mut out {
            buf.clear();
            match job {
                BlockJob::Replace {
                    caret_line, bytes, ..
                } if r.start == *caret_line => buf.extend_from_slice(bytes),
                BlockJob::Paste {
                    col, lines, space, ..
                } => {
                    if let Some(l) = lines.get(i) {
                        if s.width < *col {
                            for _ in s.width..*col {
                                buf.extend_from_slice(space);
                            }
                        }
                        buf.extend_from_slice(l);
                    }
                }
                _ => {}
            }
            o.write(w.win.bytes(cur..s.start))?;
            let here = match job {
                BlockJob::Replace { caret_line, .. } => r.start == *caret_line,
                _ => i == 0,
            };
            if here || caret.is_none() && i == 0 {
                caret = Some(start + o.len() + buf.len() as u64);
            }
            changed |= s.end > s.start || !buf.is_empty();
            o.write(&buf)?;
            cur = s.end;
        }
        peak = peak.max(w.win.buf.capacity() + out.as_ref().map_or(0, RewriteOut::held));
        i += 1;
        progress.store(
            match job {
                BlockJob::Paste { .. } => i as u64,
                _ => r.start - start + 1,
            },
            Ordering::Relaxed,
        );
        prev = Some(r);
        let last = match job {
            BlockJob::Replace { area, .. } | BlockJob::Copy { area, .. } => r.start >= area.bottom,
            BlockJob::Paste { .. } => false,
        };
        // Следующая строка: как `Engine::seg_below`.
        if last || r.next <= r.start || r.next > len {
            break;
        }
        p = r.next;
    }
    if size_copied > copy.as_ref().map_or(u64::MAX, |c| c.limit) {
        return Ok(BlockRes::TooBig(size_copied));
    }
    // Строк документа не хватило — дописать (вставка).
    if let (
        Some(o),
        BlockJob::Paste {
            lines,
            space,
            eol,
            col,
            ..
        },
    ) = (&mut out, job)
        && i < lines.len()
    {
        // Хвост последней строки — как есть.
        let mut rest = vec![0u8; (len - cur) as usize];
        let got = snap.read_at(cur, &mut rest)?;
        o.write(&rest[..got])?;
        cur = len;
        for l in &lines[i..] {
            if cancelled() {
                return Err(BlockError::Cancelled);
            }
            buf.clear();
            buf.extend_from_slice(eol);
            for _ in 0..*col {
                buf.extend_from_slice(space);
            }
            buf.extend_from_slice(l);
            o.write(&buf)?;
        }
        if caret.is_none() {
            caret = Some(start + eol.len() as u64 + space.len() as u64 * u64::from(*col));
        }
        changed = true;
        peak = peak.max(w.win.buf.capacity() + o.held());
    }
    let edit = match out {
        Some(o) if changed => Some((start..cur, o.finish(Vec::new())?)),
        _ => None,
    };
    Ok(BlockRes::Done(BlockDone {
        text,
        edit,
        caret: if edits { caret } else { None },
        peak,
    }))
}
