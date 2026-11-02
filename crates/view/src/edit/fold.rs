//! Свёртка в редакторе (S7-35, ADR-0023 §7, S7 §5.7): свёрнутое — слой отображения, не
//! правка. [`Folds`] — свёрнутые диапазоны документа (строки целиком, по возрастанию, без
//! пересечений); раскладка экрана их пропускает ([`rows_down`], [`row_up`] поверх
//! `layout`). Места свёртки — узлы дерева tree-sitter на видимой части
//! (`syntax::edit::folds`). Правка внутри скрытого и курсор в нём разворачивают, правка
//! выше сдвигает.

use std::ops::Range;

use smallvec::SmallVec;

use crate::cache::{ByteView, Missing};
use crate::layout::{self, LayoutSpec, VRow};

/// Свёрнутое: начало строки-заголовка (видна) и скрытые строки `hide` (от начала строки
/// после заголовка до начала строки после последней скрытой или конца документа).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    pub head: u64,
    pub hide: Range<u64>,
}

/// Свёрнутые диапазоны документа.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folds {
    list: Vec<Fold>,
}

impl Folds {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Fold> {
        self.list.iter()
    }

    /// Свёрнутое, скрывающее `off`.
    pub fn hidden(&self, off: u64) -> Option<&Fold> {
        let i = self.list.partition_point(|f| f.hide.end <= off);
        self.list.get(i).filter(|f| f.hide.start <= off)
    }

    /// Свёрнутое с заголовком на строке `head`.
    pub fn at_head(&self, head: u64) -> Option<&Fold> {
        self.list.iter().find(|f| f.head == head)
    }

    /// Свернуть: свёрнутое внутри поглощается.
    pub fn fold(&mut self, f: Fold) {
        if f.hide.is_empty() || f.head >= f.hide.start {
            return;
        }
        self.list
            .retain(|g| g.hide.end <= f.hide.start || g.hide.start >= f.hide.end);
        let i = self.list.partition_point(|g| g.hide.start < f.hide.start);
        self.list.insert(i, f);
    }

    /// Развернуть заголовок `head`; `true` — был свёрнут.
    pub fn unfold(&mut self, head: u64) -> bool {
        let n = self.list.len();
        self.list.retain(|f| f.head != head);
        n != self.list.len()
    }

    /// Курсор в скрытом — развернуть его; `true` — что-то развернулось.
    pub fn reveal(&mut self, off: u64) -> bool {
        let n = self.list.len();
        self.list
            .retain(|f| !(f.hide.start <= off && off < f.hide.end));
        n != self.list.len()
    }

    /// Всё развернуть (новое дерево кусков: отмена массовой правки, перевод кодировки).
    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Правка документа: на `at` убрано `old` байт, вставлено `new`. Задевает скрытое
    /// (или шов строки заголовка с ним, или начало заголовка) — развернуть; выше — сдвиг.
    pub fn edit(&mut self, at: u64, old: u64, new: u64) {
        if self.list.is_empty() {
            return;
        }
        let end = at + old;
        let shift = |x: u64| x - old + new;
        self.list.retain_mut(|f| {
            let inside = if old == 0 {
                f.hide.start <= at && at < f.hide.end
            } else {
                at < f.hide.end && end >= f.hide.start
            };
            if inside || (at < f.head && end > f.head) {
                return false;
            }
            if end <= f.hide.start {
                f.hide = shift(f.hide.start)..shift(f.hide.end);
                if end <= f.head && !(old == 0 && at == f.head) {
                    f.head = shift(f.head);
                }
            }
            true
        });
    }
}

/// `n` визуальных строк вниз с `from` (как `layout::rows_down`), скрытое пропускается;
/// `from` внутри скрытого — с последнего ряда заголовка.
pub fn rows_down(
    view: &(impl ByteView + ?Sized),
    from: (u64, u32),
    n: usize,
    spec: &LayoutSpec,
    folds: &Folds,
) -> Result<SmallVec<[VRow; 64]>, Missing> {
    if folds.is_empty() {
        return layout::rows_down(view, from, n, spec);
    }
    let mut out = SmallVec::new();
    let mut pos = from;
    if let Some(h) = folds.hidden(pos.0) {
        pos = layout::row_up(view, (h.hide.start, 0), spec)?;
    }
    while out.len() < n {
        let rows = layout::rows_down(view, pos, n - out.len(), spec)?;
        let mut jump = None;
        for r in &rows {
            out.push(*r);
            if let Some(h) = folds.hidden(r.next).filter(|h| h.hide.start == r.next) {
                jump = Some(h.hide.end);
                break;
            }
        }
        match jump {
            Some(j) if j < view.len() => pos = (j, 0),
            _ => break,
        }
    }
    Ok(out)
}

/// Позиция визуальной строки над `at` (как `layout::row_up`), скрытое пропускается.
pub fn row_up(
    view: &(impl ByteView + ?Sized),
    at: (u64, u32),
    spec: &LayoutSpec,
    folds: &Folds,
) -> Result<(u64, u32), Missing> {
    let up = layout::row_up(view, at, spec)?;
    match folds.hidden(up.0) {
        Some(h) => layout::row_up(view, (h.hide.start, 0), spec),
        None => Ok(up),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folds(list: &[(u64, Range<u64>)]) -> Folds {
        let mut f = Folds::default();
        for (head, hide) in list {
            f.fold(Fold {
                head: *head,
                hide: hide.clone(),
            });
        }
        f
    }

    #[test]
    fn hidden_and_nesting() {
        let mut f = folds(&[(0, 4..20), (30, 34..40)]);
        assert_eq!(f.hidden(4).map(|x| x.head), Some(0));
        assert_eq!(f.hidden(19).map(|x| x.head), Some(0));
        assert!(f.hidden(20).is_none());
        assert!(f.hidden(3).is_none());
        // Внешнее поглощает вложенное.
        f.fold(Fold {
            head: 25,
            hide: 28..50,
        });
        assert_eq!(f.iter().count(), 2);
        assert!(f.at_head(30).is_none());
        assert!(f.unfold(25));
        assert!(!f.unfold(25));
        assert!(f.reveal(10));
        assert!(f.is_empty());
    }

    #[test]
    fn edits_shift_or_unfold() {
        // Вставка выше — сдвиг; в строке заголовка — сдвиг скрытого, заголовок на месте.
        let mut f = folds(&[(10, 14..20)]);
        f.edit(2, 0, 3);
        assert_eq!(f.at_head(13).map(|x| x.hide.clone()), Some(17..23));
        f.edit(14, 0, 1);
        assert_eq!(f.at_head(13).map(|x| x.hide.clone()), Some(18..24));
        // Вставка в начало строки после скрытого — не задевает.
        f.edit(24, 0, 5);
        assert!(f.at_head(13).is_some());
        // Удаление перевода строки заголовка — разворачивает.
        f.edit(17, 1, 0);
        assert!(f.is_empty());
        // Вставка внутрь — разворачивает; удаление через начало заголовка — тоже.
        let mut f = folds(&[(10, 14..20)]);
        f.edit(15, 0, 1);
        assert!(f.is_empty());
        let mut f = folds(&[(10, 14..20)]);
        f.edit(8, 3, 0);
        assert!(f.is_empty());
    }
}
