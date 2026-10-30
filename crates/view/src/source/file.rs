//! `File`: `seek_read` по открытому handle — сеть, USB, follow, свежие файлы.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ByteSource, SourceKind, assert_off_ui, clamp};
use crate::sys;

#[derive(Debug)]
pub struct FileSource {
    file: File,
    path: PathBuf,
    len: AtomicU64,
    follow: bool,
}

impl FileSource {
    pub fn new(file: File, path: PathBuf, follow: bool) -> FileSource {
        let len = file.metadata().map_or(0, |m| m.len());
        FileSource {
            file,
            path,
            len: AtomicU64::new(len),
            follow,
        }
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    fn is_final(&self) -> bool {
        !self.follow
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
        SourceKind::File
    }

    fn refresh_len(&self) -> io::Result<u64> {
        let len = self.file.metadata()?.len();
        self.len.store(len, Ordering::Release);
        Ok(len)
    }

    fn seq_file(&self) -> Option<io::Result<File>> {
        Some(sys::open_seq(&self.path))
    }
}
