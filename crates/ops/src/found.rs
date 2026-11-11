//! Элементы виртуальной панели в настоящих адресах (S8-12, S8 §2.5; решение 2026-11-06
//! «s8-answers», В2 А).
//!
//! Строка `search:` — файл где-то под корнем поиска. Ops берёт его адрес у схемы
//! ([`urfin_vfs::Vfs::resolve`]), а не склеивает имя строки с адресом панели. Относительная
//! папка — путь от корня поиска без имени файла: корни — `SearchStore::roots` из
//! `SearchModel` (`AppState.ext`, данные в core), F5 и F6 воссоздают её в назначении. Имя
//! строки при нескольких корнях начинается с корня (`C:\Dev\src\a.rs`) — поэтому папку
//! считаем от настоящего адреса, а не от имени. От `urfin-search` ops не зависит.
//!
//! Несколько корней (решение 2026-11-08 «s8-copy-roots», вариант Б): папка начинается с
//! метки своего корня (`core\src`, `ops\src`), иначе одинаковые пути разных корней
//! столкнулись бы в назначении. Метка — [`labels`].

use std::path::{Component, Path, PathBuf, Prefix};

use compact_str::CompactString;
use urfin_core::search::{SearchId, SearchModel};
use urfin_core::{AppState, Entry, Location, Row};
use urfin_vfs::{Caps, Schemes};

/// Схема панели результатов поиска.
const SEARCH: &str = "search";
/// Схема панели сравнения (`ops.compare { recursive }`).
const CMP: &str = "cmp";

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
    let bases = roots(s, &t.loc).map(bases);
    items
        .iter()
        .map(|e| {
            let addr = fs.resolve(&t.loc, e).ok().filter(Location::is_local)?;
            Some(Found {
                name: e.name.clone(),
                rel: rel_of(bases.as_deref(), e, &addr.path),
                path: addr.path,
            })
        })
        .collect()
}

/// Локальный корень поиска и его метка в назначении.
#[derive(Debug)]
struct Base<'a> {
    path: &'a Path,
    depth: usize,
    /// Папка корня в назначении; пусто — корень один.
    label: PathBuf,
}

/// Локальные корни с метками ([`labels`] считает по всем корням).
fn bases(roots: &[Location]) -> Vec<Base<'_>> {
    let labels = labels(roots.iter().map(|r| r.path.as_path()));
    roots
        .iter()
        .zip(labels)
        .filter(|(r, _)| r.is_local())
        .map(|(r, label)| Base {
            path: &r.path,
            depth: r.path.components().count(),
            label,
        })
        .collect()
}

/// Метки корней для F5 и F6. Один корень (или все совпадают) — пустые метки, как раньше.
/// Иначе метка — последние `k` частей пути корня, `k` — наименьшее, при котором они не
/// совпадают с последними `k` частями ни одного другого корня. Части — буква диска (`D`)
/// или сервер и шара UNC, затем обычные компоненты: `C:\Dev\core` → `core`;
/// `C:\a\src` и `D:\b\src` → `a\src`, `b\src`; `D:\` → `D`; `C:\x\src` и `D:\x\src` →
/// `C\x\src`, `D\x\src`. Регистр не различается, как в ФС Windows.
///
/// Метка не может быть началом чужой (`a` при `a\src` у корня на другом диске): такая
/// метка берёт ещё часть, а если частей нет — ещё часть берёт длинная. Вложенный корень
/// с меткой-продолжением (`C:\a` → `a`, `C:\a\src` → `a\src`) — та же папка, его не трогаем.
fn labels<'a>(roots: impl Iterator<Item = &'a Path>) -> Vec<PathBuf> {
    let parts: Vec<Vec<String>> = roots.map(parts).collect();
    let lower = |p: &[String]| -> Vec<String> { p.iter().map(|c| c.to_lowercase()).collect() };
    // Разные корни (без учёта регистра) и номер разного корня у каждого.
    let mut uniq: Vec<Vec<String>> = Vec::new();
    let idx: Vec<usize> = parts
        .iter()
        .map(|p| {
            let l = lower(p);
            uniq.iter().position(|u| *u == l).unwrap_or_else(|| {
                uniq.push(l);
                uniq.len() - 1
            })
        })
        .collect();
    if uniq.len() < 2 {
        return vec![PathBuf::new(); parts.len()];
    }
    // Последние `k` частей.
    let tail = |p: &[String], k: usize| p.len() - k.min(p.len());
    let mut k: Vec<usize> = uniq
        .iter()
        .map(|u| {
            (1..=u.len())
                .find(|&k| {
                    uniq.iter()
                        .filter(|o| *o != u)
                        .all(|o| o[tail(o, k)..] != u[tail(u, k)..])
                })
                .unwrap_or(u.len())
        })
        .collect();
    // `k` только растут и не больше длины — цикл конечен; разные метки остаются разными.
    loop {
        let mut grew = false;
        for a in 0..uniq.len() {
            for b in 0..uniq.len() {
                let (ua, ub) = (&uniq[a], &uniq[b]);
                let (la, lb) = (&ua[tail(ua, k[a])..], &ub[tail(ub, k[b])..]);
                let prefix = la.len() < lb.len() && lb.starts_with(la);
                let nested = ub.starts_with(ua) && lb.len() - la.len() == ub.len() - ua.len();
                if !prefix || nested {
                    continue;
                }
                if k[a] < ua.len() {
                    k[a] += 1;
                } else if k[b] < ub.len() {
                    k[b] += 1;
                } else {
                    continue;
                }
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    parts
        .iter()
        .zip(idx)
        .map(|(p, i)| plain(&p[tail(p, k[i])..].iter().collect::<PathBuf>()))
        .collect()
}

/// Части пути для метки: буква диска или сервер и шара UNC, затем обычные компоненты.
fn parts(p: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(pre) => match pre.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    out.push(char::from(d).to_ascii_uppercase().to_string());
                }
                Prefix::UNC(srv, share) | Prefix::VerbatimUNC(srv, share) => {
                    out.extend([srv, share].map(|x| x.to_string_lossy().into_owned()));
                }
                Prefix::Verbatim(x) => out.push(x.to_string_lossy().into_owned()),
                Prefix::DeviceNS(_) => {}
            },
            Component::Normal(n) => out.push(n.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    out
}

/// Корни поиска панели `search:/<id>`.
fn roots<'a>(s: &'a AppState, loc: &Location) -> Option<&'a [Location]> {
    let id = search_id(loc)?;
    s.ext.get::<SearchModel>()?.get(id).map(|st| &*st.roots)
}

/// `search:/<id>[?f=…]` и `cmp:/<id>` → номер стора.
fn search_id(loc: &Location) -> Option<SearchId> {
    if !matches!(loc.backend.as_str(), SEARCH | CMP) || loc.outer.is_some() {
        return None;
    }
    let p = loc.path.to_str()?.trim_start_matches(['/', '\\']);
    let end = p.find(['/', '\\', '?']).unwrap_or(p.len());
    p[..end].parse().ok().map(SearchId)
}

/// Относительная папка элемента: метка корня и путь его каталога от самого длинного
/// корня, внутри которого он лежит. Корней нет (стор уже отпущен) — папка имени строки,
/// если имя относительное. Только обычные компоненты: `..` и корень назначения не покинут.
fn rel_of(bases: Option<&[Base<'_>]>, e: &Entry, file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(file);
    let from_root = bases
        .into_iter()
        .flatten()
        .filter_map(|b| Some((b, parent.strip_prefix(b.path).ok()?)))
        .max_by_key(|(b, _)| b.depth);
    if let Some((b, under)) = from_root {
        return b.label.join(plain(under));
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

/// Имена строк, пары «источник → назначение» и адрес другой стороны (для задачи).
pub(crate) type CmpPairs = (Vec<CompactString>, Vec<(PathBuf, PathBuf)>, Location);

/// F5 и F6 из панели сравнения `cmp:/<id>` (D-12): «источник → назначение» — со стороны
/// строки (её адрес даёт `Vfs::resolve`) на другую сторону по тому же пути от корня
/// сравнения, и адрес другой стороны для названия задачи. `None` — таб не `cmp:`; пусто —
/// нечего или сторона не локальная (запись в архив и на сервер отсюда не делаем).
pub(crate) fn cmp_pairs(s: &AppState, current: bool) -> Option<CmpPairs> {
    let t = s.panel(s.active).tab();
    if t.loc.backend != CMP {
        return None;
    }
    let roots = roots(s, &t.loc)?;
    let [left, right] = roots else {
        return None;
    };
    let fs = s.ext.get::<Schemes>()?.0.get(&t.loc).ok()?;
    let items: Vec<Entry> = if current {
        match t.current() {
            Some(Row::Entry { entry, .. }) => vec![entry.clone()],
            _ => Vec::new(),
        }
    } else {
        t.targets().map(|tg| tg.items.to_vec()).unwrap_or_default()
    };
    let mut names = Vec::new();
    let mut pairs = Vec::new();
    let mut other_root = right.clone();
    for e in &items {
        let Some(src) = fs.resolve(&t.loc, e).ok().filter(Location::is_local) else {
            continue;
        };
        let rel = plain(Path::new(e.name.as_str()));
        let (from_left, to) = if src.path == left.path.join(&rel) {
            (true, right)
        } else {
            (false, left)
        };
        if !to.is_local() || rel.as_os_str().is_empty() {
            continue;
        }
        other_root = if from_left {
            right.clone()
        } else {
            left.clone()
        };
        names.push(e.name.clone());
        pairs.push((src.path, to.path.join(&rel)));
    }
    Some((names, pairs, other_root))
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
        let bases = bases(&roots);
        let rel = |name: &str, real: &str| rel_of(Some(&bases), &entry(name), Path::new(real));
        assert_eq!(rel(r"C:\Dev\src\a.rs", r"C:\Dev\src\a.rs"), p(r"Dev\src"));
        assert_eq!(rel(r"C:\Dev\sub\x\b.rs", r"C:\Dev\sub\x\b.rs"), p(r"sub\x"));
        assert_eq!(rel(r"C:\Dev\c.rs", r"C:\Dev\c.rs"), p("Dev"));
    }

    #[test]
    fn found_rel_one_root_has_no_label() {
        let roots = [Location::local(r"C:\Dev")];
        let bases = bases(&roots);
        let rel = |real: &str| rel_of(Some(&bases), &entry("n"), Path::new(real));
        assert_eq!(rel(r"C:\Dev\src\a.rs"), p("src"));
        assert_eq!(rel(r"C:\Dev\c.rs"), p(""));
    }

    fn labels_of(roots: &[&str]) -> Vec<PathBuf> {
        labels(roots.iter().map(Path::new))
    }

    #[test]
    fn found_labels_last_component() {
        assert_eq!(
            labels_of(&[r"C:\Dev\core", r"C:\Dev\ops"]),
            [p("core"), p("ops")]
        );
        assert_eq!(labels_of(&[r"C:\Dev"]), [p("")]);
        // Один и тот же корень дважды (регистр не важен) — один корень.
        assert_eq!(labels_of(&[r"C:\Dev", r"c:\dev\"]), [p(""), p("")]);
    }

    #[test]
    fn found_labels_same_names_take_parent() {
        assert_eq!(
            labels_of(&[r"C:\a\src", r"D:\b\src", r"C:\docs"]),
            [p(r"a\src"), p(r"b\src"), p("docs")]
        );
        assert_eq!(
            labels_of(&[r"C:\x\src", r"D:\x\src"]),
            [p(r"C\x\src"), p(r"D\x\src")]
        );
        assert_eq!(
            labels_of(&[r"C:\a\Src", r"C:\b\src"]),
            [p(r"a\Src"), p(r"b\src")]
        );
    }

    #[test]
    fn found_labels_drive_and_unc() {
        assert_eq!(labels_of(&[r"D:\", r"E:\"]), [p("D"), p("E")]);
        assert_eq!(labels_of(&[r"D:\", r"C:\Dev"]), [p("D"), p("Dev")]);
        // `D:\` и `C:\D`: у диска вторых частей нет, у папки — есть.
        assert_eq!(labels_of(&[r"D:\", r"C:\D"]), [p("D"), p(r"C\D")]);
        assert_eq!(
            labels_of(&[r"\\srv\share", r"\\srv\other\x"]),
            [p("share"), p("x")]
        );
        assert_eq!(labels_of(&[r"\\?\D:\", r"C:\Dev"]), [p("D"), p("Dev")]);
    }

    #[test]
    fn found_labels_never_prefix_of_another() {
        // `a` — начало `a\src` с другого диска: `D:\a\src\f` и `C:\a\src\f` столкнулись бы.
        assert_eq!(
            labels_of(&[r"C:\a\src", r"E:\b\src", r"D:\a"]),
            [p(r"a\src"), p(r"b\src"), p(r"D\a")]
        );
        // Вложенный корень — та же папка, метки не растут.
        assert_eq!(
            labels_of(&[r"C:\a\src", r"E:\b\src", r"C:\a"]),
            [p(r"a\src"), p(r"b\src"), p("a")]
        );
        // У диска частей больше нет — растёт длинная метка.
        assert_eq!(
            labels_of(&[r"D:\", r"C:\D\x", r"E:\y\x"]),
            [p("D"), p(r"C\D\x"), p(r"y\x")]
        );
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
