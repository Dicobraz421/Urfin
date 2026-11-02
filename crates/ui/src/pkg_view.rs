//! Вкладка пакетов `pkg:` (F02-08, `screen-pkginstalled`, `screen-pkgupdates`): тексты
//! заголовка, чипов и статуса, тон ячеек. Без gpui — рисует `gpui_shell::pkg`.

use std::sync::Arc;

use urfin_core::{AppState, Row, Tab};
use urfin_i18n::FluentArgs;
use urfin_pkg::vfs::filter_of;
use urfin_pkg::{Filter, PkgBook, PkgInfo, PkgRow, Run, View};

use crate::fmt_cell::FmtCfg;

/// Фильтры-чипы по порядку: имя `pkg.filter name=…` и подпись (`search` — F02-09).
pub const FILTERS: [(&str, &str); 3] = [
    ("installed", "pkg-filter-installed"),
    ("updates", "pkg-filter-updates"),
    ("search", "pkg-filter-search"),
];

/// Таб — вкладка пакетов.
pub fn is_pkg(t: &Tab) -> bool {
    filter_of(&t.loc).is_some()
}

/// Фильтр таба `pkg:`.
pub fn filter(t: &Tab) -> Option<Filter> {
    filter_of(&t.loc)
}

/// Модель вкладки, если таб — `pkg:`.
pub fn book<'a>(s: &'a AppState, t: &Tab) -> Option<&'a Arc<PkgBook>> {
    is_pkg(t).then(|| urfin_pkg::book(s)).flatten()
}

/// Сводка вкладки (`None` — не `pkg:` или handler ещё не встал).
pub fn info(s: &AppState, t: &Tab) -> Option<PkgInfo> {
    book(s, t).map(|b| b.info())
}

/// Номер изменения модели (ключ кэша кадра заголовка).
pub fn rev(s: &AppState) -> u64 {
    urfin_pkg::book(s).map_or(0, |b| b.rev())
}

/// Подпись таба: `pkg:` (как на макете).
pub fn tab_title(t: &Tab) -> Option<String> {
    is_pkg(t).then(|| urfin_i18n::current().get("pkg-tab"))
}

/// Справа в заголовке: «16 программ» или, в «Обновлениях», «7 из 16».
pub fn head_note(f: Filter, i: &PkgInfo) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    match f {
        Filter::Installed => {
            a.set("count", i.installed);
            i18n.fmt("pkg-count", &a)
        }
        Filter::Updates => {
            a.set("n", i.updates);
            a.set("total", i.installed);
            i18n.fmt("pkg-of", &a)
        }
    }
}

/// Тон ячейки колонки схемы: «Доступна» — `accent`, «вне менеджера» — `secondary`,
/// прочее — основной текст.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    Text,
    Accent,
    Secondary,
}

/// Тон колонки `col` строки `row`.
pub fn tone(row: &PkgRow, col: &str) -> Tone {
    match col {
        "available" => Tone::Accent,
        "mgr" if row.outside => Tone::Secondary,
        _ => Tone::Text,
    }
}

/// Строка таба по имени (`Entry.name`).
pub fn row<'a>(view: &'a View, t: &Tab, name: &str) -> Option<&'a PkgRow> {
    view.row(filter(t)?, name)
}

/// Время проверки: «Проверено сегодня в 09:00», «Проверено 31.10.26 в 09:00»; идёт
/// проверка — «Проверяется…»; не проверяли — «Ещё не проверено».
pub fn checked(i: &PkgInfo, fmt: &FmtCfg, now: i64) -> String {
    let i18n = urfin_i18n::current();
    if i.busy() {
        return i18n.get("pkg-checking");
    }
    let Some(at) = i.checked_at else {
        return i18n.get(if i.loaded && i.managers.is_empty() {
            "pkg-no-managers"
        } else {
            "pkg-never"
        });
    };
    let nanos = i64::try_from(at.as_nanosecond()).unwrap_or(i64::MIN);
    let (Some(t), today) = (fmt.local(nanos), fmt.local(now)) else {
        return String::new();
    };
    let mut a = FluentArgs::new();
    a.set("time", format!("{:02}:{:02}", t.hour, t.minute));
    if today.is_some_and(|d| (d.year, d.month, d.day) == (t.year, t.month, t.day)) {
        return i18n.fmt("pkg-checked-today", &a);
    }
    a.set(
        "date",
        format!("{:02}.{:02}.{:02}", t.day, t.month, t.year.rem_euclid(100)),
    );
    i18n.fmt("pkg-checked-on", &a)
}

/// Ошибки менеджеров последней проверки: «scoop: timed out».
pub fn failures(i: &PkgInfo) -> Vec<String> {
    let i18n = urfin_i18n::current();
    i.managers
        .iter()
        .filter_map(|m| match &m.run {
            Run::Failed(e) => {
                let mut a = FluentArgs::new();
                a.set("mgr", m.mgr.as_str());
                a.set("error", e.to_string());
                Some(i18n.fmt("pkg-failed", &a))
            }
            _ => None,
        })
        .collect()
}

/// Три зоны статус-строки вкладки: пакет под курсором («Mozilla.Firefox · winget»),
/// «Выделено N из M» (без байтов: у строк пакетов размера нет) и время проверки с
/// ошибками менеджеров.
pub fn status(s: &AppState, t: &Tab, fmt: &FmtCfg) -> Option<[String; 3]> {
    let b = book(s, t)?;
    let mut a = FluentArgs::new();
    a.set("count", u64::from(t.selection.len()));
    a.set(
        "total",
        u64::from(t.listing.stats.files + t.listing.stats.dirs),
    );
    let sel = urfin_i18n::current().fmt("ui-status-selected", &a);
    let view = b.view();
    let file = match t.current() {
        Some(Row::Entry { entry, .. }) => row(&view, t, &entry.name).map_or_else(
            || entry.name.to_string(),
            |r| {
                let mgr = if r.outside {
                    urfin_i18n::current().get("pkg-outside")
                } else {
                    r.pkg.id.mgr.as_str().to_owned()
                };
                let id = if r.outside {
                    r.name.as_str()
                } else {
                    r.pkg.id.id.as_str()
                };
                format!("{id} · {mgr}")
            },
        ),
        _ => String::new(),
    };
    let mut right = vec![checked(&view.info, fmt, now_nanos())];
    right.extend(failures(&view.info));
    right.retain(|x| !x.is_empty());
    Some([file, sel, right.join(" · ")])
}

/// Каталог в командной строке у вкладки пакетов (своего каталога у неё нет): текущий
/// каталог shell, иначе путь другой панели. Не вкладка пакетов — `None`.
pub fn cmdline_dir(s: &AppState) -> Option<String> {
    if !is_pkg(s.active_tab()) {
        return None;
    }
    let cwd = s
        .ext
        .get::<urfin_term::TermState>()
        .and_then(|t| t.sessions.active())
        .and_then(|a| a.cwd.clone());
    let other = &s.panel(s.active.other()).tab().loc;
    let dir = cwd.or_else(|| other.is_local().then(|| other.path.clone()));
    Some(dir.map(|p| p.display().to_string()).unwrap_or_default())
}

/// Клавиши вкладки внизу окна (`screen-pkginstalled`, `screen-pkgupdates`).
pub fn keys(t: &Tab) -> Option<Vec<(String, String)>> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    Some(match filter(t)? {
        Filter::Installed => vec![
            k("F5", "pkg-key-upgrade"),
            k("F8", "pkg-key-uninstall"),
            k("Ctrl+R", "pkg-key-check"),
            k("Ctrl+O", "pkg-key-output"),
        ],
        Filter::Updates => vec![
            k("Insert", "pkg-key-mark"),
            k("F5", "pkg-key-upgrade-marked"),
            k("Ctrl+R", "pkg-key-check"),
            k("Ctrl+O", "pkg-key-output"),
            k("F11", "pkg-key-jobs"),
        ],
    })
}

fn now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use urfin_pkg::{Mgr, MgrLine, PkgInfo, Run};

    use super::*;

    fn fmt() -> FmtCfg {
        FmtCfg::new(&urfin_i18n::current(), false, crate::sys::Zone::utc())
    }

    #[test]
    fn texts() {
        let _ = urfin_i18n::set_language("ru");
        let mut i = PkgInfo {
            installed: 16,
            updates: 7,
            loaded: true,
            ..PkgInfo::default()
        };
        assert_eq!(head_note(Filter::Installed, &i), "16 программ");
        assert_eq!(head_note(Filter::Updates, &i), "7 из 16");
        assert_eq!(checked(&i, &fmt(), 0), "Менеджеры пакетов не найдены");
        i.managers = vec![
            MgrLine {
                mgr: Mgr::Winget,
                run: Run::Idle,
            },
            MgrLine {
                mgr: Mgr::Scoop,
                run: Run::Failed("timed out".into()),
            },
        ];
        assert_eq!(checked(&i, &fmt(), 0), "Ещё не проверено");
        assert_eq!(failures(&i), ["scoop: timed out"]);
        i.checked_at = Some("2026-11-02T09:00:00Z".parse().unwrap());
        let nanos = i64::try_from(i.checked_at.unwrap().as_nanosecond()).unwrap();
        assert!(checked(&i, &fmt(), nanos).starts_with("Проверено сегодня в "));
        let later = nanos + 3 * 86_400 * 1_000_000_000;
        assert!(checked(&i, &fmt(), later).starts_with("Проверено 0"));
        i.checking = true;
        assert_eq!(checked(&i, &fmt(), nanos), "Проверяется…");
    }
}
