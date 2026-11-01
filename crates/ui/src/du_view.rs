//! Вкладка анализа места `du:` (F01-05, F01-06): тексты заголовка и статуса, доля строки
//! в папке, сумма «освободится» по отмеченному. Без gpui — рисует `gpui_shell::du`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rustc_hash::FxHashSet;
use urfin_core::{AppState, Attrs, Entry, Tab};
use urfin_i18n::FluentArgs;
use urfin_vfs::du::{DuInfo, dvfs};

/// Фильтры-чипы (F01 §3.5) по порядку: команда `du.filter kind=…` и подпись.
pub const FILTERS: [(&str, &str); 6] = [
    ("dirs", "du-filter-dirs"),
    ("large", "du-filter-large"),
    ("types", "du-filter-types"),
    ("old", "du-filter-old"),
    ("dups", "du-filter-dups"),
    ("temp", "du-filter-temp"),
];

/// Таб — вкладка анализа места.
pub fn is_du(t: &Tab) -> bool {
    t.loc.backend == dvfs::SCHEME
}

/// Сводка анализа таба (`None` — не `du:` или анализа нет).
pub fn info(s: &AppState, t: &Tab) -> Option<DuInfo> {
    if !is_du(t) {
        return None;
    }
    dvfs::registry(s)?.info(&t.loc)
}

/// Номер изменения реестра анализов (ключ кэша кадра заголовка).
pub fn rev(s: &AppState) -> u64 {
    dvfs::registry(s).map_or(0, |r| r.rev())
}

/// Подпись таба: `du: C:\Dev` (корень анализа, как на макете).
pub fn tab_title(s: &AppState, t: &Tab) -> Option<String> {
    info(s, t).map(|i| format!("du: {}", i.root.display()))
}

/// Справа в заголовке панели: «просмотрено N файлов · X» (идёт подсчёт), «N файлов · X»
/// или, у открытого из кэша, «посчитано 2 ч назад».
pub fn head_note(i: &DuInfo) -> String {
    let i18n = urfin_i18n::current();
    if i.from_cache && !i.scanning {
        let mut a = FluentArgs::new();
        a.set("time", ago(i.scanned_at, now_nanos()));
        return i18n.fmt("du-ago", &a);
    }
    let mut a = FluentArgs::new();
    a.set("files", urfin_i18n::fmt::count(i.files));
    a.set("count", i.files);
    a.set("size", urfin_i18n::fmt::bytes_with(&i18n, i.bytes));
    i18n.fmt(
        if i.scanning {
            "du-total-scanning"
        } else {
            "du-total"
        },
        &a,
    )
}

/// Сколько прошло с `then` (нс от Unix epoch): «45 с», «12 мин», «2 ч», «3 дн.».
pub fn ago(then: i64, now: i64) -> String {
    let i18n = urfin_i18n::current();
    let s = u64::try_from(now.saturating_sub(then) / 1_000_000_000).unwrap_or(0);
    let mut a = FluentArgs::new();
    let key = match s {
        0..60 => {
            a.set("sec", s);
            "du-time-sec"
        }
        60..3600 => {
            a.set("n", s / 60);
            "du-time-min"
        }
        3600..86_400 => {
            a.set("n", s / 3600);
            "du-time-h"
        }
        _ => {
            a.set("n", s / 86_400);
            "du-time-d"
        }
    };
    i18n.fmt(key, &a)
}

/// «посчитано за 1 мин 08 с».
pub fn took(ms: u32) -> String {
    let i18n = urfin_i18n::current();
    let s = ms.div_ceil(1000);
    let mut a = FluentArgs::new();
    let time = if s < 60 {
        a.set("sec", s);
        i18n.fmt("du-time-sec", &a)
    } else {
        a.set("min", s / 60);
        a.set("sec", format!("{:02}", s % 60));
        i18n.fmt("du-time-min-sec", &a)
    };
    let mut a = FluentArgs::new();
    a.set("time", time);
    i18n.fmt("du-took", &a)
}

/// Доля строки в папке: размер к сумме размеров каталога (0…1) и подпись «39 %», «<1 %».
pub fn share(size: u64, total: u64) -> (f32, String) {
    if total == 0 {
        return (0.0, String::new());
    }
    #[allow(clippy::cast_precision_loss, reason = "доля для полосы и процента")]
    let f = (size as f64 / total as f64).clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0..=100"
    )]
    let pct = (f * 100.0).round() as u32;
    let text = if pct == 0 && size > 0 {
        "<1 %".to_owned()
    } else {
        format!("{pct} %")
    };
    #[allow(clippy::cast_possible_truncation, reason = "доля 0..=1")]
    (f as f32, text)
}

/// Сумма размеров записей каталога (знаменатель доли). Каталоги без размера — 0.
pub fn dir_total(entries: &[Entry]) -> u64 {
    entries.iter().map(|e| e.size).sum()
}

/// Освободится при удалении отмеченного (F01 §3.7): размеры с учётом вложенности —
/// отмеченный каталог поглощает отмеченных потомков. Путь — относительный, через `\`
/// (в плоских фильтрах имя строки — `dir\name`).
pub fn frees<'a>(marked: impl IntoIterator<Item = (&'a str, u64)>) -> u64 {
    let items: Vec<(&str, u64)> = marked.into_iter().collect();
    let set: FxHashSet<&str> = items.iter().map(|(p, _)| *p).collect();
    items
        .iter()
        .filter(|(p, _)| {
            let mut cut = *p;
            while let Some(i) = cut.rfind('\\') {
                cut = &cut[..i];
                if set.contains(cut) {
                    return false;
                }
            }
            true
        })
        .map(|(_, n)| n)
        .sum()
}

/// Отмечено в табе: число и «освободится» (F01-06).
pub fn marked(t: &Tab) -> (u32, u64) {
    let l = &t.listing;
    let bytes = frees(
        t.selection
            .iter()
            .filter_map(|i| l.entries.get(i as usize))
            .map(|e| (e.name.as_str(), e.size)),
    );
    (t.selection.len(), bytes)
}

/// Размер недосчитан («≥ X»).
pub fn partial(e: &Entry) -> bool {
    e.attrs.has(Attrs::SIZE_PARTIAL)
}

/// Сегменты пути вкладки: корень анализа целиком, затем папки внутри — (подпись,
/// адрес `du:`).
pub fn crumbs(i: &DuInfo) -> Vec<(String, urfin_core::Location)> {
    let mut out = vec![(
        i.root.display().to_string(),
        dvfs::location(i.id, Path::new("")),
    )];
    if let Ok(rel) = i.dir.strip_prefix(&i.root) {
        let mut acc = std::path::PathBuf::new();
        for c in rel.components() {
            acc.push(c);
            out.push((
                c.as_os_str().to_string_lossy().into_owned(),
                dvfs::location(i.id, &acc),
            ));
        }
    }
    out
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{ago, frees, share, took};

    /// F01-06: вложенная отметка не считается дважды.
    #[test]
    fn nested_marks_counted_once() {
        assert_eq!(frees([("a", 10), ("b", 5)]), 15);
        assert_eq!(frees([("a", 100), (r"a\x.bin", 40), (r"a\b\c", 7)]), 100);
        assert_eq!(frees([(r"a\b", 30), (r"a\b\c", 7), ("ab", 1)]), 31);
        // «a b» — не предок «a\b».
        assert_eq!(frees([("a b", 3), (r"a\b", 4)]), 7);
        assert_eq!(frees(std::iter::empty()), 0);
    }

    #[test]
    fn share_text() {
        assert_eq!(share(39, 100), (0.39, "39 %".to_owned()));
        assert_eq!(share(1, 1000).1, "<1 %");
        assert_eq!(share(0, 1000).1, "0 %");
        assert_eq!(share(5, 0), (0.0, String::new()));
        assert_eq!(share(200, 100).0, 1.0);
    }

    #[test]
    fn durations() {
        let _ = urfin_i18n::set_language("ru");
        let s = 1_000_000_000i64;
        assert_eq!(ago(0, 45 * s), "45 с");
        assert_eq!(ago(0, 125 * s), "2 мин");
        assert_eq!(ago(0, 7300 * s), "2 ч");
        assert_eq!(ago(0, 3 * 86_400 * s), "3 дн.");
        assert_eq!(took(68_000), "посчитано за 1 мин 08 с");
        assert_eq!(took(900), "посчитано за 1 с");
    }
}
