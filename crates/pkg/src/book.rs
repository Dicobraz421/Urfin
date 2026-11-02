//! Модель вкладки `pkg:` (F02 §2.5, F02-08): ответы менеджеров, сводные строки
//! «Установленные» и «Обновления», ход и ошибки менеджеров, кэш `state/pkg/cache.json`.
//!
//! Ответы пишут потоки запросов ([`PkgBook::set_installed`] и др.): строки
//! пересчитываются там же, под блокировкой данных, а не вида. UI-поток берёт готовый
//! вид ([`PkgBook::view`]) — короткая блокировка и `Arc`, без I/O и пересчёта.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use compact_str::{CompactString, format_compact};
pub use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::api::{Detected, Mgr, Package, PkgId, Update};
use crate::arp::ArpEntry;
use crate::check::MgrStatus;
use crate::config::PkgConfig;
use crate::merge::{InstalledMerge, InstalledRow};

/// Кэш вкладки в `state_dir/pkg`.
pub const CACHE_FILE: &str = "cache.json";

/// Фильтр вкладки: `pkg:installed`, `pkg:updates` (поиск — F02-09).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    Installed,
    Updates,
}

impl Filter {
    pub const ALL: [Filter; 2] = [Filter::Installed, Filter::Updates];

    /// Путь в `pkg:<путь>` и аргумент `pkg.filter name=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Filter::Installed => "installed",
            Filter::Updates => "updates",
        }
    }

    pub fn parse(s: &str) -> Option<Filter> {
        Filter::ALL.into_iter().find(|f| f.as_str() == s)
    }

    const fn ix(self) -> usize {
        self as usize
    }
}

/// Ход запроса менеджера — чип в строке менеджеров.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Run {
    #[default]
    Idle,
    /// Идёт запрос.
    Busy,
    /// Последний запрос не удался (текст ошибки); прежний список остаётся.
    Failed(CompactString),
}

/// Найденный менеджер и его ход.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MgrLine {
    pub mgr: Mgr,
    pub run: Run,
}

/// Сводка для заголовка, чипов фильтров и статуса.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PkgInfo {
    /// Найденные менеджеры в порядке `[pkg].order`.
    pub managers: Vec<MgrLine>,
    /// Строк в «Установленных».
    pub installed: usize,
    /// Строк в «Обновлениях».
    pub updates: usize,
    /// «Важных» среди обновлений.
    pub important: usize,
    /// Конец последней проверки (хранится в кэше).
    pub checked_at: Option<Timestamp>,
    /// Кэш прочитан или менеджеры уже ответили.
    pub loaded: bool,
    /// Идёт проверка (от Ctrl+R до ответа последнего менеджера).
    pub checking: bool,
}

impl PkgInfo {
    /// Идёт проверка или запрос хотя бы одного менеджера.
    pub fn busy(&self) -> bool {
        self.checking || self.managers.iter().any(|m| m.run == Run::Busy)
    }
}

/// Строка вкладки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgRow {
    /// Имя строки (`Entry.name`): имя программы; совпавшие без учёта регистра — с
    /// менеджером в скобках.
    pub name: CompactString,
    /// Пакет: версия — установленная, издатель — по правилам §2.4.
    pub pkg: Package,
    /// Доступная версия (`accent`).
    pub available: Option<CompactString>,
    /// Закреплён у менеджера: «Обновить все» его пропускает (F02-10).
    pub pinned: bool,
    /// «Вне менеджера» (`secondary`): обновлять не предлагаем.
    pub outside: bool,
    /// В `[pkg].important`: в «Обновлениях» — сверху, с меткой «важное».
    pub important: bool,
}

/// Строки фильтра и индекс по имени.
#[derive(Debug, Default)]
struct Rows {
    list: Vec<PkgRow>,
    index: HashMap<CompactString, usize>,
}

impl Rows {
    /// Имена — уникальные без учёта регистра (курсор и отметка держатся за имя).
    fn new(mut list: Vec<PkgRow>) -> Rows {
        let mut seen: HashSet<CompactString> = HashSet::with_capacity(list.len());
        for r in &mut list {
            if !seen.insert(r.name.to_lowercase()) {
                r.name = format_compact!("{} ({})", r.name, r.pkg.id.mgr);
                if !seen.insert(r.name.to_lowercase()) {
                    r.name = format_compact!("{} ({})", r.name, r.pkg.id.id);
                    seen.insert(r.name.to_lowercase());
                }
            }
        }
        let index = list
            .iter()
            .enumerate()
            .map(|(i, r)| (r.name.clone(), i))
            .collect();
        Rows { list, index }
    }
}

/// Готовый вид вкладки: сводка и строки фильтров.
#[derive(Debug, Default)]
pub struct View {
    pub info: PkgInfo,
    rows: [Arc<Rows>; 2],
}

impl View {
    pub fn rows(&self, f: Filter) -> &[PkgRow] {
        &self.rows[f.ix()].list
    }

    /// Строка фильтра `f` по имени (`Entry.name`).
    pub fn row(&self, f: Filter, name: &str) -> Option<&PkgRow> {
        let r = &self.rows[f.ix()];
        r.index.get(name).map(|&i| &r.list[i])
    }
}

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
}

/// Модель вкладки `pkg:`: одна на процесс, её делят backend `pkg:`, handler `pkg` и UI.
#[derive(Debug, Default)]
pub struct PkgBook {
    data: Mutex<Data>,
    view: Mutex<Arc<View>>,
    /// Растёт на каждом изменении — ключ перерисовки UI.
    rev: AtomicU64,
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
        Arc::clone(&lock(&self.view))
    }

    pub fn info(&self) -> PkgInfo {
        self.view().info.clone()
    }

    pub fn rev(&self) -> u64 {
        self.rev.load(Ordering::Acquire)
    }

    /// `[pkg].order` и `important`: порядок менеджеров и «важное» сверху.
    pub fn configure(&self, cfg: &PkgConfig) {
        self.update(true, |d| {
            d.order.clone_from(&cfg.order);
            d.important.clone_from(&cfg.important);
            sort_managers(&mut d.managers, &d.order);
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
        let mut d = lock(&self.data);
        f(&mut d);
        let rows = if rows {
            build(&d)
        } else {
            self.view().rows.clone()
        };
        let info = PkgInfo {
            managers: d.managers.clone(),
            installed: rows[0].list.len(),
            updates: rows[1].list.len(),
            important: rows[1].list.iter().filter(|r| r.important).count(),
            checked_at: d.checked_at,
            loaded: d.loaded,
            checking: d.checking,
        };
        *lock(&self.view) = Arc::new(View { info, rows });
        self.rev.fetch_add(1, Ordering::AcqRel);
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
/// - обновления — ответы менеджеров как есть, без слияния между менеджерами; имя и
///   издатель — из сводной строки того же пакета; «важные» сверху, затем по имени.
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
    let mut updates: Vec<PkgRow> = d
        .updates
        .values()
        .flatten()
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
