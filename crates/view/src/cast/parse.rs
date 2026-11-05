//! Разбор asciicast v2 (F03 §3.5): заголовок, строки-события, потоковое чтение строк
//! файла без загрузки в память. Понимает и чужие записи (asciinema): дробный
//! `timestamp`, события других видов пропускаются, битая строка — не повод отказать.

use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use serde::Deserialize;

use crate::source::{ByteSource, read_full};

/// Блок чтения строк.
const BLOCK: usize = 64 << 10;

/// Заголовок записи.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u16,
    pub height: u16,
    /// Начало записи, секунды Unix.
    pub timestamp: Option<u64>,
    /// Профиль SSH или shell; в чужих записях может не быть.
    pub title: String,
    /// Смещение первого события.
    pub body: u64,
}

#[derive(Deserialize)]
struct RawHeader {
    version: u32,
    width: u16,
    height: u16,
    timestamp: Option<f64>,
    title: Option<String>,
}

/// Заголовок из начала файла; `None` — это не asciicast v2.
pub fn header(sample: &[u8]) -> Option<Header> {
    let end = memchr::memchr(b'\n', sample)?;
    let raw: RawHeader = serde_json::from_slice(&sample[..end]).ok()?;
    if raw.version != 2 || raw.width == 0 || raw.height == 0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "секунды Unix"
    )]
    let timestamp = raw.timestamp.filter(|t| *t >= 0.0).map(|t| t as u64);
    Some(Header {
        width: raw.width,
        height: raw.height,
        timestamp,
        title: raw.title.unwrap_or_default(),
        body: end as u64 + 1,
    })
}

/// Что произошло.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev<'a> {
    /// Вывод `"o"`.
    Out(Cow<'a, str>),
    /// Смена размера `"r"`: колонки, строки.
    Resize(u16, u16),
    /// Другой вид (`"i"`, `"m"`) — на экран не влияет.
    Other,
}

/// Событие: время от начала, мс.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub ms: u64,
    pub ev: Ev<'a>,
}

/// Событие из строки; `None` — строка битая (оборванная запись, мусор).
pub fn event(line: &[u8]) -> Option<Event<'_>> {
    let (t, kind, data): (f64, &str, Cow<'_, str>) = serde_json::from_slice(line).ok()?;
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "миллисекунды записи"
    )]
    let ms = (t * 1000.0).round() as u64;
    let ev = match kind {
        "o" => Ev::Out(data),
        "r" => {
            let (w, h) = data.split_once('x')?;
            Ev::Resize(w.parse().ok()?, h.parse().ok()?)
        }
        _ => Ev::Other,
    };
    Some(Event { ms, ev })
}

/// Строки файла потоком (только вне UI-потока).
#[derive(Debug)]
pub struct Lines {
    src: Arc<dyn ByteSource>,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    /// Смещение `buf[start]` в файле.
    pos: u64,
    eof: bool,
}

impl Lines {
    pub fn new(src: Arc<dyn ByteSource>, off: u64) -> Lines {
        Lines {
            src,
            buf: vec![0; BLOCK],
            start: 0,
            end: 0,
            pos: off,
            eof: false,
        }
    }

    /// Смещение следующей строки.
    pub fn off(&self) -> u64 {
        self.pos
    }

    /// Читать со смещения `off`.
    pub fn seek(&mut self, off: u64) {
        self.start = 0;
        self.end = 0;
        self.pos = off;
        self.eof = false;
    }

    /// Следующая строка без `\n`; `None` — конец файла.
    pub fn next_line(&mut self) -> io::Result<Option<&[u8]>> {
        loop {
            if let Some(i) = memchr::memchr(b'\n', &self.buf[self.start..self.end]) {
                let s = self.start;
                self.start += i + 1;
                self.pos += i as u64 + 1;
                return Ok(Some(&self.buf[s..s + i]));
            }
            if self.eof {
                if self.start == self.end {
                    return Ok(None);
                }
                let (s, e) = (self.start, self.end);
                self.pos += (e - s) as u64;
                self.start = e;
                return Ok(Some(&self.buf[s..e]));
            }
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if self.end == self.buf.len() {
                self.buf.resize(self.buf.len() * 2, 0);
            }
            let at = self.pos + self.end as u64;
            let n = read_full(&*self.src, at, &mut self.buf[self.end..])?;
            self.eof = n == 0;
            self.end += n;
        }
    }
}
