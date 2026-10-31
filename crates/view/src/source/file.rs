//! `File`: `seek_read` по открытому handle — сеть, USB, follow, свежие файлы.
//!
//! Handle std открыт с `FILE_SHARE_READ | WRITE | DELETE`: писатель пишет, усекает,
//! удаляет и ротирует файл, пока вьювер держит его (S7 §3.8).

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{ByteSource, SourceKind, assert_off_ui, clamp};
use crate::sys;

#[derive(Debug)]
pub struct FileSource {
    file: File,
    path: PathBuf,
    len: AtomicU64,
    follow: AtomicBool,
}

impl FileSource {
    pub fn new(file: File, path: PathBuf, follow: bool) -> FileSource {
        let len = file.metadata().map_or(0, |m| m.len());
        FileSource {
            file,
            path,
            len: AtomicU64::new(len),
            follow: AtomicBool::new(follow),
        }
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    fn is_final(&self) -> bool {
        !self.follow.load(Ordering::Acquire)
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
        // В follow путь может вести уже к другому файлу (ротация): только свой handle.
        if self.follow.load(Ordering::Acquire) {
            return None;
        }
        Some(sys::open_seq(&self.path))
    }

    fn set_follow(&self, on: bool) -> bool {
        self.follow.store(on, Ordering::Release);
        true
    }

    fn file_id(&self) -> Option<(u64, u128)> {
        sys::file_id(&self.file).ok()
    }
}
