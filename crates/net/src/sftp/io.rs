//! Чтение и запись файла SFTP (`open_read`/`open_write`): блоками по запросу. Докачка и
//! параллельные каналы передач — движок операций (F03 §4).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use urfin_vfs::{VfsError, VfsRead};

use super::session::{Session, is_eof, map_err};

/// Блок `READ`/`WRITE`: в пределах лимитов OpenSSH и russh-sftp.
const CHUNK: usize = 32 * 1024;

fn to_io(e: VfsError) -> io::Error {
    match e {
        VfsError::Io { source, .. } => source,
        VfsError::NotFound(_) => io::ErrorKind::NotFound.into(),
        VfsError::Denied(_) => io::ErrorKind::PermissionDenied.into(),
        e => io::Error::other(e.to_string()),
    }
}

/// Открытый на чтение файл.
#[derive(Debug)]
pub struct SftpReader {
    s: Arc<Session>,
    handle: Option<String>,
    len: Option<u64>,
    pos: u64,
    path: PathBuf,
}

impl SftpReader {
    pub(super) fn new(s: Arc<Session>, handle: String, len: Option<u64>, path: PathBuf) -> Self {
        SftpReader {
            s,
            handle: Some(handle),
            len,
            pos: 0,
            path,
        }
    }
}

impl io::Read for SftpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(h) = self.handle.clone() else {
            return Ok(0);
        };
        if buf.is_empty() {
            return Ok(0);
        }
        let n = u32::try_from(buf.len().min(CHUNK)).unwrap_or(u32::MAX);
        let (pos, path) = (self.pos, self.path.clone());
        let data = self
            .s
            .block(move |s| async move {
                match s.raw.read(h, pos, n).await {
                    Ok(d) => Ok(d.data),
                    Err(e) if is_eof(&e) => Ok(Vec::new()),
                    Err(e) => Err(map_err(&path, e)),
                }
            })
            .map_err(to_io)?;
        let k = data.len().min(buf.len());
        buf[..k].copy_from_slice(&data[..k]);
        self.pos += k as u64;
        Ok(k)
    }
}

impl io::Seek for SftpReader {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let base = match to {
            io::SeekFrom::Start(p) => {
                self.pos = p;
                return Ok(p);
            }
            io::SeekFrom::Current(d) => (self.pos, d),
            io::SeekFrom::End(d) => (
                self.len
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "size unknown"))?,
                d,
            ),
        };
        let p = base
            .0
            .checked_add_signed(base.1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        self.pos = p;
        Ok(p)
    }
}

impl VfsRead for SftpReader {
    fn len(&self) -> Option<u64> {
        self.len
    }
}

impl Drop for SftpReader {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let s = Arc::clone(&self.s);
            self.s.detach(async move {
                let _ = s.raw.close(h).await;
            });
        }
    }
}

/// Открытый на запись файл. [`SftpWriter::finish`] закрывает и сообщает ошибку закрытия;
/// без него файл закрывается в `Drop` без ожидания.
#[derive(Debug)]
pub struct SftpWriter {
    s: Arc<Session>,
    handle: Option<String>,
    pos: u64,
    path: PathBuf,
}

impl SftpWriter {
    pub(super) fn new(s: Arc<Session>, handle: String, path: PathBuf) -> Self {
        SftpWriter {
            s,
            handle: Some(handle),
            pos: 0,
            path,
        }
    }

    /// Закрыть файл и дождаться ответа сервера.
    pub fn finish(mut self) -> Result<(), VfsError> {
        let Some(h) = self.handle.take() else {
            return Ok(());
        };
        let path = self.path.clone();
        self.s.block(move |s| async move {
            s.raw
                .close(h)
                .await
                .map(drop)
                .map_err(|e| map_err(&path, e))
        })
    }
}

impl io::Write for SftpWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(h) = self.handle.clone() else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        let chunk = buf[..buf.len().min(CHUNK)].to_vec();
        let k = chunk.len();
        let (pos, path) = (self.pos, self.path.clone());
        self.s
            .block(move |s| async move {
                s.raw
                    .write(h, pos, chunk)
                    .await
                    .map(drop)
                    .map_err(|e| map_err(&path, e))
            })
            .map_err(to_io)?;
        self.pos += k as u64;
        Ok(k)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for SftpWriter {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let s = Arc::clone(&self.s);
            self.s.detach(async move {
                let _ = s.raw.close(h).await;
            });
        }
    }
}
