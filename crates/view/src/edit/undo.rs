//! Отмена и повтор — журнал операций над кусками (ADR-0023 §4). Карточка S7-28.
//!
//! Операция не копирует текст: `Splice` хранит куски (они ссылаются на `Original`/`Add`),
//! `Root` — корни дерева до и после массовой правки («Заменить все», `editor.eol`,
//! перевод кодировки — с ней). Группа — операции и выделение до и после; новая группа —
//! пауза ≥ 1 с, переход курсора ([`Journal::break_group`]), смена вида (ввод ↔
//! удаление), команда правки ([`Journal::begin_group`]: все её операции — одна группа).
//! Подряд идущие правки группы сливаются в одну операцию (набор, Backspace, Del, режим
//! замены): серия нажатий — O(1) памяти. Память — по объёму, потолок 16 МБ: сверх —
//! старые группы отбрасываются. «Изменён» — позиция в журнале не та, что при сохранении.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::piece::{Piece, Pieces, Splice, Src, TextDoc, Tree, bytes_of};
use crate::enc::Enc;

/// Пауза ввода, после которой — новая группа.
pub const PAUSE: Duration = Duration::from_secs(1);
/// Потолок памяти журнала.
pub const CAP: usize = 16 << 20;

/// Операция журнала.
#[derive(Clone, Debug)]
pub enum Op {
    /// На `at` убраны `removed`, вставлены `inserted`.
    Splice {
        at: u64,
        removed: Pieces,
        inserted: Pieces,
    },
    /// Дерево целиком; `enc` — кодировка до и после (перевод кодировки).
    Root {
        before: Tree,
        after: Tree,
        enc: Option<(Enc, Enc)>,
    },
}

impl From<Splice> for Op {
    fn from(s: Splice) -> Op {
        Op::Splice {
            at: s.at,
            removed: s.removed,
            inserted: s.inserted,
        }
    }
}

/// Байт кусков сверх встроенных в `SmallVec`.
fn heap(p: &Pieces) -> usize {
    if p.spilled() {
        p.capacity() * size_of::<Piece>()
    } else {
        0
    }
}

impl Op {
    /// Память операции: ≈ 32 Б на кусок, у `Root` — куски обоих деревьев.
    fn cost(&self) -> usize {
        size_of::<Op>()
            + match self {
                Op::Splice {
                    removed, inserted, ..
                } => heap(removed) + heap(inserted),
                Op::Root { before, after, .. } => {
                    (before.sums().pieces + after.sums().pieces) as usize * size_of::<Piece>()
                }
            }
    }

    /// Ссылается ли на источник.
    fn uses(&self, src: Src) -> bool {
        match self {
            Op::Splice {
                removed, inserted, ..
            } => removed.iter().chain(inserted).any(|p| p.src == src),
            Op::Root { before, after, .. } => {
                before.iter().chain(after.iter()).any(|(_, p)| p.src == src)
            }
        }
    }
}

/// Вид правки для группировки.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Ввод, вставка символов, режим замены.
    Input,
    /// Backspace, Del.
    Delete,
    /// Команда правки: вставка из буфера, Ctrl+Y, отступ, `editor.md.*`, «Заменить все».
    Command,
}

/// Группа: отменяется и повторяется целиком.
#[derive(Clone, Debug)]
struct Group<S> {
    ops: Vec<Op>,
    before: S,
    after: S,
    kind: Kind,
    last: Instant,
    cost: usize,
}

impl<S> Group<S> {
    fn uses(&self, src: Src) -> bool {
        self.ops.iter().any(|o| o.uses(src))
    }
}

/// Сливает смежные непрерывные куски.
fn compact(p: &mut Pieces) {
    let mut i = 0;
    while i + 1 < p.len() {
        if let Some(m) = p[i].join(&p[i + 1]) {
            p[i] = m;
            p.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// Правка `b` сразу после `a` (вперёд — набор, Del; назад — Backspace) — одна операция.
fn merge(a: &mut Op, b: &Op) -> bool {
    let (
        Op::Splice {
            at: a_at,
            removed: a_rem,
            inserted: a_ins,
        },
        Op::Splice {
            at: b_at,
            removed: b_rem,
            inserted: b_ins,
        },
    ) = (a, b)
    else {
        return false;
    };
    if *b_at == *a_at + bytes_of(a_ins) {
        a_rem.extend_from_slice(b_rem);
        a_ins.extend_from_slice(b_ins);
    } else if *b_at + bytes_of(b_rem) == *a_at {
        *a_at = *b_at;
        a_rem.insert_many(0, b_rem.iter().copied());
        a_ins.insert_many(0, b_ins.iter().copied());
    } else {
        return false;
    }
    compact(a_rem);
    compact(a_ins);
    true
}

/// Итог отмены или повтора: выделение и кодировка, если её меняла массовая правка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<S> {
    pub sel: S,
    pub enc: Option<Enc>,
}

/// Журнал отмены документа; `S` — выделение (курсор и диапазоны документа).
#[derive(Debug)]
pub struct Journal<S> {
    groups: VecDeque<Group<S>>,
    /// Применено групп.
    pos: usize,
    /// Отброшено групп с начала (позиции — абсолютные: `base + pos`).
    base: u64,
    /// Позиция последнего сохранения; `None` — до неё не дойти.
    saved: Option<u64>,
    /// Последняя применённая группа принимает операции.
    open: bool,
    /// `begin_group`: следующая операция — новая группа этого вида.
    next: Option<Kind>,
    bytes: usize,
    cap: usize,
}

impl<S: Clone> Default for Journal<S> {
    fn default() -> Self {
        Journal::new()
    }
}

impl<S: Clone> Journal<S> {
    /// Пустой журнал; документ сохранён.
    pub fn new() -> Journal<S> {
        Journal::with_cap(CAP)
    }

    /// Свой потолок памяти (тесты).
    pub fn with_cap(cap: usize) -> Journal<S> {
        Journal {
            groups: VecDeque::new(),
            pos: 0,
            base: 0,
            saved: Some(0),
            open: false,
            next: None,
            bytes: 0,
            cap,
        }
    }

    /// Команда правки: её операции (вида `kind`) — одна новая группа.
    pub fn begin_group(&mut self, kind: Kind) {
        self.open = false;
        self.next = Some(kind);
    }

    /// Курсор перешёл (движение, мышь): следующая правка — новая группа.
    pub fn break_group(&mut self) {
        self.open = false;
    }

    /// Записывает операцию: `before`/`after` — выделение до и после неё.
    pub fn record(&mut self, op: Op, kind: Kind, before: &S, after: &S, now: Instant) {
        self.cut_redo();
        let join = self.open
            && self.next.is_none()
            && self.pos > 0
            && self.groups.get(self.pos - 1).is_some_and(|g| {
                g.kind == kind
                    && (kind == Kind::Command || now.saturating_duration_since(g.last) < PAUSE)
            });
        if join {
            let g = &mut self.groups[self.pos - 1];
            let last = g.ops.last_mut().expect("group has ops");
            let old = last.cost();
            if merge(last, &op) {
                let new = last.cost();
                g.cost = g.cost + new - old;
                self.bytes = self.bytes + new - old;
            } else {
                let c = op.cost();
                g.ops.push(op);
                g.cost += c;
                self.bytes += c;
            }
            g.after = after.clone();
            g.last = now;
        } else {
            let cost = op.cost() + size_of::<Group<S>>();
            self.groups.push_back(Group {
                ops: vec![op],
                before: before.clone(),
                after: after.clone(),
                kind: self.next.take().unwrap_or(kind),
                last: now,
                cost,
            });
            self.bytes += cost;
            self.pos += 1;
            self.open = true;
        }
        self.trim();
    }

    /// Новая правка после отмены отрезает повтор.
    fn cut_redo(&mut self) {
        if self.pos == self.groups.len() {
            return;
        }
        for g in self.groups.drain(self.pos..) {
            self.bytes -= g.cost;
        }
        if self.saved.is_some_and(|s| s > self.base + self.pos as u64) {
            self.saved = None;
        }
    }

    /// Сверх потолка — старые группы прочь (текущая остаётся).
    fn trim(&mut self) {
        let mut dropped = 0;
        while self.bytes > self.cap && self.pos > 1 {
            let g = self.groups.pop_front().expect("group to drop");
            self.bytes -= g.cost;
            self.pos -= 1;
            self.base += 1;
            dropped += 1;
        }
        if dropped > 0 {
            tracing::debug!(dropped, bytes = self.bytes, "undo journal trimmed");
        }
    }

    /// Отменяет последнюю группу: документ — как до неё, выделение — до неё.
    pub fn undo(&mut self, doc: &mut TextDoc) -> Option<Step<S>> {
        if self.pos == 0 {
            return None;
        }
        self.open = false;
        self.next = None;
        self.pos -= 1;
        let g = &self.groups[self.pos];
        let mut enc = None;
        for op in g.ops.iter().rev() {
            match op {
                Op::Splice {
                    at,
                    removed,
                    inserted,
                } => {
                    doc.splice(*at, bytes_of(inserted), removed);
                }
                Op::Root { before, enc: e, .. } => {
                    doc.set_tree(before.clone());
                    enc = e.map(|e| e.0).or(enc);
                }
            }
        }
        Some(Step {
            sel: g.before.clone(),
            enc,
        })
    }

    /// Повторяет отменённую группу: документ — как после неё, выделение — после неё.
    pub fn redo(&mut self, doc: &mut TextDoc) -> Option<Step<S>> {
        let g = self.groups.get(self.pos)?;
        self.open = false;
        self.next = None;
        let mut enc = None;
        for op in &g.ops {
            match op {
                Op::Splice {
                    at,
                    removed,
                    inserted,
                } => {
                    doc.splice(*at, bytes_of(removed), inserted);
                }
                Op::Root { after, enc: e, .. } => {
                    doc.set_tree(after.clone());
                    enc = e.map(|e| e.1).or(enc);
                }
            }
        }
        self.pos += 1;
        Some(Step {
            sel: g.after.clone(),
            enc,
        })
    }

    /// Есть что отменить (кнопка панели).
    pub fn can_undo(&self) -> bool {
        self.pos > 0
    }

    /// Есть что повторить.
    pub fn can_redo(&self) -> bool {
        self.pos < self.groups.len()
    }

    /// Документ не в сохранённом состоянии (● на вкладке).
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.base + self.pos as u64)
    }

    /// Сохранено в текущем состоянии; следующая правка — новая группа.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.base + self.pos as u64);
        self.open = false;
    }

    /// Источника больше нет (сохранение «на месте», S7-31): группы, ссылающиеся на него,
    /// и всё, что за ними не достать, отбрасываются. Возвращает число отброшенных.
    pub fn drop_src(&mut self, src: Src) -> usize {
        let n = self.groups.len();
        if let Some(i) = (self.pos..self.groups.len()).find(|&i| self.groups[i].uses(src)) {
            for g in self.groups.drain(i..) {
                self.bytes -= g.cost;
            }
            if self.saved.is_some_and(|s| s > self.base + i as u64) {
                self.saved = None;
            }
        }
        if let Some(i) = (0..self.pos).rev().find(|&i| self.groups[i].uses(src)) {
            for g in self.groups.drain(..=i) {
                self.bytes -= g.cost;
            }
            self.pos -= i + 1;
            self.base += i as u64 + 1;
            self.open = false;
        }
        n - self.groups.len()
    }

    /// Память журнала (оценка).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Групп в журнале (отмена и повтор).
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Применено групп (можно отменить).
    pub fn pos(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
#[path = "undo_tests.rs"]
mod tests;
