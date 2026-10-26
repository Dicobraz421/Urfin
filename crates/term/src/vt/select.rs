//! Выделение, прокрутка и мышь (S6-07, S6 §3.4).
//!
//! Мышь: протяжка — simple, двойной клик — semantic (слово до разделителей
//! alacritty), тройной — строка, Alt+протяжка — блок. Приложение включило отчёты мыши
//! (`DEC 1000/1002/1003`) — события уходят в PTY, а выделяет Shift+мышь. Колесо
//! листает скроллбэк; в alt-screen с `ALTERNATE_SCROLL` (1007) — стрелками.
//!
//! Координаты: окно присылает ячейку экрана (строка вида с 0 сверху, колонка), сетка
//! хранит строки относительно низа истории: `line = row − display_offset`.

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};

use super::input::{self, Modes, Mods, MouseButton};
use super::shared::TermShared;

/// Вид выделения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelKind {
    Simple,
    Semantic,
    Lines,
    Block,
}

impl SelKind {
    /// По числу кликов и Alt (S6 §3.4).
    pub fn of(clicks: u32, alt: bool) -> SelKind {
        match clicks {
            2 => SelKind::Semantic,
            n if n >= 3 => SelKind::Lines,
            _ if alt => SelKind::Block,
            _ => SelKind::Simple,
        }
    }

    fn ty(self) -> SelectionType {
        match self {
            SelKind::Simple => SelectionType::Simple,
            SelKind::Semantic => SelectionType::Semantic,
            SelKind::Lines => SelectionType::Lines,
            SelKind::Block => SelectionType::Block,
        }
    }
}

/// Ячейка экрана → точка сетки: строка вида с учётом прокрутки истории, колонка — в
/// пределах сетки.
pub fn grid_point(row: u16, col: u16, display_offset: usize, cols: usize) -> Point {
    let off = i32::try_from(display_offset).unwrap_or(i32::MAX);
    Point::new(
        Line(i32::from(row) - off),
        Column(usize::from(col).min(cols.saturating_sub(1))),
    )
}

/// Точка сетки → строка вида; `None` — вне экрана.
pub fn view_row(line: Line, display_offset: usize, rows: usize) -> Option<u16> {
    let r = i64::from(line.0) + i64::try_from(display_offset).ok()?;
    (0..i64::try_from(rows).ok()?)
        .contains(&r)
        .then(|| u16::try_from(r).ok())
        .flatten()
}

/// Выделенные ячейки по строкам вида: `(строка, первая колонка, последняя)`.
pub fn rows_of(
    sel: &SelectionRange,
    display_offset: usize,
    rows: usize,
    cols: usize,
) -> Vec<(u16, u16, u16)> {
    let last = u16::try_from(cols.saturating_sub(1)).unwrap_or(u16::MAX);
    let col = |c: Column| u16::try_from(c.0).unwrap_or(last).min(last);
    let mut out = Vec::new();
    for l in sel.start.line.0..=sel.end.line.0 {
        let Some(r) = view_row(Line(l), display_offset, rows) else {
            continue;
        };
        let (from, to) = if sel.is_block {
            (col(sel.start.column), col(sel.end.column))
        } else {
            (
                if l == sel.start.line.0 {
                    col(sel.start.column)
                } else {
                    0
                },
                if l == sel.end.line.0 {
                    col(sel.end.column)
                } else {
                    last
                },
            )
        };
        if from <= to {
            out.push((r, from, to));
        }
    }
    out
}

/// Событие мыши окна в ячейках экрана.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseIn {
    pub kind: MouseKind,
    /// Кнопка нажатия или отпускания; у движения — зажатая (`None` — без кнопок).
    pub button: Option<MouseButton>,
    pub row: u16,
    pub col: u16,
    /// Правая половина ячейки (граница выделения — после неё).
    pub right: bool,
    pub mods: Mods,
    /// Число кликов (двойной, тройной).
    pub clicks: u32,
    /// Колесо: строк, `+` — вверх (к истории).
    pub lines: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Move,
    Up,
    Wheel,
}

/// Что сделать с событием мыши.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseAction {
    /// Байты в PTY (отчёт мыши или стрелки колеса).
    Send(Vec<u8>),
    /// Начать выделение в ячейке.
    Select(SelKind),
    /// Протянуть выделение до ячейки.
    Extend,
    /// Прокрутить скроллбэк, `+` — вверх.
    Scroll(i32),
    None,
}

/// Решение по событию мыши — без состояния эмулятора, по режимам (табличный тест).
pub fn mouse_action(e: &MouseIn, m: Modes) -> MouseAction {
    // Отчёты приложению; Shift+мышь — всегда своё выделение (S6 §3.4).
    if m.mouse_report && !e.mods.shift {
        let mut out = Vec::new();
        let ev = |button: MouseButton, pressed: bool, motion: bool| input::MouseEvent {
            button,
            pressed,
            motion,
            col: e.col.saturating_add(1),
            row: e.row.saturating_add(1),
            mods: e.mods,
        };
        match e.kind {
            MouseKind::Down | MouseKind::Up => {
                if let Some(b) = e.button {
                    input::mouse(ev(b, e.kind == MouseKind::Down, false), m, &mut out);
                }
            }
            MouseKind::Move => match e.button {
                Some(b) if m.mouse_drag || m.mouse_motion => {
                    input::mouse(ev(b, true, true), m, &mut out);
                }
                None if m.mouse_motion => {
                    input::mouse(ev(MouseButton::None, true, true), m, &mut out);
                }
                _ => {}
            },
            MouseKind::Wheel => {
                let b = if e.lines > 0 {
                    MouseButton::WheelUp
                } else {
                    MouseButton::WheelDown
                };
                for _ in 0..e.lines.unsigned_abs().min(10) {
                    input::mouse(ev(b, true, false), m, &mut out);
                }
            }
        }
        return if out.is_empty() {
            MouseAction::None
        } else {
            MouseAction::Send(out)
        };
    }
    match e.kind {
        MouseKind::Wheel if e.lines == 0 => MouseAction::None,
        MouseKind::Wheel if m.alt_screen && m.alt_scroll => {
            let name = if e.lines > 0 { "up" } else { "down" };
            let mut out = Vec::new();
            for _ in 0..e.lines.unsigned_abs().min(30) {
                input::key(name, Mods::NONE, None, m, &mut out);
            }
            MouseAction::Send(out)
        }
        // В alt-screen истории нет — листать нечего.
        MouseKind::Wheel if m.alt_screen => MouseAction::None,
        MouseKind::Wheel => MouseAction::Scroll(e.lines),
        MouseKind::Down if e.button == Some(MouseButton::Left) => {
            MouseAction::Select(SelKind::of(e.clicks, e.mods.alt))
        }
        MouseKind::Move if e.button == Some(MouseButton::Left) => MouseAction::Extend,
        _ => MouseAction::None,
    }
}

/// Прокрутка скроллбэка.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollBy {
    /// Строк, `+` — вверх (к истории).
    Lines(i32),
    /// Страниц, `+` — вверх.
    Pages(i32),
    Bottom,
}

impl TermShared {
    fn point(&self, row: u16, col: u16) -> Point {
        let vt = self.vt.lock();
        let g = vt.term.grid();
        grid_point(row, col, g.display_offset(), g.columns())
    }

    /// Начать выделение в ячейке экрана.
    pub fn select_start(&self, row: u16, col: u16, right: bool, kind: SelKind) {
        let p = self.point(row, col);
        let side = if right { Side::Right } else { Side::Left };
        self.vt.lock().term.selection = Some(Selection::new(kind.ty(), p, side));
        self.mark_dirty();
    }

    /// Протянуть выделение до ячейки экрана.
    pub fn select_update(&self, row: u16, col: u16, right: bool) {
        let p = self.point(row, col);
        let side = if right { Side::Right } else { Side::Left };
        if let Some(s) = self.vt.lock().term.selection.as_mut() {
            s.update(p, side);
        }
        self.mark_dirty();
    }

    /// Выделить весь вывод: история и экран, по строкам.
    pub fn select_all(&self) {
        {
            let mut vt = self.vt.lock();
            let g = vt.term.grid();
            let top = Line(-i32::try_from(g.history_size()).unwrap_or(i32::MAX));
            let bottom = Line(i32::try_from(g.screen_lines()).unwrap_or(1) - 1);
            let last = Column(g.columns().saturating_sub(1));
            let mut s =
                Selection::new(SelectionType::Lines, Point::new(top, Column(0)), Side::Left);
            s.update(Point::new(bottom, last), Side::Right);
            vt.term.selection = Some(s);
        }
        self.mark_dirty();
    }

    /// Текст выделения; `None` — выделения нет или оно пустое.
    pub fn selection_text(&self) -> Option<String> {
        self.vt
            .lock()
            .term
            .selection_to_string()
            .filter(|t| !t.is_empty())
    }

    pub fn has_selection(&self) -> bool {
        self.vt
            .lock()
            .term
            .selection
            .as_ref()
            .is_some_and(|s| !s.is_empty())
    }

    /// Снять выделение; `true` — оно было.
    pub fn clear_selection(&self) -> bool {
        let had = self.vt.lock().term.selection.take().is_some();
        if had {
            self.mark_dirty();
        }
        had
    }

    /// Прокрутить скроллбэк.
    pub fn scroll(&self, by: ScrollBy) {
        let s = match by {
            ScrollBy::Lines(n) => Scroll::Delta(n),
            ScrollBy::Pages(n) if n > 0 => Scroll::PageUp,
            ScrollBy::Pages(_) => Scroll::PageDown,
            ScrollBy::Bottom => Scroll::Bottom,
        };
        let n = match by {
            ScrollBy::Pages(n) => n.unsigned_abs().max(1),
            _ => 1,
        };
        {
            let mut vt = self.vt.lock();
            for _ in 0..n.min(100) {
                vt.term.scroll_display(s);
            }
        }
        self.mark_dirty();
    }

    /// Насколько вид прокручен вверх от низа (строк).
    pub fn display_offset(&self) -> usize {
        self.vt.lock().term.grid().display_offset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vt::input::Modes;

    /// Приёмка S6-07 (тест конвертации координат): строка вида ↔ строка сетки с
    /// прокруткой, колонка — в пределах сетки.
    #[test]
    fn screen_to_grid_and_back() {
        assert_eq!(grid_point(0, 0, 0, 80), Point::new(Line(0), Column(0)));
        assert_eq!(grid_point(3, 5, 10, 80), Point::new(Line(-7), Column(5)));
        assert_eq!(grid_point(3, 500, 0, 80).column, Column(79));
        for off in [0usize, 1, 7, 100] {
            for row in [0u16, 5, 23] {
                let p = grid_point(row, 2, off, 80);
                assert_eq!(view_row(p.line, off, 24), Some(row));
            }
        }
        assert_eq!(view_row(Line(24), 0, 24), None);
        assert_eq!(view_row(Line(-1), 0, 24), None);
        assert_eq!(view_row(Line(-1), 1, 24), Some(0));
    }

    #[test]
    fn selection_rows_simple_and_block() {
        let p = |l, c| Point::new(Line(l), Column(c));
        let simple = SelectionRange::new(p(1, 4), p(3, 2), false);
        assert_eq!(
            rows_of(&simple, 0, 24, 10),
            vec![(1, 4, 9), (2, 0, 9), (3, 0, 2)]
        );
        let block = SelectionRange::new(p(1, 2), p(2, 5), true);
        assert_eq!(rows_of(&block, 0, 24, 10), vec![(1, 2, 5), (2, 2, 5)]);
        // Прокрутка на 2: строка −1 сетки — вторая строка вида; ушедшая вниз — нет.
        let scrolled = SelectionRange::new(p(-1, 0), p(22, 3), false);
        let r = rows_of(&scrolled, 2, 24, 10);
        assert_eq!(r.first(), Some(&(1, 0, 9)));
        assert_eq!(r.last(), Some(&(23, 0, 9)));
    }

    fn ev(kind: MouseKind, button: Option<MouseButton>, mods: Mods) -> MouseIn {
        MouseIn {
            kind,
            button,
            row: 4,
            col: 9,
            right: false,
            mods,
            clicks: 1,
            lines: 0,
        }
    }

    #[test]
    fn mouse_select_kinds() {
        let m = Modes::default();
        let alt = Mods {
            alt: true,
            ..Mods::NONE
        };
        let mut d = ev(MouseKind::Down, Some(MouseButton::Left), Mods::NONE);
        assert_eq!(mouse_action(&d, m), MouseAction::Select(SelKind::Simple));
        d.clicks = 2;
        assert_eq!(mouse_action(&d, m), MouseAction::Select(SelKind::Semantic));
        d.clicks = 3;
        assert_eq!(mouse_action(&d, m), MouseAction::Select(SelKind::Lines));
        let b = ev(MouseKind::Down, Some(MouseButton::Left), alt);
        assert_eq!(mouse_action(&b, m), MouseAction::Select(SelKind::Block));
        let mv = ev(MouseKind::Move, Some(MouseButton::Left), Mods::NONE);
        assert_eq!(mouse_action(&mv, m), MouseAction::Extend);
        let idle = ev(MouseKind::Move, None, Mods::NONE);
        assert_eq!(mouse_action(&idle, m), MouseAction::None);
    }

    /// Mouse mode: клики — отчётом SGR в PTY; Shift+мышь — выделение.
    #[test]
    fn mouse_mode_reports_and_shift_selects() {
        let m = Modes {
            mouse_report: true,
            mouse_drag: true,
            sgr_mouse: true,
            ..Modes::default()
        };
        let down = ev(MouseKind::Down, Some(MouseButton::Left), Mods::NONE);
        assert_eq!(
            mouse_action(&down, m),
            MouseAction::Send(b"\x1b[<0;10;5M".to_vec())
        );
        let up = ev(MouseKind::Up, Some(MouseButton::Left), Mods::NONE);
        assert_eq!(
            mouse_action(&up, m),
            MouseAction::Send(b"\x1b[<0;10;5m".to_vec())
        );
        let drag = ev(MouseKind::Move, Some(MouseButton::Left), Mods::NONE);
        assert_eq!(
            mouse_action(&drag, m),
            MouseAction::Send(b"\x1b[<32;10;5M".to_vec())
        );
        // Без DEC 1003 голое движение не шлётся.
        let idle = ev(MouseKind::Move, None, Mods::NONE);
        assert_eq!(mouse_action(&idle, m), MouseAction::None);
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        let sel = ev(MouseKind::Down, Some(MouseButton::Left), shift);
        assert_eq!(mouse_action(&sel, m), MouseAction::Select(SelKind::Simple));
        let mut wheel = ev(MouseKind::Wheel, None, Mods::NONE);
        wheel.lines = 2;
        assert_eq!(
            mouse_action(&wheel, m),
            MouseAction::Send(b"\x1b[<64;10;5M\x1b[<64;10;5M".to_vec())
        );
    }

    /// Колесо: скроллбэк; в alt-screen с 1007 — стрелки (`APP_CURSOR` — SS3).
    #[test]
    fn wheel_scrolls_or_sends_arrows() {
        let mut w = ev(MouseKind::Wheel, None, Mods::NONE);
        w.lines = 3;
        assert_eq!(mouse_action(&w, Modes::default()), MouseAction::Scroll(3));
        let alt = Modes {
            alt_screen: true,
            alt_scroll: true,
            app_cursor: true,
            ..Modes::default()
        };
        w.lines = -2;
        assert_eq!(
            mouse_action(&w, alt),
            MouseAction::Send(b"\x1bOB\x1bOB".to_vec())
        );
        let no_alt_scroll = Modes {
            alt_screen: true,
            ..Modes::default()
        };
        assert_eq!(mouse_action(&w, no_alt_scroll), MouseAction::None);
    }
}
