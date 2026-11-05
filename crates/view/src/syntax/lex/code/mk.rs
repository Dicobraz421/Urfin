//! Трекер Make (ADR-0024 §8): построчно. Кандидат — строка с нулевой колонки, начатая не
//! комментарием и не `else`, `endif`, `endef`, вне `define`, строка-продолжение (`\`) ничего
//! не начинает. Уровни: `ifeq`/`ifdef` (заголовок — из файла), `define`, рецепт правила
//! (свёртка).

#![allow(
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт; флаги — состояние"
)]

use super::{L_COND, L_OTHER, L_ZONE, Track, pk, word_key};
use crate::syntax::lex::{Cx, Lex, Map, St};

/// Начало строки: пробелы и первое слово.
const HEAD: u8 = 0;
/// Остаток строки.
const REST: u8 = 1;
/// `\` в остатке строки.
const REST_ESC: u8 = 2;
const REST_ESC_CR: u8 = 3;
/// Строка-комментарий.
const COMMENT: u8 = 4;
const COMMENT_ESC: u8 = 5;

pub(crate) struct Mk {
    t: Track,
    mode: u8,
    /// Единиц от начала строки.
    col: u16,
    /// Первый значимый байт — не в нулевой колонке.
    indented: bool,
    tab: bool,
    /// Прошлая строка продолжается на этой.
    cont: bool,
    /// Внутри `define`.
    def: bool,
    /// Рецепт открыт (уровень свёртки).
    recipe: bool,
    /// Директива в заголовке строки: 1 — условие, 2 — `else`, 3 — `endif`, 4 — `define`,
    /// 5 — `endef`.
    dir: u8,
    /// Начало последнего элемента (заголовок свёртки рецепта).
    item_back: u64,
    item_at: u64,
    fresh: bool,
    word: u64,
    wlen: u32,
}

impl Mk {
    pub(crate) fn resume(st: St) -> Mk {
        Mk {
            t: Track::resume(&st),
            mode: st.mode,
            col: st.b,
            indented: st.d & 1 != 0,
            tab: st.d & 2 != 0,
            cont: st.d & 4 != 0,
            def: st.d & 8 != 0,
            recipe: st.d & 16 != 0,
            dir: st.a,
            item_back: st.x,
            item_at: 0,
            fresh: true,
            word: 0,
            wlen: 0,
        }
    }

    /// Первое слово строки кончилось (или его нет).
    fn head_end(&mut self, c: u8, at: u64, end: u64, cx: &mut Cx<'_>) {
        const IFEQ: u64 = pk("ifeq");
        const IFNEQ: u64 = pk("ifneq");
        const IFDEF: u64 = pk("ifdef");
        const IFNDEF: u64 = pk("ifndef");
        const ELSE: u64 = pk("else");
        const ENDIF: u64 = pk("endif");
        const DEFINE: u64 = pk("define");
        const ENDEF: u64 = pk("endef");
        let w = word_key(self.word, self.wlen);
        let cont = std::mem::take(&mut self.cont);
        let word_end = self.wlen > 0 && !matches!(c, b':' | b'=');
        self.dir = match w {
            IFEQ | IFNEQ | IFDEF | IFNDEF if word_end => 1,
            ELSE if word_end => 2,
            ENDIF if word_end => 3,
            DEFINE if word_end => 4,
            ENDEF if word_end => 5,
            _ => 0,
        };
        if cont {
            self.dir = 0;
            self.mode = REST;
            return;
        }
        if self.def && self.dir != 5 {
            self.dir = 0;
            self.mode = REST;
            return;
        }
        if self.tab {
            // Рецепт: заголовок свёртки — правило выше.
            if !self.recipe && self.t.bad == 0 {
                self.recipe = true;
                self.t.push(cx, self.item_at, self.item_at, L_ZONE);
            }
            self.dir = 0;
            self.mode = REST;
            return;
        }
        if self.recipe && c != b'#' {
            self.recipe = false;
            self.t.pop(cx, self.t.line_at);
        }
        if c == b'#' {
            self.mode = COMMENT;
            return;
        }
        let line = self.t.line_at;
        if !self.indented && !matches!(self.dir, 2 | 3 | 5) && self.t.bad == 0 {
            self.t.cand(line, cx);
            self.item_at = line;
        }
        let _ = (at, end);
        match self.dir {
            2 => {
                if self.t.bad == 0 && cx.depth() > 0 {
                    self.t.bad = cx.depth();
                }
            }
            3 => self.t.unwind_cond(cx, line, true),
            4 => {
                self.def = true;
                self.t.push(cx, line, line, L_OTHER);
            }
            5 if std::mem::take(&mut self.def) => self.t.pop(cx, line),
            _ => {}
        }
        self.mode = REST;
    }

    /// Конец логической строки (перевод строки без `\`).
    fn line_end(&mut self, end: u64, cx: &mut Cx<'_>) {
        if std::mem::take(&mut self.dir) == 1 {
            let at = self.t.line_at;
            self.t.push(cx, at, end, L_COND);
        }
    }

    fn newline(&mut self, next: u64) {
        self.t.line_at = next;
        self.mode = HEAD;
        self.col = 0;
        self.indented = false;
        self.tab = false;
        self.word = 0;
        self.wlen = 0;
    }
}

impl Lex for Mk {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let mut st = St {
            mode: self.mode,
            a: self.dir,
            b: self.col,
            d: u8::from(self.indented)
                | u8::from(self.tab) << 1
                | u8::from(self.cont) << 2
                | u8::from(self.def) << 3
                | u8::from(self.recipe) << 4,
            x: at.saturating_sub(self.item_at),
            ..St::default()
        };
        self.t.save(&mut st, at);
        st
    }

    fn can_mark(&self) -> bool {
        self.wlen == 0
    }

    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if b.is_empty() {
            return;
        }
        let at0 = m.raw(0);
        self.t.begin(at0, m.unit());
        if std::mem::take(&mut self.fresh) {
            self.item_at = at0.saturating_sub(self.item_back);
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            match self.mode {
                HEAD => match c {
                    b' ' if self.wlen == 0 => {
                        self.indented = true;
                        self.col = self.col.saturating_add(1);
                        i += 1;
                    }
                    b'\t' if self.wlen == 0 && !self.indented => {
                        self.tab = true;
                        self.col = self.col.saturating_add(1);
                        i += 1;
                    }
                    b'a'..=b'z' => {
                        self.word = self.word << 8 | u64::from(c);
                        self.wlen += 1;
                        self.col = self.col.saturating_add(1);
                        i += 1;
                    }
                    b'\n' | b'\r' if self.wlen == 0 && !self.cont => {
                        // Пустая строка.
                        if c == b'\n' {
                            self.newline(m.raw(i + 1));
                        }
                        i += 1;
                    }
                    _ => {
                        self.head_end(c, m.raw(i), m.raw(i + 1), cx);
                        self.word = 0;
                        self.wlen = 0;
                    }
                },
                REST => match memchr::memchr2(b'\\', b'\n', &b[i..]) {
                    None => i = b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'\\' {
                            self.mode = REST_ESC;
                        } else {
                            self.line_end(m.raw(j + 1), cx);
                            self.newline(m.raw(j + 1));
                        }
                        i = j + 1;
                    }
                },
                REST_ESC => {
                    i += 1;
                    self.mode = match c {
                        b'\r' => REST_ESC_CR,
                        b'\n' => {
                            self.cont = true;
                            let dir = self.dir;
                            self.newline(m.raw(i));
                            self.dir = dir;
                            HEAD
                        }
                        _ => REST,
                    };
                }
                REST_ESC_CR => {
                    self.mode = REST_ESC;
                }
                COMMENT => match memchr::memchr2(b'\\', b'\n', &b[i..]) {
                    None => i = b.len(),
                    Some(k) => {
                        let j = i + k;
                        if b[j] == b'\\' {
                            self.mode = COMMENT_ESC;
                        } else {
                            self.newline(m.raw(j + 1));
                        }
                        i = j + 1;
                    }
                },
                _ => {
                    self.mode = COMMENT;
                    i += 1;
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof && self.recipe {
            self.recipe = false;
            self.t.pop(cx, end);
        }
    }
}
