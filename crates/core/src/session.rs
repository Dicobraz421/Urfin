//! Сессия (S1 §2.8 + S0 v3 №4, v5 №8, решение владельца №2): табы, пути, вид,
//! закладки и окно между запусками. Пишется атомарно в `state_dir/session.toml`.

use std::fs;
use std::io;

use serde::{Deserialize, Serialize};

use crate::config::{Config, Migration};
use crate::history::HistEntry;
use crate::ids::{Side, TabId};
use crate::loc::Location;
use crate::mask::Mask;
use crate::nav::current_name;
use crate::paths::{Paths, write_atomic};
use crate::state::{AppState, Panel};
use crate::tab::{ListingStatus, PanelMode, SortSpec, Tab, ViewMode};

pub const SESSION_VERSION: u32 = 1;

/// Миграции сырой сессии `.0` → `.0 + 1`, как у конфига (S1 §2.8). v1 — первая
/// версия: шагов пока нет.
static MIGRATIONS: &[Migration] = &[];

/// Сколько шагов «назад» хранить на таб.
const BACK_KEEP: usize = 20;

/// Сколько уровней вверх подниматься, если сохранённого каталога больше нет.
pub(crate) const RESTORE_UP_STEPS: u8 = 32;

/// Разбор с миграциями: сырой TOML → шаги с версии файла до `current` → `Session`.
/// Шага нет или он упал — читаем как есть (поля по умолчанию); новее `current` —
/// тоже как есть, с предупреждением.
pub(crate) fn parse(text: &str, migrations: &[Migration], current: u32) -> Result<Session, String> {
    let mut table: toml::Table = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    let from = table
        .get("version")
        .and_then(toml::Value::as_integer)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(current);
    if from > current {
        tracing::warn!(version = from, "session from a newer version, read as is");
    }
    for v in from..current {
        let Some((_, step)) = migrations.iter().find(|(f, _)| *f == v) else {
            tracing::warn!(from = v, "no session migration, read as is");
            break;
        };
        if let Err(e) = step(&mut table) {
            tracing::warn!(from = v, error = %e, "session migration failed, read as is");
            break;
        }
        table.insert("version".into(), toml::Value::Integer(i64::from(v + 1)));
    }
    toml::Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_owned())
}

/// Окно приложения (геометрию храним сами: `persistence` eframe выключен).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct WindowGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
    /// Доля ширины левой панели.
    pub split: f32,
}

/// Закладка `panel.bookmark.*` (S0 v3 №4).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BookmarkSession {
    pub n: u8,
    pub loc: Location,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Session {
    pub version: u32,
    pub window: Option<WindowGeom>,
    pub active: Side,
    pub panels: [PanelSession; 2],
    /// Свежие первыми, не больше `visited_cap`.
    pub visited: Vec<Location>,
    /// Время посещения `visited` (секунды Unix) в том же порядке; нет — 0.
    pub visited_ts: Vec<u64>,
    pub bookmarks: Vec<BookmarkSession>,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            version: SESSION_VERSION,
            window: None,
            active: Side::Left,
            panels: Default::default(),
            visited: Vec::new(),
            visited_ts: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct PanelSession {
    pub tabs: Vec<TabSession>,
    pub active: usize,
    pub visible: bool,
}

impl Default for PanelSession {
    fn default() -> Self {
        PanelSession {
            tabs: Vec::new(),
            active: 0,
            visible: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabSession {
    pub loc: Location,
    /// Имя под курсором.
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub top: u32,
    #[serde(default)]
    pub sort: SortSpec,
    #[serde(default = "full")]
    pub view_mode: ViewMode,
    /// `panel.mode` (S0 v3 №4).
    #[serde(default)]
    pub mode: PanelMode,
    /// Исходная строка маски.
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub show_hidden: bool,
    /// Шаги «назад», старые первыми, не больше 20.
    #[serde(default)]
    pub back: Vec<Location>,
}

fn full() -> ViewMode {
    ViewMode::Full
}

/// Виртуальные каталоги (результаты поиска, сравнение) в сессию не попадают
/// (S0 v5 №8).
fn is_virtual(l: &Location) -> bool {
    let mut cur = Some(l);
    while let Some(x) = cur {
        if matches!(x.backend.as_str(), "search" | "cmp") {
            return true;
        }
        cur = x.outer.as_deref();
    }
    false
}

impl Session {
    /// Снимок состояния (микросекунды; вызывается в UI-потоке).
    pub fn capture(s: &AppState, w: Option<WindowGeom>) -> Self {
        let panels = [Side::Left, Side::Right].map(|side| {
            let p = s.panel(side);
            let mut tabs = Vec::new();
            let mut active = 0;
            for (i, t) in p.tabs.iter().enumerate() {
                if let Some(ts) = capture_tab(t) {
                    if i == p.active {
                        active = tabs.len();
                    }
                    tabs.push(ts);
                }
            }
            PanelSession {
                tabs,
                active,
                visible: p.visible,
            }
        });
        let (visited, visited_ts) = s
            .visited
            .iter_at()
            .filter(|(l, _)| !is_virtual(l))
            .take(s.config.panels.visited_cap.into())
            .map(|(l, t)| (l.clone(), t))
            .unzip();
        let bookmarks = s
            .bookmarks
            .iter()
            .enumerate()
            .filter_map(|(n, l)| {
                let loc = l.clone()?;
                Some(BookmarkSession { n: n as u8, loc })
            })
            .collect();
        Session {
            version: SESSION_VERSION,
            window: w,
            active: s.active,
            panels,
            visited,
            visited_ts,
            bookmarks,
        }
    }

    /// Заменить табы, закладки и историю каталогов состоянием сессии. Табы
    /// получают новые id; загрузки запускает Core (`Core::new`). Пустая панель
    /// в сессии оставляет текущую.
    pub fn apply(self, s: &mut AppState) {
        let config = s.config.clone();
        for (side, ps) in [Side::Left, Side::Right].into_iter().zip(self.panels) {
            if ps.tabs.is_empty() {
                continue;
            }
            let tabs: Vec<Tab> = ps
                .tabs
                .into_iter()
                .map(|ts| restore_tab(TabId(0), ts, &config))
                .collect();
            let active = ps.active.min(tabs.len() - 1);
            *s.panel_mut(side) = Panel {
                tabs,
                active,
                visible: ps.visible,
            };
        }
        // До загрузок: id просто перенумеровываются по порядку.
        let mut next = 1u32;
        for p in &mut s.panels {
            for t in &mut p.tabs {
                t.id = TabId(next);
                next += 1;
            }
        }
        s.reset_tab_ids(next);
        s.active = self.active;
        if !s.panel(s.active).visible {
            s.active = s.active.other();
        }
        for (i, l) in self.visited.iter().enumerate().rev() {
            s.visited
                .touch_at(l, self.visited_ts.get(i).copied().unwrap_or(0));
        }
        for b in self.bookmarks {
            if let Some(slot) = s.bookmarks.get_mut(usize::from(b.n)) {
                *slot = Some(b.loc);
            }
        }
    }

    /// `None`, если файла нет или он битый (тогда рядом остаётся копия
    /// `session.toml.corrupt`).
    pub fn load(paths: &Paths) -> Option<Self> {
        let file = paths.session_file();
        let text = match fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(error = %e, file = %file.display(), "cannot read session");
                return None;
            }
        };
        match parse(&text, MIGRATIONS, SESSION_VERSION) {
            Ok(s) => Some(s),
            Err(e) => {
                let corrupt = file.with_file_name("session.toml.corrupt");
                tracing::warn!(error = %e, "corrupt session, kept as {}", corrupt.display());
                let _ = fs::copy(&file, &corrupt);
                None
            }
        }
    }

    /// Атомарно: временный файл, fsync, rename. Прерванная запись не портит
    /// прошлый файл.
    pub fn save(&self, paths: &Paths) -> io::Result<()> {
        let text = toml::to_string(self).map_err(io::Error::other)?;
        write_atomic(&paths.session_file(), text.as_bytes())
    }
}

fn capture_tab(t: &Tab) -> Option<TabSession> {
    let loc = if is_virtual(&t.loc) {
        // Последний реальный каталог из истории таба.
        t.history
            .back_entries()
            .iter()
            .rev()
            .map(|h| &h.loc)
            .find(|l| !is_virtual(l))?
            .clone()
    } else {
        t.loc.clone()
    };
    // Пока каталог грузится, имя для курсора ещё ждёт в `pending_focus`.
    let focus = match t.listing.status {
        ListingStatus::Loading { .. } if t.pending_focus.is_some() => t.pending_focus.clone(),
        _ => current_name(t),
    };
    let back = t.history.back_entries();
    let back = back
        .iter()
        .skip(back.len().saturating_sub(BACK_KEEP))
        .map(|h| h.loc.clone())
        .filter(|l| !is_virtual(l))
        .collect();
    Some(TabSession {
        loc,
        focus: focus.map(|f| f.to_string()),
        top: t.cursor.top,
        sort: t.sort,
        view_mode: t.view_mode,
        mode: t.mode,
        filter: t.filter.as_ref().map(|m| m.as_str().to_owned()),
        show_hidden: t.show_hidden,
        back,
    })
}

fn restore_tab(id: TabId, ts: TabSession, config: &Config) -> Tab {
    let mut t = Tab::with_config(id, ts.loc, config);
    t.sort = ts.sort;
    t.view_mode = ts.view_mode;
    t.mode = ts.mode;
    t.filter = ts.filter.and_then(|f| Mask::parse(&f).ok());
    t.show_hidden = ts.show_hidden;
    t.pending_focus = ts.focus.map(Into::into);
    t.cursor.top = ts.top;
    for loc in ts.back {
        t.history.push(HistEntry {
            loc,
            focus: None,
            top: 0,
        });
    }
    t.restore_up = RESTORE_UP_STEPS;
    t
}
