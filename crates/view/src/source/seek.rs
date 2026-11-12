//! `Seek`: не-локальный VFS с `Caps::SEEK` (`VfsRead`: seek + read под мьютексом).
//!
//! Слежение (S7 §3.8, S7-46): источник, открытый [`SeekSource::with_origin`], умеет расти —
//! `refresh_len` спрашивает длину у backend'а (`Vfs::stat`) и после изменения открывает
//! ридер заново: открытый ридер backend'а может держать снимок на момент открытия.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use urfin_core::Location;
use urfin_vfs::{Vfs, VfsRead};

use super::{ByteSource, SourceKind, assert_off_ui, clamp};

pub struct SeekSource {
    r: Mutex<Box<dyn VfsRead>>,
    len: AtomicU64,
    follow: AtomicBool,
    /// Backend и адрес файла: длина и новый ридер для слежения. `None` — источник не
    /// растёт.
    origin: Option<(Arc<dyn Vfs>, Location)>,
}

impl std::fmt::Debug for SeekSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeekSource")
            .field("len", &self.len())
            .field("follow", &self.follow.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SeekSource {
    /// Длина — от ридера, иначе из `stat`, иначе seek в конец.
    pub fn new(mut r: Box<dyn VfsRead>, size: Option<u64>) -> io::Result<SeekSource> {
        let len = match r.len().or(size) {
            Some(n) => n,
            None => r.seek(SeekFrom::End(0))?,
        };
        Ok(SeekSource {
            r: Mutex::new(r),
            len: AtomicU64::new(len),
            follow: AtomicBool::new(false),
            origin: None,
        })
    }

    /// Источник файла `loc` backend'а `vfs`: умеет слежение ([`ByteSource::set_follow`]).
    #[must_use]
    pub fn with_origin(mut self, vfs: Arc<dyn Vfs>, loc: Location, follow: bool) -> SeekSource {
        self.origin = Some((vfs, loc));
        *self.follow.get_mut() = follow;
        self
    }
}

impl ByteSource for SeekSource {
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
        let mut r = self.r.lock().unwrap_or_else(PoisonError::into_inner);
        r.seek(SeekFrom::Start(off))?;
        let mut done = 0;
        while done < n {
            match r.read(&mut buf[done..n]) {
                Ok(0) => break,
                Ok(k) => done += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Seek
    }

    fn refresh_len(&self) -> io::Result<u64> {
        assert_off_ui();
        let Some((vfs, loc)) = &self.origin else {
            return Ok(self.len());
        };
        let size = vfs.stat(loc, true).map_err(io::Error::other)?.size;
        if size != self.len() {
            // Сначала ридер, потом длина: читатель не увидит новую длину со старым
            // ридером.
            let fresh = vfs.open_read(loc).map_err(io::Error::other)?;
            *self.r.lock().unwrap_or_else(PoisonError::into_inner) = fresh;
            self.len.store(size, Ordering::Release);
        }
        Ok(size)
    }

    fn set_follow(&self, on: bool) -> bool {
        if self.origin.is_none() {
            return false;
        }
        self.follow.store(on, Ordering::Release);
        true
    }
}
