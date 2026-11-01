//! Буфер правок `Add` (ADR-0023 §2): только дописывание, блоки по 64 КБ в `Arc`
//! (срезы, отданные снимкам, не меняются). В памяти — до [`MEM_MAX`]; сверх — старые
//! полные блоки уходят во временный файл `%TEMP%\Urfin\edit-<pid>-<doc>.add`
//! (`FILE_ATTRIBUTE_TEMPORARY`, `FILE_FLAG_DELETE_ON_CLOSE`), смещение в файле — то же,
//! что в буфере. Выгруженное читается через `PageCache` (UI) или `read_at` (снимок).
//!
//! Набором порог не достичь; его достигают вставка больших объёмов и «Заменить все» с
//! группами — их дописывание идёт вне UI-потока (запись блока в файл — I/O).

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cache::PageCache;
use crate::source::{ByteSource, SourceKind, assert_off_ui, clamp};
use crate::sys;

/// Блок буфера.
pub const BLOCK: u64 = 64 << 10;
const BLOCK_US: usize = 64 << 10;
/// Байт буфера в памяти — не больше (сверх — временный файл).
pub const MEM_MAX: u64 = 16 << 20;
/// Кэш страниц выгруженной части.
const SPILL_CACHE: usize = 2 << 20;

type Block = Arc<Vec<u8>>;

/// Временный файл выгруженных блоков: растёт, удаляется ОС при закрытии последнего
/// handle.
#[derive(Debug)]
pub struct SpillSource {
    file: File,
    len: AtomicU64,
}

impl ByteSource for SpillSource {
    fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    fn is_final(&self) -> bool {
        false
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        assert_off_ui();
        let n = clamp(off, buf.len(), self.len());
        if n == 0 {
            return Ok(0);
        }
        sys::read_at(&self.file, &mut buf[..n], off)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Spool
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }
}

/// Буфер правок документа.
pub struct AddBuf {
    /// `None` — блок выгружен (выгружается всегда префикс).
    blocks: Arc<Vec<Option<Block>>>,
    len: u64,
    /// Байт в памяти.
    mem: u64,
    mem_max: u64,
    /// Выгружено блоков.
    spilled: usize,
    spill: Option<Arc<SpillSource>>,
    pub(crate) pages: PageCache,
    dir: PathBuf,
    name: String,
}

impl fmt::Debug for AddBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AddBuf")
            .field("len", &self.len)
            .field("mem", &self.mem)
            .field("spilled", &self.spilled)
            .finish_non_exhaustive()
    }
}

impl AddBuf {
    /// Буфер документа `doc`: выгрузка — `%TEMP%\Urfin\edit-<pid>-<doc>.add`.
    pub fn new(doc: u32) -> AddBuf {
        let name = format!("edit-{}-{doc}", std::process::id());
        AddBuf::with_limit(std::env::temp_dir().join("Urfin"), name, MEM_MAX)
    }

    /// Свой каталог, имя и порог памяти (тесты).
    pub fn with_limit(dir: PathBuf, name: String, mem_max: u64) -> AddBuf {
        AddBuf {
            blocks: Arc::new(Vec::new()),
            len: 0,
            mem: 0,
            mem_max: mem_max.max(BLOCK),
            spilled: 0,
            spill: None,
            pages: PageCache::new(SPILL_CACHE, 0, false),
            dir,
            name,
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Байт в памяти (без блоков, которые держат только снимки).
    pub fn mem(&self) -> u64 {
        self.mem
    }

    /// Выгружено байт.
    pub fn spilled(&self) -> u64 {
        self.spilled as u64 * BLOCK
    }

    /// Дописывает байты; возвращает их диапазон в буфере. Сверх порога — выгрузка
    /// старых блоков (I/O).
    pub fn push(&mut self, mut data: &[u8]) -> io::Result<std::ops::Range<u64>> {
        let start = self.len;
        while !data.is_empty() {
            let bi = (self.len / BLOCK) as usize;
            let at = (self.len % BLOCK) as usize;
            let blocks = Arc::make_mut(&mut self.blocks);
            if bi == blocks.len() {
                blocks.push(Some(Arc::new(Vec::with_capacity(BLOCK_US))));
            }
            let v = Arc::make_mut(blocks[bi].as_mut().expect("tail block is in memory"));
            if v.capacity() < BLOCK_US {
                v.reserve_exact(BLOCK_US - v.len());
            }
            let k = data.len().min(BLOCK_US - at);
            v.extend_from_slice(&data[..k]);
            data = &data[k..];
            self.len += k as u64;
            self.mem += k as u64;
            if self.mem > self.mem_max {
                self.spill_old()?;
            }
        }
        Ok(start..self.len)
    }

    /// Полные блоки от начала — во временный файл, пока память выше порога.
    fn spill_old(&mut self) -> io::Result<()> {
        while self.mem > self.mem_max && (self.spilled as u64 + 1) * BLOCK <= self.len {
            let file = if let Some(f) = &self.spill {
                f.clone()
            } else {
                let f = Arc::new(SpillSource {
                    file: create(&self.dir, &self.name)?,
                    len: AtomicU64::new(0),
                });
                self.pages = PageCache::with_source(SPILL_CACHE, f.clone());
                self.spill = Some(f.clone());
                f
            };
            let bi = self.spilled;
            let blocks = Arc::make_mut(&mut self.blocks);
            let b = blocks[bi].as_ref().expect("block to spill is in memory");
            sys::write_all_at(&file.file, b, bi as u64 * BLOCK)?;
            blocks[bi] = None;
            self.spilled += 1;
            self.mem -= BLOCK;
            let len = self.spilled as u64 * BLOCK;
            file.len.store(len, Ordering::Release);
            self.pages.set_len(len, false);
        }
        Ok(())
    }

    /// Байты в памяти с `off` до конца блока; `None` — блок выгружен.
    pub fn mem_chunk(&self, off: u64) -> Option<&[u8]> {
        let b = self.blocks.get((off / BLOCK) as usize)?.as_ref()?;
        b.get((off % BLOCK) as usize..)
    }

    /// Байты в памяти от начала блока до `end`; `None` — блок выгружен.
    pub fn mem_chunk_before(&self, end: u64) -> Option<&[u8]> {
        if end == 0 {
            return Some(&[]);
        }
        let bi = (end - 1) / BLOCK;
        let b = self.blocks.get(bi as usize)?.as_ref()?;
        b.get(..(end - bi * BLOCK) as usize)
    }

    /// Временный файл выгрузки (подкачка страниц в тестах).
    #[cfg(test)]
    pub(crate) fn spill_src(&self) -> Option<Arc<dyn ByteSource>> {
        self.spill.clone().map(|s| s as Arc<dyn ByteSource>)
    }

    /// Снимок: блоки в памяти и выгруженный файл (O(1)).
    pub fn snap(&self) -> AddSnap {
        AddSnap {
            blocks: self.blocks.clone(),
            spill: self.spill.clone(),
        }
    }
}

/// Буфер правок на момент снимка (`Send + Sync`).
#[derive(Clone, Debug)]
pub struct AddSnap {
    blocks: Arc<Vec<Option<Block>>>,
    spill: Option<Arc<SpillSource>>,
}

impl AddSnap {
    /// Чтение с `off` до конца блока (вне UI: выгруженное — из файла).
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let bi = (off / BLOCK) as usize;
        let at = (off % BLOCK) as usize;
        match self.blocks.get(bi) {
            Some(Some(b)) => {
                let s = b.get(at..).unwrap_or(&[]);
                let n = s.len().min(buf.len());
                buf[..n].copy_from_slice(&s[..n]);
                Ok(n)
            }
            Some(None) => {
                let f = self.spill.as_ref().ok_or(io::ErrorKind::NotFound)?;
                let n = buf.len().min(BLOCK_US - at);
                f.read_at(off, &mut buf[..n])
            }
            None => Ok(0),
        }
    }
}

/// Создаёт временный файл `<dir>\<name>.add`; имя занято (прежний буфер документа ещё
/// держат) — с суффиксом.
fn create(dir: &Path, name: &str) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    let mut n = 0u32;
    loop {
        let file = if n == 0 {
            format!("{name}.add")
        } else {
            format!("{name}-{n}.add")
        };
        match sys::open_spool(&dir.join(file)) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 64 => n += 1,
            r => return r,
        }
    }
}
