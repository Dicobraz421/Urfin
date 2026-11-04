//! Модель вкладки «Пакеты» `pkg:` для UI (F02-08a, [решение 2026-11-03 «pkg-model-core»]):
//! пакеты и менеджеры, строки «Установленных» и «Обновлений», поиск, ход действий,
//! сводка проверки. Только данные — без процессов, реестра и диска. Лежит в
//! `AppState.ext` как [`PkgModel`]; наполняет `urfin-pkg` (книга вкладки и handler
//! `pkg`), UI только читает.
//!
//! [решение 2026-11-03 «pkg-model-core»]: ../../../docs/decisions/2026-11-03-pkg-model-core.md

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use compact_str::{CompactString, format_compact};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{AppState, ArgSpec, CommandMeta, Location, ValueKind};

// ------------------------------------------------------------ пакеты и менеджеры

/// Менеджер пакетов. Порядок вариантов — порядок `[pkg].order` по умолчанию.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mgr {
    Winget,
    Scoop,
    Choco,
}

impl Mgr {
    pub const ALL: [Mgr; 3] = [Mgr::Winget, Mgr::Scoop, Mgr::Choco];

    /// Имя в конфиге, `PkgId` и `pkg:search?in=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mgr::Winget => "winget",
            Mgr::Scoop => "scoop",
            Mgr::Choco => "choco",
        }
    }

    /// Имя без учёта регистра.
    pub fn parse(s: &str) -> Option<Mgr> {
        Mgr::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(s))
    }
}

impl fmt::Display for Mgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Пакет у менеджера: winget Id, scoop `bucket/name`, choco id. Текст — `mgr:id`
/// (`winget:Mozilla.Firefox`, `scoop:main/7zip`): так он пишется в `[pkg].important`
/// и `check.json`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PkgId {
    pub mgr: Mgr,
    pub id: CompactString,
}

impl PkgId {
    /// Локальная запись winget без источника (`ARP\…`, `MSIX\…`).
    pub fn is_local(&self) -> bool {
        self.mgr == Mgr::Winget && winget_local(&self.id)
    }
}

/// Id winget — локальная запись без источника (`ARP\…`, `MSIX\…`).
pub fn winget_local(id: &str) -> bool {
    id.starts_with("ARP\\") || id.starts_with("MSIX\\")
}

/// Текст не `mgr:id` с известным менеджером и непустым id.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad package id {0:?}: expected <winget|scoop|choco>:<id>")]
pub struct BadPkgId(pub CompactString);

impl FromStr for PkgId {
    type Err = BadPkgId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (mgr, id) = s.split_once(':').unwrap_or_default();
        let id = id.trim();
        match Mgr::parse(mgr.trim()) {
            Some(mgr) if !id.is_empty() => Ok(PkgId { mgr, id: id.into() }),
            _ => Err(BadPkgId(s.into())),
        }
    }
}

impl fmt::Display for PkgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.mgr, self.id)
    }
}

impl Serialize for PkgId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PkgId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        CompactString::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub id: PkgId,
    pub name: CompactString,
    pub version: CompactString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<CompactString>,
}

/// Доступное обновление установленного пакета.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub pkg: Package,
    pub available: CompactString,
    #[serde(default)]
    pub pinned: bool,
}

/// Качество совпадения поиска; меньше — лучше (порядок групп, F02 §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MatchKind {
    Exact,
    IdTail,
    Prefix,
    Contains,
}

impl MatchKind {
    /// Совпадение запроса `q` с пакетом, без учёта регистра: id или имя целиком,
    /// часть id между `.` и `/` (`Git.Git`, `main/7zip`), начало, остальное.
    pub fn of(q: &str, id: &str, name: &str) -> MatchKind {
        let q = q.trim().to_lowercase();
        let (id, name) = (id.to_lowercase(), name.to_lowercase());
        if id == q || name == q {
            MatchKind::Exact
        } else if id.split(['.', '/']).any(|part| part == q) {
            MatchKind::IdTail
        } else if id.starts_with(&q) || name.starts_with(&q) {
            MatchKind::Prefix
        } else {
            MatchKind::Contains
        }
    }
}

/// Результат поиска.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub pkg: Package,
    pub matched: MatchKind,
}

/// Мутирующая операция.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PkgOp {
    Install,
    Upgrade,
    Uninstall,
}

/// Область поиска: все найденные менеджеры или один (`pkg:search?in=`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchIn {
    #[default]
    All,
    Winget,
    Scoop,
    Choco,
}

impl SearchIn {
    /// Один менеджер; `None` — все.
    pub const fn mgr(self) -> Option<Mgr> {
        match self {
            SearchIn::All => None,
            SearchIn::Winget => Some(Mgr::Winget),
            SearchIn::Scoop => Some(Mgr::Scoop),
            SearchIn::Choco => Some(Mgr::Choco),
        }
    }
}

// ------------------------------------------------------------ адрес вкладки

/// Схема вкладки пакетов.
pub const SCHEME: &str = "pkg";

/// Фильтр вкладки: `pkg:installed`, `pkg:updates`, `pkg:search` (F02-09).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Filter {
    Installed,
    Updates,
    Search,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::Installed, Filter::Updates, Filter::Search];

    /// Путь в `pkg:<путь>` и аргумент `pkg.filter name=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Filter::Installed => "installed",
            Filter::Updates => "updates",
            Filter::Search => "search",
        }
    }

    pub fn parse(s: &str) -> Option<Filter> {
        Filter::ALL.into_iter().find(|f| f.as_str() == s)
    }

    const fn ix(self) -> usize {
        self as usize
    }
}

/// `pkg:/<фильтр>` (CONTRACTS, S0 v6 №5): фильтр — корень вкладки, `..` нет.
pub fn location(f: Filter) -> Location {
    Location {
        backend: SCHEME.into(),
        authority: None,
        path: PathBuf::from(format!("/{}", f.as_str())),
        outer: None,
    }
}

/// Фильтр адреса `pkg:`; другой адрес — `None`.
pub fn filter_of(loc: &Location) -> Option<Filter> {
    if loc.backend != SCHEME || loc.outer.is_some() || loc.authority.is_some() {
        return None;
    }
    let p = loc.path.to_str()?;
    Filter::parse(p.trim_start_matches(['/', '\\']))
}

// ------------------------------------------------------------ вид вкладки

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

/// Сводка для заголовка, чипов фильтров, статуса и баннера обновлений.
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
    /// Конец последней проверки, Unix, наносекунды (хранится в кэше `urfin-pkg`).
    pub checked_at: Option<i64>,
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
pub struct Rows {
    list: Vec<PkgRow>,
    index: HashMap<CompactString, usize>,
}

impl Rows {
    /// Имена — уникальные без учёта регистра (курсор и отметка держатся за имя).
    pub fn new(mut list: Vec<PkgRow>) -> Rows {
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

    pub fn list(&self) -> &[PkgRow] {
        &self.list
    }
}

/// Идущее действие с пакетом: строка показывает ход (`screen-pkginstall`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Busy {
    pub op: PkgOp,
    /// Ждёт своей очереди у менеджера.
    pub queued: bool,
    /// Доля 0…100 из вывода; `None` — неопределённый ход.
    pub pct: Option<u8>,
    /// Ждёт подтверждения UAC (choco — точно, F02 §3.3).
    pub uac: bool,
}

/// Ответ менеджера на запрос поиска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Идёт запрос.
    Busy,
    /// Ответил: число найденных (0 — «ничего не нашлось»).
    Found(usize),
    /// Не ответил: ошибка или таймаут.
    Failed(CompactString),
}

/// Сводка поиска: строка, область и ответы менеджеров (строка под полем поиска).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchInfo {
    pub q: CompactString,
    pub scope: SearchIn,
    /// Менеджеры запроса в порядке `[pkg].order` и их ответы.
    pub answers: Vec<(Mgr, Answer)>,
    pub groups: usize,
    pub variants: usize,
    /// Номер запроса: растёт на каждом новом.
    pub generation: u64,
    /// Фильтр до поиска: Esc в строке поиска возвращает к нему.
    pub back: Option<Filter>,
}

impl SearchInfo {
    /// Кто-то ещё ищет.
    pub fn busy(&self) -> bool {
        self.answers.iter().any(|(_, a)| *a == Answer::Busy)
    }

    /// Запрос был и все ответили пусто (без ошибок): «ничего не нашлось».
    pub fn empty(&self) -> bool {
        !self.q.is_empty()
            && !self.busy()
            && self.variants == 0
            && self.answers.iter().all(|(_, a)| *a == Answer::Found(0))
    }
}

/// Вид строки поиска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchKind {
    /// Группа программы: раскрыта или нет, число вариантов, издатель, рекомендуемый.
    Group {
        open: bool,
        count: usize,
        publisher: Option<CompactString>,
        recommended: PkgId,
    },
    /// Вариант: пакет у менеджера; «рекомендуется» и «новейшая» (`accent`).
    Variant {
        hit: Hit,
        recommended: bool,
        newest: bool,
    },
}

/// Строка вкладки в поиске.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRow {
    /// `Entry.name`: группа — имя программы, вариант — id пакета (у scoop — без
    /// бакета); совпавшие в точности — с менеджером в скобках (`NanaZip` и `nanazip` —
    /// разные строки).
    pub name: CompactString,
    /// Номер группы (постоянный в пределах запроса).
    pub group: u32,
    pub kind: SearchKind,
}

impl SearchRow {
    /// Пакет строки: вариант — свой, группа — рекомендуемый.
    pub fn target(&self) -> &PkgId {
        match &self.kind {
            SearchKind::Group { recommended, .. } => recommended,
            SearchKind::Variant { hit, .. } => &hit.pkg.id,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, SearchKind::Group { .. })
    }
}

/// Готовые строки поиска.
#[derive(Debug, Default)]
pub struct SearchView {
    pub info: SearchInfo,
    pub list: Vec<SearchRow>,
    index: HashMap<CompactString, usize>,
}

impl SearchView {
    /// Имена строк `list` — уникальные.
    pub fn new(info: SearchInfo, list: Vec<SearchRow>) -> SearchView {
        let index = list
            .iter()
            .enumerate()
            .map(|(i, r)| (r.name.clone(), i))
            .collect();
        SearchView { info, list, index }
    }

    pub fn row(&self, name: &str) -> Option<&SearchRow> {
        self.index.get(name).map(|&i| &self.list[i])
    }

    /// Номер группы строки `name` в порядке показа: `keep` для слияния ответов —
    /// позиция курсора + 1.
    pub fn keep_for(&self, name: &str) -> usize {
        let Some(row) = self.row(name) else {
            return 0;
        };
        let mut seen = 0;
        for r in &self.list {
            if r.is_group() {
                seen += 1;
                if r.group == row.group {
                    return seen;
                }
            }
        }
        0
    }
}

/// Готовый вид вкладки: сводка, строки фильтров, поиск и идущие действия.
#[derive(Debug, Default)]
pub struct View {
    pub info: PkgInfo,
    /// «Установленные» и «Обновления» (по [`Filter`]).
    pub lists: [Arc<Rows>; 2],
    pub search: Arc<SearchView>,
    pub busy: Arc<HashMap<PkgId, Busy>>,
}

impl View {
    /// Строки «Установленных» и «Обновлений» (у поиска — [`View::search`]).
    pub fn rows(&self, f: Filter) -> &[PkgRow] {
        match f {
            Filter::Search => &[],
            _ => &self.lists[f.ix()].list,
        }
    }

    /// Строка фильтра `f` по имени (`Entry.name`).
    pub fn row(&self, f: Filter, name: &str) -> Option<&PkgRow> {
        if f == Filter::Search {
            return None;
        }
        let r = &self.lists[f.ix()];
        r.index.get(name).map(|&i| &r.list[i])
    }

    /// Ход действия с пакетом.
    pub fn busy_of(&self, id: &PkgId) -> Option<Busy> {
        self.busy.get(id).copied()
    }
}

// ------------------------------------------------------------ проверка по расписанию

/// Итог фоновой проверки (`urfin.exe --pkg-check` по расписанию или `pkg.check_now`)
/// для баннера «Доступно N обновлений · M важных — проверено …» (F02 §4,
/// `screen-pkgbanner`): есть, пока его не закрыли «Показать» или «Позже».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Обновлений (без дублей, как в «Обновлениях»).
    pub updates: usize,
    /// «Важных» среди них.
    pub important: usize,
    /// Конец проверки, Unix, наносекунды.
    pub checked_at: i64,
    /// Проверка по расписанию (иначе — «Проверить сейчас»).
    pub scheduled: bool,
}

/// Задача Планировщика `\Urfin\PkgCheck` (F02 §4) — для раздела настроек «Пакеты»
/// (F02-12): включена ли и указывает ли на этот `urfin.exe`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Task {
    /// Ещё не сверяли.
    #[default]
    Unknown,
    /// Идёт создание, удаление или сверка.
    Busy,
    /// Задачи нет.
    Absent,
    /// Задача есть и запускает этот `urfin.exe`.
    Ready,
    /// Задача запускает другой `urfin.exe` (portable-копию перенесли): путь задачи.
    OtherPath(CompactString),
    /// `schtasks` не ответил или вернул ошибку.
    Failed(CompactString),
}

// ------------------------------------------------------------ модель в `AppState.ext`

/// Опубликованный вид вкладки: пишет книга `urfin-pkg` (в том числе из потоков
/// запросов), UI берёт готовый `Arc<View>` — короткая блокировка, без пересчёта.
#[derive(Debug, Default)]
pub struct PkgFeed {
    view: Mutex<Arc<View>>,
    /// Растёт на каждой публикации — ключ перерисовки UI.
    rev: AtomicU64,
}

impl PkgFeed {
    pub fn view(&self) -> Arc<View> {
        Arc::clone(&self.lock())
    }

    pub fn info(&self) -> PkgInfo {
        self.view().info.clone()
    }

    pub fn rev(&self) -> u64 {
        self.rev.load(Ordering::Acquire)
    }

    /// Новый вид; `rev` растёт.
    pub fn publish(&self, view: View) {
        *self.lock() = Arc::new(view);
        self.rev.fetch_add(1, Ordering::AcqRel);
    }

    fn lock(&self) -> MutexGuard<'_, Arc<View>> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Запись в модель для сцен `--screens` (фикстура без запуска менеджеров): реализует
/// книга `urfin-pkg`, строки сводит она же (§2.4).
pub trait PkgFixture: Send + Sync {
    /// «Важные» (`[pkg].important` сцены).
    fn important(&self, ids: Vec<PkgId>);
    /// Найденные менеджеры (без процессов): «идёт запрос», прежние списки уходят.
    fn managers(&self, mgrs: &[Mgr]);
    /// Установленные у `mgr`.
    fn installed(&self, mgr: Mgr, pkgs: Vec<Package>);
    /// Обновления у `mgr`; запрос менеджера закончен.
    fn updates(&self, mgr: Mgr, ups: Vec<Update>);
    /// Проверка закончена сейчас.
    fn checked_now(&self);
    /// Новый запрос поиска; номер — для [`PkgFixture::answer`].
    fn search(&self, q: &str, mgrs: &[Mgr]) -> u64;
    /// Ответ менеджера на запрос `generation`.
    fn answer(&self, generation: u64, mgr: Mgr, hits: Vec<Hit>);
    /// Ход действия с пакетом; `None` — кончилось.
    fn busy(&self, id: &PkgId, busy: Option<Busy>);
}

/// Модель «Пакетов» в `AppState.ext`: ставит handler `pkg` в первом pump.
#[derive(Clone)]
pub struct PkgModel {
    /// `[pkg].order`.
    pub order: Vec<Mgr>,
    pub feed: Arc<PkgFeed>,
    pub fixture: Arc<dyn PkgFixture>,
    /// Баннер итога фоновой проверки; `None` — не показывать.
    pub notice: Option<Notice>,
    /// Задача Планировщика.
    pub task: Task,
}

impl fmt::Debug for PkgModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkgModel")
            .field("order", &self.order)
            .field("rev", &self.feed.rev())
            .field("notice", &self.notice)
            .field("task", &self.task)
            .finish_non_exhaustive()
    }
}

/// Вид вкладки из состояния (после первого pump handler `pkg`).
pub fn feed(s: &AppState) -> Option<&Arc<PkgFeed>> {
    s.ext.get::<PkgModel>().map(|m| &m.feed)
}

/// Баннер итога фоновой проверки, если он есть.
pub fn notice(s: &AppState) -> Option<&Notice> {
    s.ext.get::<PkgModel>()?.notice.as_ref()
}

// ------------------------------------------------------------ команды `pkg.*`

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, title: &'static str, args: &'static [ArgSpec]) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "pkg",
        args,
        repeatable: false,
    }
}

/// Отмеченные строки или строка под курсором.
const TARGETS: [ArgSpec; 1] = [arg("targets", ValueKind::List, false)];

/// Команды F02 §2.5 (исполняет handler `pkg` из `urfin-pkg`).
pub static COMMANDS: [CommandMeta; 17] = [
    meta(
        "pkg.open",
        "Packages…",
        &[arg("filter", ValueKind::Str, false)],
    ),
    meta(
        "pkg.filter",
        "Packages: show",
        &[arg("name", ValueKind::Str, true)],
    ),
    meta("pkg.refresh", "Packages: refresh", &[]),
    meta(
        "pkg.search",
        "Find package",
        &[
            arg("q", ValueKind::Str, false),
            arg("in", ValueKind::Str, false),
        ],
    ),
    meta("pkg.search_in", "Search in managers", &[]),
    meta("pkg.pick", "Choose variant", &[]),
    meta(
        "pkg.install",
        "Install package",
        &[arg("id", ValueKind::Str, false)],
    ),
    meta("pkg.upgrade", "Upgrade packages", &TARGETS),
    meta("pkg.upgrade_all", "Upgrade all", &[]),
    meta("pkg.uninstall", "Uninstall packages", &TARGETS),
    meta("pkg.info", "Package info", &[]),
    meta("pkg.output", "Package manager output", &[]),
    meta("pkg.important.toggle", "Mark as important", &[]),
    meta("pkg.check_now", "Check for updates now", &[]),
    meta(
        "pkg.schedule",
        "Scheduled update check",
        &[
            arg("on", ValueKind::Bool, false),
            arg("at", ValueKind::Str, false),
        ],
    ),
    meta("pkg.banner.show", "Show updates", &[]),
    meta("pkg.banner.later", "Remind later", &[]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_id_text() {
        let id: PkgId = "scoop:main/7zip".parse().unwrap();
        assert_eq!(id.mgr, Mgr::Scoop);
        assert_eq!(id.to_string(), "scoop:main/7zip");
        assert!("apt:vim".parse::<PkgId>().is_err());
        assert!("winget:".parse::<PkgId>().is_err());
        assert!(
            "winget:ARP\\Machine\\X64\\Totalcmd64"
                .parse::<PkgId>()
                .unwrap()
                .is_local()
        );
    }

    #[test]
    fn filter_location_roundtrip() {
        for f in Filter::ALL {
            assert_eq!(filter_of(&location(f)), Some(f));
        }
        assert_eq!(filter_of(&Location::local("C:\\")), None);
    }

    #[test]
    fn feed_publishes() {
        let feed = PkgFeed::default();
        assert_eq!(feed.rev(), 0);
        let rows = Rows::new(vec![]);
        feed.publish(View {
            info: PkgInfo {
                installed: 3,
                ..PkgInfo::default()
            },
            lists: [Arc::new(rows), Arc::default()],
            ..View::default()
        });
        assert_eq!(feed.rev(), 1);
        assert_eq!(feed.info().installed, 3);
    }
}
