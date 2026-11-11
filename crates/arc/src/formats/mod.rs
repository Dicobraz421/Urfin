//! Форматы архивов (S8 §3.3): zip — S8-25, tar и компрессоры — S8-30; 7z — S8-32,
//! rar — S8-34 (feature `rar`). Здесь — общие читатели данных записи.

#[cfg(feature = "rar")]
pub mod rar;
pub(crate) mod relay;
pub mod sevenz;
pub mod tar;
pub mod zip;

use std::io::{self, Read, Seek, SeekFrom};

use urfin_vfs::VfsRead;

/// Порция потока данных записи.
pub(crate) const CHUNK: usize = 256 << 10;

/// Читает, пока `buf` не заполнен или поток не кончился.
pub(crate) fn read_up_to(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Пустой файл: данных нет.
pub(crate) struct Empty;

impl Read for Empty {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl Seek for Empty {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Ok(0)
    }
}

impl VfsRead for Empty {
    fn len(&self) -> Option<u64> {
        Some(0)
    }
}

/// Запись без сжатия: поддиапазон файла архива с `seek`.
pub(crate) struct Slice {
    r: Box<dyn VfsRead>,
    start: u64,
    len: u64,
    pos: u64,
    /// Позиция `r` не совпадает с `start + pos`.
    dirty: bool,
}

impl Slice {
    pub(crate) fn new(r: Box<dyn VfsRead>, start: u64, len: u64) -> Slice {
        Slice {
            r,
            start,
            len,
            pos: 0,
            dirty: true,
        }
    }
}

impl Read for Slice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        if self.dirty {
            self.r.seek(SeekFrom::Start(self.start + self.pos))?;
            self.dirty = false;
        }
        let want = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let n = self.r.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Slice {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        if to != self.pos {
            self.pos = to;
            self.dirty = true;
        }
        Ok(to)
    }
}

impl VfsRead for Slice {
    fn len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Данные записи порциями из канала: распаковка — в своём потоке (у `zip` и `tar`
/// читатель записи заимствует архив). Закрыли читателя — отправитель узнаёт это по ошибке
/// `send`. Порция — `Vec<u8>` или [`relay::Chunk`] (держит долю бюджета конвейера).
pub(crate) struct Pump<T = Vec<u8>> {
    rx: crossbeam_channel::Receiver<io::Result<T>>,
    cur: Option<T>,
    at: usize,
    pos: u64,
    len: u64,
    done: bool,
}

impl<T> Pump<T> {
    pub(crate) fn new(rx: crossbeam_channel::Receiver<io::Result<T>>, len: u64) -> Pump<T> {
        Pump {
            rx,
            cur: None,
            at: 0,
            pos: 0,
            len,
            done: false,
        }
    }
}

impl<T: AsRef<[u8]>> Read for Pump<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(cur) = &self.cur {
                let cur = cur.as_ref();
                if self.at < cur.len() {
                    let n = buf.len().min(cur.len() - self.at);
                    buf[..n].copy_from_slice(&cur[self.at..self.at + n]);
                    self.at += n;
                    self.pos += n as u64;
                    return Ok(n);
                }
            }
            if self.done || buf.is_empty() {
                return Ok(0);
            }
            // Прочитанная порция уходит до ожидания следующей: бюджет конвейера свободен.
            self.cur = None;
            match self.rx.recv() {
                Ok(Ok(v)) => {
                    self.cur = Some(v);
                    self.at = 0;
                }
                Ok(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                Err(_) => {
                    self.done = true;
                    return Ok(0);
                }
            }
        }
    }
}

impl<T> Seek for Pump<T> {
    /// Только «где я»: сжатая запись читается подряд.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        if to == Some(self.pos) {
            return Ok(self.pos);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "compressed entry is read sequentially",
        ))
    }
}

impl<T: AsRef<[u8]> + Send> VfsRead for Pump<T> {
    fn len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// Порции потока `r` в канал `tx`, пока поток не кончился. `false` — читателя закрыли
/// или чтение упало (ошибка ушла в канал).
pub(crate) fn pump_into(
    r: &mut dyn Read,
    tx: &crossbeam_channel::Sender<io::Result<Vec<u8>>>,
) -> bool {
    loop {
        let mut buf = vec![0u8; CHUNK];
        match read_up_to(r, &mut buf) {
            Ok(0) => return true,
            Ok(n) => {
                buf.truncate(n);
                if tx.send(Ok(buf)).is_err() {
                    return false;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
                return false;
            }
        }
    }
}
