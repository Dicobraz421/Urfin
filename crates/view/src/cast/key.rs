//! Ключевой кадр: экран эмулятора как короткий поток VT (F03 §3.6). Повтор идёт не с
//! начала файла, а от ближайшего ключа: этот поток в новый эмулятор, затем события после
//! него. Держать копию сетки дороже в десятки раз, а `alacritty Term` не клонируется.
//!
//! Что поток не восстанавливает: область прокрутки DECSTBM и режимы (перенос, начало
//! координат); программы на альтернативном экране задают их при перерисовке, в обычном
//! выводе они не меняются.

use std::io::Write as _;

use urfin_term::vt::grid::{Cell, CursorKind, Defaults, Snapshot};

/// Цвета снимка для ключа: «по умолчанию» отличимы от любых цветов программы и
/// превращаются в SGR 39/49, а не в цвет темы.
pub(crate) const MARK: Defaults = Defaults {
    fg: [1, 2, 3],
    bg: [4, 5, 6],
    cursor: [7, 8, 9],
};

/// Начертание ячейки.
#[allow(clippy::struct_excessive_bools, reason = "независимые флаги SGR")]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: [u8; 3],
    bg: [u8; 3],
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

impl Pen {
    const PLAIN: Pen = Pen {
        fg: MARK.fg,
        bg: MARK.bg,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
    };

    fn of(c: &Cell) -> Pen {
        Pen {
            fg: c.fg,
            bg: c.bg,
            bold: c.bold,
            italic: c.italic,
            underline: c.underline,
            strike: c.strike,
        }
    }

    fn write(self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (on, code) in [
            (self.bold, b";1"),
            (self.italic, b";3"),
            (self.underline, b";4"),
            (self.strike, b";9"),
        ] {
            if on {
                out.extend_from_slice(code);
            }
        }
        if self.fg != MARK.fg {
            let [r, g, b] = self.fg;
            let _ = write!(out, ";38;2;{r};{g};{b}");
        }
        if self.bg != MARK.bg {
            let [r, g, b] = self.bg;
            let _ = write!(out, ";48;2;{r};{g};{b}");
        }
        out.push(b'm');
    }
}

fn blank(c: &Cell) -> bool {
    c.ch == ' ' && Pen::of(c) == Pen::PLAIN
}

/// Экран снимка (снятого с [`MARK`]) как поток VT.
pub(crate) fn synth(s: &Snapshot) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::from(s.cols) * usize::from(s.rows) / 2);
    out.extend_from_slice(b"\x1b[0m\x1b[2J");
    let mut pen = Pen::PLAIN;
    let mut utf = [0u8; 4];
    for (r, row) in s.cells.iter().enumerate() {
        let n = row.iter().rposition(|c| !blank(c)).map_or(0, |i| i + 1);
        if n == 0 {
            continue;
        }
        let _ = write!(out, "\x1b[{};1H", r + 1);
        for c in row[..n].iter().filter(|c| !c.spacer) {
            let p = Pen::of(c);
            if p != pen {
                p.write(&mut out);
                pen = p;
            }
            out.extend_from_slice(c.ch.encode_utf8(&mut utf).as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[0m");
    let (row, col) = s.cursor;
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
    match s.cursor_kind {
        CursorKind::Hidden => out.extend_from_slice(b"\x1b[?25l"),
        CursorKind::Block => out.extend_from_slice(b"\x1b[?25h\x1b[2 q"),
        CursorKind::Underline => out.extend_from_slice(b"\x1b[?25h\x1b[4 q"),
        CursorKind::Beam => out.extend_from_slice(b"\x1b[?25h\x1b[6 q"),
    }
    out
}
