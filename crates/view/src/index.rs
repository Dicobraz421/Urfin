//! `LineIndex`: разреженный индекс строк `(offset, line)` не реже чем через 1024 строки и
//! фоновый индексатор (S7 §3.3). Карточка S7-05.
//!
//! Режимы: параллельный (`rayon`, диапазоны по 64 МБ: mmap/Owned и File на SSD/NVMe) и
//! последовательный (буфер 4 МБ, отдельный handle с `FILE_FLAG_SEQUENTIAL_SCAN`: HDD,
//! сеть, Spool). Прогресс — дельты меток не чаще 10 раз в секунду (`IndexMsg`). Отмена —
//! токеном документа, проверка каждые 4 МБ. Для follow и Spool индексатор дочитывает
//! прирост (`Indexer` хранит, докуда дошёл).

use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use urfin_core::{CancelToken, Cancelled, Cx, Msg, Runtime};
use urfin_vfs::volume::Media;

use crate::cache::{ByteView, Missing};
use crate::detect::Eol;
use crate::enc::Enc;
use crate::source::{ByteSource, SourceKind, read_full};
use crate::sys;
use crate::{Doc, DocId, ViewState};

/// Метка — не реже чем через столько строк.
pub const MARK_EVERY: u64 = 1024;
/// Диапазон параллельного режима.
pub const CHUNK: usize = 64 << 20;
/// Буфер последовательного режима и шаг проверки отмены.
pub const SLICE: usize = 4 << 20;
/// Публикация дельт — не чаще.
pub const PUBLISH_EVERY: Duration = Duration::from_millis(100);

/// Разделитель строк: байт или единица UTF-16 (`0A 00` / `00 0A` на чётном смещении).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Sep {
    Byte(u8),
    Le(u8),
    Be(u8),
}

impl Sep {
    /// `\n`, у `Eol::Cr` — `\r`; UTF-16 — единицей кода.
    pub fn of(enc: &Enc, eol: Eol) -> Sep {
        let b = if eol == Eol::Cr { b'\r' } else { b'\n' };
        if !enc.is_utf16() {
            Sep::Byte(b)
        } else if enc.is_utf16be() {
            Sep::Be(b)
        } else {
            Sep::Le(b)
        }
    }

    /// Байт в разделителе.
    pub fn unit(self) -> u64 {
        match self {
            Sep::Byte(_) => 1,
            Sep::Le(_) | Sep::Be(_) => 2,
        }
    }

    pub fn byte(self) -> u8 {
        match self {
            Sep::Byte(b) | Sep::Le(b) | Sep::Be(b) => b,
        }
    }

    fn wide(self) -> bool {
        self.unit() == 2
    }
}

/// Начала строк после разделителей в `hay` (абсолютное смещение `hay` — `base`, у UTF-16
/// чётное). Разделитель UTF-16, не поместившийся в `hay` целиком, не считается.
pub fn scan(hay: &[u8], base: u64, sep: Sep, mut f: impl FnMut(u64)) {
    match sep {
        Sep::Byte(b) => {
            for i in memchr::memchr_iter(b, hay) {
                f(base + i as u64 + 1);
            }
        }
        Sep::Le(b) => {
            for i in memchr::memchr_iter(b, hay) {
                let abs = base + i as u64;
                if abs.is_multiple_of(2) && hay.get(i + 1) == Some(&0) {
                    f(abs + 2);
                }
            }
        }
        Sep::Be(b) => {
            for i in memchr::memchr_iter(b, hay) {
                let abs = base + i as u64;
                if !abs.is_multiple_of(2) && i > 0 && hay[i - 1] == 0 {
                    f(abs + 1);
                }
            }
        }
    }
}

/// Номер строки: точный или оценка (индекс не дошёл, «стр. ≈N»).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineGuess {
    Exact(u64),
    Approx(u64),
}

impl LineGuess {
    pub fn get(self) -> u64 {
        match self {
            LineGuess::Exact(n) | LineGuess::Approx(n) => n,
        }
    }
}

/// Индекс строк документа (UI-поток; пополняется `IndexMsg`). Строки — с 0: `lines` —
/// число разделителей до `upto`, то есть номер строки, начинающейся в `upto`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndex {
    /// `(начало строки, номер строки)` по возрастанию; первая — `(0, 0)`.
    pub marks: Vec<(u64, u64)>,
    pub lines: u64,
    pub upto: u64,
    pub done: bool,
}

impl Default for LineIndex {
    fn default() -> Self {
        LineIndex {
            marks: vec![(0, 0)],
            lines: 0,
            upto: 0,
            done: false,
        }
    }
}

/// Дельта индекса от индексатора.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexDelta {
    pub marks: Vec<(u64, u64)>,
    pub lines: u64,
    pub upto: u64,
    pub done: bool,
}

impl LineIndex {
    pub fn apply(&mut self, d: IndexDelta) {
        self.marks.extend(d.marks);
        self.lines = d.lines;
        self.upto = d.upto;
        self.done = d.done;
    }

    /// Последняя метка не дальше `off`.
    pub fn mark_before(&self, off: u64) -> (u64, u64) {
        let i = self.marks.partition_point(|m| m.0 <= off);
        self.marks[i.saturating_sub(1)]
    }

    /// Последняя метка не дальше строки `line`.
    pub fn mark_before_line(&self, line: u64) -> (u64, u64) {
        let i = self.marks.partition_point(|m| m.1 <= line);
        self.marks[i.saturating_sub(1)]
    }

    /// Номер строки, содержащей `off`, без байтов: точно — на метке и в конце готового
    /// индекса; иначе оценка (между метками — интерполяция, дальше `upto` — по средней
    /// длине строки). Точный номер — [`LineIndex::line_of_in`].
    pub fn line_of(&self, off: u64) -> LineGuess {
        let (mo, ml) = self.mark_before(off);
        if mo == off {
            return LineGuess::Exact(ml);
        }
        if off >= self.upto {
            if self.done {
                return LineGuess::Exact(self.lines);
            }
            let avg = (self.upto / self.lines.max(1)).max(1);
            return LineGuess::Approx(self.lines + (off - self.upto) / avg);
        }
        let i = self.marks.partition_point(|m| m.0 <= off);
        let (no, nl) = self
            .marks
            .get(i)
            .copied()
            .unwrap_or((self.upto, self.lines));
        let span = (no - mo).max(1);
        let n = u128::from(off - mo) * u128::from(nl - ml) / u128::from(span);
        LineGuess::Approx(ml + n as u64)
    }

    /// Точный номер строки, если индекс покрывает `off`: счёт разделителей от метки (не
    /// больше 1024 строк).
    pub fn line_of_in(
        &self,
        b: &(impl ByteView + ?Sized),
        off: u64,
        sep: Sep,
    ) -> Result<LineGuess, Missing> {
        if off > self.upto {
            return Ok(self.line_of(off));
        }
        let (mo, ml) = self.mark_before(off);
        Ok(LineGuess::Exact(ml + count_seps(b, mo..off, sep)?))
    }
}

/// Разделителей в `range` (целиком внутри него).
pub fn count_seps(
    b: &(impl ByteView + ?Sized),
    range: std::ops::Range<u64>,
    sep: Sep,
) -> Result<u64, Missing> {
    let mut pos = range.start;
    let mut n = 0;
    while pos < range.end {
        let c = b.chunk(pos)?;
        if c.is_empty() {
            break;
        }
        let take = c.len().min((range.end - pos) as usize);
        scan(&c[..take], pos, sep, |_| n += 1);
        pos += take as u64;
    }
    Ok(n)
}

/// Переход: начало строки `target`, сканируя от метки `from` (`viewer.goto`). `None` —
/// файл кончился раньше.
pub fn goto_line(
    b: &(impl ByteView + ?Sized),
    from: (u64, u64),
    target: u64,
    sep: Sep,
) -> Result<Option<u64>, Missing> {
    let (mut pos, mut line) = from;
    if target <= line {
        return Ok(Some(pos));
    }
    let mut found = None;
    while found.is_none() {
        let c = b.chunk(pos)?;
        if c.is_empty() {
            return Ok(None);
        }
        scan(c, pos, sep, |s| {
            if found.is_none() {
                line += 1;
                if line == target {
                    found = Some(s);
                }
            }
        });
        pos += c.len() as u64;
    }
    Ok(found)
}

/// Режим индексатора.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IndexMode {
    Parallel,
    Sequential,
}

impl IndexMode {
    /// Параллельно — память (Owned, mmap не на HDD) и File на SSD/NVMe; иначе
    /// последовательно.
    pub fn of(kind: SourceKind, media: Option<Media>) -> IndexMode {
        let par = match kind {
            SourceKind::Owned => true,
            SourceKind::Mapped => media != Some(Media::Hdd),
            SourceKind::File => matches!(media, Some(Media::Ssd | Media::Nvme)),
            SourceKind::Seek | SourceKind::Spool => false,
        };
        if par {
            IndexMode::Parallel
        } else {
            IndexMode::Sequential
        }
    }
}

/// Ошибка индексатора.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("cancelled")]
    Cancelled,
    #[error("index read: {0}")]
    Io(#[from] io::Error),
}

impl From<Cancelled> for IndexError {
    fn from(_: Cancelled) -> Self {
        IndexError::Cancelled
    }
}

/// Копит метки и отдаёт дельты не чаще [`PUBLISH_EVERY`].
struct Publisher<'a> {
    last: Instant,
    marks: Vec<(u64, u64)>,
    out: &'a mut dyn FnMut(IndexDelta),
}

impl Publisher<'_> {
    fn flush(&mut self, lines: u64, upto: u64, done: bool) {
        self.last = Instant::now();
        (self.out)(IndexDelta {
            marks: std::mem::take(&mut self.marks),
            lines,
            upto,
            done,
        });
    }

    fn maybe(&mut self, lines: u64, upto: u64) {
        if self.last.elapsed() >= PUBLISH_EVERY {
            self.flush(lines, upto, false);
        }
    }
}

/// Итог диапазона параллельного режима: разделителей и метки на 1-м, 1025-м… разделителе
/// диапазона (номер строки — внутри диапазона).
struct ChunkRes {
    count: u64,
    marks: Vec<(u64, u64)>,
}

/// Состояние индексатора: докуда дошёл (дочитывание прироста — тем же значением).
#[derive(Clone, Debug)]
pub struct Indexer {
    pub sep: Sep,
    pub lines: u64,
    pub upto: u64,
    /// Номер строки последней метки.
    last_mark: u64,
    /// Диапазон параллельного режима и блок чтения (тесты задают меньше).
    chunk: u64,
    slice: usize,
}

impl Indexer {
    /// С начала файла (BOM входит в первую строку).
    pub fn new(sep: Sep) -> Indexer {
        Indexer {
            sep,
            lines: 0,
            upto: 0,
            last_mark: 0,
            chunk: CHUNK as u64,
            slice: SLICE,
        }
    }

    /// Свои размеры диапазона и блока (чётные: UTF-16 не рвётся).
    #[must_use]
    pub fn with_blocks(mut self, chunk: u64, slice: usize) -> Indexer {
        self.chunk = chunk.max(2) & !1;
        self.slice = slice.max(2) & !1;
        self
    }

    /// Индексирует `[upto, src.len())`, публикуя дельты; последняя — с `done`, если
    /// источник финальный и дочитан.
    pub fn run(
        &mut self,
        src: &dyn ByteSource,
        mode: IndexMode,
        cancel: &CancelToken,
        publish: &mut dyn FnMut(IndexDelta),
    ) -> Result<(), IndexError> {
        let _span = tracing::debug_span!("view.index", ?mode).entered();
        let mut p = Publisher {
            last: Instant::now(),
            marks: Vec::new(),
            out: publish,
        };
        let res = if mode == IndexMode::Parallel {
            self.run_par(src, cancel, &mut p)
        } else {
            Ok(())
        };
        let res = res.and_then(|()| self.run_seq(src, mode, cancel, &mut p));
        let done = res.is_ok() && src.is_final() && self.upto >= src.len();
        if !matches!(res, Err(IndexError::Cancelled)) {
            p.flush(self.lines, self.upto, done);
        }
        res
    }

    fn run_par(
        &mut self,
        src: &dyn ByteSource,
        cancel: &CancelToken,
        p: &mut Publisher<'_>,
    ) -> Result<(), IndexError> {
        let len = src.len();
        let sep = self.sep;
        let threads = rayon::current_num_threads().max(1) as u64;
        let (chunk, slice) = (self.chunk, self.slice);
        while self.upto < len {
            let start = self.upto;
            let end = len.min(start + chunk * threads);
            let ranges: Vec<(u64, u64)> = (start..end)
                .step_by(chunk as usize)
                .map(|s| (s, end.min(s + chunk)))
                .collect();
            let res: Vec<Result<ChunkRes, IndexError>> = ranges
                .par_iter()
                .map(|&(s, e)| scan_chunk(src, s, e, sep, slice, cancel))
                .collect();
            for r in res {
                let r = r?;
                for (off, l) in r.marks {
                    p.marks.push((off, self.lines + l));
                    self.last_mark = self.lines + l;
                }
                self.lines += r.count;
            }
            self.upto = end;
            p.maybe(self.lines, self.upto);
        }
        Ok(())
    }

    fn run_seq(
        &mut self,
        src: &dyn ByteSource,
        mode: IndexMode,
        cancel: &CancelToken,
        p: &mut Publisher<'_>,
    ) -> Result<(), IndexError> {
        let len = src.len();
        if self.upto >= len {
            return Ok(());
        }
        let file = match (mode, src.seq_file()) {
            (IndexMode::Sequential, Some(Ok(f))) => Some(f),
            _ => None,
        };
        let mut buf = vec![0u8; self.slice.min((len - self.upto) as usize)];
        while self.upto < len {
            cancel.check()?;
            let want = buf.len().min((len - self.upto) as usize);
            let mut n = match &file {
                Some(f) => {
                    let mut done = 0;
                    while done < want {
                        match sys::read_at(f, &mut buf[done..want], self.upto + done as u64) {
                            Ok(0) => break,
                            Ok(k) => done += k,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    done
                }
                None => read_full(src, self.upto, &mut buf[..want])?,
            };
            if n == 0 {
                break;
            }
            // Половину единицы UTF-16 не берём, пока файл может вырасти.
            if self.sep.wide()
                && !n.is_multiple_of(2)
                && (self.upto + (n as u64) < len || !src.is_final())
            {
                n -= 1;
                if n == 0 {
                    break;
                }
            }
            let (mut lines, mut last) = (self.lines, self.last_mark);
            scan(&buf[..n], self.upto, self.sep, |s| {
                lines += 1;
                if lines - last >= MARK_EVERY {
                    p.marks.push((s, lines));
                    last = lines;
                }
            });
            (self.lines, self.last_mark) = (lines, last);
            self.upto += n as u64;
            p.maybe(self.lines, self.upto);
        }
        Ok(())
    }
}

/// Один диапазон `[s, e)` параллельного режима; отмена — каждые `slice` байт.
fn scan_chunk(
    src: &dyn ByteSource,
    s: u64,
    e: u64,
    sep: Sep,
    slice: usize,
    cancel: &CancelToken,
) -> Result<ChunkRes, IndexError> {
    let mut r = ChunkRes {
        count: 0,
        marks: Vec::new(),
    };
    let mut on = |start: u64| {
        if r.count.is_multiple_of(MARK_EVERY) {
            r.marks.push((start, r.count + 1));
        }
        r.count += 1;
    };
    if let Some(data) = src.mapped() {
        let data = &data[s as usize..e as usize];
        for (k, part) in data.chunks(slice).enumerate() {
            cancel.check()?;
            scan(part, s + (k * slice) as u64, sep, &mut on);
        }
    } else {
        let mut buf = vec![0u8; slice.min((e - s) as usize)];
        let mut pos = s;
        while pos < e {
            cancel.check()?;
            let want = buf.len().min((e - pos) as usize);
            let n = read_full(src, pos, &mut buf[..want])?;
            if n == 0 {
                break;
            }
            scan(&buf[..n], pos, sep, &mut on);
            pos += n as u64;
        }
    }
    Ok(r)
}

/// Дельта индекса → документ.
#[derive(Debug)]
pub struct IndexMsg {
    pub doc: DocId,
    /// Поколение индекса документа: после сброса (смена разделителя) старые дельты
    /// отбрасываются.
    pub epoch: u32,
    pub delta: IndexDelta,
}

impl Msg for IndexMsg {
    fn name(&self) -> &'static str {
        "view.index"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        if let Some(Doc::Viewer(d)) = view.doc_mut(self.doc) {
            d.on_index(self.epoch, self.delta);
        }
    }
}

/// Пока источник растёт (Spool), индексатор проверяет прирост так часто.
const GROW_POLL: Duration = Duration::from_millis(250);

/// Запускает индексатор документа в выделенном потоке (долгий воркер); дельты — `IndexMsg`
/// поколения `epoch`. Пока источник не финальный (Spool), дочитывает прирост.
pub fn spawn_index(
    rt: &Runtime,
    doc: DocId,
    epoch: u32,
    src: Arc<dyn ByteSource>,
    mut ix: Indexer,
    mode: IndexMode,
    cancel: CancelToken,
) -> JoinHandle<()> {
    rt.spawn_dedicated(format!("view.index.{}", doc.0), cancel, move |cx| {
        let sink = cx.sink.clone();
        let tok = cx.cancel.clone();
        let mut publish = |delta: IndexDelta| {
            let _ = sink.send(Box::new(IndexMsg { doc, epoch, delta }), &tok);
        };
        let mut was_final = false;
        loop {
            let fin = src.is_final();
            let len = src.len();
            if len > ix.upto || (fin && !was_final) {
                match ix.run(&*src, mode, &cx.cancel, &mut publish) {
                    Ok(()) => {}
                    Err(IndexError::Cancelled) => return,
                    Err(e) => {
                        tracing::warn!(error = %e, "view index stopped");
                        return;
                    }
                }
            }
            was_final = fin;
            if fin && ix.upto >= len {
                return;
            }
            std::thread::sleep(GROW_POLL);
            if cx.cancel.is_cancelled() {
                return;
            }
        }
    })
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
