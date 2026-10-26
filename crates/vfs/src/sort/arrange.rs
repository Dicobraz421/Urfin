//! `arrange()` (S4 §4.2–4.3): фильтр (hidden, маска) → `SortRec` → сортировка →
//! порядок индексов `entries`.

use std::cmp::Ordering;

use rayon::prelude::*;
use urfin_core::{ArrangeSpec, Attrs, Entry, SortKey};

use super::keys::{Names, SortRec, mtime_key};
use super::natural::{cmp_with, prefix_key};
use crate::name::ext;

/// Записей, с которых сортировка параллельная (глобальный rayon).
const PAR_SORT: usize = 16 * 1024;
/// Записей, с которых `SortRec` и фильтр строятся параллельно.
const PAR_BUILD: usize = 32 * 1024;

/// Настройки `[vfs]`, влияющие на порядок.
#[derive(Copy, Clone, Debug)]
pub struct ArrangeOpts {
    /// Natural sort (решение владельца №10).
    pub natural: bool,
    /// Dotfiles скрыты. На Windows — `vfs.dotfiles_hidden_on_windows` (решение №8),
    /// на macOS и Linux — всегда.
    pub dotfiles_hidden: bool,
}

impl Default for ArrangeOpts {
    fn default() -> Self {
        ArrangeOpts {
            natural: true,
            dotfiles_hidden: !cfg!(windows),
        }
    }
}

/// Скрыта ли запись при выключенном `show_hidden` (решение владельца №8):
/// `HIDDEN` (на Windows — атрибут, в т. ч. вместе с `SYSTEM`; на macOS —
/// `UF_HIDDEN`), dotfile — по настройке.
pub fn is_hidden(attrs: Attrs, opts: ArrangeOpts) -> bool {
    attrs.has(Attrs::HIDDEN) || (opts.dotfiles_hidden && attrs.has(Attrs::DOTFILE))
}

/// Видима ли запись: hidden и маска (маска — только к файлам, каталоги видны).
pub fn visible(e: &Entry, spec: &ArrangeSpec, opts: ArrangeOpts) -> bool {
    (spec.show_hidden || !is_hidden(e.attrs, opts))
        && (e.kind.is_dir_like() || spec.filter.as_ref().is_none_or(|m| m.matches(&e.name)))
}

/// Порядок видимых записей по `spec`.
pub fn arrange(entries: &[Entry], spec: &ArrangeSpec, opts: ArrangeOpts) -> Vec<u32> {
    let n = u32::try_from(entries.len()).expect("< 2^32 entries");
    if spec.sort.key == SortKey::Unsorted {
        return filtered(entries, spec, opts);
    }
    let rec = |i: u32| {
        let e = &entries[i as usize];
        visible(e, spec, opts).then(|| record(i, e, spec, opts))
    };
    let mut recs: Vec<SortRec> = if entries.len() >= PAR_BUILD {
        (0..n).into_par_iter().filter_map(rec).collect()
    } else {
        (0..n).filter_map(rec).collect()
    };
    sort_recs(entries, &mut recs, spec, opts);
    recs.into_iter().map(|r| r.idx).collect()
}

/// Отфильтрованная identity (`Unsorted`).
fn filtered(entries: &[Entry], spec: &ArrangeSpec, opts: ArrangeOpts) -> Vec<u32> {
    let n = u32::try_from(entries.len()).expect("< 2^32 entries");
    let keep = |&i: &u32| visible(&entries[i as usize], spec, opts);
    if entries.len() >= PAR_BUILD {
        (0..n).into_par_iter().filter(keep).collect()
    } else {
        (0..n).filter(keep).collect()
    }
}

/// `SortRec` записи для ключа `spec.sort.key`.
pub fn record(idx: u32, e: &Entry, spec: &ArrangeSpec, opts: ArrangeOpts) -> SortRec {
    let group = u8::from(!(spec.sort.dirs_first && e.kind.is_dir_like()));
    let (prefix, aux) = match spec.sort.key {
        SortKey::Name | SortKey::Unsorted => (prefix_key(&e.name, opts.natural), 0),
        // Ничьи по расширению (тысячи `.txt`) решает префикс имени, без строк.
        SortKey::Ext => (
            prefix_key(ext(&e.name), opts.natural),
            prefix_key(&e.name, opts.natural),
        ),
        SortKey::Size => (0, e.size),
        SortKey::Mtime => (0, mtime_key(e.mtime)),
    };
    let mut flags = 0;
    if is_hidden(e.attrs, opts) {
        flags |= SortRec::HIDDEN_EFFECTIVE;
    }
    if e.attrs.has(Attrs::ESCAPED_NAME) {
        flags |= SortRec::ESCAPED;
    }
    if spec.sort.key == SortKey::Ext {
        let x = ext(&e.name);
        if x.len() <= 7 && x.is_ascii() && !(opts.natural && x.bytes().any(|b| b.is_ascii_digit()))
        {
            flags |= SortRec::EXT_EXACT;
        }
    }
    SortRec::new(idx, group, prefix, aux, flags)
}

/// Сортировка `SortRec` по §4.2; `names` — имена по `idx`.
pub fn sort_recs(
    names: &(impl Names + Sync + ?Sized),
    recs: &mut [SortRec],
    spec: &ArrangeSpec,
    opts: ArrangeOpts,
) {
    let cmp = |a: &SortRec, b: &SortRec| compare(names, a, b, spec, opts);
    if recs.len() >= PAR_SORT {
        recs.par_sort_unstable_by(cmp);
    } else {
        recs.sort_unstable_by(cmp);
    }
}

/// §4.2: группа (каталоги сверху независимо от `desc`) → первичный ключ →
/// сырые байты имени; `desc` переворачивает всё, кроме группы.
pub fn compare(
    names: &(impl Names + ?Sized),
    a: &SortRec,
    b: &SortRec,
    spec: &ArrangeSpec,
    opts: ArrangeOpts,
) -> Ordering {
    let group = a.group.cmp(&b.group);
    if group != Ordering::Equal {
        return group;
    }
    let ord = key_cmp(names, a, b, spec.sort.key, opts.natural);
    if spec.sort.desc { ord.reverse() } else { ord }
}

fn key_cmp(
    names: &(impl Names + ?Sized),
    a: &SortRec,
    b: &SortRec,
    key: SortKey,
    natural: bool,
) -> Ordering {
    let by_prefix = a.prefix.cmp(&b.prefix);
    let primary = match key {
        SortKey::Name | SortKey::Unsorted => {
            if by_prefix != Ordering::Equal {
                return by_prefix;
            }
            Ordering::Equal
        }
        SortKey::Ext => {
            if by_prefix != Ordering::Equal {
                return by_prefix;
            }
            // Равный префикс у пары: оба `EXT_EXACT` или оба нет (короткое и длинное
            // расширения дают разные префиксы) — порядок остаётся полным.
            let exact = a.flags & b.flags & SortRec::EXT_EXACT != 0;
            if !exact {
                let by_ext = cmp_with(ext(names.name(a.idx)), ext(names.name(b.idx)), natural);
                if by_ext != Ordering::Equal {
                    return by_ext;
                }
            }
            // Префикс имени упорядочен как полное сравнение — разные префиксы решают.
            let by_name = a.aux.cmp(&b.aux);
            if by_name != Ordering::Equal {
                return by_name;
            }
            Ordering::Equal
        }
        SortKey::Size | SortKey::Mtime => a.aux.cmp(&b.aux),
    };
    primary.then_with(|| {
        let (na, nb) = (names.name(a.idx), names.name(b.idx));
        cmp_with(na, nb, natural).then_with(|| na.as_bytes().cmp(nb.as_bytes()))
    })
}

/// Тот же ключ, другое направление: развернуть порядок внутри групп (каталоги
/// остаются сверху) без сравнений. Совпадает с `arrange` при обратном `desc`,
/// потому что порядок полный.
pub fn reverse_within_groups(order: &mut [u32], is_top_group: impl Fn(u32) -> bool) {
    let split = order
        .iter()
        .position(|&i| !is_top_group(i))
        .unwrap_or(order.len());
    let (dirs, rest) = order.split_at_mut(split);
    dirs.reverse();
    rest.reverse();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use urfin_core::{EntryKind, Mask, SortSpec};

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

    fn spec(key: SortKey, desc: bool, dirs_first: bool) -> ArrangeSpec {
        ArrangeSpec {
            sort: SortSpec {
                key,
                desc,
                dirs_first,
            },
            filter: None,
            show_hidden: false,
        }
    }

    /// Эталон: наивный фильтр + `sort_by` по §4.2 без префиксов.
    fn reference(entries: &[Entry], spec: &ArrangeSpec, opts: ArrangeOpts) -> Vec<u32> {
        let mut idx: Vec<u32> = (0..entries.len() as u32)
            .filter(|&i| visible(&entries[i as usize], spec, opts))
            .collect();
        if spec.sort.key == SortKey::Unsorted {
            return idx;
        }
        idx.sort_by(|&x, &y| {
            let (a, b) = (&entries[x as usize], &entries[y as usize]);
            let ga = !(spec.sort.dirs_first && a.kind.is_dir_like());
            let gb = !(spec.sort.dirs_first && b.kind.is_dir_like());
            ga.cmp(&gb).then_with(|| {
                let names = || {
                    cmp_with(&a.name, &b.name, opts.natural)
                        .then_with(|| a.name.as_bytes().cmp(b.name.as_bytes()))
                };
                let o = match spec.sort.key {
                    SortKey::Name | SortKey::Unsorted => names(),
                    SortKey::Ext => {
                        cmp_with(ext(&a.name), ext(&b.name), opts.natural).then_with(names)
                    }
                    SortKey::Size => a.size.cmp(&b.size).then_with(names),
                    SortKey::Mtime => a.mtime.cmp(&b.mtime).then_with(names),
                };
                if spec.sort.desc { o.reverse() } else { o }
            })
        });
        idx
    }

    fn entries() -> impl Strategy<Value = Vec<Entry>> {
        proptest::collection::vec(
            (
                "[aAbB0-9ёе.]{0,6}(\\.(txt|rs|TXT|md))?",
                any::<bool>(),
                0u64..5,
                -3i64..3,
                any::<bool>(),
            )
                .prop_map(|(n, d, s, m, h)| entry(&n, d, s, m, h)),
            0..40,
        )
        // Имена в каталоге уникальны — порядок по ним полный.
        .prop_map(|mut v| {
            let mut seen = std::collections::HashSet::new();
            v.retain(|e| seen.insert(e.name.clone()));
            v
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

    proptest! {
        #[test]
        fn matches_reference(
            es in entries(),
            key in key(),
            desc in any::<bool>(),
            dirs_first in any::<bool>(),
            show_hidden in any::<bool>(),
            natural in any::<bool>(),
            dot in any::<bool>(),
            masked in any::<bool>(),
        ) {
            let mut sp = spec(key, desc, dirs_first);
            sp.show_hidden = show_hidden;
            if masked {
                sp.filter = Some(Mask::parse("*.txt").unwrap());
            }
            let opts = ArrangeOpts { natural, dotfiles_hidden: dot };
            prop_assert_eq!(arrange(&es, &sp, opts), reference(&es, &sp, opts));
        }

        #[test]
        fn reverse_equals_desc(es in entries(), key in key(), dirs_first in any::<bool>()) {
            prop_assume!(key != SortKey::Unsorted);
            let opts = ArrangeOpts::default();
            let mut order = arrange(&es, &spec(key, false, dirs_first), opts);
            reverse_within_groups(&mut order, |i| dirs_first && es[i as usize].kind.is_dir_like());
            prop_assert_eq!(order, arrange(&es, &spec(key, true, dirs_first), opts));
        }
    }

    #[test]
    fn hidden_rules() {
        let es = vec![
            entry("visible", false, 0, 0, false),
            entry("attr_hidden", false, 0, 0, true),
            entry(".dot", false, 0, 0, false),
        ];
        let sp = spec(SortKey::Name, false, true);
        let names = |o: Vec<u32>| {
            o.iter()
                .map(|&i| es[i as usize].name.to_string())
                .collect::<Vec<_>>()
        };
        let win = ArrangeOpts {
            natural: true,
            dotfiles_hidden: false,
        };
        assert_eq!(names(arrange(&es, &sp, win)), [".dot", "visible"]);
        let unix = ArrangeOpts {
            natural: true,
            dotfiles_hidden: true,
        };
        assert_eq!(names(arrange(&es, &sp, unix)), ["visible"]);
    }

    #[test]
    fn large_inputs_take_parallel_path() {
        let es: Vec<Entry> = (0..40_000u32)
            .map(|i| {
                entry(
                    &format!("f{}", (i * 7919) % 40_000),
                    i % 97 == 0,
                    u64::from(i % 13),
                    0,
                    false,
                )
            })
            .collect();
        let sp = spec(SortKey::Name, false, true);
        let opts = ArrangeOpts::default();
        assert_eq!(arrange(&es, &sp, opts), reference(&es, &sp, opts));
    }
}
