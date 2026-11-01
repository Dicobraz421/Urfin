//! Персистентное B+-дерево кусков: узлы в `Arc`, суммы в детях (байты, известные
//! переводы строк, байты кусков с `Lf::Unknown`, число кусков). Поиск по смещению —
//! O(log n); вставка и удаление копируют путь (`Arc::make_mut`: узел, который держит
//! только документ, меняется на месте). Узлы после правки могут выйти за пределы —
//! их чинит родитель ([`fix`]): пустые уходят, переполненные делятся, полупустые
//! сливаются с соседом.

#![allow(
    clippy::many_single_char_names,
    reason = "i, j — индексы детей, v — куски листа, r — диапазон, a, b — соседи"
)]

use std::ops::Range;
use std::sync::Arc;

use smallvec::SmallVec;

use super::{CountLf, Lf, Piece, Pieces, Src};

/// Кусков в листе — не больше.
pub const LEAF_MAX: usize = 64;
/// Детей у узла — не больше.
pub const INNER_MAX: usize = 16;
const LEAF_MIN: usize = LEAF_MAX / 4;
const INNER_MIN: usize = INNER_MAX / 4;

/// Суммы поддерева.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sums {
    pub bytes: u64,
    /// Известных переводов строк.
    pub lf: u64,
    /// Байт в кусках с `Lf::Unknown`.
    pub unknown: u64,
    pub pieces: u64,
}

impl Sums {
    fn of(p: &Piece) -> Sums {
        let (lf, unknown) = match p.lf {
            Lf::Known(n) => (u64::from(n), 0),
            Lf::Unknown => (0, p.len),
        };
        Sums {
            bytes: p.len,
            lf,
            unknown,
            pieces: 1,
        }
    }

    fn add(&mut self, o: Sums) {
        self.bytes += o.bytes;
        self.lf += o.lf;
        self.unknown += o.unknown;
        self.pieces += o.pieces;
    }
}

#[derive(Clone, Debug)]
enum Node {
    Leaf(Vec<Piece>),
    Inner(Vec<Child>),
}

#[derive(Clone, Debug)]
struct Child {
    sums: Sums,
    node: Arc<Node>,
}

impl Child {
    fn new(node: Node) -> Child {
        Child {
            sums: node.sums(),
            node: Arc::new(node),
        }
    }

    fn refresh(&mut self) {
        self.sums = self.node.sums();
    }
}

impl Node {
    fn sums(&self) -> Sums {
        let mut s = Sums::default();
        match self {
            Node::Leaf(v) => v.iter().for_each(|p| s.add(Sums::of(p))),
            Node::Inner(v) => v.iter().for_each(|c| s.add(c.sums)),
        }
        s
    }

    fn size(&self) -> usize {
        match self {
            Node::Leaf(v) => v.len(),
            Node::Inner(v) => v.len(),
        }
    }

    fn max(&self) -> usize {
        match self {
            Node::Leaf(_) => LEAF_MAX,
            Node::Inner(_) => INNER_MAX,
        }
    }

    fn min(&self) -> usize {
        match self {
            Node::Leaf(_) => LEAF_MIN,
            Node::Inner(_) => INNER_MIN,
        }
    }
}

/// Дерево кусков документа. Клон — O(1) (снимок).
#[derive(Clone, Debug)]
pub struct Tree {
    root: Arc<Node>,
    sums: Sums,
}

impl Default for Tree {
    fn default() -> Self {
        Tree {
            root: Arc::new(Node::Leaf(Vec::new())),
            sums: Sums::default(),
        }
    }
}

/// Делит `v` на равные части не больше `max`.
fn chunks<T: Clone>(v: &[T], max: usize) -> Vec<Vec<T>> {
    let n = v.len().div_ceil(max).max(1);
    let size = v.len().div_ceil(n).max(1);
    v.chunks(size).map(<[T]>::to_vec).collect()
}

/// Сливает смежные непрерывные куски в `v[lo..hi]` (границы — включая соседей).
fn join_range(v: &mut Vec<Piece>, lo: usize, hi: usize) {
    let mut j = lo.saturating_sub(1);
    let mut hi = hi.min(v.len().saturating_sub(1));
    while j < hi {
        if let Some(m) = v[j].join(&v[j + 1]) {
            v[j] = m;
            v.remove(j + 1);
            hi -= 1;
        } else {
            j += 1;
        }
    }
}

/// Чинит детей узла после правки: пустые — прочь, переполненные — делятся, полупустые
/// сливаются с соседом (на шве двух листьев куски тоже сливаются).
fn fix(kids: &mut Vec<Child>) {
    kids.retain(|c| c.sums.pieces > 0);
    let mut i = 0;
    while i < kids.len() {
        let node = &kids[i].node;
        if node.size() > node.max() {
            let parts: Vec<Child> = match &**node {
                Node::Leaf(v) => chunks(v, LEAF_MAX)
                    .into_iter()
                    .map(|c| Child::new(Node::Leaf(c)))
                    .collect(),
                Node::Inner(v) => chunks(v, INNER_MAX)
                    .into_iter()
                    .map(|c| Child::new(Node::Inner(c)))
                    .collect(),
            };
            let n = parts.len();
            kids.splice(i..=i, parts);
            i += n;
        } else {
            i += 1;
        }
    }
    let mut i = 0;
    while i < kids.len() && kids.len() > 1 {
        if kids[i].node.size() >= kids[i].node.min() {
            i += 1;
            continue;
        }
        let j = if i + 1 < kids.len() { i } else { i - 1 };
        let b = kids.remove(j + 1);
        let a = &mut kids[j];
        let merged = match (Arc::make_mut(&mut a.node), &*b.node) {
            (Node::Leaf(x), Node::Leaf(y)) => {
                let at = x.len();
                x.extend_from_slice(y);
                join_range(x, at, at);
                x.len() > LEAF_MAX
            }
            (Node::Inner(x), Node::Inner(y)) => {
                x.extend_from_slice(y);
                x.len() > INNER_MAX
            }
            _ => unreachable!("siblings of different height"),
        };
        a.refresh();
        if merged {
            let parts: Vec<Child> = match &*a.node {
                Node::Leaf(v) => chunks(v, LEAF_MAX)
                    .into_iter()
                    .map(|c| Child::new(Node::Leaf(c)))
                    .collect(),
                Node::Inner(v) => chunks(v, INNER_MAX)
                    .into_iter()
                    .map(|c| Child::new(Node::Inner(c)))
                    .collect(),
            };
            kids.splice(j..=j, parts);
        }
        i = j + 1;
    }
}

fn insert_node(node: &mut Arc<Node>, at: u64, ins: &[Piece], count: CountLf<'_>) {
    match Arc::make_mut(node) {
        Node::Leaf(v) => {
            let mut acc = 0;
            let mut idx = v.len();
            for (j, p) in v.iter().enumerate() {
                if at <= acc {
                    idx = j;
                    break;
                }
                if at < acc + p.len {
                    let (l, r) = p.split(at - acc, count);
                    v[j] = l;
                    v.insert(j + 1, r);
                    idx = j + 1;
                    break;
                }
                acc += p.len;
            }
            v.splice(idx..idx, ins.iter().copied());
            join_range(v, idx, idx + ins.len());
        }
        Node::Inner(kids) => {
            let mut acc = 0;
            let mut i = kids.len() - 1;
            for (j, c) in kids.iter().enumerate() {
                if at <= acc + c.sums.bytes {
                    i = j;
                    break;
                }
                acc += c.sums.bytes;
            }
            insert_node(&mut kids[i].node, at - acc, ins, count);
            kids[i].refresh();
            fix(kids);
        }
    }
}

fn remove_node(node: &mut Arc<Node>, r: Range<u64>, count: CountLf<'_>, out: &mut Pieces) {
    match Arc::make_mut(node) {
        Node::Leaf(v) => {
            let mut acc = 0;
            let mut j = 0;
            let mut gap = None;
            while j < v.len() && acc < r.end {
                let p = v[j];
                let (ps, pe) = (acc, acc + p.len);
                acc = pe;
                if pe <= r.start {
                    j += 1;
                    continue;
                }
                let (cut_l, cut_r) = (r.start.max(ps) - ps, r.end.min(pe) - ps);
                gap.get_or_insert(j);
                match (cut_l > 0, cut_r < p.len) {
                    (false, false) => {
                        out.push(p);
                        v.remove(j);
                    }
                    (true, false) => {
                        let (l, m) = p.split(cut_l, count);
                        out.push(m);
                        v[j] = l;
                        j += 1;
                        gap = Some(j);
                    }
                    (false, true) => {
                        let (m, rr) = p.split(cut_r, count);
                        out.push(m);
                        v[j] = rr;
                        j += 1;
                    }
                    (true, true) => {
                        let (l, rest) = p.split(cut_l, count);
                        let (m, rr) = rest.split(cut_r - cut_l, count);
                        out.push(m);
                        v[j] = l;
                        v.insert(j + 1, rr);
                        gap = Some(j + 1);
                        j += 2;
                    }
                }
            }
            if let Some(g) = gap {
                join_range(v, g, g);
            }
        }
        Node::Inner(kids) => {
            let mut acc = 0;
            for c in kids.iter_mut() {
                let (cs, ce) = (acc, acc + c.sums.bytes);
                acc = ce;
                if ce <= r.start {
                    continue;
                }
                if cs >= r.end {
                    break;
                }
                let sub = r.start.max(cs) - cs..r.end.min(ce) - cs;
                remove_node(&mut c.node, sub, count, out);
                c.refresh();
            }
            fix(kids);
        }
    }
}

/// Корень после правки: переполненный — новый уровень сверху, один ребёнок — уровень
/// снимается.
fn fix_root(root: &mut Arc<Node>) {
    loop {
        if root.size() > root.max() {
            let mut kids = vec![Child {
                sums: root.sums(),
                node: root.clone(),
            }];
            fix(&mut kids);
            *root = Arc::new(Node::Inner(kids));
            continue;
        }
        if let Node::Inner(kids) = &**root
            && kids.len() == 1
        {
            *root = kids[0].node.clone();
            continue;
        }
        if let Node::Inner(kids) = &**root
            && kids.is_empty()
        {
            *root = Arc::new(Node::Leaf(Vec::new()));
        }
        return;
    }
}

impl Tree {
    /// Дерево из кусков по порядку (смежные непрерывные сливаются).
    pub fn from_pieces(pieces: impl IntoIterator<Item = Piece>) -> Tree {
        let mut v: Vec<Piece> = Vec::new();
        for p in pieces {
            if p.len == 0 {
                continue;
            }
            match v.last().and_then(|l| l.join(&p)) {
                Some(m) => *v.last_mut().expect("last") = m,
                None => v.push(p),
            }
        }
        if v.len() <= LEAF_MAX {
            let root = Node::Leaf(v);
            return Tree {
                sums: root.sums(),
                root: Arc::new(root),
            };
        }
        let mut level: Vec<Child> = chunks(&v, LEAF_MAX)
            .into_iter()
            .map(|c| Child::new(Node::Leaf(c)))
            .collect();
        while level.len() > INNER_MAX {
            level = chunks(&level, INNER_MAX)
                .into_iter()
                .map(|c| Child::new(Node::Inner(c)))
                .collect();
        }
        let root = Node::Inner(level);
        Tree {
            sums: root.sums(),
            root: Arc::new(root),
        }
    }

    pub fn len(&self) -> u64 {
        self.sums.bytes
    }

    /// Куски источника `from` — на `to` (те же диапазоны; S7-31: прежняя версия файла
    /// после сохранения). Нет таких — то же дерево.
    #[must_use]
    pub fn map_src(&self, from: Src, to: Src) -> Tree {
        if !self.iter().any(|(_, p)| p.src == from) {
            return self.clone();
        }
        Tree::from_pieces(self.iter().map(|(_, p)| {
            let mut p = *p;
            if p.src == from {
                p.src = to;
            }
            p
        }))
    }

    pub fn is_empty(&self) -> bool {
        self.sums.bytes == 0
    }

    pub fn sums(&self) -> Sums {
        self.sums
    }

    /// Тот же корень (снимок не менялся).
    pub fn same(&self, other: &Tree) -> bool {
        Arc::ptr_eq(&self.root, &other.root)
    }

    /// Кусок, содержащий `off`, и его начало в документе; `None` — `off >= len`.
    pub fn find(&self, off: u64) -> Option<(u64, Piece)> {
        self.iter_from(off).next().map(|(s, p)| (s, *p))
    }

    /// Суммы всех кусков до куска, содержащего `off`, и сам кусок с началом.
    pub fn prefix(&self, off: u64) -> Option<(Sums, u64, Piece)> {
        if off >= self.len() {
            return None;
        }
        let mut s = Sums::default();
        let mut node = &*self.root;
        loop {
            match node {
                Node::Inner(kids) => {
                    let mut next = None;
                    for c in kids {
                        if off < s.bytes + c.sums.bytes {
                            next = Some(&*c.node);
                            break;
                        }
                        s.add(c.sums);
                    }
                    node = next?;
                }
                Node::Leaf(v) => {
                    for p in v {
                        if off < s.bytes + p.len {
                            return Some((s, s.bytes, *p));
                        }
                        s.add(Sums::of(p));
                    }
                    return None;
                }
            }
        }
    }

    /// Кусок, где кончается `n`-й (с 1) перевод строки: суммы до него, его начало и
    /// номер перевода внутри куска. Неизвестные считаются оценкой `байты / avg`.
    /// `None` — документ кончился раньше.
    pub fn find_line(&self, n: u64, avg: u64) -> Option<(Sums, u64, Piece, u64)> {
        let est = |s: &Sums| s.lf + s.unknown / avg.max(1);
        let mut need = n;
        let mut s = Sums::default();
        let mut node = &*self.root;
        loop {
            match node {
                Node::Inner(kids) => {
                    let mut next = None;
                    for c in kids {
                        let e = est(&c.sums);
                        if need <= e {
                            next = Some(&*c.node);
                            break;
                        }
                        need -= e;
                        s.add(c.sums);
                    }
                    node = next?;
                }
                Node::Leaf(v) => {
                    for p in v {
                        let e = est(&Sums::of(p));
                        if need <= e {
                            return Some((s, s.bytes, *p, need));
                        }
                        need -= e;
                        s.add(Sums::of(p));
                    }
                    return None;
                }
            }
        }
    }

    /// Куски с `Lf::Unknown` по порядку: `(начало в документе, кусок)` (обход только
    /// поддеревьев с неизвестными).
    pub fn unknown(&self) -> Vec<(u64, Piece)> {
        fn walk(node: &Node, mut at: u64, out: &mut Vec<(u64, Piece)>) {
            match node {
                Node::Leaf(v) => {
                    for p in v {
                        if p.lf == Lf::Unknown {
                            out.push((at, *p));
                        }
                        at += p.len;
                    }
                }
                Node::Inner(kids) => {
                    for c in kids {
                        if c.sums.unknown > 0 {
                            walk(&c.node, at, out);
                        }
                        at += c.sums.bytes;
                    }
                }
            }
        }
        let mut out = Vec::new();
        if self.sums.unknown > 0 {
            walk(&self.root, 0, &mut out);
        }
        out
    }

    /// Куски от того, что содержит `off`, до конца: `(начало в документе, кусок)`.
    pub fn iter_from(&self, off: u64) -> Iter<'_> {
        let mut it = Iter {
            stack: SmallVec::new(),
            pos: 0,
        };
        if off >= self.len() {
            return it;
        }
        let mut node = &*self.root;
        let mut acc = 0;
        loop {
            match node {
                Node::Inner(kids) => {
                    let mut i = kids.len() - 1;
                    for (j, c) in kids.iter().enumerate() {
                        if off < acc + c.sums.bytes {
                            i = j;
                            break;
                        }
                        acc += c.sums.bytes;
                    }
                    it.stack.push((node, i));
                    node = &kids[i].node;
                }
                Node::Leaf(v) => {
                    let mut j = v.len();
                    for (k, p) in v.iter().enumerate() {
                        if off < acc + p.len {
                            j = k;
                            break;
                        }
                        acc += p.len;
                    }
                    it.stack.push((node, j));
                    it.pos = acc;
                    return it;
                }
            }
        }
    }

    /// Все куски по порядку.
    pub fn iter(&self) -> Iter<'_> {
        self.iter_from(0)
    }

    /// Вставляет куски на `at` (на стыке — в конец левого листа: набор дописывает
    /// последний кусок `Add`).
    pub fn insert(&mut self, at: u64, pieces: &[Piece], count: CountLf<'_>) {
        debug_assert!(at <= self.len(), "Tree::insert past the end");
        let pieces: SmallVec<[Piece; 4]> = pieces.iter().copied().filter(|p| p.len > 0).collect();
        if pieces.is_empty() {
            return;
        }
        insert_node(&mut self.root, at, &pieces, count);
        fix_root(&mut self.root);
        self.sums = self.root.sums();
    }

    /// Убирает `range`; возвращает убранные куски по порядку (края разрезаны).
    pub fn remove(&mut self, range: Range<u64>, count: CountLf<'_>) -> Pieces {
        let mut out = Pieces::new();
        let r = range.start.min(self.len())..range.end.min(self.len());
        if r.is_empty() {
            return out;
        }
        remove_node(&mut self.root, r, count, &mut out);
        fix_root(&mut self.root);
        self.sums = self.root.sums();
        out
    }

    /// Шов на `at`: кусок, кончающийся там, и кусок, начинающийся там, сливаются, если
    /// непрерывны (оказались в разных листьях).
    pub fn seam(&mut self, at: u64, count: CountLf<'_>) {
        if at == 0 || at >= self.len() {
            return;
        }
        let (Some((ls, l)), Some((rs, r))) = (self.find(at - 1), self.find(at)) else {
            return;
        };
        if rs != at || ls + l.len != at {
            return;
        }
        if let Some(m) = l.join(&r) {
            self.remove(ls..at + r.len, count);
            self.insert(ls, &[m], count);
        }
    }

    /// Куски с `Lf::Unknown`, для которых `f` знает число переводов строк, становятся
    /// `Known` (обход только поддеревьев с неизвестными).
    pub fn patch_lf(&mut self, f: &mut dyn FnMut(&Piece) -> Option<u32>) -> usize {
        fn walk(node: &mut Arc<Node>, f: &mut dyn FnMut(&Piece) -> Option<u32>) -> usize {
            let mut n = 0;
            match Arc::make_mut(node) {
                Node::Leaf(v) => {
                    for p in v.iter_mut() {
                        if p.lf == Lf::Unknown
                            && let Some(k) = f(p)
                        {
                            p.lf = Lf::Known(k);
                            n += 1;
                        }
                    }
                }
                Node::Inner(kids) => {
                    for c in kids.iter_mut().filter(|c| c.sums.unknown > 0) {
                        let k = walk(&mut c.node, f);
                        if k > 0 {
                            c.refresh();
                            n += k;
                        }
                    }
                }
            }
            n
        }
        if self.sums.unknown == 0 {
            return 0;
        }
        let n = walk(&mut self.root, f);
        self.sums = self.root.sums();
        n
    }

    /// Высота (лист — 1).
    pub fn depth(&self) -> usize {
        let mut d = 1;
        let mut node = &*self.root;
        while let Node::Inner(kids) = node {
            d += 1;
            node = &kids[0].node;
        }
        d
    }

    /// Проверка инвариантов (тесты): суммы, высота листьев, размеры узлов.
    #[cfg(test)]
    pub(crate) fn check(&self) {
        fn walk(node: &Node, depth: usize, leaf_depth: &mut Option<usize>, root: bool) -> Sums {
            assert!(node.size() <= node.max(), "node overflow {}", node.size());
            if !root {
                assert!(node.size() > 0, "empty node");
            }
            match node {
                Node::Leaf(v) => {
                    assert_eq!(*leaf_depth.get_or_insert(depth), depth, "unbalanced");
                    assert!(v.iter().all(|p| p.len > 0), "empty piece");
                    node.sums()
                }
                Node::Inner(kids) => {
                    let mut s = Sums::default();
                    for c in kids {
                        let sub = walk(&c.node, depth + 1, leaf_depth, false);
                        assert_eq!(sub, c.sums, "stale sums");
                        s.add(sub);
                    }
                    s
                }
            }
        }
        let s = walk(&self.root, 1, &mut None, true);
        assert_eq!(s, self.sums, "stale root sums");
    }
}

impl<'a> IntoIterator for &'a Tree {
    type Item = (u64, &'a Piece);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// Обход кусков по порядку.
#[derive(Debug)]
pub struct Iter<'a> {
    stack: SmallVec<[(&'a Node, usize); 8]>,
    pos: u64,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (u64, &'a Piece);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (node, i) = *self.stack.last()?;
            match node {
                Node::Leaf(v) => {
                    if let Some(p) = v.get(i) {
                        self.stack.last_mut().expect("top").1 += 1;
                        let at = self.pos;
                        self.pos += p.len;
                        return Some((at, p));
                    }
                    self.stack.pop();
                    self.advance();
                }
                Node::Inner(_) => unreachable!("iterator top is a leaf"),
            }
        }
    }
}

impl<'a> Iter<'a> {
    /// Следующий ребёнок верхнего внутреннего узла и спуск к его первому листу.
    fn advance(&mut self) {
        while let Some((node, i)) = self.stack.pop() {
            let Node::Inner(kids) = node else {
                unreachable!("inner on stack")
            };
            if i + 1 < kids.len() {
                self.stack.push((node, i + 1));
                let mut n: &'a Node = &kids[i + 1].node;
                loop {
                    self.stack.push((n, 0));
                    match n {
                        Node::Inner(k) => n = &k[0].node,
                        Node::Leaf(_) => return,
                    }
                }
            }
        }
    }
}
