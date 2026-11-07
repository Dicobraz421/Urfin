//! Элементы виртуальной панели в настоящих адресах (S8-12, S8 §2.5; решение 2026-11-06
//! «s8-answers», В2 А).
//!
//! Строка `search:` — файл где-то под корнем поиска. Ops берёт его адрес у схемы
//! ([`urfin_vfs::Vfs::resolve`]), а не склеивает имя строки с адресом панели. Относительная
//! папка — путь от корня поиска без имени файла: корни — `SearchStore::roots` из
//! `SearchModel` (`AppState.ext`, данные в core), F5 и F6 воссоздают её в назначении. Имя
//! строки при нескольких корнях начинается с корня (`C:\Dev\src\a.rs`) — поэтому папку
//! считаем от настоящего адреса, а не от имени. От `urfin-search` ops не зависит.

use std::path::{Component, Path, PathBuf};

use compact_str::CompactString;
use urfin_core::search::{SearchId, SearchModel};
use urfin_core::{AppState, Entry, Location, Row};
use urfin_vfs::{Caps, Schemes};

/// Схема панели результатов поиска.
const SEARCH: &str = "search";

/// Цель операции в виртуальной панели.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// Имя строки панели (`src\Cargo.toml`): заголовки задач и ошибки.
    pub name: CompactString,
    /// Настоящий путь ОС.
    pub path: PathBuf,
    /// Относительная папка от корня поиска (`src`); пусто — файл в корне.
    pub rel: PathBuf,
}

/// Цели активного таба виртуальной схемы (`Caps::VIRTUAL`, кроме `du:` — у неё свой
/// путь) в настоящих адресах. `None` — таб не такой, или схема не отдаёт локальные
/// адреса: тогда прежний путь (отказ «ещё не сделано»). Пустой список — нечего делать.
pub(crate) fn targets(s: &AppState, current: bool) -> Option<Vec<Found>> {
    let t = s.panel(s.active).tab();
    if t.loc.is_local() || urfin_vfs::du::dvfs::localize(s, &t.loc).is_some() {
        return None;
    }
    let fs = s.ext.get::<Schemes>()?.0.get(&t.loc).ok()?;
    if !fs.caps(&t.loc).has(Caps::VIRTUAL) {
        return None;
    }
    let items: Vec<Entry> = if current {
        match t.current() {
            Some(Row::Entry { entry, .. }) => vec![entry.clone()],
            _ => Vec::new(),
        }
    } else {
        t.targets().map(|tg| tg.items.to_vec()).unwrap_or_default()
    };
    let roots = roots(s, &t.loc);
    items
        .iter()
        .map(|e| {
            let addr = fs.resolve(&t.loc, e).ok().filter(Location::is_local)?;
            Some(Found {
                name: e.name.clone(),
                rel: rel_of(roots, e, &addr.path),
                path: addr.path,
            })
        })
        .collect()
}

/// Корни поиска панели `search:/<id>`.
fn roots<'a>(s: &'a AppState, loc: &Location) -> Option<&'a [Location]> {
    let id = search_id(loc)?;
    s.ext.get::<SearchModel>()?.get(id).map(|st| &*st.roots)
}

/// `search:/<id>[?f=…]` → номер поиска.
fn search_id(loc: &Location) -> Option<SearchId> {
    if loc.backend != SEARCH || loc.outer.is_some() {
        return None;
    }
    let p = loc.path.to_str()?.trim_start_matches(['/', '\\']);
    let end = p.find(['/', '\\', '?']).unwrap_or(p.len());
    p[..end].parse().ok().map(SearchId)
}

/// Относительная папка элемента: путь его каталога от самого длинного корня, внутри
/// которого он лежит. Корней нет (стор уже отпущен) — папка имени строки, если имя
/// относительное. Только обычные компоненты: `..` и корень назначения не покинут.
fn rel_of(roots: Option<&[Location]>, e: &Entry, file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(file);
    let from_root = roots
        .into_iter()
        .flatten()
        .filter(|r| r.is_local())
        .filter_map(|r| {
            let under = parent.strip_prefix(&r.path).ok()?;
            Some((r.path.components().count(), under))
        })
        .max_by_key(|&(n, _)| n);
    if let Some((_, under)) = from_root {
        return plain(under);
    }
    let name = Path::new(e.name.as_str());
    if name.has_root() || name.is_absolute() {
        PathBuf::new()
    } else {
        plain(name.parent().unwrap_or(Path::new("")))
    }
}

/// Только обычные компоненты пути.
fn plain(p: &Path) -> PathBuf {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n),
            _ => None,
        })
        .collect()
}

/// Пары «источник → назначение» для F5 и F6: `dest\<относительная папка>\<имя>`.
pub(crate) fn pairs(found: &[Found], dest: &Path) -> Vec<(PathBuf, PathBuf)> {
    found
        .iter()
        .filter_map(|f| {
            let name = f.path.file_name()?;
            Some((f.path.clone(), dest.join(&f.rel).join(name)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use urfin_core::{Attrs, EntryKind};

    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            atime: 0,
            attrs: Attrs::default(),
            kind: EntryKind::File,
        }
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn found_rel_from_longest_root() {
        let roots = [Location::local(r"C:\Dev"), Location::local(r"C:\Dev\sub")];
        let rel = |name: &str, real: &str| rel_of(Some(&roots), &entry(name), Path::new(real));
        assert_eq!(rel(r"C:\Dev\src\a.rs", r"C:\Dev\src\a.rs"), p("src"));
        assert_eq!(rel(r"C:\Dev\sub\x\b.rs", r"C:\Dev\sub\x\b.rs"), p("x"));
        assert_eq!(rel(r"C:\Dev\c.rs", r"C:\Dev\c.rs"), p(""));
    }

    #[test]
    fn found_rel_without_roots_uses_name() {
        let rel = |name: &str, real: &str| rel_of(None, &entry(name), Path::new(real));
        assert_eq!(
            rel(r"src\core\a.rs", r"C:\Dev\src\core\a.rs"),
            p(r"src\core")
        );
        assert_eq!(rel(r"C:\Dev\src\a.rs", r"C:\Dev\src\a.rs"), p(""));
        assert_eq!(rel(r"..\..\a.rs", r"C:\a.rs"), p(""));
        assert_eq!(rel(r"\x\a.rs", r"C:\x\a.rs"), p(""));
    }

    #[test]
    fn found_search_id_parse() {
        let loc = |path: &str| Location {
            backend: SEARCH.into(),
            authority: None,
            path: path.into(),
            outer: None,
        };
        assert_eq!(search_id(&loc("/3")), Some(SearchId(3)));
        assert_eq!(search_id(&loc("/12?f=errors")), Some(SearchId(12)));
        assert_eq!(search_id(&loc("/x")), None);
        assert_eq!(search_id(&Location::local(r"C:\3")), None);
    }

    #[test]
    fn found_pairs_keep_relative_folder() {
        let f = |path: &str, rel: &str| Found {
            name: "n".into(),
            path: p(path),
            rel: p(rel),
        };
        let got = pairs(
            &[
                f(r"C:\Dev\a\Cargo.toml", "a"),
                f(r"C:\Dev\b\Cargo.toml", "b"),
            ],
            Path::new(r"D:\out"),
        );
        assert_eq!(
            got,
            [
                (p(r"C:\Dev\a\Cargo.toml"), p(r"D:\out\a\Cargo.toml")),
                (p(r"C:\Dev\b\Cargo.toml"), p(r"D:\out\b\Cargo.toml")),
            ]
        );
    }
}
