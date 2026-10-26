//! Выделение: битсет по индексам `entries`, стабилен при пересортировке.

use std::fmt;

use compact_str::CompactString;
use rustc_hash::FxHashMap;

use crate::entry::Entry;
use crate::tab::Listing;

/// Битсет по индексам `entries` (стабилен при пересортировке). `len` и `bytes`
/// всегда соответствуют установленным битам.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Selection {
    bits: Vec<u64>,
    count: u32,
    bytes: u64,
}

impl Selection {
    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Сумма `size` выделенных элементов.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn contains(&self, idx: u32) -> bool {
        let (w, b) = Self::pos(idx);
        self.bits.get(w).is_some_and(|word| word & b != 0)
    }

    /// `e` — элемент с индексом `idx` (нужен для учёта `bytes`).
    pub fn set(&mut self, idx: u32, on: bool, e: &Entry) {
        if self.contains(idx) == on {
            return;
        }
        let (w, b) = Self::pos(idx);
        if on {
            if self.bits.len() <= w {
                self.bits.resize(w + 1, 0);
            }
            self.bits[w] |= b;
            self.count += 1;
            self.bytes += e.size;
        } else {
            self.bits[w] &= !b;
            self.count -= 1;
            self.bytes -= e.size;
        }
    }

    /// Переключает; возвращает новое состояние.
    pub fn toggle(&mut self, idx: u32, e: &Entry) -> bool {
        let on = !self.contains(idx);
        self.set(idx, on, e);
        on
    }

    pub fn clear(&mut self) {
        *self = Selection::default();
    }

    /// Выделить (`on`) или снять выделение с видимых (`order`) элементов, для которых `f` истинно.
    pub fn select_where(&mut self, l: &Listing, on: bool, f: impl Fn(&Entry) -> bool) {
        for &idx in l.order.iter() {
            let e = &l.entries[idx as usize];
            if f(e) {
                self.set(idx, on, e);
            }
        }
    }

    /// Инвертировать видимые (`order`); при `files_only` каталоги не трогаются.
    pub fn invert(&mut self, l: &Listing, files_only: bool) {
        // Битсет растёт один раз, дальше — xor без проверок длины (C.4: 100k < 1 мс).
        let words = l.entries.len().div_ceil(64);
        if self.bits.len() < words {
            self.bits.resize(words, 0);
        }
        for &idx in l.order.iter() {
            let e = &l.entries[idx as usize];
            if files_only && e.kind.is_dir_like() {
                continue;
            }
            let (w, b) = Self::pos(idx);
            self.bits[w] ^= b;
            if self.bits[w] & b == 0 {
                self.count -= 1;
                self.bytes -= e.size;
            } else {
                self.count += 1;
                self.bytes += e.size;
            }
        }
    }

    /// Индексы по возрастанию.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.bits.iter().enumerate().flat_map(|(w, &word)| {
            let base = (w as u32) * 64;
            BitIter(word).map(move |b| base + b)
        })
    }

    /// Имена выделенных (для переноса через перечитывание каталога).
    pub fn names(&self, l: &Listing) -> Vec<CompactString> {
        let mut v = Vec::with_capacity(self.count as usize);
        v.extend(
            self.iter()
                .filter_map(|i| l.entries.get(i as usize))
                .map(|e| e.name.clone()),
        );
        v
    }

    /// Выделение по именам в новом листинге; отсутствующие имена пропускаются.
    pub fn restore(names: &[CompactString], l: &Listing) -> Selection {
        let mut sel = Selection::default();
        if names.is_empty() {
            return sel;
        }
        let by_name: FxHashMap<&str, u32> = l
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.as_str(), i as u32))
            .collect();
        for n in names {
            if let Some(&i) = by_name.get(n.as_str()) {
                sel.set(i, true, &l.entries[i as usize]);
            }
        }
        sel
    }

    fn pos(idx: u32) -> (usize, u64) {
        ((idx / 64) as usize, 1u64 << (idx % 64))
    }
}

impl fmt::Debug for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Selection")
            .field("count", &self.count)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

struct BitIter(u64);

impl Iterator for BitIter {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        if self.0 == 0 {
            return None;
        }
        let b = self.0.trailing_zeros();
        self.0 &= self.0 - 1;
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::Selection;
    use crate::entry::{Attrs, Entry, EntryKind};
    use crate::ids::Gen;
    use crate::tab::Listing;
    use compact_str::CompactString;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn entry(name: &str, size: u64, dir: bool) -> Entry {
        Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size,
            mtime: 0,
            attrs: Attrs::default(),
            kind: if dir { EntryKind::Dir } else { EntryKind::File },
        }
    }

    fn listing(entries: Vec<Entry>, order: Vec<u32>) -> Listing {
        Listing::with_entries(entries, order, Gen(1))
    }

    #[test]
    fn counters_follow_bits() {
        let l = listing(
            vec![
                entry("a", 10, false),
                entry("b", 5, false),
                entry("d", 0, true),
            ],
            vec![0, 1, 2],
        );
        let mut s = Selection::default();
        assert!(s.toggle(0, &l.entries[0]));
        s.set(1, true, &l.entries[1]);
        s.set(1, true, &l.entries[1]);
        assert_eq!((s.len(), s.bytes()), (2, 15));
        assert!(!s.toggle(0, &l.entries[0]));
        assert_eq!((s.len(), s.bytes()), (1, 5));
        s.invert(&l, true);
        assert_eq!(s.iter().collect::<Vec<_>>(), [0]);
        s.invert(&l, false);
        assert_eq!(s.iter().collect::<Vec<_>>(), [1, 2]);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.bytes(), 0);
    }

    #[test]
    fn indexes_beyond_first_word() {
        let entries: Vec<Entry> = (0..200)
            .map(|i| entry(&format!("f{i}"), 1, false))
            .collect();
        let l = listing(entries, (0..200).collect());
        let mut s = Selection::default();
        for i in [0, 63, 64, 127, 199] {
            s.set(i, true, &l.entries[i as usize]);
        }
        assert_eq!(s.iter().collect::<Vec<_>>(), [0, 63, 64, 127, 199]);
        assert!(!s.contains(500));
    }

    #[test]
    fn restore_after_resort_and_reload() {
        let names = ["a", "b", "c", "d"];
        let l = listing(
            names.iter().map(|n| entry(n, 1, false)).collect(),
            vec![0, 1, 2, 3],
        );
        let mut s = Selection::default();
        s.set(1, true, &l.entries[1]);
        s.set(3, true, &l.entries[3]);
        // Пересортировка меняет только order: индексы стабильны.
        let resorted = listing(l.entries.to_vec(), vec![3, 2, 1, 0]);
        assert_eq!(Selection::restore(&s.names(&l), &resorted), s);
        // Перечитывание: другие индексы, одно имя пропало, одно добавилось.
        let reloaded = listing(
            ["x", "d", "a", "b"]
                .iter()
                .map(|n| entry(n, 1, false))
                .collect(),
            vec![0, 1, 2, 3],
        );
        let r = Selection::restore(&s.names(&l), &reloaded);
        let mut got: Vec<CompactString> = r.names(&reloaded);
        got.sort();
        assert_eq!(got, ["b", "d"]);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Toggle(u32),
        Set(u32, bool),
        Clear,
        Invert(bool),
        SelectWhere(bool, u64),
        Resort(Vec<u32>),
        Reload(u32),
    }

    fn op(n: u32) -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..n).prop_map(Op::Toggle),
            (0..n, any::<bool>()).prop_map(|(i, on)| Op::Set(i, on)),
            Just(Op::Clear),
            any::<bool>().prop_map(Op::Invert),
            (any::<bool>(), 1..4u64).prop_map(|(on, m)| Op::SelectWhere(on, m)),
            Just((0..n).collect::<Vec<u32>>())
                .prop_shuffle()
                .prop_map(Op::Resort),
            any::<u32>().prop_map(Op::Reload),
        ]
    }

    fn case() -> impl Strategy<Value = (Vec<(u64, bool)>, Vec<u32>, Vec<Op>)> {
        prop::collection::vec((0..1000u64, any::<bool>()), 1..150).prop_flat_map(|items| {
            let n = items.len() as u32;
            let order = prop::sample::subsequence((0..n).collect::<Vec<u32>>(), 0..=n as usize)
                .prop_shuffle();
            (Just(items), order, prop::collection::vec(op(n), 0..40))
        })
    }

    proptest! {
        #[test]
        fn matches_hashset_model((items, order, ops) in case()) {
            let mut entries: Vec<Entry> = items
                .iter()
                .enumerate()
                .map(|(i, &(size, dir))| entry(&format!("n{i}"), size, dir))
                .collect();
            let mut l = listing(entries.clone(), order);
            let mut s = Selection::default();
            let mut model: HashSet<u32> = HashSet::new();
            for op in ops {
                match op {
                    Op::Toggle(i) => {
                        let on = s.toggle(i, &l.entries[i as usize]);
                        prop_assert_eq!(on, model.insert(i));
                        if !on {
                            model.remove(&i);
                        }
                    }
                    Op::Set(i, on) => {
                        s.set(i, on, &l.entries[i as usize]);
                        if on { model.insert(i); } else { model.remove(&i); }
                    }
                    Op::Clear => {
                        s.clear();
                        model.clear();
                    }
                    Op::Invert(files_only) => {
                        s.invert(&l, files_only);
                        for &i in l.order.iter() {
                            let dir = l.entries[i as usize].kind.is_dir_like();
                            if files_only && dir {
                                continue;
                            }
                            if !model.remove(&i) {
                                model.insert(i);
                            }
                        }
                    }
                    Op::SelectWhere(on, m) => {
                        s.select_where(&l, on, |e| e.size.is_multiple_of(m));
                        for &i in l.order.iter() {
                            if l.entries[i as usize].size.is_multiple_of(m) {
                                if on { model.insert(i); } else { model.remove(&i); }
                            }
                        }
                    }
                    Op::Resort(order) => {
                        let names = s.names(&l);
                        l = listing(entries.clone(), order);
                        s = Selection::restore(&names, &l);
                    }
                    Op::Reload(seed) => {
                        // Перечитывание: те же элементы в другом порядке индексов.
                        let names = s.names(&l);
                        let n = entries.len();
                        let rot = seed as usize % n;
                        entries.rotate_left(rot);
                        let old_to_new = |i: u32| ((i as usize + n - rot) % n) as u32;
                        model = model.into_iter().map(old_to_new).collect();
                        let order = l.order.iter().map(|&i| old_to_new(i)).collect();
                        l = listing(entries.clone(), order);
                        s = Selection::restore(&names, &l);
                    }
                }
                let mut want: Vec<u32> = model.iter().copied().collect();
                want.sort_unstable();
                prop_assert_eq!(s.iter().collect::<Vec<_>>(), want);
                prop_assert_eq!(s.len() as usize, model.len());
                let bytes: u64 = model.iter().map(|&i| l.entries[i as usize].size).sum();
                prop_assert_eq!(s.bytes(), bytes);
            }
        }
    }
}
