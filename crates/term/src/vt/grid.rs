//! Снапшот сетки для рендера (S6-06, S6 §2): видимые строки с цветами в RGB (палитра
//! xterm 256 + цвета терминала из OSC 4/10/11), флагами и курсором. Берётся под
//! замком на время копирования строк; UI рисует из снапшота, не держа замок.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use super::shared::TermShared;

/// Цвета по умолчанию, если тема терминала не задала свои: (текст, фон, курсор).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Defaults {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
}

/// Ячейка снапшота: флаги начертания — плоские, как их читает отрисовка.
#[allow(clippy::struct_excessive_bools, reason = "независимые флаги SGR")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    pub ch: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Широкий символ (две ячейки); следующая — заполнитель с `ch = ' '` и `spacer`.
    pub wide: bool,
    pub spacer: bool,
}

/// Вид курсора (DECSCUSR).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CursorKind {
    Block,
    Beam,
    Underline,
    Hidden,
}

/// Видимый экран.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Vec<Cell>>,
    /// (строка, колонка) курсора на экране.
    pub cursor: (u16, u16),
    pub cursor_kind: CursorKind,
    pub cursor_color: [u8; 3],
    /// Фон экрана (для полей вокруг сетки).
    pub bg: [u8; 3],
    /// Выделение по строкам экрана: `(строка, первая колонка, последняя)` (S6-07).
    pub selection: Vec<(u16, u16, u16)>,
}

/// Стандартные 16 цветов xterm.
const ANSI: [[u8; 3]; 16] = [
    [0x0c, 0x0c, 0x0c],
    [0xc5, 0x0f, 0x1f],
    [0x13, 0xa1, 0x0e],
    [0xc1, 0x9c, 0x00],
    [0x00, 0x37, 0xda],
    [0x88, 0x17, 0x98],
    [0x3a, 0x96, 0xdd],
    [0xcc, 0xcc, 0xcc],
    [0x76, 0x76, 0x76],
    [0xe7, 0x48, 0x56],
    [0x16, 0xc6, 0x0c],
    [0xf9, 0xf1, 0xa5],
    [0x3b, 0x78, 0xff],
    [0xb4, 0x00, 0x9e],
    [0x61, 0xd6, 0xd6],
    [0xf2, 0xf2, 0xf2],
];

/// Цвет палитры 256: 0–15 — ANSI, 16–231 — куб 6×6×6, 232–255 — серые.
pub fn indexed(i: u8) -> [u8; 3] {
    match i {
        0..=15 => ANSI[usize::from(i)],
        16..=231 => {
            let v = i - 16;
            let step = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
            [step(v / 36), step((v / 6) % 6), step(v % 6)]
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            [g, g, g]
        }
    }
}

fn rgb(c: Rgb) -> [u8; 3] {
    [c.r, c.g, c.b]
}

fn resolve(c: Color, colors: &Colors, d: &Defaults, bold: bool) -> [u8; 3] {
    match c {
        Color::Spec(c) => rgb(c),
        Color::Indexed(i) => {
            // Жирный — яркий вариант первых восьми (как xterm).
            let i = if bold && i < 8 { i + 8 } else { i };
            colors[usize::from(i)].map_or_else(|| indexed(i), rgb)
        }
        Color::Named(n) => {
            if let Some(c) = colors[n] {
                return rgb(c);
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => d.fg,
                NamedColor::Background => d.bg,
                NamedColor::Cursor => d.cursor,
                NamedColor::DimForeground => d.fg.map(|v| (u16::from(v) * 2 / 3) as u8),
                other => {
                    let i = other as usize;
                    if i < 16 {
                        let i = if bold && i < 8 { i + 8 } else { i };
                        ANSI[i]
                    } else {
                        d.fg
                    }
                }
            }
        }
    }
}

/// Ячейка эмулятора → ячейка снапшота: цвета в RGB, инверсия, тусклый, скрытый.
fn cell(c: &alacritty_terminal::term::cell::Cell, colors: &Colors, d: &Defaults) -> Cell {
    let f = c.flags;
    let bold = f.contains(Flags::BOLD);
    let mut fg = resolve(c.fg, colors, d, bold);
    let mut bg = resolve(c.bg, colors, d, false);
    if f.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if f.contains(Flags::DIM) {
        fg = fg.map(|v| (u16::from(v) * 2 / 3) as u8);
    }
    if f.contains(Flags::HIDDEN) {
        fg = bg;
    }
    Cell {
        ch: c.c,
        fg,
        bg,
        bold,
        italic: f.contains(Flags::ITALIC),
        underline: f.intersects(Flags::ALL_UNDERLINES),
        strike: f.contains(Flags::STRIKEOUT),
        wide: f.contains(Flags::WIDE_CHAR),
        spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
    }
}

impl TermShared {
    /// Строк в истории сейчас (для перевода меток в строки экрана).
    pub fn history_size(&self) -> usize {
        self.vt.lock().term.grid().history_size()
    }

    /// Последняя строка промпта между метками `a` (`A`) и `b` (`B`): от колонки `A`
    /// (если промпт в одну строку) до колонки `B`. `None` — строка ушла из истории.
    pub fn prompt_cells(
        &self,
        a: crate::session::Mark,
        b: crate::session::Mark,
        d: &Defaults,
    ) -> Option<Vec<Cell>> {
        use alacritty_terminal::index::{Column, Line};
        let vt = self.vt.lock();
        let term = &vt.term;
        let g = term.grid();
        let hist = i64::try_from(g.history_size()).ok()?;
        let line = i64::from(b.history) + i64::from(b.line) - hist;
        let top = -hist;
        if line < top || line >= i64::try_from(g.screen_lines()).ok()? {
            return None;
        }
        let from = if (a.history, a.line) == (b.history, b.line) {
            usize::from(a.col)
        } else {
            0
        };
        let to = usize::from(b.col).min(g.columns());
        let row = &g[Line(i32::try_from(line).ok()?)];
        let colors = term.colors();
        Some(
            (from..to)
                .map(|c| cell(&row[Column(c)], colors, d))
                .collect(),
        )
    }

    /// Снапшот видимого экрана (с учётом прокрутки истории).
    pub fn snapshot(&self, d: &Defaults) -> Snapshot {
        let vt = self.vt.lock();
        let term = &vt.term;
        let content = term.renderable_content();
        let (cols, rows) = (term.columns(), term.screen_lines());
        let colors = content.colors;
        let mut cells = vec![Vec::with_capacity(cols); rows];
        let offset = i32::try_from(content.display_offset).unwrap_or(0);
        for ic in content.display_iter {
            let line = ic.point.line.0 + offset;
            let Ok(r) = usize::try_from(line) else {
                continue;
            };
            if r >= rows {
                continue;
            }
            cells[r].push(cell(ic.cell, colors, d));
        }
        let cur = content.cursor;
        let cursor_kind = match cur.shape {
            CursorShape::Beam => CursorKind::Beam,
            CursorShape::Underline => CursorKind::Underline,
            CursorShape::Hidden => CursorKind::Hidden,
            _ => CursorKind::Block,
        };
        let cursor_line = cur.point.line.0 + offset;
        let selection = content
            .selection
            .map(|r| super::select::rows_of(&r, content.display_offset, rows, cols))
            .unwrap_or_default();
        Snapshot {
            cols: u16::try_from(cols).unwrap_or(u16::MAX),
            rows: u16::try_from(rows).unwrap_or(u16::MAX),
            cells,
            cursor: (
                u16::try_from(cursor_line.max(0)).unwrap_or(0),
                u16::try_from(cur.point.column.0).unwrap_or(0),
            ),
            cursor_kind,
            cursor_color: colors[NamedColor::Cursor].map_or(d.cursor, rgb),
            bg: colors[NamedColor::Background].map_or(d.bg, rgb),
            selection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_256() {
        assert_eq!(indexed(1), [0xc5, 0x0f, 0x1f]);
        assert_eq!(indexed(16), [0, 0, 0]);
        assert_eq!(indexed(231), [255, 255, 255]);
        assert_eq!(indexed(196), [255, 0, 0]);
        assert_eq!(indexed(232), [8, 8, 8]);
        assert_eq!(indexed(255), [238, 238, 238]);
    }
}
