//! Чтение и запись файла SFTP (`open_read`/`open_write`) блоками с конвейером: до
//! [`WINDOW`] запросов в полёте, поэтому скорость не упирается в RTT (F03 §4.1). Докачка
//! (запись с байта `n`) и параллельные файлы — движок операций (F03 §4.2).

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot::Receiver;
use urfin_vfs::{VfsError, VfsRead};

use super::session::{Session, is_eof, map_err};

/// Блок `READ`/`WRITE`: в пределах лимитов OpenSSH и russh-sftp.
const CHUNK: usize = 32 * 1024;
/// Запросов в полёте на файл.
const WINDOW: usize = 16;

fn to_io(e: VfsError) -> io::Error {
    match e {
        VfsError::Io { source, .. } => source,
        VfsError::NotFound(_) => io::ErrorKind::NotFound.into(),
        VfsError::Denied(_) => io::ErrorKind::PermissionDenied.into(),
        e => io::Error::other(e.to_string()),
    }
}

/// Блок чтения в полёте: смещение, сколько просили, ответ.
type Pending = (u64, usize, Receiver<Result<Vec<u8>, VfsError>>);

/// Открытый на чтение файл: упреждающее чтение подряд, `Seek` сбрасывает очередь.
#[derive(Debug)]
pub struct SftpReader {
    s: Arc<Session>,
    handle: Option<String>,
    len: Option<u64>,
    pos: u64,
    path: PathBuf,
    /// Запросы вперёд от `pos`, по порядку.
    ahead: VecDeque<Pending>,
    /// Смещение следующего запроса.
    next: u64,
    /// Хвост прочитанного блока, ещё не отданный.
    buf: Vec<u8>,
    eof: bool,
}

impl SftpReader {
    pub(super) fn new(s: Arc<Session>, handle: String, len: Option<u64>, path: PathBuf) -> Self {
        SftpReader {
            s,
            handle: Some(handle),
            len,
            pos: 0,
            path,
            ahead: VecDeque::new(),
            next: 0,
            buf: Vec::new(),
            eof: false,
        }
    }

    fn reset(&mut self) {
        self.ahead.clear();
        self.buf.clear();
        self.next = self.pos;
        self.eof = false;
    }

    /// Дозаказать блоки до окна (за известным концом файла — не больше одного).
    fn fill(&mut self, h: &str) {
        while self.ahead.len() < WINDOW {
            if let Some(len) = self.len
                && self.next >= len
                && !self.ahead.is_empty()
            {
                break;
            }
            let (at, h, path) = (self.next, h.to_owned(), self.path.clone());
            let n = u32::try_from(CHUNK).unwrap_or(u32::MAX);
            let rx = self.s.start(move |s| async move {
                match s.raw.read(h, at, n).await {
                    Ok(d) => Ok(d.data),
                    Err(e) if is_eof(&e) => Ok(Vec::new()),
                    Err(e) => Err(map_err(&path, e)),
                }
            });
            self.ahead.push_back((at, CHUNK, rx));
            self.next += CHUNK as u64;
        }
    }
}

impl io::Read for SftpReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some(h) = self.handle.clone() else {
            return Ok(0);
        };
        if out.is_empty() {
            return Ok(0);
        }
        while self.buf.is_empty() {
            if self.eof {
                return Ok(0);
            }
            self.fill(&h);
            let Some((at, want, rx)) = self.ahead.pop_front() else {
                return Ok(0);
            };
            let data = self.s.wait(rx).map_err(to_io)?;
            if data.is_empty() {
                self.eof = true;
                self.ahead.clear();
                return Ok(0);
            }
            if data.len() < want {
                // Короткий ответ: следующие блоки заказаны не с того места.
                self.ahead.clear();
                self.next = at + data.len() as u64;
            }
            self.buf = data;
        }
        let k = self.buf.len().min(out.len());
        out[..k].copy_from_slice(&self.buf[..k]);
        self.buf.drain(..k);
        self.pos += k as u64;
        Ok(k)
    }
}

impl io::Seek for SftpReader {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let base = match to {
            io::SeekFrom::Start(p) => (p, 0),
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
        if p != self.pos {
            self.pos = p;
            self.reset();
        }
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

/// Открытый на запись файл: блоки уходят конвейером, ошибка видна на следующих `write`
/// или в [`SftpWriter::finish`] — он ждёт все блоки, делает `fsync@openssh.com` (если
/// сервер его объявил) и закрывает файл; без него файл закрывается в `Drop` без
/// ожидания.
#[derive(Debug)]
pub struct SftpWriter {
    s: Arc<Session>,
    handle: Option<String>,
    pos: u64,
    path: PathBuf,
    sent: VecDeque<Receiver<Result<(), VfsError>>>,
}

impl SftpWriter {
    pub(super) fn new(s: Arc<Session>, handle: String, path: PathBuf, pos: u64) -> Self {
        SftpWriter {
            s,
            handle: Some(handle),
            pos,
            path,
            sent: VecDeque::new(),
        }
    }

    /// Дождаться блоков в полёте, пока их больше `keep`.
    fn drain(&mut self, keep: usize) -> Result<(), VfsError> {
        while self.sent.len() > keep {
            if let Some(rx) = self.sent.pop_front() {
                self.s.wait(rx)?;
            }
        }
        Ok(())
    }

    /// Все блоки записаны; `fsync` (если есть) и закрыть файл с ответом сервера.
    pub fn finish(mut self) -> Result<(), VfsError> {
        let res = self.drain(0);
        let Some(h) = self.handle.take() else {
            return res;
        };
        if let Err(e) = res {
            let s = Arc::clone(&self.s);
            self.s.detach(async move {
                let _ = s.raw.close(h).await;
            });
            return Err(e);
        }
        let (path, fsync) = (self.path.clone(), self.s.fsync);
        self.s.block(move |s| async move {
            let err = |e| map_err(&path, e);
            if fsync {
                s.raw.fsync(h.clone()).await.map_err(err)?;
            }
            s.raw.close(h).await.map(drop).map_err(err)
        })
    }
}

impl io::Write for SftpWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(h) = self.handle.clone() else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        self.drain(WINDOW - 1).map_err(to_io)?;
        let chunk = buf[..buf.len().min(CHUNK)].to_vec();
        let k = chunk.len();
        let (pos, path) = (self.pos, self.path.clone());
        let rx = self.s.start(move |s| async move {
            s.raw
                .write(h, pos, chunk)
                .await
                .map(drop)
                .map_err(|e| map_err(&path, e))
        });
        self.sent.push_back(rx);
        self.pos += k as u64;
        Ok(k)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain(0).map_err(to_io)
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
