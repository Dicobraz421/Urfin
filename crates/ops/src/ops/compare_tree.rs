//! Рекурсивное сравнение `ops.compare { recursive }` (S8 §4, S5 §4.13; макет D-12,
//! решение 2026-11-11 «cmp»): обе стороны обходятся вместе одним `walk_with` — листер
//! читает каталог слева и справа, сравнивает записи и отдаёт обходчику только общие
//! подкаталоги. Папка, которой нет с другой стороны, — одна строка без обхода внутрь.
//!
//! Различия пишутся в `SearchStore` (`roots` — левая и правая сторона), панель — `cmp:/<id>`
//! (`urfin-search`). Строка — путь от корня сравнения (`crates\ops\src\compare.rs`),
//! `Hit.status` — [`ONLY_LEFT`]…[`DIFFERS`], `Hit.root` — сторона строки: где файл есть,
//! у пары — более новая, у `differs` — левая. У пары вторая строка того же файла (тот же
//! `root` и имя) — `Entry` другой стороны: размер и дата для статуса окна. Равные файлы и
//! общие папки не пишутся. Строки дописываются в конце обхода одним блоком, по пути.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use compact_str::CompactString;
use urfin_core::search::{Hit, SearchStore};
use urfin_core::{CancelToken, Entry, EntryKind, Location};
use urfin_vfs::VfsError;
use urfin_vfs::walk::{
    DirLister, DirRef, DirVisitor, Parent, RawEntry, RawName, WalkOpts, walk_with,
};

use crate::names::name_key;
use crate::remote::End;

/// Файл или папка только слева.
pub const ONLY_LEFT: u8 = 1;
/// Только справа.
pub const ONLY_RIGHT: u8 = 2;
/// С обеих сторон, слева новее.
pub const NEWER_LEFT: u8 = 3;
/// Справа новее.
pub const NEWER_RIGHT: u8 = 4;
/// Время равно с допуском, размер или содержимое — нет.
pub const DIFFERS: u8 = 5;

/// Имя статуса в контракте и событиях (`only_left`, …); 0 и прочее — `None`.
pub fn status_name(s: u8) -> Option<&'static str> {
    Some(match s {
        ONLY_LEFT => "only_left",
        ONLY_RIGHT => "only_right",
        NEWER_LEFT => "newer_left",
        NEWER_RIGHT => "newer_right",
        DIFFERS => "differs",
        _ => return None,
    })
}

/// Итоги: строки по статусам и равные пары файлов.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub only_left: u32,
    pub only_right: u32,
    pub newer_left: u32,
    pub newer_right: u32,
    pub differs: u32,
    pub equal: u32,
}

impl Totals {
    /// Строк в панели: различия всех видов.
    pub fn rows(&self) -> u32 {
        self.only_left + self.only_right + self.newer_left + self.newer_right + self.differs
    }
}

/// Сторона сравнения: чем читать и откуда.
#[derive(Clone, Debug)]
pub(crate) struct Side {
    pub end: End,
    pub root: Location,
}

/// Что нужно сравнению деревьев.
pub(crate) struct TreeCompare<'a> {
    pub left: &'a Side,
    pub right: &'a Side,
    /// Допуск времени изменения, нс (обе стороны).
    pub tol: i64,
    /// Пары одного размера и времени — ещё и по содержимому (xxh3).
    pub content: bool,
    pub store: &'a SearchStore,
    pub threads: usize,
    pub halted: &'a (dyn Fn() -> bool + Sync),
}

/// Каталог пары: адреса сторон и путь от корня с разделителем на конце.
struct PairDir {
    left: Location,
    right: Location,
    prefix: CompactString,
}

/// Строка до сортировки: ключ пути, строка стороны и (у пары) вторая сторона.
struct Row {
    key: CompactString,
    main: Hit,
    other: Option<Hit>,
}

#[derive(Default)]
struct Counters {
    only_left: AtomicU32,
    only_right: AtomicU32,
    newer_left: AtomicU32,
    newer_right: AtomicU32,
    differs: AtomicU32,
    equal: AtomicU32,
}

/// Листер пары: метка каталога — номер в `dirs`; общий подкаталог отдаётся обходчику с
/// номером ребёнка в `RawEntry::file_id` (визитор возвращает его меткой).
struct PairLister<'a> {
    run: &'a TreeCompare<'a>,
    dirs: Mutex<Vec<PairDir>>,
    rows: Mutex<Vec<Row>>,
    counts: Counters,
    /// Имена без учёта регистра: хотя бы одна сторона — локальный диск.
    fold: bool,
}

/// Ключ сортировки «по пути»: без учёта регистра, разделитель — раньше любого символа,
/// поэтому содержимое папки идёт сразу за ней.
fn path_key(rel: &str) -> CompactString {
    name_key(rel).replace('\\', "\u{1}").into()
}

fn rel_entry(prefix: &str, e: &Entry) -> Entry {
    let mut name = CompactString::with_capacity(prefix.len() + e.name.len());
    name.push_str(prefix);
    name.push_str(&e.name);
    Entry {
        name,
        size: if e.kind.is_dir_like() { 0 } else { e.size },
        ..e.clone()
    }
}

fn hit(e: Entry, root: u16, status: u8) -> Hit {
    Hit {
        e,
        root,
        line: 0,
        hl: (0, 0),
        preview: None,
        status,
    }
}

/// Содержимое двух файлов одинаково: локальные — сначала первые 64 КБ, затем целиком;
/// остальные — xxh3 целиком через `open_read`. `None` — не прочитать.
pub(crate) fn same_files(a: (&End, &Location), b: (&End, &Location)) -> Option<bool> {
    if a.0.is_local() && b.0.is_local() {
        return super::compare::same_content(&a.1.path, &b.1.path);
    }
    let hash = |end: &End, at: &Location| -> Option<u128> {
        let mut file = end.open_read(at).ok()?;
        let mut hasher = xxhash_rust::xxh3::Xxh3::new();
        let mut buf = vec![0u8; 256 << 10];
        loop {
            match std::io::Read::read(&mut file, &mut buf).ok()? {
                0 => break,
                got => hasher.update(&buf[..got]),
            }
        }
        Some(hasher.digest128())
    };
    Some(hash(a.0, a.1)? == hash(b.0, b.1)?)
}

impl PairLister<'_> {
    fn key(&self, name: &str) -> CompactString {
        if self.fold {
            name_key(name)
        } else {
            name.into()
        }
    }

    fn count(&self, status: u8) {
        let c = &self.counts;
        let n = match status {
            ONLY_LEFT => &c.only_left,
            ONLY_RIGHT => &c.only_right,
            NEWER_LEFT => &c.newer_left,
            NEWER_RIGHT => &c.newer_right,
            _ => &c.differs,
        };
        n.fetch_add(1, Ordering::Relaxed);
    }

    /// Строка только одной стороны (`root`: 0 — слева, 1 — справа).
    fn only(&self, out: &mut Vec<Row>, prefix: &str, e: &Entry, root: u16) {
        let status = if root == 0 { ONLY_LEFT } else { ONLY_RIGHT };
        self.count(status);
        let e = rel_entry(prefix, e);
        out.push(Row {
            key: path_key(&e.name),
            main: hit(e, root, status),
            other: None,
        });
    }

    /// Пара файлов: статус или «равны».
    fn pair(&self, out: &mut Vec<Row>, dir: &PairDir, l: &Entry, r: &Entry) {
        let run = self.run;
        let dt = i128::from(l.mtime) - i128::from(r.mtime);
        let tol = i128::from(run.tol);
        let status = if dt > tol {
            NEWER_LEFT
        } else if dt < -tol {
            NEWER_RIGHT
        } else if l.size != r.size {
            DIFFERS
        } else if run.content && !(run.halted)() {
            let a = run.left.end.join(&dir.left, &l.name);
            let b = run.right.end.join(&dir.right, &r.name);
            if same_files((&run.left.end, &a), (&run.right.end, &b)) == Some(false) {
                DIFFERS
            } else {
                0
            }
        } else {
            0
        };
        if status == 0 {
            self.counts.equal.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.count(status);
        let (le, re) = (rel_entry(&dir.prefix, l), rel_entry(&dir.prefix, r));
        let (main, other, root) = if status == NEWER_RIGHT {
            (re, le, 1)
        } else {
            (le, re, 0)
        };
        out.push(Row {
            key: path_key(&main.name),
            main: hit(main, root, status),
            other: Some(hit(other, root, status)),
        });
    }
}

impl DirLister for PairLister<'_> {
    fn list(
        &self,
        dir: &DirRef,
        out: &mut dyn FnMut(&RawEntry<'_>) -> bool,
    ) -> Result<Option<Parent>, VfsError> {
        let (left, right, prefix) = {
            let dirs = self.dirs.lock().unwrap_or_else(PoisonError::into_inner);
            let d = &dirs[dir.tag as usize];
            (d.left.clone(), d.right.clone(), d.prefix.clone())
        };
        let run = self.run;
        let l = run.left.end.list(&left, &CancelToken::new())?;
        let r = run.right.end.list(&right, &CancelToken::new())?;
        let mut by_key: HashMap<CompactString, (Option<&Entry>, Option<&Entry>)> =
            HashMap::with_capacity(l.len().max(r.len()));
        for e in &l {
            by_key.entry(self.key(&e.name)).or_default().0 = Some(e);
        }
        for e in &r {
            by_key.entry(self.key(&e.name)).or_default().1 = Some(e);
        }
        let here = PairDir {
            left,
            right,
            prefix,
        };
        let mut rows = Vec::new();
        let mut kids = Vec::new();
        let mut files = 0u64;
        for (_, pair) in by_key {
            if (run.halted)() {
                break;
            }
            match pair {
                (Some(a), Some(b)) => {
                    let (da, db) = (a.kind.is_dir_like(), b.kind.is_dir_like());
                    if a.kind == EntryKind::Dir && b.kind == EntryKind::Dir {
                        kids.push((a.clone(), b.clone()));
                    } else if da && db {
                        // Ссылки на каталоги не обходятся и не сравниваются.
                    } else if da || db {
                        // Папка с одной стороны, файл с другой — две строки.
                        self.only(&mut rows, &here.prefix, a, 0);
                        self.only(&mut rows, &here.prefix, b, 1);
                    } else {
                        files += 1;
                        self.pair(&mut rows, &here, a, b);
                    }
                }
                (Some(a), None) => {
                    files += u64::from(!a.kind.is_dir_like());
                    self.only(&mut rows, &here.prefix, a, 0);
                }
                (None, Some(b)) => {
                    files += u64::from(!b.kind.is_dir_like());
                    self.only(&mut rows, &here.prefix, b, 1);
                }
                (None, None) => {}
            }
        }
        run.store.info.files.fetch_add(files, Ordering::Relaxed);
        if !rows.is_empty() {
            self.rows
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .append(&mut rows);
        }
        for (a, b) in kids {
            let tag = {
                let mut dirs = self.dirs.lock().unwrap_or_else(PoisonError::into_inner);
                let mut prefix = here.prefix.clone();
                prefix.push_str(&a.name);
                prefix.push('\\');
                dirs.push(PairDir {
                    left: run.left.end.join(&here.left, &a.name),
                    right: run.right.end.join(&here.right, &b.name),
                    prefix,
                });
                dirs.len() - 1
            };
            let raw = RawEntry {
                name: RawName::Str {
                    name: &a.name,
                    escaped: false,
                },
                kind: EntryKind::Dir,
                logical: 0,
                disk: 0,
                mtime: a.mtime,
                file_id: tag as u64,
                flags: 0,
            };
            if !out(&raw) {
                break;
            }
        }
        Ok(None)
    }
}

/// Входит в каждый общий подкаталог: метку дал листер.
struct Kids<'a>(&'a SearchStore);

impl DirVisitor for Kids<'_> {
    type Acc = ();

    fn entry(&self, _dir: &DirRef, _acc: &mut (), e: &RawEntry<'_>) -> Option<u32> {
        u32::try_from(e.file_id).ok()
    }

    fn done(&self, dir: &DirRef, _acc: (), res: Result<(), VfsError>) {
        let info = &self.0.info;
        info.dirs.fetch_add(1, Ordering::Relaxed);
        match res {
            Ok(()) | Err(VfsError::Cancelled) => {}
            Err(e) => {
                info.denied.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(target: "ops", dir = %dir.loc.path.display(), error = %e, "compare: folder skipped");
            }
        }
    }
}

/// Сравнить деревья: блокирует поток до конца или отмены; строки — в стор одним блоком,
/// стор не закрывается (`finish` — у вызывающего). `tick` — в этом потоке.
pub(crate) fn compare_tree(run: &TreeCompare<'_>, tick: impl FnMut()) -> Totals {
    let lister = PairLister {
        run,
        dirs: Mutex::new(vec![PairDir {
            left: run.left.root.clone(),
            right: run.right.root.clone(),
            prefix: CompactString::default(),
        }]),
        rows: Mutex::default(),
        counts: Counters::default(),
        fold: run.left.end.is_local() || run.right.end.is_local(),
    };
    let opts = WalkOpts {
        threads: run.threads.max(1),
        first_tick: Duration::from_millis(16),
        period: Duration::from_millis(34),
    };
    let roots = vec![DirRef::root(run.left.root.clone(), 0)];
    walk_with(&lister, roots, opts, run.halted, &Kids(run.store), tick);
    let mut rows = std::mem::take(&mut *lister.rows.lock().unwrap_or_else(PoisonError::into_inner));
    rows.sort_unstable_by(|a, b| match a.key.cmp(&b.key) {
        CmpOrdering::Equal => a.main.root.cmp(&b.main.root),
        o => o,
    });
    let mut hits = Vec::with_capacity(rows.len() * 2);
    for r in rows {
        hits.push(r.main);
        hits.extend(r.other);
    }
    run.store.append(hits);
    let c = &lister.counts;
    let n = |a: &AtomicU32| a.load(Ordering::Relaxed);
    Totals {
        only_left: n(&c.only_left),
        only_right: n(&c.only_right),
        newer_left: n(&c.newer_left),
        newer_right: n(&c.newer_right),
        differs: n(&c.differs),
        equal: n(&c.equal),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use urfin_core::search::SearchModel;

    use super::*;

    fn set_time(p: &Path, secs: u64) {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn put(root: &Path, rel: &str, data: &[u8], secs: u64) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, data).unwrap();
        set_time(&p, secs);
    }

    fn run(l: &Path, r: &Path, content: bool) -> (Totals, Vec<(String, u16, u8, u64)>) {
        let mut m = SearchModel::default();
        let store = m.open(vec![Location::local(l), Location::local(r)]);
        let (left, right) = (
            Side {
                end: End::Local,
                root: Location::local(l),
            },
            Side {
                end: End::Local,
                root: Location::local(r),
            },
        );
        let halted = || false;
        let tc = TreeCompare {
            left: &left,
            right: &right,
            tol: 0,
            content,
            store: &store,
            threads: 2,
            halted: &halted,
        };
        let t = compare_tree(&tc, || {});
        let rows = store
            .rows(0..usize::MAX)
            .iter()
            .map(|h| (h.e.name.to_string(), h.root, h.status, h.e.size))
            .collect();
        (t, rows)
    }

    #[test]
    fn tree_statuses_order_and_second_side() {
        let d = tempfile::Builder::new().prefix("urfin-").tempdir().unwrap();
        let (l, r) = (d.path().join("l"), d.path().join("r"));
        put(&l, r"same\a.txt", b"abc", 100);
        put(&r, r"same\A.TXT", b"abc", 100);
        put(&l, r"same\newer.txt", b"left", 200);
        put(&r, r"same\newer.txt", b"right!", 100);
        put(&l, r"same\older.txt", b"x", 100);
        put(&r, r"same\older.txt", b"yy", 200);
        put(&l, r"same\size.txt", b"1", 100);
        put(&r, r"same\size.txt", b"22", 100);
        put(&l, r"same\body.txt", b"aa", 100);
        put(&r, r"same\body.txt", b"ab", 100);
        put(&l, r"only-left\deep\x.txt", b"x", 100);
        put(&r, r"only-right.txt", b"x", 100);

        let (t, rows) = run(&l, &r, false);
        assert_eq!(
            (
                t.only_left,
                t.only_right,
                t.newer_left,
                t.newer_right,
                t.differs,
                t.equal
            ),
            (1, 1, 1, 1, 1, 2),
            "{rows:?}"
        );
        let names: Vec<_> = rows.iter().map(|r| (r.0.as_str(), r.1, r.2)).collect();
        assert_eq!(
            names,
            [
                ("only-left", 0, ONLY_LEFT),
                ("only-right.txt", 1, ONLY_RIGHT),
                (r"same\newer.txt", 0, NEWER_LEFT),
                (r"same\newer.txt", 0, NEWER_LEFT),
                (r"same\older.txt", 1, NEWER_RIGHT),
                (r"same\older.txt", 1, NEWER_RIGHT),
                (r"same\size.txt", 0, DIFFERS),
                (r"same\size.txt", 0, DIFFERS),
            ]
        );
        // Вторая строка пары — другая сторона: у `newer_left` — правый файл.
        assert_eq!((rows[2].3, rows[3].3), (4, 6));
        assert_eq!((rows[4].3, rows[5].3), (2, 1));

        let (t, _) = run(&l, &r, true);
        assert_eq!((t.differs, t.equal), (2, 1), "content: body.txt differs");
    }
}
