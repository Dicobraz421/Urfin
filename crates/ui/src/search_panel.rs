//! Панель результатов поиска `search:/<id>` (S8-11, `screen-searchpanel`,
//! `screen-searchgrep`): запрос вкладки, тексты заголовка и статуса, клавиши. Без gpui —
//! рисует `gpui_shell::search`.
//!
//! Строки панели дают `SearchVfs` и листинг S4; запрос поиска окну сообщает handler
//! `search` событием `search.panel`, фича окна передаёт его командой `ui.search.label` —
//! он живёт в [`UiState`](crate::UiState), пока жив стор.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use compact_str::CompactString;
use urfin_core::search::{SearchId, SearchModel, SearchStore};
use urfin_core::{AppState, Args, CmdError, Command, CommandMeta, Cx, Location, Tab, ValueKind};
use urfin_i18n::FluentArgs;

use crate::handler::{Part, arg, meta, ui_state};
use crate::search_view::{self, Kinds};

/// Схема панели результатов.
pub const SCHEME: &str = "search";

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const LABEL: [urfin_core::ArgSpec; 5] = [
    arg("id", ValueKind::Int, true),
    arg("name", ValueKind::Str, false),
    arg("text", ValueKind::Str, false),
    arg("where", ValueKind::Str, false),
    arg("kinds", ValueKind::Str, false),
];

const COMMANDS: &[CommandMeta] = &[meta("ui.search.label", "Search tab query", &LABEL)];

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    if cmd.id.as_str() != "ui.search.label" {
        return None;
    }
    let id = cmd.args.int("id").and_then(|v| u32::try_from(v).ok());
    if let Some(id) = id {
        let alive: Vec<u32> = cx
            .state
            .ext
            .get::<SearchModel>()
            .map(|m| {
                let list = cx.state.ext.get::<crate::UiState>();
                list.into_iter()
                    .flat_map(|u| u.search_queries.iter().map(|(k, _)| *k))
                    .filter(|k| m.get(SearchId(*k)).is_some())
                    .collect()
            })
            .unwrap_or_default();
        let q = PanelQuery::from_args(&cmd.args);
        remember(&mut ui_state(cx).search_queries, &alive, id, q);
    }
    Some(Ok(()))
}

/// Запрос поиска `id` — в список; запросы отпущенных сторов (нет в `alive`) уходят.
fn remember(list: &mut Vec<(u32, PanelQuery)>, alive: &[u32], id: u32, q: PanelQuery) {
    list.retain(|(k, _)| alive.contains(k) && *k != id);
    list.push((id, q));
}

/// Запрос панели: поля формы поиска, которым она открыта.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PanelQuery {
    pub name: CompactString,
    pub text: CompactString,
    /// Поле «В»: корни через `;`.
    pub place: CompactString,
    pub kinds: Kinds,
}

impl PanelQuery {
    /// Из `search.panel` / `ui.search.label`.
    pub fn from_args(a: &Args) -> PanelQuery {
        PanelQuery {
            name: a.str("name").unwrap_or_default().into(),
            text: a.str("text").unwrap_or_default().into(),
            place: a.str("where").unwrap_or_default().into(),
            kinds: Kinds::parse(a.str("kinds")),
        }
    }

    /// Строка заголовка: `*.toml в C:\Dev`, `«Lane::Bulk» в C:\Dev\urfin\crates`.
    pub fn line(&self) -> String {
        let mut a = FluentArgs::new();
        a.set("root", self.place.to_string());
        if self.text.is_empty() {
            a.set("mask", self.mask().to_owned());
            urfin_i18n::current().fmt("ui-search-query-name", &a)
        } else {
            a.set("text", self.text.to_string());
            urfin_i18n::current().fmt("ui-search-query-text", &a)
        }
    }

    /// Имя вкладки: текст, а без него — маска (`Lane::Bulk`, `*.toml`).
    pub fn title(&self) -> &str {
        if self.text.is_empty() {
            self.mask()
        } else {
            &self.text
        }
    }

    fn mask(&self) -> &str {
        if self.name.trim().is_empty() {
            "*"
        } else {
            &self.name
        }
    }
}

/// Номер поиска панели `search:/<id>`; вид «без доступа» (`?f=errors`) и адреса строк —
/// не панель результатов.
pub fn panel_id(loc: &Location) -> Option<SearchId> {
    if loc.backend != SCHEME || loc.outer.is_some() {
        return None;
    }
    let s = loc.path.to_str()?.trim_start_matches(['/', '\\']);
    s.parse().ok().map(SearchId)
}

/// Стор панели таба `t`.
pub fn store<'a>(s: &'a AppState, t: &Tab) -> Option<&'a Arc<SearchStore>> {
    s.ext.get::<SearchModel>()?.get(panel_id(&t.loc)?)
}

/// Запрос панели таба `t`, если окно его уже знает.
pub fn query<'a>(s: &'a AppState, t: &Tab) -> Option<&'a PanelQuery> {
    let id = panel_id(&t.loc)?;
    s.ext
        .get::<crate::UiState>()?
        .search_queries
        .iter()
        .find(|(k, _)| *k == id.0)
        .map(|(_, q)| q)
}

/// Имя вкладки панели результатов — запрос; пока окно его не знает — «Поиск».
pub fn tab_title(s: &AppState, t: &Tab) -> Option<String> {
    panel_id(&t.loc)?;
    Some(query(s, t).map_or_else(
        || urfin_i18n::current().get("ui-search-chip"),
        |q| q.title().to_owned(),
    ))
}

/// Поиск идёт: тонкий прогресс, «найдено · просмотрено», Ctrl+Break.
pub fn running(s: &AppState, t: &Tab) -> bool {
    store(s, t).is_some_and(|st| !st.is_done())
}

/// Поиск идёт, а листинг панели уже догнал стор: сцене `--screens` можно снимать.
pub fn caught_up(s: &AppState, t: &Tab) -> bool {
    store(s, t).is_some_and(|st| {
        !st.is_done() && st.files_len() > 0 && t.listing.entries.len() == st.files_len()
    })
}

/// Справа в заголовке: пока идёт — «найдено N · просмотрено M», готово — «N файлов ·
/// время» (у поиска папок — «Найдено N папок · время»).
pub fn head_note(store: &SearchStore, kinds: Kinds) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    let files = store.files_len();
    if !store.is_done() {
        a.set("found", files);
        a.set("scanned", store.info.files.load(Ordering::Relaxed));
        return i18n.fmt("ui-search-panel-progress", &a);
    }
    let time = search_view::time_text(store.info.elapsed_ms.load(Ordering::Acquire));
    a.set("count", files);
    if kinds == Kinds::Dirs {
        return format!("{} · {time}", i18n.fmt("ui-search-found-dirs", &a));
    }
    a.set("time", time);
    i18n.fmt("ui-search-panel-total", &a)
}

/// Байты начала имени строки, что рисуются приглушённо: папка `dir\` до последнего `\`.
pub fn dim_len(name: &str) -> usize {
    name.rfind(['\\', '/']).map_or(0, |i| i + 1)
}

/// Имя файла строки без папки (статус-строка).
pub fn short_name(name: &str) -> &str {
    &name[dim_len(name)..]
}

/// Свободное место — у тома первого корня поиска.
pub fn space_root(s: &AppState, loc: &Location) -> Option<Location> {
    let id = panel_id(loc)?;
    s.ext.get::<SearchModel>()?.get(id)?.roots.first().cloned()
}

/// Клавиши панели (`screen-searchpanel`, `screen-searchgrep`): команда, клавиша по
/// умолчанию, подпись.
const RUNNING_KEYS: [(&str, &str, &str); 3] = [
    ("search.goto", "Ctrl+PgUp", "keybar-search-goto-folder"),
    ("search.files", "Alt+F7", "keybar-search-files-edit"),
    ("search.cancel", "Ctrl+Break", "keybar-search-cancel"),
];
const DONE_KEYS: [(&str, &str, &str); 4] = [
    ("search.view", "F3", "keybar-search-view-hit"),
    ("search.goto", "Ctrl+PgUp", "keybar-search-goto-folder"),
    ("search.files", "Alt+F7", "keybar-search-files-edit"),
    ("panel.refresh", "Ctrl+R", "keybar-search-recheck"),
];

/// Клавиша в подписи keybar — как у Far: `PgUp`, `Break`.
fn pretty(chord: &str) -> String {
    chord
        .replace("PageUp", "PgUp")
        .replace("PageDown", "PgDn")
        .replace("Pause", "Break")
}

/// Клавиши панели результатов активной панели по состоянию поиска; `None` — не она.
pub fn keys(s: &AppState) -> Option<Vec<(String, String)>> {
    let t = s.active_tab();
    let st = store(s, t)?;
    let list: &[(&str, &str, &str)] = if st.is_done() {
        &DONE_KEYS
    } else {
        &RUNNING_KEYS
    };
    Some(bound_keys(s, list))
}

/// Клавиши keybar по списку (команда, клавиша по умолчанию, подпись): привязанная в
/// активной раскладке, иначе по умолчанию.
pub(crate) fn bound_keys(
    s: &AppState,
    list: &[(&'static str, &str, &str)],
) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let km = s.ext.get::<urfin_keymap::ActiveKeymap>();
    list.iter()
        .map(|&(cmd, key, label)| {
            let bound = km.and_then(|k| k.keymap.keys_for(&Command::new(cmd), s).first().cloned());
            let key = bound.map_or_else(|| key.to_owned(), |c| pretty(&c.to_string()));
            (key, i18n.get(label))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use urfin_core::{Attrs, Entry, EntryKind};

    use super::*;

    fn loc(path: &str) -> Location {
        Location {
            backend: SCHEME.into(),
            authority: None,
            path: path.into(),
            outer: None,
        }
    }

    #[test]
    fn panel_address_is_only_the_files_view() {
        assert_eq!(panel_id(&loc("/3")), Some(SearchId(3)));
        assert_eq!(panel_id(&loc("/3?f=errors")), None);
        assert_eq!(panel_id(&loc(r"/3/src\a.rs")), None);
        assert_eq!(panel_id(&Location::local(r"C:\3")), None);
    }

    #[test]
    fn query_line_and_tab_title_follow_the_mockups() {
        urfin_i18n::set_language("ru");
        let names = PanelQuery {
            name: "*.toml".into(),
            place: r"C:\Dev".into(),
            ..PanelQuery::default()
        };
        assert_eq!(names.line(), r"*.toml в C:\Dev");
        assert_eq!(names.title(), "*.toml");
        let grep = PanelQuery {
            name: "*.rs".into(),
            text: "Lane::Bulk".into(),
            place: r"C:\Dev\urfin\crates".into(),
            ..PanelQuery::default()
        };
        assert_eq!(grep.line(), r"«Lane::Bulk» в C:\Dev\urfin\crates");
        assert_eq!(grep.title(), "Lane::Bulk");
        assert_eq!(PanelQuery::default().title(), "*");
    }

    #[test]
    fn row_name_dims_its_folder() {
        assert_eq!(dim_len(r"urfin\crates\urfin-ui\Cargo.toml"), 22);
        assert_eq!(
            short_name(r"urfin\crates\urfin-ui\Cargo.toml"),
            "Cargo.toml"
        );
        assert_eq!(dim_len("Cargo.toml"), 0);
        assert_eq!(short_name(r"C:\Dev\a.rs"), "a.rs");
    }

    fn hit(name: &str) -> urfin_core::search::Hit {
        urfin_core::search::Hit {
            e: Entry {
                name: name.into(),
                size: 1,
                mtime: 0,
                ctime: 0,
                atime: 0,
                attrs: Attrs::default(),
                kind: EntryKind::File,
            },
            root: 0,
            line: 0,
            hl: (0, 0),
            preview: None,
            status: 0,
        }
    }

    #[test]
    fn head_note_tells_progress_then_total() {
        urfin_i18n::set_language("ru");
        let store = SearchModel::default().open(vec![Location::local(r"C:\Dev")]);
        store.append((0..23).map(|i| hit(&format!("f{i}.toml"))).collect());
        store.info.files.store(4812, Ordering::Relaxed);
        assert_eq!(
            head_note(&store, Kinds::Files),
            "найдено 23 · просмотрено 4\u{a0}812"
        );
        store.finish(600);
        assert_eq!(head_note(&store, Kinds::Files), "23 файла · 0,6 с");
        assert_eq!(head_note(&store, Kinds::Dirs), "Найдено 23 папки · 0,6 с");
    }

    #[test]
    fn one_query_per_live_search() {
        let q = |name: &str| PanelQuery {
            name: name.into(),
            ..PanelQuery::default()
        };
        let mut list = Vec::new();
        remember(&mut list, &[], 1, q("*.rs"));
        remember(&mut list, &[1], 2, q("*.toml"));
        remember(&mut list, &[1, 2], 1, q("*.md"));
        assert_eq!(list, [(2, q("*.toml")), (1, q("*.md"))]);
        // Стор 2 отпущен — его запрос уходит со следующим.
        remember(&mut list, &[1], 3, q("*.txt"));
        assert_eq!(list, [(1, q("*.md")), (3, q("*.txt"))]);
    }
}
