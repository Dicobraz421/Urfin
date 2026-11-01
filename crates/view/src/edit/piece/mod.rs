//! Движок редактора: куски на персистентном B+-дереве (ADR-0023 §2, §3, §6). Карточка
//! S7-26.
//!
//! Документ — последовательность [`Piece`] поверх источников: исходный файл (`Original`),
//! буфер правок (`Add`, [`AddBuf`]) и временные файлы массовых правок (`Rewrite(n)`).
//! Байты — в кодировке документа, смещения — байтовые (у UTF-16 чётные). Дерево [`Tree`]
//! держит узлы в `Arc`: снимок ([`TextSnap`]) — клон корня, правка копирует только путь.
//! Без gpui и без I/O на UI-потоке: [`TextView`] читает только кэши страниц, `TextSnap`
//! и [`TextHay`] — источники (вне UI).

mod add;
mod text;
mod tree;

#[cfg(test)]
pub(crate) mod tests;

use smallvec::SmallVec;

pub use add::{AddBuf, AddSnap, BLOCK, MEM_MAX, SpillSource};
pub use text::{TextDoc, TextHay, TextSnap, TextView};
pub use tree::{INNER_MAX, Iter, LEAF_MAX, Sums, Tree};

/// Кусок не длиннее этого: известное число переводов строк помещается в `u32`, длина
/// чётная (UTF-16 не рвётся). Исходник больше — несколько кусков.
pub const PIECE_MAX: u64 = 1 << 31;

/// Источник байт куска.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Src {
    /// Исходный файл (после BOM).
    Original,
    /// Буфер правок: только дописывание.
    Add,
    /// Временный файл результата массовой правки («Заменить все», перевод кодировки).
    Rewrite(u16),
}

/// Переводов строк в куске (`Sep` документа): известно или ещё нет (индекс не дошёл,
/// страниц не было при разрезании).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lf {
    Known(u32),
    Unknown,
}

/// Кусок документа: `len` байт источника `src` с `start`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    pub src: Src,
    pub start: u64,
    pub len: u64,
    pub lf: Lf,
}

/// Куски правки: обычно один-два.
pub type Pieces = SmallVec<[Piece; 2]>;

/// Правка дерева: на `at` убраны `removed`, вставлены `inserted` (журнал отмены, S7-28).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Splice {
    pub at: u64,
    pub removed: Pieces,
    pub inserted: Pieces,
}

/// Переводов строк в диапазоне источника; `None` — не узнать без чтения (страниц нет).
pub type CountLf<'a> = &'a dyn Fn(Src, std::ops::Range<u64>) -> Option<u32>;

impl Piece {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }

    /// Делит кусок на `[0, at)` и `[at, len)`; переводы строк — счётом меньшей части.
    pub fn split(&self, at: u64, count: CountLf<'_>) -> (Piece, Piece) {
        debug_assert!(
            at > 0 && at < self.len,
            "Piece::split: {at} of {}",
            self.len
        );
        let mut left = Piece { len: at, ..*self };
        let mut right = Piece {
            start: self.start + at,
            len: self.len - at,
            ..*self
        };
        if let Lf::Known(n) = self.lf {
            let mid = self.start + at;
            let (a, b) = if at <= self.len - at {
                match count(self.src, self.start..mid) {
                    Some(a) if a <= n => (a, n - a),
                    _ => (u32::MAX, u32::MAX),
                }
            } else {
                match count(self.src, mid..self.end()) {
                    Some(b) if b <= n => (n - b, b),
                    _ => (u32::MAX, u32::MAX),
                }
            };
            if a == u32::MAX {
                left.lf = Lf::Unknown;
                right.lf = Lf::Unknown;
            } else {
                left.lf = Lf::Known(a);
                right.lf = Lf::Known(b);
            }
        }
        (left, right)
    }

    /// Смежные куски одного источника с непрерывными диапазонами — один кусок (куски
    /// `Add` не переходят границу блока, длина — не больше [`PIECE_MAX`]).
    pub fn join(&self, b: &Piece) -> Option<Piece> {
        if self.src != b.src || self.end() != b.start || self.len + b.len > PIECE_MAX {
            return None;
        }
        if self.src == Src::Add && self.start / BLOCK != (b.end() - 1) / BLOCK {
            return None;
        }
        let lf = match (self.lf, b.lf) {
            (Lf::Known(x), Lf::Known(y)) => Lf::Known(x.checked_add(y)?),
            (Lf::Unknown, Lf::Unknown) => Lf::Unknown,
            _ => return None,
        };
        Some(Piece {
            len: self.len + b.len,
            lf,
            ..*self
        })
    }
}

/// Длина кусков в байтах.
pub fn bytes_of(p: &[Piece]) -> u64 {
    p.iter().map(|p| p.len).sum()
}
