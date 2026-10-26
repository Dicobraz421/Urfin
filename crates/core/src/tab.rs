//! Таб панели: каталог, листинг, курсор, выделение.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::cancel::CancelToken;
use crate::config::Config;
use crate::entry::Entry;
use crate::history::NavHistory;
use crate::ids::{Gen, TabId};
use crate::loc::Location;
use crate::mask::Mask;
use crate::selection::Selection;

/// Курсор в пространстве строк таба (строка 0 — `..`, если она есть).
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: u32,
    /// Первая видимая строка (S0 v3 №3: задаётся `panel.viewport`, меняет `rev`).
    pub top: u32,
    /// Строк на страницу; 0 — неизвестно (UI ещё не сообщил).
    pub page: u32,
}

impl Cursor {
    /// Держит `row` в `0..rows`, а `top` — так, чтобы курсор был виден и страница
    /// не уходила за конец списка.
    pub fn clamp(&mut self, rows: u32) {
        if rows == 0 {
            self.row = 0;
            self.top = 0;
            return;
        }
        self.row = self.row.min(rows - 1);
        if self.page > 0 {
            self.top = self.top.min(rows.saturating_sub(self.page));
            if self.row >= self.top + self.page {
                self.top = self.row + 1 - self.page;
            }
        }
        self.top = self.top.min(self.row);
    }
}

/// Строка таба.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Row<'a> {
    /// Виртуальная `..`: её нет в `entries`, её нельзя выделить.
    Parent,
    Entry {
        idx: u32,
        entry: &'a Entry,
    },
}

/// Цели операции (S5).
#[derive(Clone, Debug)]
pub struct Targets {
    pub base: Location,
    pub items: Arc<[Entry]>,
    pub from_selection: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListingStatus {
    Idle,
    Loading { since: Instant },
    Complete,
    Failed(Arc<str>),
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct ListingStats {
    pub files: u32,
    pub dirs: u32,
    /// Сумма размеров файлов.
    pub bytes: u64,
}

/// Содержимое каталога. `entries` и `order` делит с воркерами через `Arc`.
#[derive(Clone, Debug)]
pub struct Listing {
    /// Индексы стабильны в пределах `gen`.
    pub entries: Arc<Vec<Entry>>,
    /// Видимый порядок (сортировка + фильтр), делает S4.
    pub order: Arc<Vec<u32>>,
    pub status: ListingStatus,
    pub stats: ListingStats,
    pub generation: Gen,
    /// Растёт на `panel.sort`, `filter`, `hidden.toggle`; `Order`/`Patch` со старым
    /// `seq` отбрасываются (S0 v2 №3).
    pub arrange_seq: u32,
}

impl Listing {
    /// Пустой, `Idle`.
    pub fn new(generation: Gen) -> Self {
        Listing {
            entries: Arc::default(),
            order: Arc::default(),
            status: ListingStatus::Idle,
            stats: ListingStats::default(),
            generation,
            arrange_seq: 0,
        }
    }

    /// Готовый листинг (`Complete`) со статистикой по всем `entries`.
    pub fn with_entries(entries: Vec<Entry>, order: Vec<u32>, generation: Gen) -> Self {
        debug_assert!(order.iter().all(|&i| (i as usize) < entries.len()));
        let mut stats = ListingStats::default();
        for e in &entries {
            if e.kind.is_dir_like() {
                stats.dirs += 1;
            } else {
                stats.files += 1;
                stats.bytes += e.size;
            }
        }
        Listing {
            entries: Arc::new(entries),
            order: Arc::new(order),
            status: ListingStatus::Complete,
            stats,
            generation,
            arrange_seq: 0,
        }
    }
}

/// Сортировка. Недостающие в конфиге поля берутся из `Default`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortSpec {
    pub key: SortKey,
    pub desc: bool,
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        SortSpec {
            key: SortKey::Name,
            desc: false,
            dirs_first: true,
        }
    }
}

/// Ключ сортировки; расширение — через S0.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Name,
    Ext,
    Size,
    Mtime,
    Unsorted,
}

/// Набор колонок.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Brief,
    Medium,
    Full,
    Wide,
    /// Пользовательский набор колонок `[ui.view_modes]`, M2+ (S0 v3 №6).
    Custom(u8),
}

/// Что показывает панель (S0 v3 №4, `panel.mode`); хранится в сессии.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelMode {
    #[default]
    Files,
    Info,
    QView,
    Tree,
}

/// Таб панели.
#[derive(Debug)]
pub struct Tab {
    pub id: TabId,
    pub loc: Location,
    pub listing: Listing,
    pub cursor: Cursor,
    /// Имя, на которое встать после загрузки.
    pub pending_focus: Option<CompactString>,
    pub selection: Selection,
    pub prev_selection: Option<Vec<CompactString>>,
    pub sort: SortSpec,
    pub filter: Option<Mask>,
    pub show_hidden: bool,
    pub view_mode: ViewMode,
    pub mode: PanelMode,
    pub history: NavHistory,
    /// Растёт при любой мутации таба — ключ кэшей UI.
    pub rev: u64,
    /// Отмена текущей загрузки.
    pub(crate) cancel: Option<CancelToken>,
    /// Double-buffer для `panel.refresh`.
    pub(crate) incoming: Option<Incoming>,
    /// `seq`, с которым запрошена загрузка (сравнивается с `arrange_seq` на `Done`).
    pub(crate) load_seq: u32,
    /// Колонок в области панели (`panel.viewport`), для `panel.cursor.left/right`.
    pub(crate) cols: u32,
    /// Таб из сессии: если каталога нет, подниматься к родителю (осталось шагов).
    pub(crate) restore_up: u8,
    /// Пришёл `DoneUnsorted`: `Complete` — с финальным `Order` (ADR-0017).
    pub(crate) final_order: bool,
}

/// Перечитываемый листинг до замены текущего.
#[derive(Debug)]
pub(crate) struct Incoming {
    pub(crate) listing: Listing,
    pub(crate) started: Instant,
    /// Пришёл `DoneUnsorted`: подмена — с финальным `Order` (ADR-0017).
    pub(crate) final_order: bool,
}

impl Tab {
    /// Пустой таб в `loc`: листинг `Idle`, вид Full (S2: Explorer-native), сортировка
    /// по имени, каталоги первыми.
    pub fn new(id: TabId, loc: Location) -> Self {
        Tab {
            id,
            loc,
            listing: Listing::new(Gen::default()),
            cursor: Cursor::default(),
            pending_focus: None,
            selection: Selection::default(),
            prev_selection: None,
            sort: SortSpec::default(),
            filter: None,
            show_hidden: false,
            view_mode: ViewMode::Full,
            mode: PanelMode::Files,
            history: NavHistory::default(),
            rev: 0,
            cancel: None,
            incoming: None,
            load_seq: 0,
            cols: 1,
            restore_up: 0,
            final_order: false,
        }
    }

    /// Новый таб с настройками панелей из конфига (сортировка, вид, скрытые, история).
    pub fn with_config(id: TabId, loc: Location, c: &Config) -> Self {
        let mut t = Tab::new(id, loc);
        t.sort = c.panels.sort;
        t.view_mode = c.panels.view_mode;
        t.show_hidden = c.general.show_hidden;
        t.history = NavHistory::new(c.panels.history_cap.into());
        t
    }

    /// `..` есть везде, кроме корня (у корня контейнера она ведёт в `outer`).
    pub fn has_parent_row(&self) -> bool {
        self.loc.has_parent()
    }

    /// `..` (если есть) + видимые элементы.
    pub fn rows(&self) -> u32 {
        u32::from(self.has_parent_row()) + self.listing.order.len() as u32
    }

    pub fn row(&self, r: u32) -> Option<Row<'_>> {
        let r = if self.has_parent_row() {
            if r == 0 {
                return Some(Row::Parent);
            }
            r - 1
        } else {
            r
        };
        let idx = *self.listing.order.get(r as usize)?;
        let entry = self.listing.entries.get(idx as usize)?;
        Some(Row::Entry { idx, entry })
    }

    pub fn current(&self) -> Option<Row<'_>> {
        self.row(self.cursor.row)
    }

    /// Цели операции: выделение или элемент под курсором (семантика Far). `None` на `..`.
    ///
    /// Выделенные берутся в видимом порядке (`order`); выделенные, скрытые фильтром,
    /// не входят. Если среди видимых выделенных нет — элемент под курсором.
    pub fn targets(&self) -> Option<Targets> {
        if !self.selection.is_empty() {
            let items: Vec<Entry> = self
                .listing
                .order
                .iter()
                .filter(|&&i| self.selection.contains(i))
                .map(|&i| self.listing.entries[i as usize].clone())
                .collect();
            if !items.is_empty() {
                return Some(Targets {
                    base: self.loc.clone(),
                    items: items.into(),
                    from_selection: true,
                });
            }
        }
        match self.current()? {
            Row::Parent => None,
            Row::Entry { entry, .. } => Some(Targets {
                base: self.loc.clone(),
                items: Arc::new([entry.clone()]),
                from_selection: false,
            }),
        }
    }

    /// Заголовок таба: имя каталога; у корня — путь (`C:\`, `/`), у корня
    /// контейнера — имя контейнера.
    pub fn title(&self) -> Cow<'_, str> {
        if let Some(n) = self.loc.file_name() {
            return n.to_string_lossy();
        }
        if let Some(n) = self.loc.outer.as_deref().and_then(|o| o.file_name()) {
            return n.to_string_lossy();
        }
        self.loc.path.to_string_lossy()
    }

    /// Отметить мутацию таба.
    pub fn touch(&mut self) {
        self.rev += 1;
    }

    /// Новый листинг: курсор клампится под новое число строк.
    pub fn set_listing(&mut self, l: Listing) {
        self.listing = l;
        self.clamp_cursor();
        self.touch();
    }

    /// Поставить курсор на строку (с клампом и прокруткой к ней).
    pub fn set_cursor_row(&mut self, row: u32) {
        self.cursor.row = row;
        self.clamp_cursor();
        self.touch();
    }

    pub fn clamp_cursor(&mut self) {
        let rows = self.rows();
        self.cursor.clamp(rows);
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Listing, PanelMode, Row, Tab, ViewMode};
    use crate::entry::{Attrs, Entry, EntryKind};
    use crate::ids::{Gen, TabId};
    use crate::loc::Location;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn root() -> &'static str {
        if cfg!(windows) { r"C:\" } else { "/" }
    }

    fn sub() -> &'static str {
        if cfg!(windows) { r"C:\src" } else { "/src" }
    }

    fn entries(n: u32) -> Vec<Entry> {
        (0..n)
            .map(|i| Entry {
                ctime: i64::MIN,
                atime: i64::MIN,
                name: format!("f{i}").into(),
                size: u64::from(i) * 10,
                mtime: 0,
                attrs: Attrs::default(),
                kind: if i == 0 {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
            })
            .collect()
    }

    fn tab(loc: &str, n: u32) -> Tab {
        let mut t = Tab::new(TabId(1), Location::local(loc));
        t.set_listing(Listing::with_entries(
            entries(n),
            (0..n).rev().collect(),
            Gen(1),
        ));
        t
    }

    #[test]
    fn parent_row_only_outside_root() {
        let t = tab(root(), 3);
        assert!(!t.has_parent_row());
        assert_eq!(t.rows(), 3);
        assert!(matches!(t.row(0), Some(Row::Entry { idx: 2, .. })));
        assert_eq!(t.row(3), None);

        let t = tab(sub(), 3);
        assert!(t.has_parent_row());
        assert_eq!(t.rows(), 4);
        assert_eq!(t.row(0), Some(Row::Parent));
        assert!(matches!(t.row(1), Some(Row::Entry { idx: 2, .. })));
        assert!(matches!(t.row(3), Some(Row::Entry { idx: 0, .. })));
        assert_eq!(t.row(4), None);

        // Корень контейнера: `..` ведёт наружу.
        let mut inside = Tab::new(
            TabId(2),
            Location {
                backend: "zip".into(),
                path: PathBuf::from("/"),
                outer: Some(Arc::new(Location::local(sub()))),
            },
        );
        assert!(inside.has_parent_row());
        inside.set_listing(Listing::new(Gen(1)));
        assert_eq!(inside.rows(), 1);
        assert_eq!(inside.title(), "src");
    }

    #[test]
    fn cursor_clamps_when_rows_change() {
        let mut t = tab(sub(), 10);
        t.cursor.page = 4;
        t.set_cursor_row(9);
        assert_eq!((t.cursor.row, t.cursor.top), (9, 6));
        t.set_listing(Listing::with_entries(entries(3), vec![0, 1, 2], Gen(2)));
        assert_eq!(t.rows(), 4);
        assert_eq!((t.cursor.row, t.cursor.top), (3, 0));
        t.set_listing(Listing::new(Gen(3)));
        assert_eq!((t.cursor.row, t.cursor.top), (0, 0));
        assert_eq!(t.current(), Some(Row::Parent));
        let mut t = tab(root(), 0);
        t.set_cursor_row(5);
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.current(), None);
    }

    #[test]
    fn cursor_keeps_row_visible() {
        let mut c = Cursor {
            row: 2,
            top: 10,
            page: 5,
        };
        c.clamp(100);
        assert_eq!((c.row, c.top), (2, 2));
        let mut c = Cursor {
            row: 50,
            top: 0,
            page: 5,
        };
        c.clamp(100);
        assert_eq!((c.row, c.top), (50, 46));
        let mut c = Cursor {
            row: 99,
            top: 99,
            page: 10,
        };
        c.clamp(100);
        assert_eq!((c.row, c.top), (99, 90));
        let mut c = Cursor {
            row: 1,
            top: 7,
            page: 0,
        };
        c.clamp(3);
        assert_eq!((c.row, c.top), (1, 1));
    }

    #[test]
    fn targets_selection_then_cursor_none_on_parent() {
        let mut t = tab(sub(), 4); // строки: .., f3, f2, f1, f0
        t.set_cursor_row(0);
        assert!(t.targets().is_none());
        t.set_cursor_row(2);
        let tg = t.targets().unwrap();
        assert!(!tg.from_selection);
        assert_eq!(tg.items.len(), 1);
        assert_eq!(tg.items[0].name, "f2");
        assert_eq!(tg.base, Location::local(sub()));

        let l = t.listing.clone();
        t.selection.set(1, true, &l.entries[1]);
        t.selection.set(3, true, &l.entries[3]);
        t.set_cursor_row(0);
        let tg = t.targets().unwrap();
        assert!(tg.from_selection);
        let names: Vec<_> = tg.items.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["f3", "f1"]);

        // Выделение скрыто фильтром → цель под курсором.
        t.listing.order = Arc::new(vec![2, 0]);
        t.set_cursor_row(1);
        let tg = t.targets().unwrap();
        assert!(!tg.from_selection);
        assert_eq!(tg.items[0].name, "f2");
    }

    #[test]
    fn titles_and_defaults() {
        assert_eq!(tab(sub(), 0).title(), "src");
        assert_eq!(tab(root(), 0).title(), root());
        let t = Tab::new(TabId(7), Location::local(sub()));
        assert_eq!(t.view_mode, ViewMode::Full);
        assert_eq!(t.mode, PanelMode::Files);
        assert!(t.listing.entries.is_empty());
    }

    #[test]
    fn listing_stats() {
        let l = Listing::with_entries(entries(4), vec![0, 1, 2, 3], Gen(1));
        assert_eq!((l.stats.dirs, l.stats.files, l.stats.bytes), (1, 3, 60));
    }

    #[test]
    fn enums_serde() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct S {
            view: ViewMode,
            custom: ViewMode,
            mode: PanelMode,
            sort: super::SortSpec,
        }
        let s = S {
            view: ViewMode::Brief,
            custom: ViewMode::Custom(3),
            mode: PanelMode::QView,
            sort: super::SortSpec::default(),
        };
        let text = toml::to_string(&s).unwrap();
        assert!(text.contains("view = \"brief\""), "{text}");
        assert!(text.contains("mode = \"qview\""), "{text}");
        assert!(text.contains("key = \"name\""), "{text}");
        assert_eq!(toml::from_str::<S>(&text).unwrap(), s);
    }
}
