use proptest::prelude::*;
use urfin_core::{Attrs, EntryKind, Mask, SortSpec};

use super::*;
use crate::sort::arrange;

fn entry(name: &str, dir: bool, size: u64, mtime: i64, hidden: bool) -> Entry {
    let mut a = 0;
    if hidden {
        a |= Attrs::HIDDEN;
    }
    if crate::name::is_dotfile(name) {
        a |= Attrs::DOTFILE;
    }
    Entry {
        ctime: i64::MIN,
        atime: i64::MIN,
        name: name.into(),
        size,
        mtime,
        attrs: Attrs(a),
        kind: if dir { EntryKind::Dir } else { EntryKind::File },
    }
}

/// Core: `update` → `remove` по убыванию (`swap_remove`) → `append` → `order`.
fn apply(entries: &mut Vec<Entry>, p: &Patch) -> Vec<u32> {
    assert_eq!(entries.len() as u32, p.base_len);
    for (i, e) in &p.update {
        entries[*i as usize] = e.clone();
    }
    let mut last = u32::MAX;
    for &i in &p.remove {
        assert!(
            i < last,
            "remove must be strictly descending: {:?}",
            p.remove
        );
        last = i;
        entries.swap_remove(i as usize);
    }
    entries.extend(p.append.iter().cloned());
    p.order.to_vec()
}

#[derive(Clone, Debug)]
enum Op {
    /// Новое имя (или перезапись существующего, если совпало).
    Add(String, bool, u64, i64, bool),
    /// Изменить запись с номером `i % len`: размер, mtime, скрытость.
    Modify(usize, u64, i64, bool),
    Remove(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (
            "[aAbB0-9ёе.]{1,6}(\\.(txt|rs|TXT|md))?",
            any::<bool>(),
            0u64..5,
            -3i64..3,
            any::<bool>()
        )
            .prop_map(|(n, d, s, m, h)| Op::Add(n, d, s, m, h)),
        (any::<usize>(), 0u64..5, -3i64..3, any::<bool>())
            .prop_map(|(i, s, m, h)| Op::Modify(i, s, m, h)),
        any::<usize>().prop_map(Op::Remove),
    ]
}

fn initial() -> impl Strategy<Value = Vec<Entry>> {
    proptest::collection::vec(
        (
            "[aAbB0-9ёе.]{1,6}(\\.(txt|rs|TXT|md))?",
            any::<bool>(),
            0u64..5,
            -3i64..3,
            any::<bool>(),
        ),
        0..40,
    )
    .prop_map(|v| {
        let mut seen = std::collections::HashSet::new();
        v.into_iter()
            .filter(|(n, ..)| seen.insert(n.clone()))
            .map(|(n, d, s, m, h)| entry(&n, d, s, m, h))
            .collect()
    })
}

fn key() -> impl Strategy<Value = SortKey> {
    prop_oneof![
        Just(SortKey::Name),
        Just(SortKey::Ext),
        Just(SortKey::Size),
        Just(SortKey::Mtime),
        Just(SortKey::Unsorted),
    ]
}

/// Модель ФС: каталог как множество имён; партия операций → `Changes` (как после
/// debounce: по имени одно итоговое изменение).
#[allow(clippy::many_single_char_names, reason = "поля записи в операциях")]
fn batch(fs: &mut Vec<Entry>, ops: &[Op]) -> Changes {
    use std::collections::BTreeMap;
    // имя → итог: Some(запись) или None (удалена); только для затронутых.
    let mut touched: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    let mut cur: BTreeMap<String, Entry> =
        fs.iter().map(|e| (e.name.to_string(), e.clone())).collect();
    for op in ops {
        match op {
            Op::Add(n, d, s, m, h) => {
                let e = entry(n, *d, *s, *m, *h);
                cur.insert(n.clone(), e.clone());
                touched.insert(n.clone(), Some(e));
            }
            Op::Modify(i, s, m, h) => {
                if cur.is_empty() {
                    continue;
                }
                let name = cur.keys().nth(i % cur.len()).unwrap().clone();
                let old = &cur[&name];
                let e = entry(&name, old.kind.is_dir_like(), *s, *m, *h);
                cur.insert(name.clone(), e.clone());
                touched.insert(name, Some(e));
            }
            Op::Remove(i) => {
                if cur.is_empty() {
                    continue;
                }
                let name = cur.keys().nth(i % cur.len()).unwrap().clone();
                cur.remove(&name);
                touched.insert(name, None);
            }
        }
    }
    *fs = cur.into_values().collect();
    let mut ch = Changes::default();
    for (n, e) in touched {
        match e {
            Some(e) => ch.upsert.push(e),
            None => ch.removed.push(n.into()),
        }
    }
    ch
}

fn names_in(order: &[u32], entries: &[Entry]) -> Vec<String> {
    order
        .iter()
        .map(|&i| entries[i as usize].name.to_string())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Приёмка: после любой последовательности изменений entries/order совпадают с
    /// полным refresh + arrange (как множество записей и как видимый порядок).
    #[test]
    fn patch_matches_full_refresh(
        start in initial(),
        batches in proptest::collection::vec(proptest::collection::vec(op(), 0..8), 1..6),
        key in key(),
        desc in any::<bool>(),
        dirs_first in any::<bool>(),
        show_hidden in any::<bool>(),
        natural in any::<bool>(),
        masked in any::<bool>(),
    ) {
        let spec = ArrangeSpec {
            sort: SortSpec { key, desc, dirs_first },
            filter: masked.then(|| Mask::parse("*.txt").unwrap()),
            show_hidden,
        };
        let opts = ArrangeOpts { natural, dotfiles_hidden: true };
        let mut fs = start.clone();
        let mut entries = start;
        let mut order = arrange(&entries, &spec, opts);
        let mut index = DirIndex::build(&entries, Gen(1));
        for ops in &batches {
            let ch = batch(&mut fs, ops);
            if let Some(p) = build_patch(&entries, &order, &mut index, &ch, &spec, opts, 0) {
                order = apply(&mut entries, &p);
            }
            prop_assert_eq!(index.len as usize, entries.len());
            // Записи — те же, что на «диске».
            let mut have: Vec<_> = entries.iter().map(|e| (e.name.to_string(), e.size, e.mtime, e.attrs.0, e.kind as u8)).collect();
            let mut want: Vec<_> = fs.iter().map(|e| (e.name.to_string(), e.size, e.mtime, e.attrs.0, e.kind as u8)).collect();
            have.sort();
            want.sort();
            prop_assert_eq!(have, want);
            // Порядок — как у полного arrange по тем же записям.
            let full = arrange(&entries, &spec, opts);
            prop_assert_eq!(names_in(&order, &entries), names_in(&full, &entries));
        }
    }
}

/// Устаревший индекс (core отбросил прошлый патч) перестраивается, а не портит патч.
#[test]
fn stale_index_is_rebuilt() {
    let spec = ArrangeSpec {
        sort: SortSpec::default(),
        filter: None,
        show_hidden: true,
    };
    let opts = ArrangeOpts::default();
    let entries = vec![
        entry("a", false, 1, 0, false),
        entry("b", false, 1, 0, false),
        entry("c", false, 1, 0, false),
    ];
    let order = arrange(&entries, &spec, opts);
    let mut index = DirIndex::build(&entries, Gen(1));
    // Патч, который core «не применил»: индекс уехал вперёд.
    let ch = Changes {
        upsert: vec![entry("d", false, 1, 0, false)],
        removed: vec!["a".into()],
    };
    let _ = build_patch(&entries, &order, &mut index, &ch, &spec, opts, 0).unwrap();
    // Следующий патч — к прежним записям.
    let ch = Changes {
        upsert: vec![entry("c", false, 9, 0, false)],
        removed: vec![],
    };
    let p = build_patch(&entries, &order, &mut index, &ch, &spec, opts, 0).unwrap();
    assert_eq!(p.update.len(), 1);
    assert_eq!(p.update[0].0, 2, "index of `c` in the unpatched entries");
}

#[test]
fn nothing_to_do() {
    let spec = ArrangeSpec {
        sort: SortSpec::default(),
        filter: None,
        show_hidden: true,
    };
    let entries = vec![entry("a", false, 1, 0, false)];
    let mut index = DirIndex::build(&entries, Gen(1));
    let ch = Changes {
        upsert: vec![],
        removed: vec!["missing".into()],
    };
    assert!(
        build_patch(
            &entries,
            &[0],
            &mut index,
            &ch,
            &spec,
            ArrangeOpts::default(),
            0
        )
        .is_none()
    );
}
