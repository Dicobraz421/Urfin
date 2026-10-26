//! Inline-список над cmdline (S6 §3.1 слой «inline-оверлеи»): история Alt+F8 (S6-16) и
//! варианты автокомплита (S6-17). Модель без UI: фильтр, выбор, прокрутка; окно
//! рисует [`Menu::window`].
//!
//! Фильтр истории — сначала подстрока, затем fuzzy (буквы по порядку), без учёта
//! регистра; новые — выше. На 5000 записей — доли миллисекунды на нажатие: записи
//! хранят строку в нижнем регистре.

use compact_str::CompactString;

use crate::complete::Candidate;
use crate::history::{Entry, Store};

/// Сколько строк видно в списке (S6 §6.2: до 12).
pub const ROWS: usize = 12;

/// Что за список.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// Alt+F8: фильтр, «этот shell / все shells».
    History { filter: String, all: bool },
    /// Tab: строка до комплита (Esc — вернуть).
    Complete { base: String, cursor: usize },
}

/// Пункт списка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub text: String,
    /// Подпись справа: shell (все shells) или источник варианта.
    pub hint: CompactString,
    pub lock: bool,
    pub native: bool,
    /// Запись истории: shell и номер.
    pub at: Option<(CompactString, usize)>,
}

#[derive(Clone, Debug)]
pub struct Menu {
    pub kind: MenuKind,
    /// Пункты комплита; у истории — пусто (пункты строит [`Menu::rows`] из `hits`).
    pub items: Vec<Item>,
    /// Найденные записи истории: shell и номер записи.
    pub hits: Vec<(CompactString, usize)>,
    pub sel: usize,
    /// Варианты комплита (в порядке `items`).
    pub cands: Vec<Candidate>,
    /// Shell истории.
    pub shell: CompactString,
}

/// Совпадение `needle` с `hay` по буквам в порядке (оба — в нижнем регистре).
pub fn fuzzy(hay: &str, needle: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|n| it.any(|h| h == n))
}

/// Отфильтровать записи: сначала подстрока, затем fuzzy; внутри — новые выше.
/// `entries` — от старых к новым.
pub fn filter<'a>(
    entries: impl DoubleEndedIterator<Item = (&'a CompactString, usize, &'a Entry)> + Clone,
    filter: &str,
) -> Vec<(&'a CompactString, usize, &'a Entry)> {
    let f = filter.to_lowercase();
    if f.is_empty() {
        return entries.rev().collect();
    }
    let mut sub = Vec::new();
    let mut fz = Vec::new();
    for e in entries.rev() {
        if e.2.lower().contains(f.as_str()) {
            sub.push(e);
        } else if fuzzy(e.2.lower(), &f) {
            fz.push(e);
        }
    }
    sub.append(&mut fz);
    sub
}

impl Menu {
    /// Список истории shell `shell` (или всех) по фильтру.
    pub fn history(store: &Store, shell: &str, filter_text: &str, all: bool) -> Menu {
        let mut m = Menu {
            kind: MenuKind::History {
                filter: filter_text.to_owned(),
                all,
            },
            items: Vec::new(),
            hits: Vec::new(),
            sel: 0,
            cands: Vec::new(),
            shell: shell.into(),
        };
        m.refilter(store);
        m
    }

    /// Пересобрать найденное (фильтр, удаление, закрепление).
    pub fn refilter(&mut self, store: &Store) {
        let MenuKind::History { filter: f, all } = &self.kind else {
            return;
        };
        let hits: Vec<(CompactString, usize)> = if *all {
            // Все shells: одним списком по времени.
            let mut merged: Vec<(&CompactString, usize, &Entry)> = store
                .by_shell
                .iter()
                .flat_map(|(k, h)| h.entries().iter().enumerate().map(move |(i, e)| (k, i, e)))
                .collect();
            merged.sort_by_key(|(_, _, e)| e.t);
            filter(merged.into_iter(), f)
                .into_iter()
                .map(|(k, i, _)| (k.clone(), i))
                .collect()
        } else {
            match store.by_shell.get_key_value(self.shell.as_str()) {
                Some((k, h)) => filter(h.entries().iter().enumerate().map(|(i, e)| (k, i, e)), f)
                    .into_iter()
                    .map(|(k, i, _)| (k.clone(), i))
                    .collect(),
                None => Vec::new(),
            }
        };
        self.hits = hits;
        self.sel = self.sel.min(self.len().saturating_sub(1));
    }

    /// Сколько пунктов.
    pub fn len(&self) -> usize {
        if self.is_history() {
            self.hits.len()
        } else {
            self.items.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Пункт `i`: у истории — из записи хранилища.
    pub fn item(&self, store: &Store, i: usize) -> Option<Item> {
        if !self.is_history() {
            return self.items.get(i).cloned();
        }
        let (k, n) = self.hits.get(i)?;
        let e = store.get(k)?.entries().get(*n)?;
        let all = matches!(self.kind, MenuKind::History { all: true, .. });
        Some(item(k, *n, e, all))
    }

    /// Видимые пункты (окно [`Menu::window`]).
    pub fn rows(&self, store: &Store) -> Vec<Item> {
        let (top, n) = self.window();
        (top..top + n).filter_map(|i| self.item(store, i)).collect()
    }

    /// Варианты комплита; выбран первый.
    pub fn complete(base: &str, cursor: usize, cands: Vec<Candidate>) -> Menu {
        let i18n = urfin_i18n::current();
        let items = cands
            .iter()
            .map(|c| Item {
                text: c.label.clone(),
                hint: i18n
                    .get(match c.source {
                        crate::complete::Source::Path if c.dir => "term-complete-dir",
                        crate::complete::Source::Path => "term-complete-file",
                        crate::complete::Source::Command => "term-complete-command",
                        crate::complete::Source::History => "term-complete-history",
                    })
                    .into(),
                lock: false,
                native: false,
                at: None,
            })
            .collect();
        Menu {
            kind: MenuKind::Complete {
                base: base.to_owned(),
                cursor,
            },
            items,
            hits: Vec::new(),
            sel: 0,
            cands,
            shell: CompactString::default(),
        }
    }

    pub fn is_history(&self) -> bool {
        matches!(self.kind, MenuKind::History { .. })
    }

    /// Сдвинуть выбор на `d` (по кругу у комплита, до края у истории).
    pub fn step(&mut self, d: isize) {
        let n = self.len();
        if n == 0 {
            return;
        }
        self.sel = if self.is_history() {
            self.sel.saturating_add_signed(d).min(n - 1)
        } else {
            (self.sel.cast_signed() + d)
                .rem_euclid(n.cast_signed())
                .cast_unsigned()
        };
    }

    /// Видимые строки: первая и сколько (выбранный — в окне).
    pub fn window(&self) -> (usize, usize) {
        let len = self.len();
        let n = len.min(ROWS);
        let top = self.sel.saturating_sub(ROWS - 1).min(len - n);
        (top, n)
    }
}

fn item(k: &CompactString, i: usize, e: &Entry, all: bool) -> Item {
    Item {
        text: e.cmd.replace('\n', " ⏎ "),
        hint: if all {
            k.clone()
        } else {
            CompactString::default()
        },
        lock: e.lock,
        native: e.native,
        at: Some((k.clone(), i)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;

    fn store(cmds: &[&str]) -> Store {
        let mut h = History::default();
        for (i, c) in cmds.iter().enumerate() {
            h.push(Entry::new(c, i as u64 + 1), 10_000);
        }
        let mut s = Store::default();
        s.by_shell.insert("pwsh".into(), h);
        s
    }

    #[test]
    fn substring_then_fuzzy_newest_first() {
        let s = store(&["git status", "get-gitlog", "dir", "git log", "gxixtx"]);
        let m = Menu::history(&s, "pwsh", "GIT", false);
        let got: Vec<String> = m.rows(&s).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["git log", "get-gitlog", "git status", "gxixtx"]);
        let all = Menu::history(&s, "pwsh", "", false);
        assert_eq!(all.item(&s, 0).unwrap().text, "gxixtx");
        assert!(fuzzy("get-childitem", "gci") && !fuzzy("dir", "gci"));
    }

    #[test]
    fn all_shells_by_time() {
        let mut s = store(&["a1", "a2"]);
        let mut h = History::default();
        // Время у всех разное: порядок не зависит от обхода HashMap.
        h.push(Entry::new("b1", 0), 10);
        h.push(Entry::new("b9", 9), 10);
        s.by_shell.insert("cmd".into(), h);
        let m = Menu::history(&s, "pwsh", "", true);
        let got: Vec<(String, String)> = m
            .rows(&s)
            .into_iter()
            .map(|i| (i.text, i.hint.to_string()))
            .collect();
        let want = [("b9", "cmd"), ("a2", "pwsh"), ("a1", "pwsh"), ("b1", "cmd")];
        assert_eq!(got, want.map(|(a, b)| (a.to_owned(), b.to_owned())));
    }

    #[test]
    fn window_follows_selection() {
        let cmds: Vec<String> = (0..30).map(|i| format!("c{i}")).collect();
        let refs: Vec<&str> = cmds.iter().map(String::as_str).collect();
        let mut m = Menu::history(&store(&refs), "pwsh", "", false);
        assert_eq!(m.window(), (0, ROWS));
        m.step(20);
        assert_eq!(m.window(), (20 - (ROWS - 1), ROWS));
        m.step(100);
        assert_eq!(m.sel, 29);
        m.step(-100);
        assert_eq!(m.sel, 0);
    }

    /// Приёмка S6-16: Alt+F8 фильтрует 5000 записей <1 мс на нажатие (release; в
    /// debug — с запасом).
    #[test]
    fn filter_5000_fast() {
        let cmds: Vec<String> = (0..5000)
            .map(|i| format!("git commit -m \"change number {i}\" --author someone"))
            .collect();
        let refs: Vec<&str> = cmds.iter().map(String::as_str).collect();
        let s = store(&refs);
        let mut m = Menu::history(&s, "pwsh", "", false);
        // Худшее нажатие из набора (медиана трёх прогонов — от шума планировщика).
        let mut worst = std::time::Duration::ZERO;
        for f in ["c", "ch", "cha", "chan", "chang", "change 4", "zzz"] {
            m.kind = MenuKind::History {
                filter: f.into(),
                all: false,
            };
            let mut runs = [std::time::Duration::ZERO; 3];
            for r in &mut runs {
                let t = std::time::Instant::now();
                m.refilter(&s);
                let _ = m.rows(&s);
                *r = t.elapsed();
            }
            runs.sort();
            worst = worst.max(runs[1]);
        }
        let limit = if cfg!(debug_assertions) {
            20_000
        } else {
            1_000
        };
        assert!(worst.as_micros() < limit, "{worst:?}");
    }
}
