//! Повтор записи в эмулятор (F03 §3.6): экран в момент `t`. Вперёд — следующие события;
//! перемотка — от ближайшего ключа, бюджет ≤50 мс для файла 10 МБ. Только вне UI-потока.

use std::io;
use std::sync::Arc;

use urfin_term::sys::conpty::PtySize;
use urfin_term::vt::grid::{Defaults, Snapshot};
use urfin_term::vt::shared::TermShared;

use super::parse::{Ev, Header, Lines, event};
use super::scan::{CastIndex, emulator};
use crate::source::ByteSource;

/// Прочитанное, но ещё не наступившее событие.
#[derive(Debug)]
enum Next {
    Out(String),
    Resize(u16, u16),
}

/// Плеер без часов.
#[derive(Debug)]
pub struct Replay {
    index: Arc<CastIndex>,
    term: Arc<TermShared>,
    lines: Lines,
    next: Option<(u64, Next)>,
    ms: u64,
}

impl Replay {
    pub fn new(src: Arc<dyn ByteSource>, h: &Header, index: Arc<CastIndex>) -> Replay {
        Replay {
            index,
            term: emulator(h.width, h.height),
            lines: Lines::new(src, h.body),
            next: None,
            ms: 0,
        }
    }

    pub fn ms(&self) -> u64 {
        self.ms
    }

    pub fn duration_ms(&self) -> u64 {
        self.index.duration_ms
    }

    /// Экран эмулятора.
    pub fn snapshot(&self, d: &Defaults) -> Snapshot {
        self.term.snapshot(d)
    }

    /// Экран к моменту `ms`. `true` — экран мог измениться.
    pub fn seek(&mut self, ms: u64) -> io::Result<bool> {
        let _span = tracing::debug_span!("viewer.cast.seek", ms).entered();
        let ms = ms.min(self.index.duration_ms);
        if ms == self.ms {
            return Ok(false);
        }
        let index = self.index.clone();
        let key = &index.keys[index.key_before(ms)];
        // Вперёд от места дальше ключа — ближе, чем от ключа.
        if ms < self.ms || self.lines.off() < key.off {
            let term = emulator(key.cols, key.rows);
            if let Some(b) = key.base.and_then(|b| index.keys.get(b as usize)) {
                term.inject(&b.vt);
                term.inject(b"\x1b[?1049h");
            }
            term.inject(&key.vt);
            self.term = term;
            self.lines.seek(key.off);
            self.next = None;
        }
        self.advance(ms)?;
        Ok(true)
    }

    /// Проигрывает события до `ms` включительно. `true` — экран мог измениться.
    pub fn advance(&mut self, ms: u64) -> io::Result<bool> {
        let mut any = false;
        if self.next.as_ref().is_some_and(|(t, _)| *t <= ms)
            && let Some((_, n)) = self.next.take()
        {
            self.apply(n);
            any = true;
        }
        while self.next.is_none() {
            let Some(line) = self.lines.next_line()? else {
                break;
            };
            let Some(ev) = event(line) else {
                continue;
            };
            let at = ev.ms;
            let n = match ev.ev {
                Ev::Out(s) => Next::Out(s.into_owned()),
                Ev::Resize(w, h) => Next::Resize(w, h),
                Ev::Other => continue,
            };
            if at > ms {
                self.next = Some((at, n));
            } else {
                self.apply(n);
                any = true;
            }
        }
        self.ms = ms;
        Ok(any)
    }

    fn apply(&self, n: Next) {
        match n {
            Next::Out(s) => self.term.inject(s.as_bytes()),
            Next::Resize(cols, rows) => {
                self.term.resize(PtySize {
                    cols: cols.max(1),
                    rows: rows.max(1),
                });
            }
        }
    }
}
