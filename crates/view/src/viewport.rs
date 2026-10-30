//! Видимая область и прокрутка документа вьювера (S7-07, решение 2026-10-30
//! «viewer-viewport»): UI шлёт размер окна событием `InboxMsg::Viewport`, handler по нему
//! и по `viewer.scroll` двигает позицию, выводит совпадение на треть экрана и просит
//! видимые страницы у кэша. Раскладка — по резидентным страницам: чего нет — просим и
//! повторяем после `PagesMsg` (без I/O в UI-потоке).

use std::ops::Range;

use urfin_core::Runtime;

use super::ViewerDoc;
use crate::cache::{Missing, PAGE, Prio};
use crate::layout::{LayoutSpec, Mode, row_up, rows_down};

/// Окно до первого `Viewport`.
const DEFAULT_VIEW: (u32, u32) = (80, 25);

/// Прокрутка `viewer.scroll dir= n=`: строки и страницы — `n` раз, горизонталь — на `n`
/// колонок. В Hex строка — `hex_cols` байт, горизонтали нет.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scroll {
    Up(u32),
    Down(u32),
    PageUp(u32),
    PageDown(u32),
    Home,
    End,
    Left(u32),
    Right(u32),
}

impl Scroll {
    /// Из аргументов команды: `dir` — `up|down|page_up|page_down|home|end|left|right`.
    pub fn parse(dir: &str, n: Option<i64>) -> Option<Scroll> {
        let n = n.map_or(1, |n| u32::try_from(n.max(1)).unwrap_or(u32::MAX));
        Some(match dir {
            "up" => Scroll::Up(n),
            "down" => Scroll::Down(n),
            "page_up" => Scroll::PageUp(n),
            "page_down" => Scroll::PageDown(n),
            "home" => Scroll::Home,
            "end" => Scroll::End,
            "left" => Scroll::Left(n),
            "right" => Scroll::Right(n),
            _ => return None,
        })
    }
}

fn byte_range(m: &Missing) -> Range<u64> {
    m.pages.start * PAGE..m.pages.end * PAGE
}

impl ViewerDoc {
    /// Колонки и строки окна (до первого `Viewport` — 80×25).
    pub fn viewport(&self) -> (u32, u32) {
        if self.view_cols == 0 || self.view_rows == 0 {
            DEFAULT_VIEW
        } else {
            (self.view_cols, self.view_rows)
        }
    }

    /// Раскладка по окну из `Viewport`.
    pub fn view_layout(&self) -> LayoutSpec {
        self.layout(self.viewport().0)
    }

    /// `InboxMsg::Viewport`: размер окна. `false` — тот же.
    pub fn set_viewport(&mut self, cols: u32, rows: u32, rt: &Runtime) -> bool {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.view_cols, self.view_rows) {
            return false;
        }
        self.view_cols = cols;
        self.view_rows = rows;
        self.settle(rt);
        self.rev += 1;
        true
    }

    /// `viewer.scroll`. `true` — позиция сменилась (или ждёт страниц).
    pub fn scroll(&mut self, s: Scroll, rt: &Runtime) -> bool {
        if !self.is_ready() {
            return false;
        }
        // Прокрутка пользователя важнее совпадения, ждущего вывода.
        self.reveal = None;
        let changed = self.scroll_now(s).unwrap_or_else(|_| {
            self.pending = Some(s);
            true
        });
        self.settle(rt);
        if changed {
            self.rev += 1;
        }
        changed
    }

    /// Отложенное (прокрутка, совпадение) — если страницы есть; затем просьба видимых
    /// страниц и запуск loader'а. Зовут handler и `Msg` — не кадр UI.
    pub(crate) fn settle(&mut self, rt: &Runtime) {
        if !self.is_ready() || self.realign {
            // `realign_top` уже попросил свои страницы.
            self.pages.pump(rt, self.id, &self.cancel);
            return;
        }
        let mut need = None;
        if let Some(s) = self.pending.take() {
            match self.scroll_now(s) {
                Ok(true) => self.rev += 1,
                Ok(false) => {}
                Err(m) => {
                    self.pending = Some(s);
                    need = Some(m);
                }
            }
        }
        if need.is_none() && self.reveal.is_some() {
            need = self.apply_reveal().err();
        }
        let want = match need {
            Some(m) => byte_range(&m),
            None => self.visible_range(),
        };
        self.pages.want(want, Prio::Visible);
        self.pages.pump(rt, self.id, &self.cancel);
    }

    /// Байты видимых строк; на месте непрочитанного — его страницы.
    fn visible_range(&self) -> Range<u64> {
        let (_, rows) = self.viewport();
        let spec = self.view_layout();
        let pos = self.pos(&spec);
        match rows_down(&self.pages, pos, rows as usize, &spec) {
            Ok(r) => match (r.first(), r.last()) {
                (Some(a), Some(z)) => a.start.min(pos.0)..z.next.max(a.start + 1),
                _ => pos.0..pos.0 + 1,
            },
            Err(m) => byte_range(&m),
        }
    }

    /// Прокрутка по резидентным страницам. `Err` — нужных страниц нет (позиция прежняя).
    fn scroll_now(&mut self, s: Scroll) -> Result<bool, Missing> {
        let (_, rows) = self.viewport();
        let spec = self.view_layout();
        let before = (self.top, self.top_sub, self.hscroll);
        let hex = spec.mode == Mode::Hex;
        match s {
            Scroll::Left(n) => self.hscroll = self.hscroll.saturating_sub(n),
            Scroll::Right(n) if !hex && self.wrap.is_none() => {
                self.hscroll = self.hscroll.saturating_add(n).min(self.max_line);
            }
            Scroll::Right(_) => {}
            _ if hex => self.scroll_hex(s, &spec, rows),
            Scroll::Home => (self.top, self.top_sub) = (0, 0),
            Scroll::Up(n) => self.rows_up(n, &spec)?,
            Scroll::PageUp(n) => self.rows_up(n.saturating_mul(rows), &spec)?,
            Scroll::Down(n) => self.rows_down_by(n, rows, &spec)?,
            Scroll::PageDown(n) => self.rows_down_by(n.saturating_mul(rows), rows, &spec)?,
            Scroll::End => self.scroll_end(rows, &spec)?,
        }
        Ok(before != (self.top, self.top_sub, self.hscroll))
    }

    fn rows_up(&mut self, n: u32, spec: &LayoutSpec) -> Result<(), Missing> {
        let mut p = self.pos(spec);
        for _ in 0..n {
            if p == (0, 0) {
                break;
            }
            p = row_up(&self.pages, p, spec)?;
        }
        (self.top, self.top_sub) = p;
        Ok(())
    }

    /// Вниз на `n` строк, но не дальше «последняя строка — внизу экрана».
    fn rows_down_by(&mut self, n: u32, rows: u32, spec: &LayoutSpec) -> Result<(), Missing> {
        let p = self.pos(spec);
        let r = rows_down(
            &self.pages,
            p,
            (n as usize).saturating_add(rows as usize),
            spec,
        )?;
        let rows = rows as usize;
        if r.len() <= rows {
            return Ok(());
        }
        let k = (n as usize).min(r.len() - rows);
        (self.top, self.top_sub) = r[k].pos();
        Ok(())
    }

    /// Конец: последняя строка документа — внизу экрана.
    fn scroll_end(&mut self, rows: u32, spec: &LayoutSpec) -> Result<(), Missing> {
        let len = self.pages.len();
        let mut p = (len, 0);
        for _ in 0..rows {
            if p == (0, 0) {
                break;
            }
            p = row_up(&self.pages, p, spec)?;
        }
        if p.0 >= len {
            p = (0, 0);
        }
        (self.top, self.top_sub) = p;
        Ok(())
    }

    /// Hex: строка — `hex_cols` байт, точный конец без индекса.
    fn scroll_hex(&mut self, s: Scroll, spec: &LayoutSpec, rows: u32) {
        let c = u64::from(spec.hex_cols.max(1));
        let rows = u64::from(rows);
        let cur = self.top / c * c;
        let max_top = self.pages.len().div_ceil(c).saturating_sub(rows) * c;
        let down = |k: u64| {
            cur.saturating_add(k.saturating_mul(c))
                .min(max_top.max(cur))
        };
        let up = |k: u64| cur.saturating_sub(k.saturating_mul(c));
        self.top = match s {
            Scroll::Up(n) => up(u64::from(n)),
            Scroll::Down(n) => down(u64::from(n)),
            Scroll::PageUp(n) => up(u64::from(n) * rows),
            Scroll::PageDown(n) => down(u64::from(n) * rows),
            Scroll::Home => 0,
            Scroll::End => max_top,
            Scroll::Left(_) | Scroll::Right(_) => cur,
        };
        self.top_sub = 0;
    }

    /// Совпадение `reveal` — на треть экрана от верха (`top` уже у начала его строки).
    fn apply_reveal(&mut self) -> Result<(), Missing> {
        let Some(at) = self.reveal else {
            return Ok(());
        };
        let (_, rows) = self.viewport();
        let spec = self.view_layout();
        let k = rows / 3;
        if spec.mode == Mode::Hex {
            let c = u64::from(spec.hex_cols.max(1));
            self.top = (at / c).saturating_sub(u64::from(k)) * c;
            self.top_sub = 0;
        } else {
            let mut p = (self.top, 0);
            for _ in 0..k {
                if p == (0, 0) {
                    break;
                }
                p = row_up(&self.pages, p, &spec)?;
            }
            (self.top, self.top_sub) = p;
        }
        self.reveal = None;
        self.rev += 1;
        Ok(())
    }
}
