//! Панель сравнения `cmp:/<id>` (S8-39, D-12, `screen-cmppanel`): итоги и режим
//! сравнения, тексты заголовка, статуса окна и клавиш, подписи и тона статусов строк. Без
//! gpui — рисует `gpui_shell::compare`.
//!
//! Строки панели дают `SearchVfs::cmp` и листинг S4; итоги приходят событием окна
//! `ops.compare.result { id, recursive, … }` — фича окна передаёт их командой
//! `ui.cmp.result`, они живут в [`UiState`](crate::UiState), пока жив стор.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use urfin_core::search::{SearchId, SearchModel, SearchStore};
use urfin_core::{AppState, Args, CmdError, Command, CommandMeta, Cx, Entry, Location, Tab};
use urfin_core::{ArgSpec, ValueKind};
use urfin_i18n::FluentArgs;
use urfin_vfs::ExtCell;

use crate::fmt_cell::{Cell, ColKind, FmtCfg, cell_text};
use crate::handler::{Part, arg, meta, ui_state};

/// Схема панели сравнения.
pub const SCHEME: &str = "cmp";

/// Статусы строк в порядке строки итогов (контракт `Hit.status` 1…5).
pub const STATUSES: [&str; 5] = [
    "only_left",
    "only_right",
    "newer_left",
    "newer_right",
    "differs",
];

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const RESULT: [ArgSpec; 8] = [
    arg("id", ValueKind::Int, true),
    arg("mode", ValueKind::Str, false),
    arg("only_left", ValueKind::Int, false),
    arg("only_right", ValueKind::Int, false),
    arg("newer_left", ValueKind::Int, false),
    arg("newer_right", ValueKind::Int, false),
    arg("differs", ValueKind::Int, false),
    arg("equal", ValueKind::Int, false),
];

const COMMANDS: &[CommandMeta] = &[meta("ui.cmp.result", "Compare tab totals", &RESULT)];

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    if cmd.id.as_str() != "ui.cmp.result" {
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
                    .flat_map(|u| u.cmp_totals.iter().map(|(k, _)| *k))
                    .filter(|k| m.get(SearchId(*k)).is_some())
                    .collect()
            })
            .unwrap_or_default();
        let list = &mut ui_state(cx).cmp_totals;
        list.retain(|(k, _)| alive.contains(k) && *k != id);
        list.push((id, Totals::from_args(&cmd.args)));
    }
    Some(Ok(()))
}

/// Итоги сравнения: счётчики по [`STATUSES`], одинаковых и режим.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Totals {
    pub counts: [u32; 5],
    pub equal: u64,
    /// `mode=content` — по содержимому, иначе по размеру и дате.
    pub content: bool,
}

impl Totals {
    /// Из `ops.compare.result` / `ui.cmp.result`.
    pub fn from_args(a: &Args) -> Totals {
        let n = |k: &str| u32::try_from(a.int(k).unwrap_or(0)).unwrap_or(0);
        Totals {
            counts: STATUSES.map(n),
            equal: u64::try_from(a.int("equal").unwrap_or(0)).unwrap_or(0),
            content: a.str("mode") == Some("content"),
        }
    }

    /// Аргументы `ui.cmp.result` для стора `id`.
    pub fn command(id: u32, a: &Args) -> Command {
        let t = Totals::from_args(a);
        let mut c = Command::new("ui.cmp.result")
            .arg("id", i64::from(id))
            .arg("mode", if t.content { "content" } else { "quick" })
            .arg("equal", i64::try_from(t.equal).unwrap_or(i64::MAX));
        for (k, n) in STATUSES.iter().zip(t.counts) {
            c = c.arg(k, i64::from(n));
        }
        c
    }
}

/// Роль цвета статуса: `only_*` — `exec`, `newer_*` — `accent_text`, `differs` — `modified`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Exec,
    Accent,
    Modified,
}

/// Половина значка «две панели»: залита (файл есть, у пары — более новая сторона),
/// контуром или пунктиром (стороны нет).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    Filled,
    Outline,
    Dashed,
}

/// Статус по имени из `Vfs::ext(…, "status")`.
pub fn status_code(s: &str) -> Option<&'static str> {
    STATUSES.iter().copied().find(|c| *c == s)
}

pub fn tone(code: &str) -> Tone {
    match code {
        "only_left" | "only_right" => Tone::Exec,
        "newer_left" | "newer_right" => Tone::Accent,
        _ => Tone::Modified,
    }
}

/// Значок статуса: левая и правая половины.
pub fn halves(code: &str) -> (Half, Half) {
    match code {
        "only_left" => (Half::Filled, Half::Dashed),
        "only_right" => (Half::Dashed, Half::Filled),
        "newer_left" => (Half::Filled, Half::Outline),
        "newer_right" => (Half::Outline, Half::Filled),
        _ => (Half::Outline, Half::Outline),
    }
}

/// Подпись статуса (`только слева`, `left only`, …).
pub fn label(code: &str) -> String {
    let key = match code {
        "only_left" => "ui-cmp-only_left",
        "only_right" => "ui-cmp-only_right",
        "newer_left" => "ui-cmp-newer_left",
        "newer_right" => "ui-cmp-newer_right",
        _ => "ui-cmp-differs",
    };
    urfin_i18n::current().get(key)
}

/// Строка лежит на левой стороне: где файл есть, у пары — более новая, у `differs` — левая.
pub fn on_left(code: &str) -> bool {
    !matches!(code, "only_right" | "newer_right")
}

/// Номер сравнения панели `cmp:/<id>`; адреса строк — не панель.
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

/// Итоги панели таба `t`, если сравнение закончено.
pub fn totals<'a>(s: &'a AppState, t: &Tab) -> Option<&'a Totals> {
    let id = panel_id(&t.loc)?;
    s.ext
        .get::<crate::UiState>()?
        .cmp_totals
        .iter()
        .find(|(k, _)| *k == id.0)
        .map(|(_, t)| t)
}

/// Свободное место — у тома левой стороны.
pub fn space_root(s: &AppState, loc: &Location) -> Option<Location> {
    let id = panel_id(loc)?;
    s.ext.get::<SearchModel>()?.get(id)?.roots.first().cloned()
}

/// Сравнение идёт: тонкий прогресс, Ctrl+Break.
pub fn running(s: &AppState, t: &Tab) -> bool {
    store(s, t).is_some_and(|st| !st.is_done())
}

/// Сторона сравнения в заголовке: локальная — путь, иначе uri.
fn shown(loc: &Location) -> String {
    if loc.is_local() {
        loc.path.display().to_string()
    } else {
        loc.to_uri()
    }
}

/// Имя стороны во вкладке: последняя папка (у корня тома — сам путь).
fn short(loc: &Location) -> String {
    let base = if loc.path.file_name().is_none() {
        loc.outer.as_deref().unwrap_or(loc)
    } else {
        loc
    };
    base.path
        .file_name()
        .map_or_else(|| shown(base), |n| n.to_string_lossy().into_owned())
}

/// Стороны через `↔`: полные — в заголовке, имена — во вкладке.
fn pair(store: &SearchStore, f: fn(&Location) -> String) -> Option<String> {
    let [l, r] = &*store.roots else {
        return None;
    };
    Some(format!("{} \u{2194} {}", f(l), f(r)))
}

/// Заголовок: «C:\Dev\urfin ↔ D:\Temp\urfin-main».
pub fn head_line(store: &SearchStore) -> String {
    pair(store, shown).unwrap_or_default()
}

/// Имя вкладки: «urfin ↔ urfin-main»; стор отпущен — «Сравнение».
pub fn tab_title(s: &AppState, t: &Tab) -> Option<String> {
    panel_id(&t.loc)?;
    Some(
        store(s, t)
            .and_then(|st| pair(st, short))
            .unwrap_or_else(|| urfin_i18n::current().get("ui-cmp-chip")),
    )
}

/// Справа в заголовке: пока идёт — «различий N · просмотрено M», готово — «одинаковых N ·
/// по содержимому» (или «по размеру и дате»).
pub fn head_note(store: &SearchStore, totals: Option<&Totals>) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    match totals {
        Some(t) if store.is_done() => {
            a.set("count", t.equal);
            let mode = i18n.get(if t.content {
                "ui-cmp-mode-content"
            } else {
                "ui-cmp-mode-quick"
            });
            format!("{} · {mode}", i18n.fmt("ui-cmp-equal", &a))
        }
        _ if store.is_done() => String::new(),
        _ => {
            a.set("found", store.files_len());
            a.set("scanned", store.info.files.load(Ordering::Relaxed));
            i18n.fmt("ui-cmp-progress", &a)
        }
    }
}

/// Статус окна слева у файла панели сравнения: «compare.rs · слева 5,3 КБ · 11.11.26
/// 10:37 · справа 4,9 КБ · 10.11.26 18:20» — обе стороны пары; у файла одной стороны —
/// только она. Папка — `None` (как обычно). Ячейки — из памяти backend'а, без I/O.
pub fn status_line(s: &AppState, t: &Tab, e: &Entry, fmt: &FmtCfg) -> Option<String> {
    panel_id(&t.loc)?;
    if e.kind.is_dir_like() {
        return None;
    }
    let vfs = s.ext.get::<urfin_vfs::Schemes>()?.with_columns(&t.loc)?;
    let code = match vfs.ext(&t.loc, e, "status")? {
        ExtCell::Text(c) => status_code(&c)?,
        _ => return None,
    };
    let other = match (vfs.ext(&t.loc, e, "size2"), vfs.ext(&t.loc, e, "mtime2")) {
        (Some(ExtCell::Size(size)), Some(ExtCell::Date(mtime))) => Some(Entry {
            size,
            mtime,
            ..e.clone()
        }),
        _ => None,
    };
    let (left, right) = if on_left(code) {
        (Some(e.clone()), other)
    } else {
        (other, Some(e.clone()))
    };
    let i18n = urfin_i18n::current();
    let mut cell = Cell::new();
    let mut side = |key: &str, e: &Entry| {
        let mut a = FluentArgs::new();
        cell_text(ColKind::Size, e, fmt, &mut cell);
        a.set("size", cell.as_str().trim().to_owned());
        cell_text(ColKind::Mtime, e, fmt, &mut cell);
        a.set("date", cell.as_str().trim().to_owned());
        i18n.fmt(key, &a)
    };
    let mut parts = vec![crate::search_panel::short_name(&e.name).to_owned()];
    if let Some(l) = &left {
        parts.push(side("ui-cmp-left", l));
    }
    if let Some(r) = &right {
        parts.push(side("ui-cmp-right", r));
    }
    Some(parts.join(" · "))
}

/// Ячейка статуса строки — код статуса (`None` — не статус).
pub fn cell_code(v: &ExtCell) -> Option<&'static str> {
    match v {
        ExtCell::Text(c) => status_code(c),
        _ => None,
    }
}

/// Клавиши панели (`screen-cmppanel`): команда, клавиша по умолчанию, подпись.
const RUNNING_KEYS: [(&str, &str, &str); 2] = [
    ("search.goto", "Ctrl+PgUp", "keybar-search-goto-folder"),
    ("search.cancel", "Ctrl+Break", "keybar-search-cancel"),
];
const DONE_KEYS: [(&str, &str, &str); 3] = [
    ("ops.copy", "F5", "keybar-cmp-copy"),
    ("search.goto", "Ctrl+PgUp", "keybar-search-goto-folder"),
    ("panel.refresh", "Ctrl+R", "keybar-cmp-again"),
];

/// Клавиши панели сравнения активной панели; `None` — не она.
pub fn keys(s: &AppState) -> Option<Vec<(String, String)>> {
    let st = store(s, s.active_tab())?;
    let list: &[(&str, &str, &str)] = if st.is_done() {
        &DONE_KEYS
    } else {
        &RUNNING_KEYS
    };
    Some(crate::search_panel::bound_keys(s, list))
}

#[cfg(test)]
mod tests {
    use urfin_core::search::Hit;
    use urfin_core::{Attrs, EntryKind};

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
    fn panel_address_is_only_the_root() {
        assert_eq!(panel_id(&loc("/4")), Some(SearchId(4)));
        assert_eq!(panel_id(&loc(r"/4/src\a.rs")), None);
        let search = Location {
            backend: crate::search_panel::SCHEME.into(),
            ..loc("/4")
        };
        assert_eq!(panel_id(&search), None);
    }

    #[test]
    fn statuses_follow_the_mockup() {
        assert_eq!(
            STATUSES.map(tone),
            [
                Tone::Exec,
                Tone::Exec,
                Tone::Accent,
                Tone::Accent,
                Tone::Modified
            ]
        );
        assert_eq!(halves("only_left"), (Half::Filled, Half::Dashed));
        assert_eq!(halves("newer_right"), (Half::Outline, Half::Filled));
        assert_eq!(halves("differs"), (Half::Outline, Half::Outline));
        assert_eq!(status_code("newer_left"), Some("newer_left"));
        assert_eq!(status_code("equal"), None);
        assert!(on_left("differs") && !on_left("newer_right"));
        urfin_i18n::set_language("ru");
        assert_eq!(label("only_right"), "только справа");
        urfin_i18n::set_language("en");
        assert_eq!(label("only_right"), "right only");
        urfin_i18n::set_language("ru");
    }

    fn hit(name: &str) -> Hit {
        Hit {
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
            status: 5,
        }
    }

    #[test]
    fn head_tells_progress_then_identical_and_mode() {
        urfin_i18n::set_language("ru");
        let store = SearchModel::default().open(vec![
            Location::local(r"C:\Dev\urfin"),
            Location::local(r"D:\Temp\urfin-main"),
        ]);
        assert_eq!(head_line(&store), r"C:\Dev\urfin ↔ D:\Temp\urfin-main");
        assert_eq!(pair(&store, short).as_deref(), Some("urfin ↔ urfin-main"));
        store.append(vec![hit("a.rs"), hit("b.rs")]);
        store.info.files.store(1200, Ordering::Relaxed);
        assert_eq!(
            head_note(&store, None),
            "различий 2 · просмотрено 1\u{a0}200"
        );
        store.finish(10);
        let args = Args::default()
            .with("mode", "content")
            .with("differs", 2)
            .with("equal", 1187);
        let t = Totals::from_args(&args);
        assert_eq!(t.counts, [0, 0, 0, 0, 2]);
        assert_eq!(
            head_note(&store, Some(&t)),
            "одинаковых 1\u{a0}187 · по содержимому"
        );
        let quick = Totals {
            content: false,
            ..t
        };
        assert_eq!(
            head_note(&store, Some(&quick)),
            "одинаковых 1\u{a0}187 · по размеру и дате"
        );
        // Команда окна переносит те же итоги.
        let c = Totals::command(3, &args);
        assert_eq!(c.args.int("id"), Some(3));
        assert_eq!(Totals::from_args(&c.args), t);
    }

    #[test]
    fn tab_name_of_a_volume_root_is_its_path() {
        assert_eq!(short(&Location::local(r"D:\")), r"D:\");
        assert_eq!(short(&Location::local(r"C:\Dev\urfin")), "urfin");
    }
}
