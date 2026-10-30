//! `Seek`: не-локальный VFS с `Caps::SEEK` (`VfsRead`: seek + read под мьютексом).

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Mutex, PoisonError};

use urfin_vfs::VfsRead;

use super::{ByteSource, SourceKind, assert_off_ui, clamp};

pub struct SeekSource {
    r: Mutex<Box<dyn VfsRead>>,
    len: u64,
}

impl std::fmt::Debug for SeekSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeekSource")
            .field("len", &self.len)
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
            len,
        })
    }
}

impl ByteSource for SeekSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn is_final(&self) -> bool {
        true
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        assert_off_ui();
        let n = clamp(off, buf.len(), self.len);
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
        Ok(self.len)
    }
}
