//! Переход, «назад» и закладки документа вьювера (S7 §3.5, карточка S7-10).
//!
//! `viewer.goto` разбирает [`crate::goto::parse`]; смещения и проценты применяются сразу,
//! строка — по индексу: метка не дальше 1024 строк до неё, дальше счёт разделителей по
//! кэшу страниц ([`crate::index::goto_line`]). Индекс ещё не дошёл до строки — ждём его
//! (`goto_wait`, прогресс в статусе, Esc отменяет); нужных страниц нет — просим их и
//! повторяем после `PagesMsg`. Перед переходом позиция кладётся в `back`.

use urfin_core::Runtime;

use super::ViewerDoc;
use crate::cache::Missing;
use crate::goto::Goto;
use crate::index::{LineGuess, Sep, goto_line};
use crate::layout::{Mode, line_start_before};

/// Позиций в `back` — не больше (старые уходят).
pub const BACK_MAX: usize = 16;
/// Закладок (Ctrl+Shift+0…9).
pub const BOOKMARKS: usize = 10;
/// Страниц, которые переход просит за раз.
const GOTO_PAGES: u64 = 16;

/// Переход, ждущий индекса или страниц.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GotoWait {
    /// Строка с 0.
    Line(u64),
    /// Сдвиг на `n` строк от строки, начинающейся в `from`.
    Rel { from: u64, n: i64 },
}

impl ViewerDoc {
    /// `viewer.goto`: смещение и процент — сразу, строка — когда хватает индекса.
    pub fn goto(&mut self, g: Goto, rt: &Runtime) {
        self.goto_wait = None;
        self.goto_done = false;
        if !self.is_ready() {
            return;
        }
        let hex = self.mode == Mode::Hex;
        match g {
            Goto::Percent(p) => {
                let off = u128::from(self.len) * u128::from(p) / 10_000;
                self.jump_offset(u64::try_from(off).unwrap_or(u64::MAX));
            }
            Goto::Offset(o) => self.jump_offset(o),
            Goto::Number { n, .. } if hex => self.jump_offset(n),
            Goto::Rel(k) if hex => {
                let cur = self.sel.as_ref().map_or(self.top, |s| s.start);
                self.jump_offset(cur.saturating_add_signed(k));
            }
            Goto::Number { n, .. } => self.goto_wait = Some(GotoWait::Line(n.saturating_sub(1))),
            Goto::Rel(n) => {
                self.goto_wait = Some(GotoWait::Rel { from: self.top, n });
            }
        }
        self.settle(rt);
        self.rev += 1;
    }

    /// Esc при ожидании индекса: переход отменяется. `false` — ничего не ждали.
    pub fn goto_cancel(&mut self) -> bool {
        let was = self.goto_wait.take().is_some();
        if was {
            self.rev += 1;
        }
        was
    }

    /// Переход состоялся после ожидания (сбрасывает признак): строка перехода закрывается.
    pub fn take_goto_done(&mut self) -> bool {
        std::mem::take(&mut self.goto_done)
    }

    /// Текущая позиция — в `back` (подряд одинаковые не копятся).
    fn push_back(&mut self) {
        let p = self.top;
        if self.back.last() == Some(&p) {
            return;
        }
        if self.back.len() >= BACK_MAX {
            self.back.remove(0);
        }
        self.back.push(p);
    }

    /// Позиция `pos` (текст — к началу строки по кэшу, Hex — строка с курсором).
    fn place(&mut self, pos: u64) {
        self.reveal = None;
        self.pending = None;
        self.top_sub = 0;
        let last = self.len.saturating_sub(1);
        let pos = pos.min(last);
        if self.mode == Mode::Hex {
            let (_, rows) = self.viewport();
            let c = u64::from(self.view_layout().hex_cols.max(1));
            let max_top = self.len.div_ceil(c).saturating_sub(u64::from(rows)) * c;
            self.top = (pos / c * c).min(max_top);
            self.sel = Some(pos..pos);
            self.realign = false;
        } else {
            self.top = pos;
            self.sel = None;
            self.realign_top();
        }
        self.rev += 1;
    }

    fn jump_offset(&mut self, off: u64) {
        self.push_back();
        self.place(off);
        self.goto_done = true;
    }

    /// Начало строки `pos` уже известно точно.
    fn jump_line(&mut self, pos: u64) {
        self.push_back();
        self.reveal = None;
        self.pending = None;
        self.sel = None;
        self.top = pos;
        self.top_sub = 0;
        self.realign = false;
        self.goto_wait = None;
        self.goto_done = true;
        self.rev += 1;
    }

    /// Ждущий переход: `Err` — нужны страницы; `Ok` — перешли или ждём индекс.
    pub(super) fn try_goto(&mut self) -> Result<(), Missing> {
        let Some(w) = self.goto_wait else {
            return Ok(());
        };
        let sep = Sep::of(&self.enc, self.eol);
        let line = match w {
            GotoWait::Line(n) => n,
            GotoWait::Rel { from, n } => {
                if from > self.index.upto && !self.index.done {
                    return Ok(());
                }
                match self
                    .index
                    .line_of_in(&self.pages, from, sep)
                    .map_err(widen)?
                {
                    LineGuess::Exact(k) => k.saturating_add_signed(n),
                    LineGuess::Approx(_) => return Ok(()),
                }
            }
        };
        self.goto_wait = Some(GotoWait::Line(line));
        let ix = &self.index;
        let line = if ix.done { line.min(ix.lines) } else { line };
        if line > ix.lines {
            // Индекс ещё не дошёл: ждём `IndexMsg`.
            return Ok(());
        }
        let Some(mut pos) =
            goto_line(&self.pages, ix.mark_before_line(line), line, sep).map_err(widen)?
        else {
            self.goto_wait = None;
            return Ok(());
        };
        if pos >= self.len && line > 0 {
            // Пустая «строка» после последнего разделителя — переход на последнюю.
            pos =
                line_start_before(&self.pages, self.len - 1, &self.view_layout()).map_err(widen)?;
        }
        self.jump_line(pos);
        Ok(())
    }

    /// `viewer.back`: прошлая позиция. `false` — возвращаться некуда.
    pub fn back(&mut self, rt: &Runtime) -> bool {
        if !self.is_ready() {
            return false;
        }
        let Some(p) = self.back.pop() else {
            return false;
        };
        self.goto_wait = None;
        self.place(p);
        if self.mode == Mode::Hex {
            self.sel = None;
        }
        self.settle(rt);
        true
    }

    /// Ctrl+Shift+N: закладка на текущей позиции.
    pub fn bookmark_set(&mut self, n: usize) {
        if let Some(b) = self.bookmarks.get_mut(n) {
            *b = Some(self.top);
            self.rev += 1;
        }
    }

    /// Ctrl+N: к закладке. `false` — закладки нет.
    pub fn bookmark_go(&mut self, n: usize, rt: &Runtime) -> bool {
        let Some(p) = self.bookmarks.get(n).copied().flatten() else {
            return false;
        };
        if !self.is_ready() {
            return false;
        }
        self.goto_wait = None;
        self.push_back();
        self.place(p);
        if self.mode == Mode::Hex {
            self.sel = None;
        }
        self.settle(rt);
        true
    }
}

/// Переход просит страницы пачкой: счёт от метки идёт подряд.
fn widen(Missing { pages }: Missing) -> Missing {
    Missing {
        pages: pages.start..pages.end.max(pages.start + GOTO_PAGES),
    }
}
