//! Блоки команд над сеткой (S6-21, best effort): прокрутка к строке буфера, текст
//! строк, поиск по regex в скроллбэке (alacritty `RegexSearch`), гиперссылка OSC 8 в
//! ячейке.
//!
//! Строки — «абсолютные» ([`crate::session::Mark::abs`]): от верха всего буфера. Пока
//! скроллбэк не насыщен, они стабильны; потом верхние уходят и метки старых промптов
//! сдвигаются — переходы по ним неточны (приёмка оговаривает).

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::search::{Match, RegexSearch};

use super::shared::TermShared;

impl TermShared {
    /// Абсолютная строка верхнего края вида.
    pub fn top_abs(&self) -> i64 {
        let vt = self.vt.lock();
        let g = vt.term.grid();
        i64::try_from(g.history_size()).unwrap_or(i64::MAX)
            - i64::try_from(g.display_offset()).unwrap_or(0)
    }

    /// Прокрутить так, чтобы абсолютная строка `abs` была вверху вида (насколько
    /// позволяет буфер).
    pub fn scroll_to_abs(&self, abs: i64) {
        {
            let mut vt = self.vt.lock();
            let g = vt.term.grid();
            let hist = i64::try_from(g.history_size()).unwrap_or(0);
            let want = (hist - abs).clamp(0, hist);
            let now = i64::try_from(g.display_offset()).unwrap_or(0);
            let delta = i32::try_from(want - now).unwrap_or(0);
            if delta != 0 {
                vt.term.scroll_display(Scroll::Delta(delta));
            }
        }
        self.mark_dirty();
    }

    /// Текст абсолютных строк `from..to` (хвостовые пробелы и пустые строки в конце
    /// убраны).
    pub fn rows_text(&self, from: i64, to: i64) -> String {
        let vt = self.vt.lock();
        let g = vt.term.grid();
        let hist = i64::try_from(g.history_size()).unwrap_or(0);
        let screen = i64::try_from(g.screen_lines()).unwrap_or(0);
        let cols = g.columns();
        let mut out: Vec<String> = Vec::new();
        for abs in from.max(0)..to {
            let line = abs - hist;
            if line < -hist || line >= screen {
                continue;
            }
            let Ok(l) = i32::try_from(line) else {
                continue;
            };
            let row = &g[Line(l)];
            let text: String = (0..cols).map(|c| row[Column(c)].c).collect();
            out.push(text.trim_end().to_owned());
        }
        while out.last().is_some_and(String::is_empty) {
            out.pop();
        }
        out.join("\r\n")
    }

    /// Следующее совпадение `regex` после начала прошлого `from` (или от низа буфера — назад, от верха —
    /// вперёд); найденное выделяется и прокручивается в вид.
    pub fn find(&self, regex: &mut RegexSearch, back: bool, from: Option<Point>) -> Option<Match> {
        let found = {
            let mut vt = self.vt.lock();
            let term = &mut vt.term;
            let g = term.grid();
            let last = Column(g.columns().saturating_sub(1));
            let top = Line(-i32::try_from(g.history_size()).unwrap_or(0));
            let bottom = Line(i32::try_from(g.screen_lines()).unwrap_or(1) - 1);
            let (origin, dir, side) = match (from, back) {
                // Начало прошлого совпадения: шаг в сторону поиска, чтобы не найти его же.
                (Some(p), true) => (p.sub(term, Boundary::None, 1), Direction::Left, Side::Left),
                (Some(p), false) => (
                    p.add(term, Boundary::None, 1),
                    Direction::Right,
                    Side::Right,
                ),
                (None, true) => (Point::new(bottom, last), Direction::Left, Side::Left),
                (None, false) => (Point::new(top, Column(0)), Direction::Right, Side::Right),
            };
            let m = term.search_next(regex, origin, dir, side, None)?;
            let mut sel = alacritty_terminal::selection::Selection::new(
                alacritty_terminal::selection::SelectionType::Simple,
                *m.start(),
                Side::Left,
            );
            sel.update(*m.end(), Side::Right);
            term.selection = Some(sel);
            term.scroll_to_point(*m.start());
            m
        };
        self.mark_dirty();
        Some(found)
    }

    /// Гиперссылка OSC 8 в ячейке экрана (строка вида, колонка).
    pub fn hyperlink_at(&self, row: u16, col: u16) -> Option<String> {
        let vt = self.vt.lock();
        let g = vt.term.grid();
        if usize::from(col) >= g.columns() {
            return None;
        }
        let p = super::select::grid_point(row, col, g.display_offset(), g.columns());
        g[p].hyperlink().map(|h| h.uri().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use crate::session::{SessionEvent, SessionOpts, SessionSink, parse_inline};
    use crate::sys::conpty::PtySize;

    use super::*;

    struct Quiet;
    impl SessionSink for Quiet {
        fn poke(&self) {}
        fn event(&self, _ev: SessionEvent) {}
    }

    fn opts() -> SessionOpts {
        SessionOpts {
            size: PtySize { cols: 40, rows: 5 },
            scrollback: 1000,
            osc52: "off".into(),
            nonce: None,
        }
    }

    #[test]
    fn rows_scroll_find_and_links() {
        let mut text = String::new();
        for i in 0..30 {
            let _ = write!(text, "line {i}\r\n");
        }
        text.push_str("\x1b]8;;file:///C:/x.txt\x1b\\link\x1b]8;;\x1b\\ end");
        let sh = parse_inline(text.as_bytes(), &opts(), &Quiet);
        assert_eq!(sh.rows_text(3, 5), "line 3\r\nline 4");
        sh.scroll_to_abs(10);
        assert_eq!(sh.top_abs(), 10);
        let mut re = RegexSearch::new("line 2[0-9]").unwrap();
        let m = sh.find(&mut re, true, None).unwrap();
        assert!(sh.selection_text().unwrap().starts_with("line 29"), "{m:?}");
        let m2 = sh.find(&mut re, true, Some(*m.start())).unwrap();
        assert!(m2.start() < m.start());
        sh.scroll(super::super::select::ScrollBy::Bottom);
        let rows = 5u16;
        assert_eq!(
            sh.hyperlink_at(rows - 1, 1).as_deref(),
            Some("file:///C:/x.txt")
        );
        assert_eq!(sh.hyperlink_at(rows - 1, 6), None);
    }
}
