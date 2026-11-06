//! Фильтр записи (S8 §2.1): дешёвые проверки первыми — вид, имя, размер и время из
//! `RawEntry`, без обращений к ОС. Содержимое — [`crate::content`].
//!
//! Маска проверяет имя. Исключение с разделителем пути (`*.rs|target\*`) проверяет ещё и
//! путь от корня: только так оно может сработать на файл внутри `target`.

use urfin_core::{EntryKind, Mask};

use crate::query::{KindFilter, SearchQuery};

/// Часть маски после `|` вне кавычек.
fn exclude_part(src: &str) -> Option<&str> {
    let mut quoted = false;
    for (i, c) in src.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '|' if !quoted => return Some(&src[i + 1..]),
            _ => {}
        }
    }
    None
}

/// Фильтр запроса: один на обход, читается из всех потоков.
#[derive(Clone, Debug)]
pub struct EntryFilter {
    kinds: KindFilter,
    name: Option<Mask>,
    /// Исключения по пути от корня.
    path_exclude: Option<Mask>,
    size: Option<(u64, u64)>,
    mtime: Option<(i64, i64)>,
    text: bool,
}

impl EntryFilter {
    pub fn new(q: &SearchQuery) -> EntryFilter {
        let path_exclude = q
            .name
            .as_ref()
            .and_then(|m| exclude_part(m.as_str()))
            .filter(|e| e.contains(['\\', '/']))
            .and_then(|e| Mask::parse(e).ok());
        EntryFilter {
            kinds: q.kinds,
            name: q.name.clone(),
            path_exclude,
            size: q.size,
            mtime: q.mtime,
            text: q.text.is_some(),
        }
    }

    /// Вид подходит: проверка до разбора имени. Содержимое есть только у файлов.
    pub fn kind_ok(&self, kind: EntryKind) -> bool {
        if self.text {
            return matches!(kind, EntryKind::File | EntryKind::LinkFile);
        }
        match self.kinds {
            KindFilter::Both => true,
            KindFilter::Dirs => kind.is_dir_like(),
            KindFilter::Files => !kind.is_dir_like(),
        }
    }

    /// Нужен ли путь от корня для [`EntryFilter::name_ok`].
    pub fn wants_path(&self) -> bool {
        self.path_exclude.is_some()
    }

    /// Имя `name` с путём от корня `rel` (пусто, если [`EntryFilter::wants_path`] ложно).
    pub fn name_ok(&self, name: &str, rel: &str) -> bool {
        self.name.as_ref().is_none_or(|m| m.matches(name))
            && !self.path_exclude.as_ref().is_some_and(|m| m.matches(rel))
    }

    /// Размер (только у файлов) и время.
    pub fn meta_ok(&self, kind: EntryKind, size: u64, mtime: i64) -> bool {
        let sized = !kind.is_dir_like();
        (!sized || self.size.is_none_or(|(a, b)| (a..=b).contains(&size)))
            && self.mtime.is_none_or(|(a, b)| (a..=b).contains(&mtime))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn filter(q: &SearchQuery) -> EntryFilter {
        EntryFilter::new(q)
    }

    #[test]
    fn exclude_with_a_path_checks_the_path() {
        let q = SearchQuery {
            name: Mask::parse(r"*.rs,*.toml|target\*").ok(),
            ..SearchQuery::default()
        };
        let f = filter(&q);
        assert!(f.wants_path());
        assert!(f.name_ok("scan.rs", r"src\scan.rs"));
        assert!(!f.name_ok("scan.rs", r"target\debug\scan.rs"));
        assert!(!f.name_ok("readme.md", "readme.md"));
    }

    #[test]
    fn quoted_pipe_is_not_an_exclude() {
        assert_eq!(exclude_part(r#""a|b"|c"#), Some("c"));
        assert_eq!(exclude_part(r#""a|b""#), None);
    }

    #[test]
    fn text_query_takes_real_files_only() {
        let q = SearchQuery {
            kinds: KindFilter::Both,
            text: Some(crate::TextQuery::default()),
            ..SearchQuery::default()
        };
        let f = filter(&q);
        assert!(f.kind_ok(EntryKind::File) && f.kind_ok(EntryKind::LinkFile));
        assert!(!f.kind_ok(EntryKind::Dir) && !f.kind_ok(EntryKind::Other));
    }

    #[test]
    fn size_does_not_apply_to_folders() {
        let q = SearchQuery {
            kinds: KindFilter::Both,
            size: Some((10, 20)),
            ..SearchQuery::default()
        };
        let f = filter(&q);
        assert!(f.meta_ok(EntryKind::Dir, 0, 0));
        assert!(!f.meta_ok(EntryKind::File, 0, 0));
        assert!(f.meta_ok(EntryKind::File, 20, 0));
    }

    /// Наивный разбор той же формы маски: `*.ext` через запятую.
    fn naive_name(exts: &[&str], name: &str) -> bool {
        let lower = name.to_lowercase();
        exts.iter().any(|e| lower.ends_with(&format!(".{e}")))
    }

    fn kind() -> impl Strategy<Value = EntryKind> {
        prop::sample::select(vec![
            EntryKind::File,
            EntryKind::Dir,
            EntryKind::LinkFile,
            EntryKind::LinkDir,
            EntryKind::LinkBroken,
            EntryKind::Other,
        ])
    }

    proptest! {
        #[test]
        fn matcher_equals_the_naive_one(
            kinds in prop::sample::select(vec![KindFilter::Files, KindFilter::Dirs, KindFilter::Both]),
            size in prop::option::of((0u64..64, 0u64..64)),
            mtime in prop::option::of((-8i64..8, -8i64..8)),
            entry in (kind(), "[a-zA-Z]{1,4}\\.(rs|TOML|txt)", 0u64..64, -8i64..8),
        ) {
            let (k, name, s, t) = entry;
            let f = filter(&SearchQuery {
                kinds,
                name: Mask::parse("*.rs,*.toml").ok(),
                size,
                mtime,
                ..SearchQuery::default()
            });
            let got = f.kind_ok(k) && f.name_ok(&name, "") && f.meta_ok(k, s, t);
            let dir = matches!(k, EntryKind::Dir | EntryKind::LinkDir);
            let want = match kinds {
                KindFilter::Files => !dir,
                KindFilter::Dirs => dir,
                KindFilter::Both => true,
            } && naive_name(&["rs", "toml"], &name)
                && (dir || size.is_none_or(|(a, b)| a <= s && s <= b))
                && mtime.is_none_or(|(a, b)| a <= t && t <= b);
            prop_assert_eq!(got, want);
        }
    }
}
