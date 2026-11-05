//! Трекер Diff (ADR-0024 §8): грамматика построчная, и роль строки — от её начала, поэтому
//! кандидат — каждая строка. Уровни для свёртки: `diff …` и `@@ …`.

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use super::{L_GROUP, Track};
use crate::syntax::lex::{Cx, Lex, Map, St};

pub(crate) struct Diff {
    t: Track,
    /// Первые единицы строки (до 5), `n` — сколько.
    head: u64,
    n: u8,
    /// Уровень `diff`, `@@` открыт.
    file: bool,
    hunk: bool,
}

impl Diff {
    pub(crate) fn resume(st: St) -> Diff {
        Diff {
            t: Track::resume(&st),
            head: st.x,
            n: st.a,
            file: st.d & 1 != 0,
            hunk: st.d & 2 != 0,
        }
    }

    /// Начало строки известно: уровни.
    fn line(&mut self, at: u64, cx: &mut Cx<'_>) {
        let h = self.head.to_be_bytes();
        let start = &h[8 - usize::from(self.n)..];
        if start.starts_with(b"diff ") {
            if self.hunk {
                self.hunk = false;
                self.t.pop(cx, at);
            }
            if self.file {
                self.t.pop(cx, at);
            }
            self.file = true;
            self.t.push_bare(cx, at, L_GROUP);
        } else if start.starts_with(b"@@") {
            if self.hunk {
                self.t.pop(cx, at);
            }
            self.hunk = true;
            self.t.push_bare(cx, at, L_GROUP);
        }
    }
}

impl Lex for Diff {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            x: self.head,
            a: self.n,
            // Начало строки — назад на прочитанные единицы.
            b: if self.n < 5 { u16::from(self.n) } else { 0 },
            d: u8::from(self.file) | u8::from(self.hunk) << 1,
            ..St::default()
        };
        self.t.save(&mut st, at);
        st
    }

    fn can_mark(&self) -> bool {
        true
    }

    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if b.is_empty() {
            return;
        }
        self.t.begin(m.raw(0), m.unit());
        let mut i = 0;
        while i < b.len() {
            if self.n == 0 {
                // Первая единица строки: граница — после точки на начале строки, а не до неё.
                self.t.line_at = m.raw(i);
                self.t.cand(self.t.line_at, cx);
            }
            if self.n < 5 {
                let c = b[i];
                if c == b'\n' {
                    self.n = 6;
                } else {
                    self.head = self.head << 8 | u64::from(c);
                    self.n += 1;
                    i += 1;
                    if self.n == 5 {
                        self.line(self.t.line_at, cx);
                        self.n = 6;
                    }
                    continue;
                }
            }
            match memchr::memchr(b'\n', &b[i..]) {
                None => return,
                Some(k) => {
                    self.head = 0;
                    self.n = 0;
                    i += k + 1;
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof {
            if self.hunk {
                self.hunk = false;
                self.t.pop(cx, end);
            }
            if self.file {
                self.file = false;
                self.t.pop(cx, end);
            }
        }
    }
}
