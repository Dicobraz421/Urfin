//! Колонки списка после имени (S2 §2.1–2.3): Full и Wide, пользовательские наборы
//! `[ui.view_modes.<name>]` (`ViewMode::Custom(n)`, S2-19) и их раскладка в строке.
//! Без gpui: панель берёт отсюда ширины и места колонок, шапка — подписи.

use std::collections::BTreeMap;

use compact_str::CompactString;
use smallvec::SmallVec;
use urfin_core::{ConfigWarning, ViewMode};

use crate::config::ViewModeCfg;
use crate::fmt_cell::ColKind;

/// Колонка набора и её ширина, px.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Column {
    pub kind: ColKind,
    pub w: f32,
}

/// Набор колонок `[ui.view_modes.<name>]`; `cols` — после имени, слева направо.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSet {
    pub name: CompactString,
    /// Подпись в переключателе.
    pub title: CompactString,
    pub cols: Vec<Column>,
}

/// Колонка на месте: `x` — левый край от левого края имени.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Slot {
    pub kind: ColKind,
    pub x: f32,
    pub w: f32,
}

/// Отступ текста колонки, прижатой влево, от предыдущей колонки.
pub const GAP: f32 = 12.0;
/// Ширина из конфига не меньше и не больше этого.
const W_RANGE: (f32, f32) = (24.0, 600.0);

const FULL: [Column; 2] = [
    Column {
        kind: ColKind::Size,
        w: ColKind::Size.default_width(),
    },
    Column {
        kind: ColKind::Mtime,
        w: ColKind::Mtime.default_width(),
    },
];
const WIDE: [Column; 1] = [FULL[0]];

const ALL: [ColKind; 8] = [
    ColKind::Name,
    ColKind::Ext,
    ColKind::Type,
    ColKind::Size,
    ColKind::Mtime,
    ColKind::Ctime,
    ColKind::Atime,
    ColKind::Attrs,
];

impl ColKind {
    /// Имя колонки в конфиге.
    pub fn id(self) -> &'static str {
        match self {
            ColKind::Name => "name",
            ColKind::Ext => "ext",
            ColKind::Type => "type",
            ColKind::Size => "size",
            ColKind::Mtime => "mtime",
            ColKind::Ctime => "ctime",
            ColKind::Atime => "atime",
            ColKind::Attrs => "attrs",
        }
    }

    /// Колонка по имени из конфига; `owner` — с S4.
    pub fn parse(id: &str) -> Option<ColKind> {
        let id = id.trim();
        ALL.into_iter().find(|k| k.id().eq_ignore_ascii_case(id))
    }

    /// Ширина по умолчанию, px (размер и дата — как в Full, S2 §2.1).
    pub const fn default_width(self) -> f32 {
        match self {
            ColKind::Name => 0.0,
            ColKind::Ext => 64.0,
            ColKind::Type => 130.0,
            ColKind::Size => 92.0,
            ColKind::Mtime | ColKind::Ctime | ColKind::Atime => 120.0,
            ColKind::Attrs => 56.0,
        }
    }

    /// Текст прижат вправо: размер и даты; прочее — влево с отступом [`GAP`].
    pub fn right_aligned(self) -> bool {
        matches!(
            self,
            ColKind::Size | ColKind::Mtime | ColKind::Ctime | ColKind::Atime
        )
    }

    /// Ключ `panel.sort` для клика по заголовку; у прочих колонок сортировки нет.
    pub fn sort_key(self) -> Option<&'static str> {
        match self {
            ColKind::Name => Some("name"),
            ColKind::Ext => Some("ext"),
            ColKind::Size => Some("size"),
            ColKind::Mtime => Some("mtime"),
            _ => None,
        }
    }

    /// Подпись в шапке. У дат создания и доступа своих строк ещё нет (полоса i18n,
    /// S2-19a) — заголовок пуст.
    pub fn header_key(self) -> Option<&'static str> {
        match self {
            ColKind::Name => Some("ui-col-name"),
            ColKind::Ext => Some("ui-sort-ext"),
            ColKind::Type => Some("ui-info-type"),
            ColKind::Size => Some("ui-col-size"),
            ColKind::Mtime => Some("ui-col-mtime"),
            ColKind::Attrs => Some("ui-info-attrs"),
            ColKind::Ctime | ColKind::Atime => None,
        }
    }
}

/// Наборы из конфига по порядку имён (номер — `n` в `ViewMode::Custom(n)`).
/// Неизвестная колонка или ширина, повтор — пропуск с предупреждением.
pub fn parse_sets(cfg: &BTreeMap<String, ViewModeCfg>) -> (Vec<ColumnSet>, Vec<ConfigWarning>) {
    let mut warnings = Vec::new();
    let mut sets = Vec::new();
    for (i, (name, v)) in cfg.iter().enumerate() {
        let key = format!("ui.view_modes.{name}");
        if i > usize::from(u8::MAX) {
            warnings.push(ConfigWarning {
                key,
                msg: format!("more than {} column sets", u16::from(u8::MAX) + 1),
            });
            break;
        }
        let mut cols: Vec<Column> = Vec::new();
        for id in &v.columns {
            match ColKind::parse(id) {
                // Имя — всегда первым и на всю оставшуюся ширину.
                Some(ColKind::Name) => {}
                Some(kind) if cols.iter().any(|c| c.kind == kind) => {
                    warnings.push(ConfigWarning {
                        key: key.clone(),
                        msg: format!("duplicate column {id:?}"),
                    });
                }
                Some(kind) => cols.push(Column {
                    kind,
                    w: kind.default_width(),
                }),
                None => warnings.push(ConfigWarning {
                    key: key.clone(),
                    msg: format!("unknown column {id:?}"),
                }),
            }
        }
        for (id, w) in &v.widths {
            match ColKind::parse(id).and_then(|k| cols.iter_mut().find(|c| c.kind == k)) {
                Some(c) if w.is_finite() => c.w = w.clamp(W_RANGE.0, W_RANGE.1),
                Some(_) => warnings.push(ConfigWarning {
                    key: key.clone(),
                    msg: format!("bad width of {id:?}"),
                }),
                None => warnings.push(ConfigWarning {
                    key: key.clone(),
                    msg: format!("width of a column not in the set: {id:?}"),
                }),
            }
        }
        sets.push(ColumnSet {
            name: name.as_str().into(),
            title: v.title.as_deref().unwrap_or(name).into(),
            cols,
        });
    }
    (sets, warnings)
}

/// Номер набора по имени (`ui.columns set=`).
pub fn find(sets: &[ColumnSet], name: &str) -> Option<u8> {
    sets.iter()
        .position(|s| s.name == name)
        .and_then(|i| u8::try_from(i).ok())
}

/// Набор вида `Custom(n)`; набора нет (удалён из конфига) — `None`.
pub fn custom(mode: ViewMode, sets: &[ColumnSet]) -> Option<&ColumnSet> {
    match mode {
        ViewMode::Custom(n) => sets.get(usize::from(n)),
        _ => None,
    }
}

/// Колонки после имени для вида: Full — размер и дата, Wide — размер, Brief и
/// Medium — только имена; `Custom(n)` — набор, а без набора — как Full.
pub fn of(mode: ViewMode, sets: &[ColumnSet]) -> &[Column] {
    match mode {
        ViewMode::Brief | ViewMode::Medium => &[],
        ViewMode::Wide => &WIDE,
        ViewMode::Full => &FULL,
        ViewMode::Custom(_) => custom(mode, sets).map_or(&FULL[..], |s| &s.cols),
    }
}

/// У вида есть строка заголовков колонок: Full и пользовательские наборы.
pub fn has_header(mode: ViewMode) -> bool {
    matches!(mode, ViewMode::Full | ViewMode::Custom(_))
}

/// Раскладка строки: от левого края имени до правого края строки — `avail` px. Имени
/// нужно не меньше `name_min`: не влезает — колонки уходят с конца (сначала дата,
/// потом размер, как в Full). Возвращает ширину имени и места колонок вплотную
/// к правому краю.
pub fn layout(cols: &[Column], avail: f32, name_min: f32) -> (f32, SmallVec<[Slot; 8]>) {
    let mut n = cols.len();
    let total = |n: usize| cols[..n].iter().map(|c| c.w).sum::<f32>();
    while n > 0 && avail - total(n) < name_min {
        n -= 1;
    }
    let name_w = (avail - total(n)).max(0.0);
    let mut x = name_w;
    let slots = cols[..n]
        .iter()
        .map(|c| {
            let s = Slot {
                kind: c.kind,
                x,
                w: c.w,
            };
            x += c.w;
            s
        })
        .collect();
    (name_w, slots)
}

/// Набор, номер которого сдвинулся при перечитывании конфига: `Custom(n)` по прежним
/// наборам → по новым (по имени); набора больше нет — Full.
pub fn remap(mode: ViewMode, old: &[ColumnSet], new: &[ColumnSet]) -> ViewMode {
    let Some(set) = custom(mode, old) else {
        return mode;
    };
    find(new, &set.name).map_or(ViewMode::Full, ViewMode::Custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> BTreeMap<String, ViewModeCfg> {
        toml::from_str::<crate::config::UiCfg>(text)
            .unwrap()
            .view_modes
    }

    const DEV: &str = r#"
[view_modes.dev]
title = "Разработка"
columns = ["name", "type", "size", "mtime", "ctime", "attrs"]
widths = { type = 140 }
[view_modes.audit]
columns = ["name", "atime", "owner", "Size", "size"]
"#;

    fn kinds(cols: &[Column]) -> Vec<ColKind> {
        cols.iter().map(|c| c.kind).collect()
    }

    #[test]
    fn sets_by_name_order_with_warnings() {
        let (sets, warnings) = parse_sets(&cfg(DEV));
        assert_eq!(
            sets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["audit", "dev"],
            "номер набора — по порядку имён"
        );
        let dev = &sets[1];
        assert_eq!(dev.title, "Разработка");
        assert_eq!(
            kinds(&dev.cols),
            [
                ColKind::Type,
                ColKind::Size,
                ColKind::Mtime,
                ColKind::Ctime,
                ColKind::Attrs
            ],
            "имя — не колонка набора, оно всегда первое"
        );
        assert_eq!(dev.cols[0].w, 140.0);
        assert_eq!(dev.cols[1].w, ColKind::Size.default_width());
        let audit = &sets[0];
        assert_eq!(audit.title, "audit", "без title — имя набора");
        assert_eq!(kinds(&audit.cols), [ColKind::Atime, ColKind::Size]);
        let msgs: Vec<String> = warnings.iter().map(ToString::to_string).collect();
        assert_eq!(warnings.len(), 2, "{msgs:?}");
        assert_eq!(warnings[0].key, "ui.view_modes.audit");
        assert!(
            warnings[0].msg.contains("unknown column \"owner\""),
            "{msgs:?}"
        );
        assert!(
            warnings[1].msg.contains("duplicate column \"size\""),
            "{msgs:?}"
        );
    }

    #[test]
    fn bad_widths_warn_and_clamp() {
        let (sets, warnings) = parse_sets(&cfg(
            "[view_modes.x]\ncolumns = [\"type\", \"attrs\"]\nwidths = { type = 5000, mtime = 90, attrs = 1 }\n",
        ));
        assert_eq!(sets[0].cols[0].w, 600.0);
        assert_eq!(sets[0].cols[1].w, 24.0);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].msg.contains("not in the set: \"mtime\""));
    }

    #[test]
    fn columns_of_view_modes() {
        let (sets, _) = parse_sets(&cfg(DEV));
        assert_eq!(
            kinds(of(ViewMode::Full, &sets)),
            [ColKind::Size, ColKind::Mtime]
        );
        assert_eq!(kinds(of(ViewMode::Wide, &sets)), [ColKind::Size]);
        assert_eq!(kinds(of(ViewMode::Brief, &sets)), []);
        assert_eq!(kinds(of(ViewMode::Medium, &sets)), []);
        assert_eq!(of(ViewMode::Custom(1), &sets), &sets[1].cols[..]);
        assert_eq!(
            of(ViewMode::Custom(7), &sets),
            of(ViewMode::Full, &sets),
            "набора нет — как Full"
        );
        assert!(has_header(ViewMode::Custom(0)) && has_header(ViewMode::Full));
        assert!(!has_header(ViewMode::Wide) && !has_header(ViewMode::Brief));
        assert_eq!(find(&sets, "dev"), Some(1));
        assert_eq!(find(&sets, "nope"), None);
    }

    #[test]
    fn layout_packs_columns_to_the_right_edge() {
        let (sets, _) = parse_sets(&cfg(DEV));
        let cols = &sets[1].cols;
        // type 140 + size 92 + mtime и ctime по 120 + attrs 56 = 528.
        let (name_w, slots) = layout(cols, 800.0, 120.0);
        assert_eq!(name_w, 800.0 - 528.0);
        assert_eq!(slots.len(), 5);
        assert_eq!(slots[0].x, name_w);
        for w in slots.windows(2) {
            assert_eq!(w[0].x + w[0].w, w[1].x, "вплотную");
        }
        let last = slots[slots.len() - 1];
        assert_eq!(last.x + last.w, 800.0, "последняя — у правого края");
    }

    #[test]
    fn narrow_row_drops_columns_from_the_end() {
        let (sets, _) = parse_sets(&cfg(DEV));
        let cols = &sets[1].cols;
        // Без attrs (56) и ctime (120) нужно 352 + имя 120.
        let (name_w, slots) = layout(cols, 472.0, 120.0);
        assert_eq!(
            slots.iter().map(|s| s.kind).collect::<Vec<_>>(),
            [ColKind::Type, ColKind::Size, ColKind::Mtime]
        );
        assert_eq!(name_w, 120.0);
        let (name_w, slots) = layout(cols, 100.0, 120.0);
        assert!(slots.is_empty());
        assert_eq!(name_w, 100.0, "имя — всё, что есть");
        assert_eq!(layout(cols, -5.0, 120.0).0, 0.0);
    }

    #[test]
    fn full_layout_matches_the_old_rule() {
        // Full: в узкой панели сначала уходит дата, потом размер (S2 §2.1).
        let full = of(ViewMode::Full, &[]);
        let k = |avail: f32| {
            layout(full, avail, 120.0)
                .1
                .iter()
                .map(|s| s.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(k(332.0), [ColKind::Size, ColKind::Mtime]);
        assert_eq!(k(331.0), [ColKind::Size]);
        assert_eq!(k(212.0), [ColKind::Size]);
        assert_eq!(k(211.0), []);
    }

    #[test]
    fn remap_follows_the_set_name() {
        let (old, _) = parse_sets(&cfg(DEV));
        let (new, _) = parse_sets(&cfg(&format!(
            "{DEV}[view_modes.aaa]\ncolumns = [\"ext\"]\n"
        )));
        assert_eq!(
            remap(ViewMode::Custom(1), &old, &new),
            ViewMode::Custom(2),
            "dev сдвинулся: aaa встал первым"
        );
        let (gone, _) = parse_sets(&cfg("[view_modes.audit]\ncolumns = []\n"));
        assert_eq!(remap(ViewMode::Custom(1), &old, &gone), ViewMode::Full);
        assert_eq!(remap(ViewMode::Wide, &old, &gone), ViewMode::Wide);
    }

    #[test]
    fn every_column_parses_and_has_a_width() {
        for k in ALL {
            assert_eq!(ColKind::parse(k.id()), Some(k));
            assert!(
                k == ColKind::Name || k.default_width() >= W_RANGE.0,
                "{k:?}"
            );
        }
        assert_eq!(ColKind::parse(" MTIME "), Some(ColKind::Mtime));
        assert_eq!(ColKind::parse("owner"), None, "owner — с S4");
    }
}
