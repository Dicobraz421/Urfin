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

use smallvec::SmallVec;

use super::piece::{Piece, Pieces, Splice, Src, TextDoc, Tree, bytes_of};
use crate::detect::Eol;
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
    /// Дерево целиком; `enc` — кодировка до и после (перевод кодировки), `eol` —
    /// концы строк до и после (`editor.eol`).
    Root {
        before: Tree,
        after: Tree,
        enc: Option<(Enc, Enc)>,
        eol: Option<(Eol, Eol)>,
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

    /// Временные файлы `Rewrite(n)`, на которые ссылается операция.
    fn rewrites(&self, out: &mut Slots) {
        let mut add = |p: &Piece| {
            if let Src::Rewrite(n) = p.src
                && !out.contains(&n)
            {
                out.push(n);
            }
        };
        match self {
            Op::Splice {
                removed, inserted, ..
            } => removed.iter().chain(inserted).for_each(add),
            Op::Root { before, after, .. } => {
                before.iter().chain(after.iter()).for_each(|(_, p)| add(p));
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

/// Номера временных файлов `Rewrite(n)`.
type Slots = SmallVec<[u16; 2]>;

/// Группа: отменяется и повторяется целиком.
#[derive(Clone, Debug)]
struct Group<S> {
    ops: Vec<Op>,
    before: S,
    after: S,
    kind: Kind,
    last: Instant,
    cost: usize,
    /// Временные файлы `Rewrite(n)`, на которые ссылаются операции группы.
    rw: Slots,
}

impl<S> Group<S> {
    fn uses(&self, src: Src) -> bool {
        self.ops.iter().any(|o| o.uses(src))
    }

    /// Временные файлы группы — заново (переименование источника).
    fn recount(&mut self) {
        self.rw.clear();
        for op in &self.ops {
            op.rewrites(&mut self.rw);
        }
    }
}

/// Номера `add` — в `gone` (без повторов).
fn push_gone(gone: &mut Slots, add: &[u16]) {
    for n in add {
        if !gone.contains(n) {
            gone.push(*n);
        }
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

/// Итог отмены или повтора: выделение, кодировка и концы строк, если их меняла
/// массовая правка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<S> {
    pub sel: S,
    pub enc: Option<Enc>,
    pub eol: Option<Eol>,
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
    /// Сколько раз отрезались группы (повтор новой правкой, `drop_src`): отметка
    /// [`Mark`] до отрезания может указывать на другое состояние.
    cuts: u64,
    /// Временные файлы `Rewrite(n)` отброшенных групп (обрезка, отрезанный повтор): им,
    /// может быть, больше никто не нужен ([`Journal::take_gone`], S7-44).
    gone: Slots,
}

/// Состояние журнала в момент снимка (сохранение потоком, S7-31): документ сохранён в
/// нём, даже если после снимка правили.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    abs: u64,
    cuts: u64,
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
            cuts: 0,
            gone: Slots::new(),
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
            op.rewrites(&mut g.rw);
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
            let mut rw = Slots::new();
            op.rewrites(&mut rw);
            self.groups.push_back(Group {
                rw,
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
            push_gone(&mut self.gone, &g.rw);
        }
        self.cuts += 1;
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
            push_gone(&mut self.gone, &g.rw);
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
        let mut eol = None;
        for op in g.ops.iter().rev() {
            match op {
                Op::Splice {
                    at,
                    removed,
                    inserted,
                } => {
                    doc.splice(*at, bytes_of(inserted), removed);
                }
                Op::Root {
                    before,
                    enc: e,
                    eol: l,
                    ..
                } => {
                    doc.set_tree(before.clone());
                    enc = e.map(|e| e.0).or(enc);
                    eol = l.map(|l| l.0).or(eol);
                }
            }
        }
        Some(Step {
            sel: g.before.clone(),
            enc,
            eol,
        })
    }

    /// Повторяет отменённую группу: документ — как после неё, выделение — после неё.
    pub fn redo(&mut self, doc: &mut TextDoc) -> Option<Step<S>> {
        let g = self.groups.get(self.pos)?;
        self.open = false;
        self.next = None;
        let mut enc = None;
        let mut eol = None;
        for op in &g.ops {
            match op {
                Op::Splice {
                    at,
                    removed,
                    inserted,
                } => {
                    doc.splice(*at, bytes_of(removed), inserted);
                }
                Op::Root {
                    after,
                    enc: e,
                    eol: l,
                    ..
                } => {
                    doc.set_tree(after.clone());
                    enc = e.map(|e| e.1).or(enc);
                    eol = l.map(|l| l.1).or(eol);
                }
            }
        }
        self.pos += 1;
        Some(Step {
            sel: g.after.clone(),
            enc,
            eol,
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

    /// Снимок для сохранения: текущая группа закрывается (правка после снимка — новая
    /// группа), отметка — для [`Journal::mark_saved_at`].
    pub fn checkpoint(&mut self) -> Mark {
        self.open = false;
        Mark {
            abs: self.base + self.pos as u64,
            cuts: self.cuts,
        }
    }

    /// Сохранено состояние отметки `m` (снимок): после снимка могли править — тогда
    /// «изменён» остаётся; группы отрезаны с тех пор — состояние не найти.
    pub fn mark_saved_at(&mut self, m: Mark) {
        let end = self.base + self.groups.len() as u64;
        self.saved = (m.cuts == self.cuts && m.abs >= self.base && m.abs <= end).then_some(m.abs);
        if self.saved == Some(self.base + self.pos as u64) {
            self.open = false;
        }
    }

    /// Источник переименован (сохранение потоком, S7-31: прежняя версия файла —
    /// `Rewrite(n)`, `Original` — новый файл): куски групп ссылаются на `to`.
    pub fn rename_src(&mut self, from: Src, to: Src) {
        let map = |p: &mut Piece| {
            if p.src == from {
                p.src = to;
            }
        };
        for g in &mut self.groups {
            for op in &mut g.ops {
                match op {
                    Op::Splice {
                        removed, inserted, ..
                    } => {
                        removed.iter_mut().for_each(map);
                        inserted.iter_mut().for_each(map);
                    }
                    Op::Root { before, after, .. } => {
                        *before = before.map_src(from, to);
                        *after = after.map_src(from, to);
                    }
                }
            }
            g.recount();
        }
    }

    /// Временные файлы `Rewrite(n)` отброшенных с прошлого вызова групп (и отмеченные
    /// [`Journal::note_gone`]) — кандидаты на освобождение: документ проверяет, что на них
    /// не ссылаются ни журнал ([`Journal::uses_rewrite`]), ни текст (S7-44).
    pub fn take_gone(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.gone).into_vec()
    }

    /// Временный файл `Rewrite(n)` — кандидат на освобождение (прежняя версия файла
    /// после сохранения, S7-31: ссылок на неё в журнале может не быть).
    pub fn note_gone(&mut self, n: u16) {
        push_gone(&mut self.gone, &[n]);
    }

    /// Группы журнала (отмена и повтор) ссылаются на `Rewrite(n)`.
    pub fn uses_rewrite(&self, n: u16) -> bool {
        self.groups.iter().any(|g| g.rw.contains(&n))
    }

    /// Источника больше нет (сохранение «на месте», S7-31): группы, ссылающиеся на него,
    /// и всё, что за ними не достать, отбрасываются. Возвращает число отброшенных.
    pub fn drop_src(&mut self, src: Src) -> usize {
        let n = self.groups.len();
        if let Some(i) = (self.pos..self.groups.len()).find(|&i| self.groups[i].uses(src)) {
            for g in self.groups.drain(i..) {
                self.bytes -= g.cost;
                push_gone(&mut self.gone, &g.rw);
            }
            if self.saved.is_some_and(|s| s > self.base + i as u64) {
                self.saved = None;
            }
            self.cuts += 1;
        }
        if let Some(i) = (0..self.pos).rev().find(|&i| self.groups[i].uses(src)) {
            for g in self.groups.drain(..=i) {
                self.bytes -= g.cost;
                push_gone(&mut self.gone, &g.rw);
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
