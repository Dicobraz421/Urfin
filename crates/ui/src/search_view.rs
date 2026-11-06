//! Экран поиска (S8 §2.5, `screen-search`): форма и команды запуска, тексты итога и статуса,
//! части фрагмента, окно строк. Без gpui — рисует `gpui_shell::search`.
//!
//! Данные — `urfin_core::search` (решение 2026-11-06 «s8-answers», В1). Команды `ui.search.*`
//! меняют то, что фича окна сама менять не может: фокус Core и строку курсора по клику.

use compact_str::CompactString;
use urfin_core::search::{Hit, SearchModel, SearchStore};
use urfin_core::{
    ArgSpec, Args, CmdError, Command, CommandMeta, Cx, Focus, OverlayKind, Value, ValueKind,
};
use urfin_i18n::FluentArgs;

use crate::field_history::FieldKind;
use crate::handler::{Part, meta, open, set_focus, ui_state};

pub(crate) const PART: Part = Part {
    commands: COMMANDS,
    handle,
};

const COMMANDS: &[CommandMeta] = &[
    meta(
        "ui.search.focus",
        "Focus search screen",
        &[ArgSpec {
            name: "open",
            kind: ValueKind::Bool,
            required: false,
        }],
    ),
    meta(
        "ui.search.row",
        "Result row",
        &[ArgSpec {
            name: "row",
            kind: ValueKind::Int,
            required: true,
        }],
    ),
];

fn handle(cmd: &Command, cx: &mut Cx<'_>) -> Option<Result<(), CmdError>> {
    match cmd.id.as_str() {
        "ui.search.focus" => {
            if cmd.args.bool("open").unwrap_or(true) {
                open(cx, OverlayKind::Search, |_| {});
            } else if cx.state.focus == Focus::Overlay(OverlayKind::Search) {
                ui_state(cx).close_overlay();
                set_focus(cx, Focus::Panel);
            }
        }
        "ui.search.row" => {
            let row = cmd.args.int("row").unwrap_or(0).max(0);
            if let Some(m) = cx.state.ext.get_mut::<SearchModel>() {
                let last = m.current().map_or(0, |s| s.rows_len()).saturating_sub(1);
                let row = usize::try_from(row).unwrap_or(usize::MAX).min(last);
                m.set_cursor(u32::try_from(row).unwrap_or(u32::MAX));
            }
        }
        _ => return None,
    }
    Some(Ok(()))
}

/// Что ищет экран: Alt+F7 — файлы, Alt+T — папки.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Kinds {
    #[default]
    Files,
    Dirs,
}

impl Kinds {
    fn parse(s: Option<&str>) -> Kinds {
        if s == Some("dirs") {
            Kinds::Dirs
        } else {
            Kinds::Files
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kinds::Files => "files",
            Kinds::Dirs => "dirs",
        }
    }
}

/// Форма экрана: только поля макета (В3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub name: CompactString,
    pub text: CompactString,
    /// Поле «В», как введено.
    pub place: CompactString,
    pub case: bool,
    pub regex: bool,
    pub recursive: bool,
    pub kinds: Kinds,
}

impl Form {
    /// Из `UiEvent::Ext { id: "search.screen" }`.
    pub fn from_args(a: &Args) -> Form {
        Form {
            name: a.str("name").unwrap_or_default().into(),
            text: a.str("text").unwrap_or_default().into(),
            place: a.str("where").unwrap_or_default().into(),
            case: a.bool("case").unwrap_or(false),
            regex: a.bool("regex").unwrap_or(false),
            recursive: a.bool("recursive").unwrap_or(true),
            kinds: Kinds::parse(a.str("kinds")),
        }
    }

    /// Enter в форме: поля — в их историю (№70), затем `search.run`.
    pub fn run(&self) -> Vec<Command> {
        let mut out: Vec<Command> = [
            (FieldKind::Mask, &self.name),
            (FieldKind::Find, &self.text),
            (FieldKind::Path, &self.place),
        ]
        .into_iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(kind, text)| {
            Command::new("ui.field.remember")
                .arg("kind", kind.as_str())
                .arg("text", text.trim())
        })
        .collect();
        out.push(
            Command::new("search.run")
                .arg("name", self.name.as_str())
                .arg("text", self.text.as_str())
                .arg("where", vec![Value::Str(self.place.as_str().into())])
                .arg("case", self.case)
                .arg("regex", self.regex)
                .arg("recursive", self.recursive)
                .arg("kinds", self.kinds.as_str()),
        );
        out
    }
}

/// Части фрагмента: до совпадения, совпадение, после. `hl` вне строки или не на границе
/// символа — фрагмент без подсветки.
pub fn hit_parts(h: &Hit) -> (&str, &str, &str) {
    let p = h.preview.as_deref().unwrap_or_default();
    let (a, b) = (usize::from(h.hl.0), usize::from(h.hl.1));
    if a < b && b <= p.len() && p.is_char_boundary(a) && p.is_char_boundary(b) {
        (&p[..a], &p[a..b], &p[b..])
    } else {
        (p, "", "")
    }
}

/// Первая видимая строка: курсор — в окне из `rows` целых строк, конец ряда не отрывается.
pub fn follow(top: usize, cursor: usize, rows: usize, len: usize) -> usize {
    let rows = rows.max(1);
    let top = if cursor < top {
        cursor
    } else if cursor >= top + rows {
        cursor + 1 - rows
    } else {
        top
    };
    clamp_top(top, rows, len)
}

/// Верх окна: за концом ряда пустоты нет.
pub fn clamp_top(top: usize, rows: usize, len: usize) -> usize {
    top.min(len.saturating_sub(rows.max(1)))
}

/// Строка под точкой `y` от верха списка.
pub fn row_at(y: f32, row_h: f32, top: usize, len: usize) -> Option<usize> {
    if y < 0.0 || row_h <= 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "строк на экране десятки, значение неотрицательно"
    )]
    let row = top + (y / row_h) as usize;
    (row < len).then_some(row)
}

/// Время поиска: секунды с десятыми (`0,4 с`), от минуты — `1 мин 08 с`.
pub fn time_text(ms: u64) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    if ms < 59_950 {
        #[allow(clippy::cast_precision_loss, reason = "меньше минуты")]
        let sec = ms.div_ceil(100) as f64 / 10.0;
        a.set("sec", sec);
        i18n.fmt("du-time-sec", &a)
    } else {
        let s = ms.div_ceil(1000);
        a.set("min", s / 60);
        a.set("sec", format!("{:02}", s % 60));
        i18n.fmt("du-time-min-sec", &a)
    }
}

/// Итог слева под списком: совпадения, а без текста — файлы или папки.
fn found(store: &SearchStore, by_name: bool, kinds: Kinds) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    a.set("count", store.rows_len());
    if !by_name {
        a.set("files", store.files_len());
        return i18n.fmt("ui-search-summary", &a);
    }
    i18n.fmt(
        match kinds {
            Kinds::Files => "ui-search-found-files",
            Kinds::Dirs => "ui-search-found-dirs",
        },
        &a,
    )
}

/// Запрос без текста: только по имени.
fn by_name(m: &SearchModel) -> bool {
    m.history().front().is_some_and(|q| q.text.is_empty())
}

/// Итог под списком: слева найдено, справа просмотрено. `ms` — время поиска.
pub fn summary(m: &SearchModel, kinds: Kinds, ms: u64) -> (String, String) {
    let Some(store) = m.current() else {
        return Default::default();
    };
    let mut a = FluentArgs::new();
    a.set(
        "count",
        store.info.files.load(std::sync::atomic::Ordering::Relaxed),
    );
    a.set("time", time_text(ms));
    (
        found(store, by_name(m), kinds),
        urfin_i18n::current().fmt("ui-search-scanned", &a),
    )
}

/// Статус-строка экрана: запрос · число · «готово за».
pub fn status(m: &SearchModel, kinds: Kinds) -> [String; 3] {
    let (Some(store), Some(q)) = (m.current(), m.history().front()) else {
        return Default::default();
    };
    let i18n = urfin_i18n::current();
    let query = [q.text.as_str(), q.name.as_str()]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or("*");
    let mut a = FluentArgs::new();
    a.set("query", query.to_owned());
    a.set("root", q.place.to_string());
    let left = i18n.fmt("ui-search-status", &a);
    let middle = if q.text.is_empty() {
        found(store, true, kinds)
    } else {
        let mut a = FluentArgs::new();
        a.set("total", store.rows_len());
        i18n.fmt("ui-find-count", &a)
    };
    let right = if store.is_done() {
        let ms = store
            .info
            .elapsed_ms
            .load(std::sync::atomic::Ordering::Acquire);
        let mut a = FluentArgs::new();
        a.set("time", time_text(ms));
        i18n.fmt("ui-search-status-done", &a)
    } else {
        String::new()
    };
    [left, middle, right]
}

#[cfg(test)]
mod tests {
    use urfin_core::search::PastQuery;
    use urfin_core::{Attrs, Entry, EntryKind, Location};
    use urfin_i18n::{Catalog, I18n};

    use super::*;

    fn hit(name: &str, preview: Option<&str>, hl: (u16, u16)) -> Hit {
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
            line: 1,
            hl,
            preview: preview.map(Into::into),
            status: 0,
        }
    }

    fn lang(code: &str) -> I18n {
        Catalog::embedded().build(&code.parse().unwrap()).0
    }

    fn found_text(code: &str, key: &str, n: u64) -> String {
        let mut a = FluentArgs::new();
        a.set("count", n);
        lang(code).fmt(key, &a)
    }

    #[test]
    fn found_counts_agree_in_both_languages() {
        let files = "ui-search-found-files";
        let dirs = "ui-search-found-dirs";
        let want = [
            (1, "Найден 1 файл", "Найдена 1 папка"),
            (2, "Найдено 2 файла", "Найдено 2 папки"),
            (5, "Найдено 5 файлов", "Найдено 5 папок"),
            (21, "Найден 21 файл", "Найдена 21 папка"),
        ];
        for (n, f, d) in want {
            assert_eq!(found_text("ru", files, n), f);
            assert_eq!(found_text("ru", dirs, n), d);
        }
        let want = [
            (1, "Found 1 file", "Found 1 folder"),
            (2, "Found 2 files", "Found 2 folders"),
            (5, "Found 5 files", "Found 5 folders"),
            (21, "Found 21 files", "Found 21 folders"),
        ];
        for (n, f, d) in want {
            assert_eq!(found_text("en", files, n), f);
            assert_eq!(found_text("en", dirs, n), d);
        }
    }

    #[test]
    fn form_runs_with_a_list_in_where_and_remembers_fields() {
        let form = Form {
            name: "*.rs".into(),
            text: "spawn_blocking".into(),
            place: r"C:\Dev".into(),
            case: true,
            recursive: true,
            kinds: Kinds::Dirs,
            ..Form::default()
        };
        let cmds = form.run();
        assert_eq!(cmds.len(), 4);
        let kinds: Vec<_> = cmds[..3]
            .iter()
            .filter_map(|c| c.args.str("kind"))
            .collect();
        assert_eq!(kinds, ["mask", "find", "path"]);
        let run = &cmds[3];
        assert_eq!(run.id.as_str(), "search.run");
        assert_eq!(run.args.str("name"), Some("*.rs"));
        assert_eq!(run.args.bool("case"), Some(true));
        assert_eq!(run.args.str("kinds"), Some("dirs"));
        assert!(
            run.args.list("where").is_some(),
            "`where` — список, иначе реестр откажет"
        );
    }

    #[test]
    fn empty_fields_do_not_reach_history() {
        let cmds = Form::default().run();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id.as_str(), "search.run");
    }

    #[test]
    fn form_reads_screen_args() {
        let a = Args::default()
            .with("name", "*.toml")
            .with("where", r"C:\x")
            .with("recursive", false)
            .with("kinds", "dirs");
        let f = Form::from_args(&a);
        assert_eq!(f.name, "*.toml");
        assert_eq!(f.place, r"C:\x");
        assert!(!f.recursive);
        assert_eq!(f.kinds, Kinds::Dirs);
    }

    #[test]
    fn hit_parts_split_and_survive_bad_ranges() {
        let h = hit("a", Some("pool.spawn(x)"), (5, 10));
        assert_eq!(hit_parts(&h), ("pool.", "spawn", "(x)"));
        let h = hit("a", Some("пул"), (1, 3));
        assert_eq!(hit_parts(&h), ("пул", "", ""), "не граница символа");
        let h = hit("a", Some("abc"), (1, 40));
        assert_eq!(hit_parts(&h), ("abc", "", ""));
        let h = hit("a", None, (0, 0));
        assert_eq!(hit_parts(&h), ("", "", ""));
    }

    #[test]
    fn follow_keeps_the_cursor_in_the_window() {
        assert_eq!(follow(0, 5, 10, 100), 0);
        assert_eq!(follow(0, 10, 10, 100), 1);
        assert_eq!(follow(40, 39, 10, 100), 39);
        assert_eq!(follow(90, 99, 10, 100), 90);
        assert_eq!(follow(95, 99, 10, 100), 90, "конец ряда не отрывается");
        assert_eq!(follow(0, 0, 10, 0), 0);
        assert_eq!(follow(0, 999_999, 30, 1_000_000), 999_970);
    }

    #[test]
    fn row_under_the_pointer() {
        assert_eq!(row_at(0.0, 24.0, 10, 100), Some(10));
        assert_eq!(row_at(47.9, 24.0, 10, 100), Some(11));
        assert_eq!(row_at(240.0, 24.0, 0, 5), None, "ниже последней");
        assert_eq!(row_at(-1.0, 24.0, 0, 5), None);
    }

    fn model(text: &str, kinds_rows: usize) -> SearchModel {
        let mut m = SearchModel::default();
        let store = m.open(vec![Location::local(r"C:\Dev")]);
        store.append(
            (0..kinds_rows)
                .map(|i| hit(&format!("f{}", i / 2), None, (0, 0)))
                .collect(),
        );
        m.remember(PastQuery {
            name: "*.rs".into(),
            text: text.into(),
            place: r"C:\Dev".into(),
            ..PastQuery::default()
        });
        m.show(Some(store.id));
        m
    }

    #[test]
    fn name_search_counts_files_not_matches() {
        urfin_i18n::set_language("ru");
        let m = model("", 1);
        assert_eq!(summary(&m, Kinds::Files, 400).0, "Найден 1 файл");
        assert_eq!(summary(&m, Kinds::Dirs, 400).0, "Найдена 1 папка");
        assert_eq!(status(&m, Kinds::Files)[1], "Найден 1 файл");
        let m = model("spawn", 9);
        assert_eq!(
            summary(&m, Kinds::Files, 400).0,
            "Найдено 9 совпадений в 5 файлах"
        );
        assert_eq!(status(&m, Kinds::Files)[1], "9 совпадений");
    }

    #[test]
    fn status_tells_query_and_time() {
        urfin_i18n::set_language("ru");
        let m = model("spawn", 1);
        let z = status(&m, Kinds::Files);
        assert_eq!(z[0], r"Поиск «spawn» в C:\Dev");
        assert_eq!(z[2], "", "пока идёт — времени нет");
        m.current().unwrap().finish(400);
        assert_eq!(status(&m, Kinds::Files)[2], "готово за 0,4 с");
        let m = model("", 1);
        assert_eq!(status(&m, Kinds::Files)[0], r"Поиск «*.rs» в C:\Dev");
        assert_eq!(status(&SearchModel::default(), Kinds::Files)[0], "");
    }

    #[test]
    fn time_has_tenths_under_a_minute() {
        urfin_i18n::set_language("ru");
        assert_eq!(time_text(400), "0,4 с");
        assert_eq!(time_text(12_300), "12,3 с");
        assert_eq!(time_text(68_000), "1 мин 08 с");
    }
}
