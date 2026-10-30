//! Состояние приложения. Владеет им Core в UI-потоке; мутации — только в handlers
//! и `Msg` (ADR-0001).

use std::any::{Any, TypeId};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use compact_str::CompactString;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::cmd::{Args, Command, CommandId};
use crate::config::{Config, ConfigWarning};
use crate::history::VisitedDirs;
use crate::ids::{PromptId, Side, TabId};
use crate::jobs::JobRegistry;
use crate::loc::Location;
use crate::prompt::Prompts;
use crate::tab::Tab;
use crate::text::tr;

/// Куда идёт ввод (S0 v3 №1, v5 №1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Focus {
    #[default]
    Panel,
    Cmdline,
    Prompt(PromptId),
    Viewer,
    Editor,
    Terminal,
    Overlay(OverlayKind),
}

/// Inline-оверлей с фокусом.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum OverlayKind {
    Menu,
    QuickSearch,
    InlineEdit,
    Draft,
    Palette,
    /// Поиск, замена и goto во вьювере и редакторе (S7).
    Find,
}

/// Панель: набор табов.
#[derive(Debug)]
pub struct Panel {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub visible: bool,
}

impl Panel {
    pub fn new(tab: Tab) -> Self {
        Panel {
            tabs: vec![tab],
            active: 0,
            visible: true,
        }
    }

    /// Активный таб. Инвариант: табов ≥ 1, `active < tabs.len()`.
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Сигнал фронтенду; забирается `Core::take_ui_events` раз в кадр.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    Toast {
        level: Level,
        text: String,
    },
    ScrollToCursor(TabId),
    FocusChanged(Focus),
    /// Сигналы для S2/S6/S7 без изменения core.
    Ext {
        id: CommandId,
        args: Args,
    },
}

/// Inline-баннер: «прошлый запуск упал», ошибки конфига.
#[derive(Clone, Debug, PartialEq)]
pub struct Banner {
    pub level: Level,
    pub text: String,
    /// Кнопки: подпись и команда (S0 v2 №15).
    pub actions: SmallVec<[(CompactString, Command); 3]>,
}

/// Состояние других крейтов в `AppState` (S0 v2 №10): ключ — тип, объявленный в
/// своём крейте. Мутации — только в Handler/Msg.
#[derive(Default)]
pub struct Extensions(FxHashMap<TypeId, Box<dyn Any>>);

impl Extensions {
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.0.get(&TypeId::of::<T>())?.downcast_ref()
    }

    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.0.get_mut(&TypeId::of::<T>())?.downcast_mut()
    }

    /// Возвращает прежнее значение этого типа.
    pub fn insert<T: 'static>(&mut self, v: T) -> Option<T> {
        self.0
            .insert(TypeId::of::<T>(), Box::new(v))
            .and_then(|old| old.downcast().ok().map(|b| *b))
    }
}

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extensions")
            .field("len", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Состояние приложения.
#[derive(Debug)]
pub struct AppState {
    pub panels: [Panel; 2],
    pub active: Side,
    pub focus: Focus,
    pub config: Arc<Config>,
    pub jobs: JobRegistry,
    pub prompts: Prompts,
    /// Глобальный MRU каталогов (Alt+F12).
    pub visited: VisitedDirs,
    pub banner: Option<Banner>,
    /// Закладки каталогов 0–9 (`panel.bookmark.*`, S0 v3 №4); хранятся в сессии.
    pub bookmarks: [Option<Location>; 10],
    pub ext: Extensions,
    /// Растёт при любой мутации — ключ кэшей UI.
    pub rev: u64,
    pub quit: bool,
    ui_events: Vec<UiEvent>,
    next_tab: u32,
    /// Запрос, открытый по вето на выход (`app.quit`).
    pub(crate) quit_prompt: Option<PromptId>,
    /// Текущий баннер — о проблемах конфига (снимается при чистой перезагрузке).
    config_banner: bool,
    pub(crate) session: SessionMarks,
    /// Настройки из UI, ждущие записи в `config.toml` (S2-22).
    pub(crate) settings: crate::settings::Pending,
}

/// Отметки для записи сессии (Core, конец pump).
#[derive(Debug, Default)]
pub(crate) struct SessionMarks {
    /// Что-то из сессии изменилось (табы, пути, вид, закладки).
    pub(crate) dirty: bool,
    /// `app.save_session`: записать без ожидания.
    pub(crate) force: bool,
}

impl AppState {
    pub fn new(left: Panel, right: Panel) -> Self {
        let next_tab = left
            .tabs
            .iter()
            .chain(&right.tabs)
            .map(|t| t.id.0)
            .max()
            .unwrap_or(0)
            + 1;
        AppState {
            panels: [left, right],
            active: Side::Left,
            focus: Focus::Panel,
            config: Arc::default(),
            jobs: JobRegistry::default(),
            prompts: Prompts::default(),
            visited: VisitedDirs::default(),
            banner: None,
            bookmarks: Default::default(),
            ext: Extensions::default(),
            rev: 0,
            quit: false,
            ui_events: Vec::new(),
            next_tab,
            quit_prompt: None,
            config_banner: false,
            session: SessionMarks::default(),
            settings: crate::settings::Pending::default(),
        }
    }

    /// Баннер «прошлый запуск упал» (S1 §2.9, S0 v2 №15): показать каталог с
    /// crash-файлом или скрыть.
    pub(crate) fn set_crash_banner(&mut self, file: &Path) {
        let dir = file.parent().unwrap_or(file).to_path_buf();
        let mut actions = SmallVec::new();
        actions.push((
            tr("core-action-show-logs", &[]).into(),
            Command::new("panel.go").arg("path", Location::local(dir)),
        ));
        actions.push((
            tr("core-action-dismiss", &[]).into(),
            Command::new("app.dismiss_banner"),
        ));
        self.banner = Some(Banner {
            level: Level::Error,
            text: tr("core-crashed", &[("file", &file.display().to_string())]),
            actions,
        });
        self.config_banner = false;
        self.touch();
    }

    pub(crate) fn dismiss_banner(&mut self) {
        self.banner = None;
        self.config_banner = false;
        self.touch();
    }

    pub(crate) fn reset_tab_ids(&mut self, next: u32) {
        self.next_tab = next;
    }

    /// Отметить изменение, которое попадает в сессию.
    pub(crate) fn mark_session_dirty(&mut self) {
        self.session.dirty = true;
    }

    /// Применить конфиг: ёмкость истории каталогов и баннер о проблемах. Табы
    /// получают настройки панелей при создании (`Tab::with_config`).
    pub(crate) fn set_config(&mut self, c: Arc<Config>, warnings: &[ConfigWarning]) {
        // Политика организации (F00-01) — поверх файла: её кладёт в `ext` фронтенд.
        let c = match self.ext.get::<crate::PolicySet>() {
            Some(p) if !p.is_empty() => {
                let mut c = Arc::unwrap_or_clone(c);
                p.apply(&mut c);
                Arc::new(c)
            }
            _ => c,
        };
        self.visited.set_cap(c.panels.visited_cap.into());
        self.config = c;
        if let Some(first) = warnings.first() {
            self.banner = Some(Banner {
                level: Level::Warn,
                text: tr(
                    "core-config-warnings",
                    &[
                        ("count", &warnings.len().to_string()),
                        ("first", &first.to_string()),
                    ],
                ),
                actions: SmallVec::new(),
            });
            self.config_banner = true;
        } else if self.config_banner {
            self.banner = None;
            self.config_banner = false;
        }
        self.touch();
    }

    /// Новый уникальный id таба.
    pub(crate) fn alloc_tab_id(&mut self) -> TabId {
        let id = TabId(self.next_tab);
        self.next_tab += 1;
        id
    }

    pub fn panel(&self, s: Side) -> &Panel {
        &self.panels[s.idx()]
    }

    pub fn panel_mut(&mut self, s: Side) -> &mut Panel {
        &mut self.panels[s.idx()]
    }

    pub fn active_panel(&self) -> &Panel {
        self.panel(self.active)
    }

    pub fn active_tab(&self) -> &Tab {
        self.active_panel().tab()
    }

    pub fn passive_tab(&self) -> &Tab {
        self.panel(self.active.other()).tab()
    }

    pub fn tab(&self, id: TabId) -> Option<(Side, &Tab)> {
        [Side::Left, Side::Right].into_iter().find_map(|s| {
            self.panel(s)
                .tabs
                .iter()
                .find(|t| t.id == id)
                .map(|t| (s, t))
        })
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.panels
            .iter_mut()
            .flat_map(|p| p.tabs.iter_mut())
            .find(|t| t.id == id)
    }

    /// Отметить мутацию.
    pub fn touch(&mut self) {
        self.rev += 1;
    }

    /// Событие для фронтенда. Тоже изменение: поднимает `rev`, чтобы UI перерисовался.
    pub(crate) fn push_ui(&mut self, e: UiEvent) {
        self.ui_events.push(e);
        self.touch();
    }

    pub(crate) fn take_ui_events(&mut self) -> Vec<UiEvent> {
        std::mem::take(&mut self.ui_events)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, Banner, Extensions, Focus, Level, OverlayKind, Panel, UiEvent};
    use crate::cmd::Command;
    use crate::ids::{Side, TabId};
    use crate::loc::Location;
    use crate::tab::Tab;
    use smallvec::smallvec;

    fn state() -> AppState {
        let l = |id, p| Tab::new(TabId(id), Location::local(p));
        let mut left = Panel::new(l(1, "/a"));
        left.tabs.push(l(2, "/b"));
        left.active = 1;
        AppState::new(left, Panel::new(l(3, "/c")))
    }

    #[test]
    fn navigation_helpers() {
        let mut s = state();
        assert_eq!(s.active_tab().id, TabId(2));
        assert_eq!(s.passive_tab().id, TabId(3));
        s.active = Side::Right;
        assert_eq!(s.active_tab().id, TabId(3));
        assert_eq!(s.passive_tab().id, TabId(2));
        assert_eq!(
            s.tab(TabId(1)).map(|(side, t)| (side, t.id)),
            Some((Side::Left, TabId(1)))
        );
        assert_eq!(s.tab(TabId(3)).map(|(side, _)| side), Some(Side::Right));
        assert!(s.tab(TabId(9)).is_none());
        s.tab_mut(TabId(1)).unwrap().show_hidden = true;
        assert!(s.panel(Side::Left).tabs[0].show_hidden);
        s.panel_mut(Side::Right).visible = false;
        assert!(!s.panel(Side::Right).visible);
    }

    #[test]
    fn ui_events_are_taken_once() {
        let mut s = state();
        s.push_ui(UiEvent::FocusChanged(Focus::Overlay(OverlayKind::Find)));
        s.push_ui(UiEvent::ScrollToCursor(TabId(2)));
        assert_eq!(s.take_ui_events().len(), 2);
        assert_eq!(s.take_ui_events(), []);
    }

    #[test]
    fn extensions_typemap() {
        #[derive(Debug, PartialEq)]
        struct OpsUi(u32);
        struct Other;
        let mut e = Extensions::default();
        assert!(e.get::<OpsUi>().is_none());
        assert_eq!(e.insert(OpsUi(1)), None);
        e.get_mut::<OpsUi>().unwrap().0 += 1;
        assert_eq!(e.get::<OpsUi>(), Some(&OpsUi(2)));
        assert_eq!(e.insert(OpsUi(5)), Some(OpsUi(2)));
        assert!(e.get::<Other>().is_none());
        e.insert(Other);
        assert!(e.get::<Other>().is_some());
    }

    #[test]
    fn banner_actions_and_focus_default() {
        let b = Banner {
            level: Level::Warn,
            text: "crashed".into(),
            actions: smallvec![
                ("continue".into(), Command::new("app.save_session")),
                ("clear".into(), Command::new("app.reload_config")),
            ],
        };
        assert_eq!(b.actions.len(), 2);
        assert!(!b.actions.spilled());
        assert_eq!(state().focus, Focus::Panel);
        assert_eq!(Focus::default(), Focus::Panel);
    }
}
