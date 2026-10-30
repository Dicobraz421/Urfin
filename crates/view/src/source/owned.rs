//! `Owned`: файл до 4 МБ целиком в памяти.

use std::fs::File;
use std::io::{self, Read};
use std::sync::Arc;

use super::{ByteSource, SourceKind, assert_off_ui, clamp};

#[derive(Clone, Debug)]
pub struct OwnedSource {
    data: Arc<[u8]>,
}

impl OwnedSource {
    pub fn new(data: impl Into<Arc<[u8]>>) -> OwnedSource {
        OwnedSource { data: data.into() }
    }

    /// Читает файл целиком (`size` — подсказка ёмкости).
    pub fn read(mut file: File, size: u64) -> io::Result<OwnedSource> {
        let mut v = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        file.read_to_end(&mut v)?;
        Ok(OwnedSource::new(v))
    }

    pub fn from_reader(mut r: impl Read) -> io::Result<OwnedSource> {
        let mut v = Vec::new();
        r.read_to_end(&mut v)?;
        Ok(OwnedSource::new(v))
    }
}

impl ByteSource for OwnedSource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn is_final(&self) -> bool {
        true
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        assert_off_ui();
        let n = clamp(off, buf.len(), self.len());
        if n > 0 {
            let at = off as usize;
            buf[..n].copy_from_slice(&self.data[at..at + n]);
        }
        Ok(n)
    }

    fn mapped(&self) -> Option<&[u8]> {
        Some(&self.data)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Owned
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }
}
