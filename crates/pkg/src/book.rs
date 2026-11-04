//! Модель вкладки `pkg:` (F02 §2.5, F02-08): ответы менеджеров, сводные строки
//! «Установленные» и «Обновления», ход и ошибки менеджеров, кэш `state/pkg/cache.json`.
//!
//! Ответы пишут потоки запросов ([`PkgBook::set_installed`] и др.): строки
//! пересчитываются там же, под блокировкой данных, а не вида. UI-поток берёт готовый
//! вид ([`PkgBook::view`]) — короткая блокировка и `Arc`, без I/O и пересчёта.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use compact_str::CompactString;
pub use jiff::Timestamp;
use serde::{Deserialize, Serialize};
pub use urfin_core::pkg::{Busy, Filter, MgrLine, PkgFeed, PkgInfo, PkgRow, Rows, Run, View};

use crate::api::{Detected, Hit, Mgr, MgrCaps, Package, PkgError, PkgId, Update};
use crate::arp::ArpEntry;
use crate::check::MgrStatus;
use crate::config::{PkgConfig, SearchIn};
use crate::merge::{InstalledMerge, InstalledRow};
use crate::search::Search;

/// Кэш вкладки в `state_dir/pkg`.
pub const CACHE_FILE: &str = "cache.json";

/// Кэш вкладки на диске (`state/pkg/cache.json`): ответы менеджеров последней проверки,
/// ARP для издателей и дата. Открытие вкладки показывает его без запуска процессов.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Timestamp>,
    #[serde(default)]
    pub managers: Vec<MgrStatus>,
    #[serde(default)]
    pub installed: BTreeMap<Mgr, Vec<Package>>,
    #[serde(default)]
    pub updates: BTreeMap<Mgr, Vec<Update>>,
    #[serde(default)]
    pub arp: Vec<ArpEntry>,
}

impl PkgCache {
    /// Нет файла — `None`; битый — `None` с предупреждением в логе.
    pub fn load(path: &Path) -> Option<PkgCache> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes)
            .inspect_err(|e| tracing::warn!(error = %e, path = %path.display(), "pkg: cache"))
            .ok()
    }

    /// Атомарно: временный файл рядом и переименование.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = serde_json::to_vec(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

/// Сырые данные: ответы менеджеров и настройки, от которых зависят строки.
#[derive(Debug, Default)]
struct Data {
    order: Vec<Mgr>,
    important: Vec<PkgId>,
    managers: Vec<MgrLine>,
    /// Найденные с версией и возможностями — в `managers.toml`.
    detected: Vec<Detected>,
    installed: BTreeMap<Mgr, Vec<Package>>,
    updates: BTreeMap<Mgr, Vec<Update>>,
    arp: Vec<ArpEntry>,
    checked_at: Option<Timestamp>,
    loaded: bool,
    checking: bool,
    search: Option<Search>,
    busy: HashMap<PkgId, Busy>,
}

impl Data {
    fn search(&mut self) -> &mut Search {
        let order = self.order.clone();
        self.search
            .get_or_insert_with(|| Search::new(&order, SearchIn::All))
    }
}

/// Модель вкладки `pkg:`: одна на процесс, её делят backend `pkg:` и handler `pkg`;
/// готовый вид публикует в [`PkgFeed`] (его читает UI через `urfin_core::pkg`).
#[derive(Debug, Default)]
pub struct PkgBook {
    data: Mutex<Data>,
    feed: Arc<PkgFeed>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PkgBook {
    pub fn new(cfg: &PkgConfig) -> Arc<PkgBook> {
        let book = PkgBook::default();
        book.configure(cfg);
        Arc::new(book)
    }

    /// Вид для UI и backend'а (без пересчёта).
    pub fn view(&self) -> Arc<View> {
        self.feed.view()
    }

    pub fn info(&self) -> PkgInfo {
        self.feed.info()
    }

    /// Растёт на каждом изменении — ключ перерисовки UI.
    pub fn rev(&self) -> u64 {
        self.feed.rev()
    }

    /// Опубликованный вид — в `urfin_core::pkg::PkgModel`.
    pub fn feed(&self) -> &Arc<PkgFeed> {
        &self.feed
    }

    /// `[pkg].order` и `important`: порядок менеджеров и «важное» сверху.
    pub fn configure(&self, cfg: &PkgConfig) {
        self.update(true, |d| {
            d.order.clone_from(&cfg.order);
            d.important.clone_from(&cfg.important);
            sort_managers(&mut d.managers, &d.order);
            if d.search.is_none() {
                d.search = Some(Search::new(&cfg.order, cfg.search_in));
            }
        });
    }

    // ------------------------------------------------------------ поиск (F02-09)

    /// Новый запрос `q` к менеджерам `mgrs` (в порядке `[pkg].order`): прежние ответы
    /// уходят. Номер запроса — для [`PkgBook::search_answer`].
    pub fn search_start(&self, q: &str, mgrs: &[Mgr]) -> u64 {
        let mut g = 0;
        self.update_with(false, true, |d| {
            let order = d.order.clone();
            g = d.search().start(q, mgrs, &order);
        });
        g
    }

    /// Ответ менеджера на запрос `generation`; `keep` — групп выше курсора и под ним
    /// ([`crate::SearchMerge::add`]). Устаревший ответ — `false`.
    pub fn search_answer(
        &self,
        generation: u64,
        mgr: Mgr,
        res: Result<Vec<Hit>, PkgError>,
        keep: usize,
    ) -> bool {
        let mut ok = false;
        self.update_with(false, true, |d| {
            ok = d.search().answer(generation, mgr, res, keep);
        });
        ok
    }

    /// Область поиска (мешочек «в [Все ▾]»).
    pub fn search_scope(&self, scope: SearchIn) {
        self.update_with(false, true, |d| d.search().scope = scope);
    }

    /// Фильтр, к которому вернёт Esc в строке поиска.
    pub fn search_back(&self, back: Filter) {
        self.update_with(false, true, |d| d.search().back = Some(back));
    }

    /// Свернуть или раскрыть группу (`pkg.pick` на группе).
    pub fn search_toggle(&self, group: u32) {
        self.update_with(false, true, |d| {
            let s = d.search();
            if !s.closed.remove(&group) {
                s.closed.insert(group);
            }
        });
    }

    // ------------------------------------------------------------ действия (F02-10)

    /// Ход действия с пакетом `id`; `None` — действие кончилось.
    pub fn set_busy(&self, id: &PkgId, busy: Option<Busy>) {
        self.update_with(false, false, |d| match busy {
            Some(b) => {
                d.busy.insert(id.clone(), b);
            }
            None => {
                d.busy.remove(id);
            }
        });
    }

    /// Кэш прочитан (`None` — кэша нет): вкладке есть что показать.
    pub fn load(&self, cache: Option<PkgCache>) {
        self.update(true, |d| {
            d.loaded = true;
            let Some(c) = cache else {
                return;
            };
            d.installed = c.installed;
            d.updates = c.updates;
            d.arp = c.arp;
            d.checked_at = c.checked_at;
            d.managers = c
                .managers
                .into_iter()
                .map(|m| MgrLine {
                    mgr: m.mgr,
                    run: match m.error {
                        Some(e) if !m.ok => Run::Failed(e),
                        _ => Run::Idle,
                    },
                })
                .collect();
            sort_managers(&mut d.managers, &d.order);
        });
    }

    /// Снимок для `cache.json`.
    pub fn cache(&self) -> PkgCache {
        let d = lock(&self.data);
        PkgCache {
            checked_at: d.checked_at,
            managers: d
                .managers
                .iter()
                .map(|m| MgrStatus {
                    mgr: m.mgr,
                    ok: !matches!(m.run, Run::Failed(_)),
                    error: match &m.run {
                        Run::Failed(e) => Some(e.clone()),
                        _ => None,
                    },
                })
                .collect(),
            installed: d.installed.clone(),
            updates: d.updates.clone(),
            arp: d.arp.clone(),
        }
    }

    /// Найденные менеджеры с версиями — для `managers.toml`.
    pub fn detected(&self) -> Vec<Detected> {
        lock(&self.data).detected.clone()
    }

    /// Начало проверки: известные менеджеры — «идёт запрос».
    pub fn start(&self) {
        self.update(false, |d| {
            d.loaded = true;
            d.checking = true;
            for m in &mut d.managers {
                m.run = Run::Busy;
            }
        });
    }

    /// Обнаружение (без процессов): найденные — «идёт запрос»; пропавшие уходят вместе
    /// со списками. `arp` — свежие записи ARP для издателей.
    pub fn found(&self, found: Vec<Detected>, arp: Vec<ArpEntry>) {
        self.update(true, |d| {
            d.managers = found
                .iter()
                .map(|f| MgrLine {
                    mgr: f.mgr,
                    run: Run::Busy,
                })
                .collect();
            sort_managers(&mut d.managers, &d.order);
            d.installed.retain(|m, _| found.iter().any(|f| f.mgr == *m));
            d.updates.retain(|m, _| found.iter().any(|f| f.mgr == *m));
            d.detected = found;
            d.arp = arp;
            d.loaded = true;
        });
    }

    /// Найденные менеджеры без начала проверки (поиск до первой проверки): списки и
    /// ход не трогаются.
    pub fn known(&self, found: Vec<Detected>) {
        self.update(false, |d| {
            if d.managers.is_empty() {
                d.managers = found
                    .iter()
                    .map(|f| MgrLine {
                        mgr: f.mgr,
                        run: Run::Idle,
                    })
                    .collect();
                sort_managers(&mut d.managers, &d.order);
            }
            d.detected = found;
        });
    }

    /// Установленные у `mgr`.
    pub fn set_installed(&self, mgr: Mgr, pkgs: Vec<Package>) {
        self.update(true, |d| {
            d.installed.insert(mgr, pkgs);
        });
    }

    /// Обновления у `mgr`; запрос менеджера закончен.
    pub fn set_updates(&self, mgr: Mgr, ups: Vec<Update>, found: Option<Detected>) {
        self.update(true, |d| {
            d.updates.insert(mgr, ups);
            set_run(d, mgr, Run::Idle);
            if let Some(f) = found
                && let Some(x) = d.detected.iter_mut().find(|x| x.mgr == f.mgr)
            {
                *x = f;
            }
        });
    }

    /// Запрос `mgr` не удался: прежние списки остаются.
    pub fn failed(&self, mgr: Mgr, error: CompactString) {
        self.update(false, |d| set_run(d, mgr, Run::Failed(error)));
    }

    /// Проверка закончена (все менеджеры ответили или не удались).
    pub fn finish(&self, at: Timestamp) {
        self.update(false, |d| {
            d.checking = false;
            if d.managers.iter().any(|m| m.run == Run::Idle) || d.managers.is_empty() {
                d.checked_at = Some(at);
            }
        });
    }

    /// Изменение данных; `rows` — строки пересчитать, иначе — только сводку.
    fn update(&self, rows: bool, f: impl FnOnce(&mut Data)) {
        self.update_with(rows, false, f);
    }

    /// Изменение данных: `rows` — пересчитать строки списков, `search` — строки поиска.
    fn update_with(&self, rows: bool, search: bool, f: impl FnOnce(&mut Data)) {
        let mut d = lock(&self.data);
        f(&mut d);
        let was = self.view();
        let rows = if rows { build(&d) } else { was.lists.clone() };
        let search = match &d.search {
            Some(s) if search || was.search.info.generation != s.generation => s.view(),
            _ => Arc::clone(&was.search),
        };
        let busy = if *was.busy == d.busy {
            Arc::clone(&was.busy)
        } else {
            Arc::new(d.busy.clone())
        };
        let info = PkgInfo {
            managers: d.managers.clone(),
            installed: rows[0].list().len(),
            updates: rows[1].list().len(),
            important: rows[1].list().iter().filter(|r| r.important).count(),
            checked_at: d.checked_at.map(nanos),
            loaded: d.loaded,
            checking: d.checking,
        };
        self.feed.publish(View {
            info,
            lists: rows,
            search,
            busy,
        });
    }
}

/// Время проверки для [`PkgInfo::checked_at`]: Unix, наносекунды.
pub(crate) fn nanos(t: Timestamp) -> i64 {
    i64::try_from(t.as_nanosecond()).unwrap_or(i64::MIN)
}

/// Фикстура сцен `--screens` (`ui::demo_pkg`): те же пути, что у ответов менеджеров.
impl urfin_core::pkg::PkgFixture for PkgBook {
    fn important(&self, ids: Vec<PkgId>) {
        self.update(true, |d| d.important = ids);
    }

    fn managers(&self, mgrs: &[Mgr]) {
        let found = mgrs
            .iter()
            .map(|&mgr| Detected {
                mgr,
                exe: format!("{mgr}.exe").into(),
                version: None,
                caps: MgrCaps::empty(),
            })
            .collect();
        self.found(found, Vec::new());
    }

    fn installed(&self, mgr: Mgr, pkgs: Vec<Package>) {
        self.set_installed(mgr, pkgs);
    }

    fn updates(&self, mgr: Mgr, ups: Vec<Update>) {
        self.set_updates(mgr, ups, None);
    }

    fn checked_now(&self) {
        self.finish(Timestamp::now());
    }

    fn search(&self, q: &str, mgrs: &[Mgr]) -> u64 {
        self.search_start(q, mgrs)
    }

    fn answer(&self, generation: u64, mgr: Mgr, hits: Vec<Hit>) {
        self.search_answer(generation, mgr, Ok(hits), 0);
    }

    fn busy(&self, id: &PkgId, busy: Option<Busy>) {
        self.set_busy(id, busy);
    }
}

fn set_run(d: &mut Data, mgr: Mgr, run: Run) {
    if let Some(m) = d.managers.iter_mut().find(|m| m.mgr == mgr) {
        m.run = run;
    }
}

fn sort_managers(m: &mut [MgrLine], order: &[Mgr]) {
    let rank = |x: Mgr| {
        order
            .iter()
            .position(|o| *o == x)
            .unwrap_or(order.len() + x as usize)
    };
    m.sort_by_key(|l| rank(l.mgr));
}

/// Строки «Установленных» и «Обновлений».
///
/// - установленные — сводный список (§2.4) по имени; «Доступна» — обновление того же
///   пакета у его менеджера; у «вне менеджера» обновлений нет;
/// - обновления — ответы менеджеров; дубль winget у программы, которую ведёт choco или
///   scoop (та же программа по псевдонимам §2.4), скрыт — строка своего менеджера
///   ([решение 2026-11-02 «pkg-updates»](../../../docs/decisions/2026-11-02-pkg-updates.md)
///   №1); имя и издатель — из сводной строки того же пакета; «важные» сверху, затем по
///   имени.
fn build(d: &Data) -> [Arc<Rows>; 2] {
    let mut merge = InstalledMerge::new(d.arp.clone());
    for (mgr, list) in &d.installed {
        merge.set(*mgr, list.clone());
    }
    let merged = merge.rows();
    let ups: HashMap<&PkgId, &Update> = d
        .updates
        .values()
        .flatten()
        .map(|u| (&u.pkg.id, u))
        .collect();
    let important = |r: Option<&InstalledRow>, id: &PkgId| {
        d.important.contains(id)
            || r.is_some_and(|r| r.twins.iter().any(|t| d.important.contains(t)))
    };
    let installed: Vec<PkgRow> = merged
        .iter()
        .map(|r| {
            let up = ups.get(&r.pkg.id).filter(|_| !r.outside);
            PkgRow {
                name: r.pkg.name.clone(),
                pkg: r.pkg.clone(),
                available: up.map(|u| u.available.clone()),
                pinned: up.is_some_and(|u| u.pinned),
                outside: r.outside,
                important: important(Some(r), &r.pkg.id),
            }
        })
        .collect();
    let by_id: HashMap<&PkgId, &InstalledRow> = merged.iter().map(|r| (&r.pkg.id, r)).collect();
    let twins: HashSet<&PkgId> = merged.iter().flat_map(|r| r.twins.iter()).collect();
    let owned: HashSet<CompactString> = [Mgr::Choco, Mgr::Scoop]
        .iter()
        .filter_map(|m| d.installed.get(m))
        .flatten()
        .flat_map(crate::merge::program_keys)
        .collect();
    let duplicate = |u: &Update| {
        u.pkg.id.mgr == Mgr::Winget
            && (twins.contains(&u.pkg.id)
                || crate::merge::program_keys(&u.pkg)
                    .iter()
                    .any(|k| owned.contains(k)))
    };
    let mut updates: Vec<PkgRow> = d
        .updates
        .values()
        .flatten()
        .filter(|u| !duplicate(u))
        .map(|u| {
            let row = by_id.get(&u.pkg.id).copied();
            let mut pkg = u.pkg.clone();
            if let Some(r) = row {
                pkg.name.clone_from(&r.pkg.name);
                if pkg.publisher.is_none() {
                    pkg.publisher.clone_from(&r.pkg.publisher);
                }
                if pkg.source.is_none() {
                    pkg.source.clone_from(&r.pkg.source);
                }
            }
            PkgRow {
                name: pkg.name.clone(),
                available: Some(u.available.clone()),
                pinned: u.pinned,
                outside: false,
                important: important(row, &u.pkg.id),
                pkg,
            }
        })
        .collect();
    updates.sort_by_cached_key(|r| (!r.important, r.name.to_lowercase(), r.pkg.id.clone()));
    [Arc::new(Rows::new(installed)), Arc::new(Rows::new(updates))]
}

#[cfg(test)]
#[path = "book_tests.rs"]
mod tests;
