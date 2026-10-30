//! История просмотра: позиция, кодировка, режим и перенос по файлу (S7 §3.1 п. 3, №36).
//! S7-08 — модель в памяти: запись при закрытии, восстановление при открытии. Файл
//! `state_dir/view-history.json`, список Alt+F11 и записи редактора — S7-14.

use std::collections::VecDeque;

use compact_str::CompactString;
use urfin_core::Location;

use crate::layout::{Mode, WrapKind};

/// Что помнится о файле.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewMemo {
    pub loc: Location,
    /// Начало верхней строки (`ViewerDoc::top`).
    pub top: u64,
    pub hscroll: u32,
    /// Имя WHATWG кодировки, выбранной вручную; `None` — детект.
    pub enc: Option<CompactString>,
    pub mode: Mode,
    pub wrap: Option<WrapKind>,
    /// Документ Markdown: доля высоты верхнего блока ×10 000 (`top` — его начало, S7 §3.10).
    pub frac: u16,
}

/// Последние `max` файлов; новые — в конце.
#[derive(Clone, Debug)]
pub struct ViewHistory {
    items: VecDeque<ViewMemo>,
    max: usize,
}

impl Default for ViewHistory {
    fn default() -> Self {
        ViewHistory::new(500)
    }
}

impl ViewHistory {
    pub fn new(max: usize) -> ViewHistory {
        ViewHistory {
            items: VecDeque::new(),
            max: max.max(1),
        }
    }

    /// Запись о файле, если есть.
    pub fn get(&self, loc: &Location) -> Option<&ViewMemo> {
        self.items.iter().rev().find(|m| m.loc == *loc)
    }

    /// Запоминает (прежняя запись о файле уходит, новая — в конец).
    pub fn record(&mut self, memo: ViewMemo) {
        self.items.retain(|m| m.loc != memo.loc);
        self.items.push_back(memo);
        while self.items.len() > self.max {
            self.items.pop_front();
        }
    }

    /// От старых к новым.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ViewMemo> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memo(p: &str, top: u64) -> ViewMemo {
        ViewMemo {
            loc: Location::local(p),
            top,
            hscroll: 0,
            enc: None,
            mode: Mode::Text,
            wrap: None,
            frac: 0,
        }
    }

    #[test]
    fn record_replaces_and_trims() {
        let mut h = ViewHistory::new(2);
        h.record(memo(r"C:\a", 1));
        h.record(memo(r"C:\b", 2));
        h.record(memo(r"C:\a", 3));
        assert_eq!(h.get(&Location::local(r"C:\a")).map(|m| m.top), Some(3));
        h.record(memo(r"C:\c", 4));
        assert_eq!(h.len(), 2);
        assert!(h.get(&Location::local(r"C:\b")).is_none());
        let order: Vec<u64> = h.iter().map(|m| m.top).collect();
        assert_eq!(order, [3, 4]);
    }
}
