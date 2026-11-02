//! Вкладка анализа места `du:` (F01-05…F01-08): тексты заголовка и статуса, доля строки
//! в папке, сумма «освободится» по отмеченному, вид строк фильтров (группы «Типов»,
//! разделы «Временных», папка перед именем). Без gpui — рисует `gpui_shell::du`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rustc_hash::FxHashSet;
use urfin_core::{AppState, Attrs, Entry, Tab};
use urfin_i18n::FluentArgs;
use urfin_vfs::du::{DuFilter, DuInfo, DuType, dvfs};

/// Фильтры-чипы (F01 §3.5) по порядку: команда `du.filter kind=…` и подпись.
pub const FILTERS: [(&str, &str); 6] = [
    ("dirs", "du-filter-dirs"),
    ("large", "du-filter-large"),
    ("types", "du-filter-types"),
    ("old", "du-filter-old"),
    ("dups", "du-filter-dups"),
    ("temp", "du-filter-temp"),
];

/// Пороги «Старые» — чипы рядом с фильтрами и подписи.
pub const AGES: [(&str, &str); 4] = [
    ("6m", "du-age-6m"),
    ("1y", "du-age-1y"),
    ("2y", "du-age-2y"),
    ("5y", "du-age-5y"),
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

/// Вид вкладки `du:` (фильтр).
pub fn filter(t: &Tab) -> Option<DuFilter> {
    dvfs::filter_of(&t.loc)
}

/// Порог «Старые» вкладки (`age=` адреса).
pub fn age(t: &Tab) -> Option<compact_str::CompactString> {
    dvfs::view(&t.loc).map(|(_, _, v)| v.age)
}

/// Как рисовать строку вида фильтра.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    Plain,
    /// Группа «Типов»: треугольник, полужирно; раскрыта или нет.
    Group {
        open: bool,
    },
    /// Файл раскрытой группы — с отступом.
    Member,
    /// «ещё N файлов» — с отступом, вторичным цветом.
    More,
    /// Раздел «Временных»: полужирно, размер раздела — после названия.
    Section,
}

/// Вид вкладки для отрисовки строк: фильтр и раскрытые группы «Типов» (бит
/// `DuType as u8`); не `du:` — `None`.
pub fn look(s: &AppState, t: &Tab) -> Option<(DuFilter, u16)> {
    let f = filter(t)?;
    let open = dvfs::registry(s).map_or(0, |r| r.open_mask(&t.loc));
    Some((f, open))
}

/// Вид строки `e` в виде `f` с раскрытыми группами `open`.
pub fn row_kind_of(f: DuFilter, open: u16, e: &Entry) -> RowKind {
    use urfin_core::EntryKind;
    match (f, e.kind) {
        (DuFilter::Types, EntryKind::Other) => match DuType::from_label(&e.name) {
            Some(ty) => RowKind::Group {
                open: open & (1 << ty as u8) != 0,
            },
            None => RowKind::More,
        },
        (DuFilter::Types, _) => RowKind::Member,
        (DuFilter::Temp, EntryKind::Other) => RowKind::Section,
        _ => RowKind::Plain,
    }
}

/// Вид строки `e` вкладки `t`.
pub fn row_kind(s: &AppState, t: &Tab, e: &Entry) -> RowKind {
    look(s, t).map_or(RowKind::Plain, |(f, open)| row_kind_of(f, open, e))
}

/// Байты имени, что рисуются вторичным цветом: папка `dir\` перед именем в плоских
/// видах («Крупные», «Старые», «Типы», «Временные»); иначе 0.
pub fn dim_len_of(f: Option<DuFilter>, name: &str) -> usize {
    match f {
        Some(DuFilter::Dirs) | None => 0,
        Some(_) => name.rfind('\\').map_or(0, |i| i + 1),
    }
}

/// Ключ подписи колонки имени: «Группа» у «Типов», «Где» у «Временных».
pub fn name_header(t: &Tab) -> &'static str {
    match filter(t) {
        Some(DuFilter::Types) => "du-col-group",
        Some(DuFilter::Temp) => "du-col-where",
        _ => "ui-col-name",
    }
}

/// Подсказки после имён во «Временных» (`screen-dutemp`): «сборки Cargo»; у каталогов
/// без доступа — предупреждение.
pub fn hints(s: &AppState, t: &Tab) -> Vec<(String, String, bool)> {
    if filter(t) != Some(DuFilter::Temp) {
        return Vec::new();
    }
    dvfs::registry(s)
        .map(|r| r.hints(&t.loc))
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, h)| !h.text.is_empty())
        .map(|(k, h)| (k.to_string(), h.text, h.warn))
        .collect()
}

/// Строка статуса вида: слева у группы «Типов» — её расширения («Архивы: .zip .7z…»),
/// в середине — «7 групп · 486 530 файлов» / «Найдено 9 · 40,1 ГБ».
pub fn status(s: &AppState, t: &Tab) -> (Option<String>, Option<String>) {
    use urfin_core::{EntryKind, Row};
    let i18n = urfin_i18n::current();
    let view = filter(t);
    let left = match (view, t.current()) {
        (Some(DuFilter::Types), Some(Row::Entry { entry, .. }))
            if entry.kind == EntryKind::Other =>
        {
            DuType::from_label(&entry.name).map(|ty| {
                let exts: Vec<String> = ty.exts().iter().take(6).map(|e| format!(".{e}")).collect();
                format!("{}: {}", entry.name, exts.join(" "))
            })
        }
        _ => None,
    };
    let entries = &t.listing.entries;
    let mid = match view {
        Some(DuFilter::Types) => {
            let groups = entries
                .iter()
                .filter(|e| e.kind == EntryKind::Other && DuType::from_label(&e.name).is_some())
                .count();
            let files = info(s, t).map_or(0, |i| i.files);
            let mut args = FluentArgs::new();
            args.set("groups", groups);
            args.set("files", urfin_i18n::fmt::count(files));
            args.set("count", files);
            Some(i18n.fmt("du-types-status", &args))
        }
        Some(DuFilter::Temp) => {
            let (found, bytes) = entries
                .iter()
                .filter(|e| e.kind != EntryKind::Other)
                .fold((0u64, 0u64), |(c, b), e| (c + 1, b + e.size));
            let mut args = FluentArgs::new();
            args.set("count", found);
            args.set("size", urfin_i18n::fmt::bytes_with(&i18n, bytes));
            Some(i18n.fmt("du-temp-status", &args))
        }
        _ => None,
    };
    (left, mid)
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

/// Знаменатель доли: сумма размеров записей каталога (каталоги без размера — 0). В
/// «Типах» — сумма групп, во «Временных» — крупнейшая строка (полоса — от неё,
/// `screen-dutemp`).
pub fn dir_total(t: &Tab) -> u64 {
    let entries = &t.listing.entries;
    match filter(t) {
        Some(DuFilter::Types) => entries
            .iter()
            .filter(|e| {
                e.kind == urfin_core::EntryKind::Other && DuType::from_label(&e.name).is_some()
            })
            .map(|e| e.size)
            .sum(),
        Some(DuFilter::Temp) => entries
            .iter()
            .filter(|e| e.kind != urfin_core::EntryKind::Other)
            .map(|e| e.size)
            .max()
            .unwrap_or(0),
        _ => entries.iter().map(|e| e.size).sum(),
    }
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
