//! Поиск вкладки `pkg:` (F02-09, `screen-pkgsearch`): запрос, область («Все» или один
//! менеджер), ответы менеджеров по мере готовности ([`SearchMerge`]), строки — группа
//! программы и её варианты (менеджер, версия, источник), свёрнутые группы.
//!
//! Модель живёт в [`crate::PkgBook`]; ответы пишет handler в UI-потоке (слияние —
//! память, без I/O), строки пересчитываются там же.

use std::collections::HashSet;
use std::sync::Arc;

use compact_str::{CompactString, format_compact};
pub use urfin_core::pkg::{Answer, SearchInfo, SearchKind, SearchRow, SearchView};

use crate::api::{Hit, Mgr, PkgError};
use crate::book::Filter;
use crate::config::SearchIn;
use crate::merge::SearchMerge;

/// Состояние поиска (под блокировкой данных книги).
#[derive(Debug)]
pub(crate) struct Search {
    pub q: CompactString,
    pub scope: SearchIn,
    pub merge: SearchMerge,
    pub answers: Vec<(Mgr, Answer)>,
    pub closed: HashSet<u32>,
    pub generation: u64,
    pub back: Option<Filter>,
}

impl Search {
    pub fn new(order: &[Mgr], scope: SearchIn) -> Search {
        Search {
            q: CompactString::default(),
            scope,
            merge: SearchMerge::new(order),
            answers: Vec::new(),
            closed: HashSet::new(),
            generation: 0,
            back: None,
        }
    }

    /// Новый запрос `q` к менеджерам `mgrs`: прежние ответы уходят.
    pub fn start(&mut self, q: &str, mgrs: &[Mgr], order: &[Mgr]) -> u64 {
        self.generation += 1;
        self.q = q.into();
        self.merge = SearchMerge::new(order);
        self.closed.clear();
        self.answers = mgrs.iter().map(|m| (*m, Answer::Busy)).collect();
        self.generation
    }

    /// Новый порядок менеджеров при открытых результатах (F02-12).
    pub fn reorder(&mut self, order: &[Mgr]) {
        self.merge.reorder(order);
        self.answers
            .sort_by_key(|(m, _)| order.iter().position(|o| o == m).unwrap_or(order.len()));
    }

    /// Ответ менеджера на запрос `generation` (устаревший — без изменений, `false`).
    pub fn answer(
        &mut self,
        generation: u64,
        mgr: Mgr,
        res: Result<Vec<Hit>, PkgError>,
        keep: usize,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let a = match res {
            Ok(hits) => {
                let n = hits.len();
                self.merge.add(mgr, hits, keep);
                Answer::Found(n)
            }
            Err(e) => Answer::Failed(e.to_string().into()),
        };
        match self.answers.iter_mut().find(|(m, _)| *m == mgr) {
            Some((_, x)) => *x = a,
            None => self.answers.push((mgr, a)),
        }
        true
    }

    /// Строки: группа, затем её варианты (если раскрыта).
    pub fn view(&self) -> Arc<SearchView> {
        let mut list = Vec::new();
        let mut variants = 0;
        for g in self.merge.groups() {
            let open = !self.closed.contains(&g.id);
            let rec = g.recommended();
            list.push(SearchRow {
                name: g.title.clone(),
                group: g.id,
                kind: SearchKind::Group {
                    open,
                    count: g.variants.len(),
                    publisher: g.variants.iter().find_map(|v| v.hit.pkg.publisher.clone()),
                    recommended: rec.pkg.id.clone(),
                },
            });
            variants += g.variants.len();
            if !open {
                continue;
            }
            for (i, v) in g.variants.iter().enumerate() {
                // scoop — имя без бакета (`screen-pkgsearch`: «7zip», «nanazip»).
                let id = &v.hit.pkg.id;
                let name = match id.mgr {
                    Mgr::Scoop => crate::scoop::app_name(&id.id).into(),
                    _ => id.id.clone(),
                };
                list.push(SearchRow {
                    name,
                    group: g.id,
                    kind: SearchKind::Variant {
                        hit: v.hit.clone(),
                        recommended: i == 0,
                        newest: v.newest,
                    },
                });
            }
        }
        let mut seen: HashSet<CompactString> = HashSet::with_capacity(list.len());
        for r in &mut list {
            if seen.insert(r.name.clone()) {
                continue;
            }
            let tag = match &r.kind {
                SearchKind::Variant { hit, .. } => hit.pkg.id.mgr.as_str(),
                SearchKind::Group { .. } => "",
            };
            let base = r.name.clone();
            let mut n = 2;
            r.name = if tag.is_empty() {
                format_compact!("{base} ({n})")
            } else {
                format_compact!("{base} ({tag})")
            };
            while !seen.insert(r.name.clone()) {
                r.name = format_compact!("{base} ({n})");
                n += 1;
            }
        }
        Arc::new(SearchView::new(
            SearchInfo {
                q: self.q.clone(),
                scope: self.scope,
                answers: self.answers.clone(),
                groups: self.merge.groups().len(),
                variants,
                generation: self.generation,
                back: self.back,
            },
            list,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{MatchKind, Package, PkgId};

    fn hit(mgr: Mgr, id: &str, name: &str, version: &str) -> Hit {
        let id = PkgId { mgr, id: id.into() };
        Hit {
            matched: MatchKind::of("7zip", &id.id, name),
            pkg: Package {
                publisher: (mgr == Mgr::Winget).then(|| "Igor Pavlov".into()),
                source: Some(if mgr == Mgr::Winget { "winget" } else { "main" }.into()),
                id,
                name: name.into(),
                version: version.into(),
            },
        }
    }

    fn names(s: &Search) -> Vec<String> {
        s.view().list.iter().map(|r| r.name.to_string()).collect()
    }

    /// Ответы по одному; устаревший ответ не меняет поиск; свёрнутая группа без
    /// вариантов; пустой ответ и ошибка различимы.
    #[test]
    fn answers_and_rows() {
        let mut s = Search::new(&Mgr::ALL, SearchIn::All);
        let g = s.start("7zip", &[Mgr::Winget, Mgr::Scoop, Mgr::Choco], &Mgr::ALL);
        assert!(s.view().info.busy());
        assert!(s.answer(
            g,
            Mgr::Scoop,
            Ok(vec![hit(Mgr::Scoop, "main/7zip", "7zip", "24.08")]),
            0
        ));
        assert_eq!(names(&s), ["7zip", "7zip (scoop)"]);
        assert!(s.answer(
            g,
            Mgr::Winget,
            Ok(vec![hit(Mgr::Winget, "7zip.7zip", "7-Zip", "24.08")]),
            0
        ));
        assert_eq!(names(&s), ["7-Zip", "7zip.7zip", "7zip"]);
        let v = s.view();
        assert!(matches!(
            &v.list[1].kind,
            SearchKind::Variant {
                recommended: true,
                newest: true,
                ..
            }
        ));
        assert_eq!(v.row("7zip").unwrap().target().id, "main/7zip");
        assert_eq!(v.keep_for("7zip"), 1);
        assert_eq!(v.row("7-Zip").unwrap().target().id, "7zip.7zip");
        assert!(!s.answer(g - 1, Mgr::Choco, Ok(Vec::new()), 0));
        s.answer(g, Mgr::Choco, Err(PkgError::Timeout), 0);
        let i = s.view().info.clone();
        assert!(!i.busy() && !i.empty());
        assert_eq!(
            i.answers[2],
            (Mgr::Choco, Answer::Failed("timed out".into()))
        );
        s.closed.insert(v.list[0].group);
        assert_eq!(names(&s), ["7-Zip"]);

        let g = s.start("zzz", &[Mgr::Winget, Mgr::Scoop], &Mgr::ALL);
        s.answer(g, Mgr::Winget, Ok(Vec::new()), 0);
        assert!(!s.view().info.empty());
        s.answer(g, Mgr::Scoop, Ok(Vec::new()), 0);
        assert!(s.view().info.empty());
    }

    /// Совпавшие имена строк — с менеджером в скобках.
    #[test]
    fn unique_names() {
        let mut s = Search::new(&Mgr::ALL, SearchIn::All);
        let g = s.start("git", &[Mgr::Scoop, Mgr::Choco], &Mgr::ALL);
        s.answer(
            g,
            Mgr::Scoop,
            Ok(vec![hit(Mgr::Scoop, "git", "git", "2.47")]),
            0,
        );
        s.answer(
            g,
            Mgr::Choco,
            Ok(vec![hit(Mgr::Choco, "git", "git", "2.47")]),
            0,
        );
        assert_eq!(names(&s), ["git", "git (scoop)", "git (choco)"]);
    }
}
