//! Раздел настроек «Пакеты» без окна (F02-12, `screen-settings-pkg.png`): порядок
//! менеджеров, чипы важных программ и поиск по установленным, время расписания, строка о
//! задаче Планировщика. Вид — `gpui_shell::settings_pkg`, модель — `urfin_core::pkg`.

use compact_str::CompactString;
use urfin_core::pkg::{
    Filter, MatchKind, Mgr, PkgId, PkgInfo, PkgModel, Schedule, Task, View, filter_of, parse_at,
};
use urfin_core::{AppState, Command, Row, Value};

/// Сколько подсказок показывает поиск по установленным.
const MATCHES: usize = 6;

/// Чип порядка менеджеров: не найденный на машине — пунктиром.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MgrChip {
    pub mgr: Mgr,
    pub version: Option<CompactString>,
    pub found: bool,
}

/// Чипы в порядке `[pkg].order`; не названные в нём менеджеры — после.
pub fn order_chips(order: &[Mgr], info: &PkgInfo) -> Vec<MgrChip> {
    let mut seen: Vec<Mgr> = Vec::with_capacity(Mgr::ALL.len());
    for m in order.iter().chain(&Mgr::ALL) {
        if !seen.contains(m) {
            seen.push(*m);
        }
    }
    seen.into_iter()
        .map(|mgr| {
            let line = info.managers.iter().find(|l| l.mgr == mgr);
            MgrChip {
                mgr,
                version: line.and_then(|l| l.version.clone()),
                found: line.is_some(),
            }
        })
        .collect()
}

/// Порядок после переноса чипа `what` на место чипа `onto`; на себя — без изменений.
pub fn moved(order: &[Mgr], what: Mgr, onto: Mgr) -> Option<Vec<Mgr>> {
    if what == onto {
        return None;
    }
    let at = order.iter().position(|m| *m == onto)?;
    let mut out: Vec<Mgr> = order.iter().copied().filter(|m| *m != what).collect();
    out.insert(at, what);
    Some(out)
}

/// Записать порядок: `[pkg].order` списком, перечитывание применит его на лету.
pub fn order_cmd(order: &[Mgr]) -> Command {
    let list: Vec<Value> = order
        .iter()
        .map(|m| Value::Str(m.as_str().into()))
        .collect();
    Command::new("ui.setting")
        .arg("key", "pkg.order")
        .arg("list", list)
}

/// Добавить в важные или убрать.
pub fn toggle_cmd(id: &PkgId) -> Command {
    Command::new("pkg.important.toggle").arg("id", id.to_string())
}

/// Чипы важных: имя из «Установленных»; пакета там нет (удалён, кэша ещё нет) — id.
pub fn important_chips(view: &View, ids: &[PkgId]) -> Vec<(PkgId, String)> {
    let rows = view.rows(Filter::Installed);
    ids.iter()
        .map(|id| {
            let name = rows
                .iter()
                .find(|r| r.pkg.id == *id)
                .map_or_else(|| id.id.to_string(), |r| r.name.to_string());
            (id.clone(), name)
        })
        .collect()
}

/// Поиск важных по установленным: ещё не важные и не «вне менеджера» (обновлять их нечем),
/// лучшие совпадения сверху; пустой запрос — первые по имени.
pub fn matches(view: &View, q: &str) -> Vec<(PkgId, String)> {
    let q = q.trim();
    let low = q.to_lowercase();
    let mut found: Vec<_> = view
        .rows(Filter::Installed)
        .iter()
        .filter(|r| !r.important && !r.outside)
        .filter(|r| {
            low.is_empty()
                || r.name.to_lowercase().contains(&low)
                || r.pkg.id.id.to_lowercase().contains(&low)
        })
        .map(|r| (MatchKind::of(q, &r.pkg.id.id, &r.name), r))
        .collect();
    found.sort_by_cached_key(|(kind, r)| (*kind, r.name.to_lowercase()));
    found
        .into_iter()
        .take(MATCHES)
        .map(|(_, r)| (r.pkg.id.clone(), r.name.to_string()))
        .collect()
}

/// Время из поля: при включённом расписании задача пересоздаётся с новым временем.
/// Не `ЧЧ:ММ` или то же время — без команд (поле вернётся к настройке).
pub fn at_cmds(text: &str, c: &Schedule) -> Vec<Command> {
    let Some((h, m)) = parse_at(text) else {
        return Vec::new();
    };
    if !c.enabled || parse_at(&c.at) == Some((h, m)) {
        return Vec::new();
    }
    vec![
        Command::new("pkg.schedule")
            .arg("on", true)
            .arg("at", format!("{h:02}:{m:02}")),
    ]
}

/// Что не так с задачей Планировщика: строка под расписанием и нужна ли кнопка «Обновить»
/// (создать задачу заново).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskLine {
    /// Задача запускает другой `urfin.exe` (portable-копию перенесли): путь задачи.
    OtherPath(CompactString),
    /// Расписание включено, задачи нет (удалили в Планировщике).
    Absent,
    /// `schtasks` не ответил: текст ошибки.
    Failed(CompactString),
}

/// Строка о задаче; всё в порядке, идёт сверка или расписание выключено — нет строки.
pub fn task_line(task: &Task, enabled: bool) -> Option<TaskLine> {
    match task {
        Task::OtherPath(p) => Some(TaskLine::OtherPath(p.clone())),
        Task::Failed(e) => Some(TaskLine::Failed(e.clone())),
        Task::Absent if enabled => Some(TaskLine::Absent),
        Task::Unknown | Task::Busy | Task::Ready | Task::Absent => None,
    }
}

/// Строка под курсором вкладки `pkg:` для контекстного меню: важная ли; `None` — не
/// строка пакета или «вне менеджера» (важность ей не нужна).
pub fn important_at(s: &AppState) -> Option<bool> {
    let t = s.active_tab();
    let f = filter_of(&t.loc)?;
    let Some(Row::Entry { entry, .. }) = t.current() else {
        return None;
    };
    let model = s.ext.get::<PkgModel>()?;
    let view = model.feed.view();
    if f == Filter::Search {
        let id = view.search.row(&entry.name)?.target();
        return Some(model.config.important.contains(id));
    }
    let row = view.row(f, &entry.name)?;
    (!row.outside).then_some(row.important)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use urfin_core::pkg::{MgrLine, Package, PkgRow, Rows, Run};

    use super::*;

    fn line(mgr: Mgr, version: &str) -> MgrLine {
        MgrLine {
            mgr,
            run: Run::Idle,
            version: Some(version.into()),
        }
    }

    fn row(id: &str, name: &str, important: bool, outside: bool) -> PkgRow {
        PkgRow {
            name: name.into(),
            pkg: Package {
                id: id.parse().unwrap(),
                name: name.into(),
                version: "1.0".into(),
                source: None,
                publisher: None,
            },
            available: None,
            pinned: false,
            outside,
            important,
        }
    }

    fn view(rows: Vec<PkgRow>) -> View {
        View {
            lists: [Arc::new(Rows::new(rows)), Arc::default()],
            ..View::default()
        }
    }

    #[test]
    fn chips_follow_order_and_mark_missing() {
        let info = PkgInfo {
            managers: vec![line(Mgr::Winget, "1.9"), line(Mgr::Scoop, "0.5.3")],
            ..PkgInfo::default()
        };
        let chips = order_chips(&[Mgr::Scoop], &info);
        assert_eq!(
            chips.iter().map(|c| c.mgr).collect::<Vec<_>>(),
            [Mgr::Scoop, Mgr::Winget, Mgr::Choco]
        );
        assert_eq!(chips[0].version.as_deref(), Some("0.5.3"));
        assert!(chips[1].found);
        assert!(!chips[2].found);
    }

    #[test]
    fn drag_moves_chip_to_target_place() {
        let o = Mgr::ALL;
        assert_eq!(
            moved(&o, Mgr::Winget, Mgr::Choco).unwrap(),
            [Mgr::Scoop, Mgr::Choco, Mgr::Winget]
        );
        assert_eq!(
            moved(&o, Mgr::Choco, Mgr::Winget).unwrap(),
            [Mgr::Choco, Mgr::Winget, Mgr::Scoop]
        );
        assert_eq!(
            moved(&o, Mgr::Winget, Mgr::Scoop).unwrap(),
            [Mgr::Scoop, Mgr::Winget, Mgr::Choco]
        );
        assert_eq!(moved(&o, Mgr::Scoop, Mgr::Scoop), None);
    }

    #[test]
    fn order_command_writes_list() {
        let cmd = order_cmd(&[Mgr::Scoop, Mgr::Winget]);
        assert_eq!(cmd.args.str("key"), Some("pkg.order"));
        let list = cmd.args.list("list").unwrap();
        assert_eq!(
            list,
            &[Value::Str("scoop".into()), Value::Str("winget".into())]
        );
    }

    #[test]
    fn important_chip_shows_name_or_id() {
        let v = view(vec![row("winget:Mozilla.Firefox", "Firefox", true, false)]);
        let ids: Vec<PkgId> = ["winget:Mozilla.Firefox", "scoop:main/7zip"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let names: Vec<String> = important_chips(&v, &ids).into_iter().map(|c| c.1).collect();
        assert_eq!(names, ["Firefox", "main/7zip"]);
    }

    #[test]
    fn matches_search_installed() {
        let v = view(vec![
            row("winget:Mozilla.Firefox", "Firefox", true, false),
            row("winget:Git.Git", "Git", false, false),
            row("winget:GitHub.cli", "GitHub CLI", false, false),
            row("winget:ARP\\Machine\\Tortoise", "TortoiseGit", false, true),
            row("scoop:main/jq", "jq", false, false),
        ]);
        let names = |q: &str| -> Vec<String> { matches(&v, q).into_iter().map(|m| m.1).collect() };
        assert_eq!(
            names("git"),
            ["Git", "GitHub CLI"],
            "точное сверху, без «вне менеджера»"
        );
        assert_eq!(names("FIRE"), [] as [&str; 0], "уже важная");
        assert_eq!(names("main/j"), ["jq"], "и по id");
        assert_eq!(names(""), ["Git", "GitHub CLI", "jq"]);
    }

    #[test]
    fn at_field_commands() {
        let on = Schedule {
            enabled: true,
            at: "09:00".into(),
        };
        let cmds = at_cmds(" 7:05 ", &on);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id.as_str(), "pkg.schedule");
        assert_eq!(cmds[0].args.bool("on"), Some(true));
        assert_eq!(cmds[0].args.str("at"), Some("07:05"));
        assert_eq!(at_cmds("9:00", &on).len(), 0, "то же время");
        assert_eq!(at_cmds("25:00", &on).len(), 0);
        assert_eq!(at_cmds("abc", &on).len(), 0);
        let off = Schedule {
            enabled: false,
            ..on
        };
        assert_eq!(at_cmds("10:00", &off).len(), 0);
    }

    #[test]
    fn task_lines() {
        assert_eq!(task_line(&Task::Ready, true), None);
        assert_eq!(task_line(&Task::Busy, true), None);
        assert_eq!(task_line(&Task::Absent, false), None);
        assert_eq!(task_line(&Task::Absent, true), Some(TaskLine::Absent));
        assert_eq!(
            task_line(&Task::OtherPath(r"E:\old\urfin.exe".into()), false),
            Some(TaskLine::OtherPath(r"E:\old\urfin.exe".into()))
        );
        assert_eq!(
            task_line(&Task::Failed("нет".into()), false),
            Some(TaskLine::Failed("нет".into()))
        );
    }
}
