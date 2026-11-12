//! `File`: `seek_read` по открытому handle — сеть, USB, follow, свежие файлы.
//!
//! Handle std открыт с `FILE_SHARE_READ | WRITE | DELETE`: писатель пишет, усекает,
//! удаляет и ротирует файл, пока вьювер держит его (S7 §3.8). Редактор держит файл
//! больше 16 МБ без `FILE_SHARE_WRITE`; сохранение «на месте» переводит источник на
//! копию тех же байтов ([`ByteSource::move_to`], S7-44) — handle закрывается.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock, RwLockReadGuard};

use super::{ByteSource, SourceKind, assert_off_ui, clamp};
use crate::sys;

#[derive(Debug)]
pub struct FileSource {
    file: RwLock<File>,
    path: PathBuf,
    len: AtomicU64,
    follow: AtomicBool,
    /// Читается копия ([`ByteSource::move_to`]): путь ведёт уже к другим байтам.
    moved: AtomicBool,
}

impl FileSource {
    pub fn new(file: File, path: PathBuf, follow: bool) -> FileSource {
        let len = file.metadata().map_or(0, |m| m.len());
        FileSource {
            file: RwLock::new(file),
            path,
            len: AtomicU64::new(len),
            follow: AtomicBool::new(follow),
            moved: AtomicBool::new(false),
        }
    }

    fn file(&self) -> RwLockReadGuard<'_, File> {
        self.file.read().unwrap_or_else(PoisonError::into_inner)
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
        sys::read_at(&self.file(), &mut buf[..n], off)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::File
    }

    fn refresh_len(&self) -> io::Result<u64> {
        let len = self.file().metadata()?.len();
        self.len.store(len, Ordering::Release);
        Ok(len)
    }

    fn seq_file(&self) -> Option<io::Result<File>> {
        // В follow путь может вести уже к другому файлу (ротация): только свой handle.
        if self.follow.load(Ordering::Acquire) || self.moved.load(Ordering::Acquire) {
            return None;
        }
        Some(sys::open_seq(&self.path))
    }

    fn set_follow(&self, on: bool) -> bool {
        self.follow.store(on, Ordering::Release);
        true
    }

    fn file_id(&self) -> Option<(u64, u128)> {
        sys::file_id(&self.file()).ok()
    }

    fn move_to(&self, copy: File) -> bool {
        if self.follow.load(Ordering::Acquire) {
            return false;
        }
        let old = std::mem::replace(
            &mut *self.file.write().unwrap_or_else(PoisonError::into_inner),
            copy,
        );
        self.moved.store(true, Ordering::Release);
        drop(old);
        true
    }
}
