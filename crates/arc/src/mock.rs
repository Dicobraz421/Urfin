//! Мок-формат для тестов, бенча индекса и сцен: текстовый файл с оглавлением.
//!
//! ```text
//! URFIN-MOCK
//! f <size> <packed> <mtime> <путь>   — файл
//! d <путь>                           — каталог
//! gen <n>                            — n синтетических файлов `dNNN/sN/fNNNNNNN.bin`
//! solid | encrypted                  — флаги архива
//! ```
//!
//! Данные файла — детерминированные байты длины `size`. Две схемы: `mock` — оглавление
//! сразу (как zip), `mocks` — потоком (как tar.gz).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use urfin_core::{Attrs, CancelToken, EntryKind};
use urfin_vfs::{Caps, ExtractStream, Extracted, VfsError, VfsRead};

use crate::format::{ArcSource, Format, Scan, corrupt, format_error};
use crate::index::{ArchiveIndex, EntryLoc, IndexBuilder, NodeId, RawEntry};

/// Первая строка файла мок-архива.
pub const MAGIC: &str = "URFIN-MOCK";

/// Записей за один шаг разбора.
const STEP: usize = 8192;

/// Мок-формат.
#[derive(Debug, Default)]
pub struct MockFormat {
    streaming: bool,
    /// Своя схема вместо `mock`/`mocks`: мок под видом `zip` (тесты Enter, сцены).
    scheme: Option<&'static str>,
    scans: Arc<AtomicUsize>,
}

impl MockFormat {
    /// Оглавление сразу: схема `mock`.
    pub fn catalog() -> MockFormat {
        MockFormat::default()
    }

    /// Записи потоком: схема `mocks`.
    pub fn streaming() -> MockFormat {
        MockFormat {
            streaming: true,
            ..MockFormat::default()
        }
    }

    /// Под видом схемы `scheme`.
    #[must_use]
    pub fn with_scheme(mut self, scheme: &'static str) -> MockFormat {
        self.scheme = Some(scheme);
        self
    }

    /// Сколько раз разбиралось оглавление (чтений архива).
    pub fn scans(&self) -> usize {
        self.scans.load(Ordering::Relaxed)
    }
}

/// Текст мок-архива: `files` — (путь, размер).
pub fn text(files: &[(&str, u64)]) -> String {
    let mut s = format!("{MAGIC}\n");
    for (p, size) in files {
        let _ = writeln!(s, "f {size} {} 1700000000000000000 {p}", size / 2);
    }
    s
}

/// Байт `j` данных записи номер `seed`.
fn byte(seed: u32, j: u64) -> u8 {
    ((u64::from(seed) * 31 + j) % 251) as u8
}

/// Данные записи номер `seed` длины `size`.
pub fn data(seed: u32, size: u64) -> Vec<u8> {
    (0..size).map(|j| byte(seed, j)).collect()
}

struct MockScan {
    outer: urfin_core::Location,
    lines: std::vec::IntoIter<String>,
    /// Синтетические: следующий номер и сколько всего.
    gen_at: u32,
    gen_n: u32,
    seq: u32,
    path: String,
}

impl Scan for MockScan {
    fn step(&mut self, b: &mut IndexBuilder, cancel: &CancelToken) -> Result<bool, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let mut n = 0;
        while n < STEP {
            if self.gen_at < self.gen_n {
                let i = self.gen_at;
                self.gen_at += 1;
                self.path.clear();
                let _ = write!(
                    self.path,
                    "d{:03}/s{}/f{i:07}.bin",
                    i % 1000,
                    (i / 1000) % 10
                );
                let size = u64::from(i % 4096);
                b.push(RawEntry {
                    mtime: 1_700_000_000_000_000_000 + i64::from(i),
                    ..RawEntry::file(&self.path, size, size / 3, EntryLoc::Index(i))
                });
                n += 1;
                continue;
            }
            let Some(line) = self.lines.next() else {
                return Ok(false);
            };
            n += 1;
            let line = line.trim_end_matches('\r');
            let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
            match tag {
                "" => {}
                "solid" => b.set_solid(true),
                "encrypted" => b.set_encrypted(true),
                "d" => {
                    b.push(RawEntry::dir(rest));
                }
                "gen" => {
                    self.gen_n = rest
                        .trim()
                        .parse()
                        .map_err(|e| corrupt(&self.outer, format!("gen: {e}")))?;
                    self.gen_at = 0;
                }
                "f" => {
                    let mut it = rest.splitn(4, ' ');
                    let mut num = |what: &str| -> Result<i64, VfsError> {
                        it.next()
                            .and_then(|v| v.parse::<i64>().ok())
                            .ok_or_else(|| corrupt(&self.outer, format!("f: {what}")))
                    };
                    let size = num("size")?.max(0).cast_unsigned();
                    let packed = num("packed")?.max(0).cast_unsigned();
                    let mtime = num("mtime")?;
                    let path = it.next().unwrap_or_default();
                    self.seq += 1;
                    let attrs = if packed > size {
                        Attrs(Attrs::ENCRYPTED)
                    } else {
                        Attrs::default()
                    };
                    b.push(RawEntry {
                        path,
                        kind: EntryKind::File,
                        attrs,
                        size,
                        packed,
                        mtime,
                        loc: EntryLoc::Index(1_000_000_000 + self.seq),
                    });
                }
                other => return Err(corrupt(&self.outer, format!("unknown line {other:?}"))),
            }
        }
        Ok(true)
    }
}

/// Данные записи в памяти, с `Seek`.
#[derive(Debug)]
pub struct MemRead(pub Cursor<Vec<u8>>);

impl Read for MemRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for MemRead {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}

impl VfsRead for MemRead {
    fn len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

/// Поток плана: файлы по порядку узлов, данные — по одному в памяти.
struct MockStream {
    todo: VecDeque<(u32, u32, u64)>,
    cur: Vec<u8>,
}

impl ExtractStream for MockStream {
    fn next(&mut self, cancel: &CancelToken) -> Result<Option<Extracted<'_>>, VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let Some((input, seed, size)) = self.todo.pop_front() else {
            return Ok(None);
        };
        self.cur = data(seed, size);
        Ok(Some((input, Box::new(self.cur.as_slice()))))
    }
}

fn seed_of(idx: &ArchiveIndex, node: NodeId) -> u32 {
    match idx.node(node).loc {
        EntryLoc::Index(i) => i,
        _ => 0,
    }
}

impl Format for MockFormat {
    fn scheme(&self) -> &'static str {
        match (self.scheme, self.streaming) {
            (Some(s), _) => s,
            (None, true) => "mocks",
            (None, false) => "mock",
        }
    }

    fn catalog_first(&self) -> bool {
        !self.streaming
    }

    fn scan(&self, src: &ArcSource) -> Result<Box<dyn Scan>, VfsError> {
        self.scans.fetch_add(1, Ordering::Relaxed);
        let mut text = String::new();
        src.open()?
            .read_to_string(&mut text)
            .map_err(|_| format_error(src.outer()))?;
        let mut lines = text
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter();
        if lines.next().as_deref().map(str::trim_end) != Some(MAGIC) {
            return Err(format_error(src.outer()));
        }
        Ok(Box::new(MockScan {
            outer: src.outer().clone(),
            lines,
            gen_at: 0,
            gen_n: 0,
            seq: 0,
            path: String::new(),
        }))
    }

    fn open_entry(
        &self,
        _src: &ArcSource,
        idx: &ArchiveIndex,
        node: NodeId,
    ) -> Result<Box<dyn VfsRead>, VfsError> {
        let n = idx.node(node);
        Ok(Box::new(MemRead(Cursor::new(data(
            seed_of(idx, node),
            n.size,
        )))))
    }

    fn entry_caps(&self, _idx: &ArchiveIndex, _node: NodeId) -> u32 {
        if self.streaming {
            Caps::SEQUENTIAL
        } else {
            Caps::SEEK
        }
    }

    fn plan(
        &self,
        _src: &ArcSource,
        idx: &Arc<ArchiveIndex>,
        files: &[(u32, NodeId)],
        parallel: u8,
    ) -> Option<Vec<Box<dyn ExtractStream>>> {
        let k = usize::from(parallel.max(1)).min(files.len().max(1));
        let mut sorted = files.to_vec();
        sorted.sort_unstable_by_key(|&(_, node)| seed_of(idx, node));
        let mut streams: Vec<VecDeque<(u32, u32, u64)>> = vec![VecDeque::new(); k];
        for (i, &(input, node)) in sorted.iter().enumerate() {
            streams[i % k].push_back((input, seed_of(idx, node), idx.node(node).size));
        }
        Some(
            streams
                .into_iter()
                .map(|todo| {
                    Box::new(MockStream {
                        todo,
                        cur: Vec::new(),
                    }) as Box<dyn ExtractStream>
                })
                .collect(),
        )
    }

    fn transfer_channels(&self) -> u16 {
        4
    }
}
