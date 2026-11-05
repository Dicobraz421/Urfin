//! Вкладка пакетов `pkg:` (F02-08, `screen-pkginstalled`, `screen-pkgupdates`): тексты
//! заголовка, чипов и статуса, тон ячеек. Без gpui — рисует `gpui_shell::pkg`.

use std::sync::Arc;

use urfin_core::pkg::{
    Answer, Filter, Mgr, Notice, PkgFeed, PkgInfo, PkgRow, Run, SearchIn, SearchInfo, SearchKind,
    SearchRow, View, filter_of,
};
use urfin_core::{AppState, Row, Tab};
use urfin_i18n::FluentArgs;

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
pub fn book<'a>(s: &'a AppState, t: &Tab) -> Option<&'a Arc<PkgFeed>> {
    is_pkg(t).then(|| urfin_core::pkg::feed(s)).flatten()
}

/// Сводка вкладки (`None` — не `pkg:` или handler ещё не встал).
pub fn info(s: &AppState, t: &Tab) -> Option<PkgInfo> {
    book(s, t).map(|b| b.info())
}

/// Номер изменения модели (ключ кэша кадра заголовка).
pub fn rev(s: &AppState) -> u64 {
    urfin_core::pkg::feed(s).map_or(0, |b| b.rev())
}

/// Подпись таба: `pkg:` (как на макете).
pub fn tab_title(t: &Tab) -> Option<String> {
    is_pkg(t).then(|| urfin_i18n::current().get("pkg-tab"))
}

/// Справа в заголовке: «16 программ», в «Обновлениях» — «7 из 16», в поиске — «4
/// программы · 8 вариантов».
pub fn head_note_of(f: Filter, i: &PkgInfo, search: &SearchInfo) -> String {
    if f != Filter::Search {
        return head_note(f, i);
    }
    if search.q.is_empty() {
        return String::new();
    }
    let mut a = FluentArgs::new();
    a.set("groups", search.groups);
    a.set("variants", search.variants);
    urfin_i18n::current().fmt("pkg-search-note", &a)
}

/// Справа в заголовке «Установленных» и «Обновлений».
pub fn head_note(f: Filter, i: &PkgInfo) -> String {
    let i18n = urfin_i18n::current();
    let mut a = FluentArgs::new();
    match f {
        Filter::Search => String::new(),
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
    /// Метка в рамке `accent` («рекомендуется»).
    Badge,
}

/// Тон колонки `col` строки `row`.
pub fn tone(row: &PkgRow, col: &str) -> Tone {
    match col {
        "available" => Tone::Accent,
        "mgr" if row.outside => Tone::Secondary,
        _ => Tone::Text,
    }
}

/// Тон колонки строки поиска: новейшая версия и ход действия — `accent`,
/// «рекомендуется» — метка.
pub fn search_tone(row: &SearchRow, col: &str) -> Tone {
    match (&row.kind, col) {
        (SearchKind::Group { .. } | SearchKind::Variant { newest: true, .. }, "version") => {
            Tone::Accent
        }
        (
            SearchKind::Variant {
                recommended: true, ..
            },
            "publisher",
        ) => Tone::Badge,
        _ => Tone::Text,
    }
}

/// Мини-полоса хода в строке группы поиска (`screen-pkginstall`, после
/// «устанавливается…»): доля 0…1; в очереди, у окна UAC и без доли полосы нет.
pub fn mini_bar(view: &View, row: &SearchRow) -> Option<f32> {
    if !row.is_group() {
        return None;
    }
    let b = view.group_busy(row.group)?;
    (!b.queued && !b.uac && !b.maybe_uac)
        .then_some(b.pct)
        .flatten()
        .map(|p| f32::from(p.min(100)) / 100.0)
}

/// Строка таба по имени (`Entry.name`).
pub fn row<'a>(view: &'a View, t: &Tab, name: &str) -> Option<&'a PkgRow> {
    view.row(filter(t)?, name)
}

/// Строка поиска таба по имени.
pub fn search_row<'a>(view: &'a View, t: &Tab, name: &str) -> Option<&'a SearchRow> {
    (filter(t) == Some(Filter::Search))
        .then(|| view.search.row(name))
        .flatten()
}

/// Ответ менеджера под строкой поиска: текст и вид.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AnswerLook {
    /// «winget · 4».
    Done,
    /// «scoop ищет…» — со значком хода.
    Busy,
    /// «choco не ответил», «Ничего не нашлось» — цветом предупреждения.
    Warn,
}

/// Строка ответов менеджеров под полем поиска (`screen-pkgsearch`).
pub fn answers(info: &SearchInfo) -> Vec<(String, AnswerLook)> {
    let i18n = urfin_i18n::current();
    let mut out: Vec<(String, AnswerLook)> = info
        .answers
        .iter()
        .map(|(m, a)| match a {
            Answer::Found(n) => (format!("{m} · {n}"), AnswerLook::Done),
            Answer::Busy => (
                format!("{m} {}", i18n.get("pkg-search-busy")),
                AnswerLook::Busy,
            ),
            Answer::Failed(_) => (
                format!("{m} {}", i18n.get("pkg-search-failed")),
                AnswerLook::Warn,
            ),
        })
        .collect();
    if !info.q.is_empty() && info.answers.is_empty() {
        out.push((i18n.get("pkg-search-none"), AnswerLook::Warn));
    } else if info.empty() {
        out.push((i18n.get("pkg-search-empty"), AnswerLook::Warn));
    }
    out
}

/// Подпись чипа «Найти»: с запросом — «Найти «7zip»».
pub fn search_chip(info: &SearchInfo) -> String {
    let i18n = urfin_i18n::current();
    if info.q.is_empty() {
        return i18n.get("pkg-filter-search");
    }
    let mut a = FluentArgs::new();
    a.set("q", info.q.to_string());
    i18n.fmt("pkg-search-chip", &a)
}

/// Пункты мешочка менеджеров (`screen-pkgmgrpick`): значение `in=`, подпись, пояснение
/// справа и доступен ли (ненайденный — серым).
pub fn scope_items(info: &PkgInfo) -> Vec<(SearchIn, String, String, bool)> {
    let i18n = urfin_i18n::current();
    let mut out = vec![(
        SearchIn::All,
        i18n.get("pkg-search-all"),
        i18n.get("pkg-search-parallel"),
        true,
    )];
    for (scope, m) in [
        (SearchIn::Winget, Mgr::Winget),
        (SearchIn::Scoop, Mgr::Scoop),
        (SearchIn::Choco, Mgr::Choco),
    ] {
        let found = info.managers.iter().any(|l| l.mgr == m) || info.managers.is_empty();
        out.push((
            scope,
            m.as_str().to_owned(),
            if found {
                String::new()
            } else {
                i18n.get("pkg-search-absent")
            },
            found,
        ));
    }
    out
}

/// Значение `pkg.search in=`.
pub fn scope_arg(s: SearchIn) -> &'static str {
    match s {
        SearchIn::All => "all",
        SearchIn::Winget => "winget",
        SearchIn::Scoop => "scoop",
        SearchIn::Choco => "choco",
    }
}

/// Подпись области на кнопке: «Все» или имя менеджера.
pub fn scope_label(s: SearchIn) -> String {
    match s.mgr() {
        None => urfin_i18n::current().get("pkg-search-all"),
        Some(m) => m.as_str().to_owned(),
    }
}

/// «Порядок: winget › scoop › choco» внизу мешочка.
pub fn order_note(order: &[Mgr]) -> String {
    let list: Vec<&str> = order.iter().map(|m| m.as_str()).collect();
    let mut a = FluentArgs::new();
    a.set("order", list.join(" › "));
    urfin_i18n::current().fmt("pkg-search-order", &a)
}

/// Клавиши строки поиска внизу окна: поле или открытый мешочек менеджеров.
pub fn search_keys(pouch: bool) -> Vec<(String, String)> {
    let i18n = urfin_i18n::current();
    let k = |key: &str, label: &str| (key.to_owned(), i18n.get(label));
    if pouch {
        vec![
            k("↑↓", "pkg-key-choose"),
            k("Enter", "pkg-key-apply"),
            k("Esc", "pkg-key-close-list"),
        ]
    } else {
        vec![
            k("↓", "pkg-key-results"),
            k("Alt+↓", "pkg-key-manager"),
            k("Esc", "pkg-key-close"),
        ]
    }
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
    let nanos = at;
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

/// Баннер фоновой проверки (`screen-pkgbanner`): полужирное «Доступно 7 обновлений ·
/// 2 важных» и вторичное «проверено по расписанию сегодня в 09:00» (у «Проверить
/// сейчас» — без «по расписанию»).
pub fn notice(n: &Notice, fmt: &FmtCfg, now: i64) -> (String, String) {
    let i18n = urfin_i18n::current();
    let count = |c: usize| {
        let mut a = FluentArgs::new();
        a.set("count", c);
        a
    };
    let mut head = i18n.fmt("pkg-banner-updates", &count(n.updates));
    if n.important > 0 {
        head.push_str(" · ");
        head.push_str(&i18n.fmt("pkg-banner-important", &count(n.important)));
    }
    let (Some(t), today) = (fmt.local(n.checked_at), fmt.local(now)) else {
        return (head, String::new());
    };
    let mut a = FluentArgs::new();
    a.set("time", format!("{:02}:{:02}", t.hour, t.minute));
    let same_day = today.is_some_and(|d| (d.year, d.month, d.day) == (t.year, t.month, t.day));
    let key = match (n.scheduled, same_day) {
        (true, true) => "pkg-banner-scheduled-today",
        (true, false) => "pkg-banner-scheduled-on",
        (false, true) => "pkg-banner-manual-today",
        (false, false) => "pkg-banner-manual-on",
    };
    if !same_day {
        a.set(
            "date",
            format!("{:02}.{:02}.{:02}", t.day, t.month, t.year.rem_euclid(100)),
        );
    }
    (head, i18n.fmt(key, &a))
}

/// Сейчас, Unix, наносекунды.
pub fn now_ns() -> i64 {
    now_nanos()
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
        Some(Row::Entry { entry, .. }) if filter(t) == Some(Filter::Search) => {
            search_row(&view, t, &entry.name).map_or_else(String::new, |r| {
                let id = r.target();
                format!("{} · {}", id.id, id.mgr)
            })
        }
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
        // `screen-pkginstall`.
        Filter::Search => vec![
            k("F5", "pkg-key-install-marked"),
            k("F8", "pkg-key-uninstall"),
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
    use urfin_core::pkg::{Mgr, MgrLine, PkgInfo, Run};

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
                version: None,
            },
            MgrLine {
                mgr: Mgr::Scoop,
                run: Run::Failed("timed out".into()),
                version: None,
            },
        ];
        assert_eq!(checked(&i, &fmt(), 0), "Ещё не проверено");
        assert_eq!(failures(&i), ["scoop: timed out"]);
        // 2026-11-02T09:00:00Z.
        let nanos = 1_793_610_000 * 1_000_000_000;
        i.checked_at = Some(nanos);
        assert!(checked(&i, &fmt(), nanos).starts_with("Проверено сегодня в "));
        let later = nanos + 3 * 86_400 * 1_000_000_000;
        assert!(checked(&i, &fmt(), later).starts_with("Проверено 0"));
        i.checking = true;
        assert_eq!(checked(&i, &fmt(), nanos), "Проверяется…");
    }

    /// Баннер (`screen-pkgbanner`): числа с падежами, «по расписанию», дата не сегодня.
    #[test]
    fn notice_texts() {
        let _ = urfin_i18n::set_language("ru");
        // 2026-11-02T09:00:00Z.
        let at = 1_793_610_000 * 1_000_000_000;
        let mut n = Notice {
            updates: 7,
            important: 2,
            checked_at: at,
            scheduled: true,
        };
        let (head, when) = notice(&n, &fmt(), at);
        assert_eq!(head, "Доступно 7 обновлений · 2 важных");
        assert_eq!(when, "проверено по расписанию сегодня в 09:00");
        n.updates = 1;
        n.important = 1;
        assert_eq!(notice(&n, &fmt(), at).0, "Доступно 1 обновление · 1 важное");
        n.updates = 3;
        n.important = 0;
        n.scheduled = false;
        let (head, when) = notice(&n, &fmt(), at + 86_400 * 1_000_000_000);
        assert_eq!(head, "Доступно 3 обновления");
        assert_eq!(when, "проверено 02.11.26 в 09:00");
        let _ = urfin_i18n::set_language("en");
        n.important = 2;
        n.scheduled = true;
        let (head, when) = notice(&n, &fmt(), at);
        assert_eq!(head, "3 updates available · 2 important");
        assert_eq!(when, "checked on schedule today at 09:00");
    }
}
