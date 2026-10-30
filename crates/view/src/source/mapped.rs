//! `Mapped`: memmap2, страницы без копии (политика S7 §0, отображение — `view::sys`).

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use memmap2::Mmap;

use super::{ByteSource, SourceKind, assert_off_ui, clamp};
use crate::sys;

#[derive(Debug)]
pub struct MappedSource {
    map: Arc<Mmap>,
    path: PathBuf,
}

impl MappedSource {
    pub fn new(file: &File, path: PathBuf) -> io::Result<MappedSource> {
        Ok(MappedSource {
            map: Arc::new(sys::map(file)?),
            path,
        })
    }
}

impl ByteSource for MappedSource {
    fn len(&self) -> u64 {
        self.map.len() as u64
    }

    fn is_final(&self) -> bool {
        true
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        assert_off_ui();
        let n = clamp(off, buf.len(), self.len());
        if n > 0 {
            let at = off as usize;
            buf[..n].copy_from_slice(&self.map[at..at + n]);
        }
        Ok(n)
    }

    fn mapped(&self) -> Option<&[u8]> {
        Some(&self.map)
    }

    fn map(&self) -> Option<&Arc<Mmap>> {
        Some(&self.map)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Mapped
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }

    fn seq_file(&self) -> Option<io::Result<File>> {
        Some(sys::open_seq(&self.path))
    }
}
