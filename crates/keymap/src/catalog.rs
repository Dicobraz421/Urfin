//! Таблица раздела «Клавиатура» (`screen-settings-keys.png`): строки — команды реестра и
//! привязки действующей раскладки с понятным местом и конфликтами. Считается один раз на
//! раскладку и язык (кэш — у ui); поиск идёт по готовой строке.

use compact_str::CompactString;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use urfin_core::{Command, Value};
use urfin_i18n::I18n;

use crate::compile::label_key;
use crate::edit::Target;
use crate::key::Chord;
use crate::keymap::{Binding, Keymap, norm};
use crate::when::{FlagRegistry, When};

/// Клавиша строки: у каждой — свой контекст («Удалить»: F8 в `panel`, Del — в `panel &&
/// cmdline_empty`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyChip {
    pub chord: Chord,
    pub label: CompactString,
    pub context: CompactString,
}

/// Строка таблицы.
#[derive(Clone, Debug)]
pub struct KeyRow {
    pub target: Target,
    pub title: String,
    /// Ключ Fluent колонки «Где».
    pub place: &'static str,
    /// Контекст новой клавиши.
    pub context: CompactString,
    pub chips: SmallVec<[KeyChip; 2]>,
    /// Быстрый поиск по Alt+букве: не привязка, а настройка `[keymap] quick_search`.
    pub alt_letter: bool,
    /// Отличается от пресета: есть что сбрасывать.
    pub changed: bool,
    /// Команды, спорящие с этой за клавишу в том же месте.
    pub conflicts: SmallVec<[String; 1]>,
    /// Первая в паре конфликта: подпись длиннее (`screen-settings-keys.png`).
    pub conflict_first: bool,
    /// Текст поиска: название, id, клавиши (нижний регистр).
    hay: String,
}

/// Вся таблица.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub rows: Vec<KeyRow>,
    /// Клавиш с конфликтом.
    pub conflicts: u32,
}

/// Привязка, с которой спорит новое сочетание.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clash {
    pub title: String,
    pub context: CompactString,
    pub target: Target,
}

/// Служебные команды: их не вызывают клавишей.
const HIDDEN: [&str; 6] = [
    "demo.",
    "screens.",
    "keymap.pending",
    "keymap.bind",
    "keymap.unbind",
    "ui.keys",
];

/// Первые в месте — пространства файловых операций (`screen-settings-keys.png`).
const FIRST: [&str; 4] = ["ops.", "sel.", "file.", "panel."];

/// Порядок мест в таблице.
const PLACES: [&str; 19] = [
    "keys-where-panel",
    "keys-where-both",
    "keys-where-window",
    "keys-where-cmdline",
    "keys-where-cmdmenu",
    "keys-where-tree",
    "keys-where-qs",
    "keys-where-viewer",
    "keys-where-cast",
    "keys-where-editor",
    "keys-where-terminal",
    "keys-where-sshpair",
    "keys-where-sshgroup",
    "keys-where-du",
    "keys-where-pkg",
    "keys-where-search",
    "keys-where-overlay",
    "keys-where-prompt",
    "keys-where-other",
];

/// Положительные атомы выражения (без `!`).
fn atoms(when: &str) -> SmallVec<[&str; 4]> {
    let mut out = SmallVec::new();
    let mut rest = when;
    while let Some(i) = rest.find(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        let negated = rest[..i].trim_end().ends_with('!');
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(tail.len());
        if !negated {
            out.push(&tail[..end]);
        }
        rest = &tail[end..];
    }
    out
}

/// Особые вкладки и режимы: важнее фокуса.
const SPECIAL: [(&str, &str); 10] = [
    ("in_du", "keys-where-du"),
    ("in_pkg", "keys-where-pkg"),
    ("in_search", "keys-where-search"),
    ("search_screen", "keys-where-search"),
    ("in_sshpair", "keys-where-sshpair"),
    ("ssh_group", "keys-where-sshgroup"),
    ("cast_view", "keys-where-cast"),
    ("tree", "keys-where-tree"),
    ("qs", "keys-where-qs"),
    ("cmdline_menu", "keys-where-cmdmenu"),
];

/// Фокус.
const FOCUS: [(&str, &str); 7] = [
    ("panel", "keys-where-panel"),
    ("cmdline", "keys-where-cmdline"),
    ("viewer", "keys-where-viewer"),
    ("editor", "keys-where-editor"),
    ("terminal", "keys-where-terminal"),
    ("prompt", "keys-where-prompt"),
    ("global", "keys-where-window"),
];

/// Ключ Fluent места (колонка «Где») по контексту: особые вкладки и режимы важнее фокуса.
pub fn place(context: &str) -> &'static str {
    let list = atoms(context);
    let has = |n: &str| list.contains(&n);
    if let Some((_, k)) = SPECIAL.iter().find(|(n, _)| has(n)) {
        return k;
    }
    if has("panel") && has("cmdline") {
        return "keys-where-both";
    }
    if let Some((_, k)) = FOCUS.iter().find(|(n, _)| has(n)) {
        return k;
    }
    if list.iter().any(|x| x.starts_with("overlay")) {
        return "keys-where-overlay";
    }
    "keys-where-other"
}

/// Привязки без перекрытых: у одного сочетания в одном контексте живёт последняя.
pub(crate) fn live(list: &[Binding]) -> Vec<&Binding> {
    let mut newest: FxHashMap<(&Chord, String), usize> = FxHashMap::default();
    for (i, b) in list.iter().enumerate() {
        newest.insert((&b.chord, norm(&b.src.context)), i);
    }
    let mut keep: Vec<usize> = newest.into_values().collect();
    keep.sort_unstable();
    keep.into_iter().map(|i| &list[i]).collect()
}

/// Название команды: своя подпись варианта (`keys-panel-toggle-left`), подпись keybar
/// (`Сортировка: Имя`) или аргументы в скобках.
fn title_of(c: &Command, km: &Keymap, i18n: &I18n) -> String {
    let id = c.id.as_str();
    let key = format!("cmd-{}", id.replace('.', "-"));
    let base = if i18n.has(&key) {
        i18n.get(&key)
    } else {
        km.metas()
            .iter()
            .find(|m| m.id == id)
            .map_or_else(|| id.to_owned(), |m| m.title.to_owned())
    };
    if c.args.iter().next().is_none() {
        return base;
    }
    let label = label_key(c);
    let tail = label.strip_prefix("keybar-").unwrap_or(&label);
    let own = format!("keys-{tail}");
    if i18n.has(&own) {
        return i18n.get(&own);
    }
    // Подпись варианта — если она не просто сокращение самой команды.
    let plain = format!("keybar-{}", id.replace('.', "-"));
    if tail.len() > key.len() - "cmd-".len()
        && i18n.has(&label)
        && i18n.get(&label) != i18n.get(&plain)
    {
        return format!("{base}: {}", i18n.get(&label));
    }
    let args: Vec<String> = c.args.iter().map(|(k, v)| arg_text(k, v)).collect();
    format!("{base} ({})", args.join(", "))
}

fn arg_text(k: &str, v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Bool(true) => k.to_owned(),
        Value::Bool(false) => format!("{k}: off"),
        Value::Int(n) => n.to_string(),
        other => format!("{other:?}"),
    }
}

fn title_for(cmds: &[Command], km: &Keymap, i18n: &I18n) -> String {
    let parts: Vec<String> = cmds.iter().map(|c| title_of(c, km, i18n)).collect();
    parts.join(" + ")
}

/// Контекст команды без клавиши: как у других её привязок или у её пространства; иначе
/// `global`.
fn home(id: &str, list: &[&Binding]) -> CompactString {
    if id == "keymap.qs.start" {
        return "panel".into();
    }
    let ns = id.split('.').next().unwrap_or(id);
    let mut counts: FxHashMap<&str, u32> = FxHashMap::default();
    for pick in [
        &(|c: &Command| c.id.as_str() == id) as &dyn Fn(&Command) -> bool,
        &|c: &Command| c.id.as_str().split('.').next() == Some(ns),
    ] {
        for b in list {
            if b.cmds.iter().any(pick) {
                *counts.entry(&b.src.context).or_default() += 1;
            }
        }
        if let Some((c, _)) = counts
            .iter()
            .max_by_key(|(c, n)| (**n, std::cmp::Reverse(**c)))
        {
            return (*c).into();
        }
    }
    "global".into()
}

impl Catalog {
    /// `alt_letter` — быстрый поиск начинается с Alt+буквы.
    #[allow(clippy::too_many_lines, reason = "таблица собирается сверху вниз")]
    pub fn build(km: &Keymap, i18n: &I18n, alt_letter: bool) -> Catalog {
        let os = km.os();
        let now = live(km.bindings());
        let base = live(km.preset_bindings());
        let mut rows: Vec<KeyRow> = Vec::new();
        let mut index: FxHashMap<(String, &'static str), usize> = FxHashMap::default();
        let mut was: Vec<SmallVec<[(Chord, String); 2]>> = Vec::new();
        let mut row_of = |b: &Binding, rows: &mut Vec<KeyRow>, was: &mut Vec<_>| -> usize {
            let target = Target::of(b);
            let at = place(&b.src.context);
            *index.entry((target.key(), at)).or_insert_with(|| {
                rows.push(KeyRow {
                    title: title_for(&b.cmds, km, i18n),
                    place: at,
                    context: b.src.context.clone(),
                    chips: SmallVec::new(),
                    alt_letter: false,
                    changed: false,
                    conflicts: SmallVec::new(),
                    conflict_first: false,
                    hay: String::new(),
                    target,
                });
                was.push(SmallVec::new());
                rows.len() - 1
            })
        };
        for b in &now {
            let i = row_of(b, &mut rows, &mut was);
            rows[i].chips.push(KeyChip {
                label: b.chord.display(os),
                chord: b.chord.clone(),
                context: b.src.context.clone(),
            });
        }
        for b in &base {
            let i = row_of(b, &mut rows, &mut was);
            was[i].push((b.chord.clone(), norm(&b.src.context)));
        }
        for (r, was) in rows.iter_mut().zip(&was) {
            r.changed = r.chips.len() != was.len()
                || !r
                    .chips
                    .iter()
                    .all(|c| was.contains(&(c.chord.clone(), norm(&c.context))));
        }
        // Команды без клавиши. С обязательными аргументами клавишей не вызвать.
        for m in km.metas() {
            let bound = rows.iter().any(|r| {
                r.target
                    .cmds
                    .iter()
                    .any(|c| c.split(' ').next() == Some(m.id))
            });
            if bound
                || m.args.iter().any(|a| a.required)
                || HIDDEN.iter().any(|h| m.id.starts_with(h))
            {
                continue;
            }
            let context = home(m.id, &now);
            let cmd = Command::new(m.id);
            rows.push(KeyRow {
                title: title_of(&cmd, km, i18n),
                place: place(&context),
                context,
                chips: SmallVec::new(),
                alt_letter: false,
                changed: false,
                conflicts: SmallVec::new(),
                conflict_first: false,
                hay: String::new(),
                target: Target::command(m.id),
            });
        }
        if alt_letter
            && let Some(r) = rows
                .iter_mut()
                .find(|r| r.target.key() == "keymap.qs.start")
        {
            r.alt_letter = true;
        }
        let order: FxHashMap<&str, usize> = km
            .metas()
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id, i))
            .collect();
        let rank = |r: &KeyRow| {
            let id = r.target.cmds[0].split(' ').next().unwrap_or_default();
            (
                PLACES
                    .iter()
                    .position(|p| *p == r.place)
                    .unwrap_or(PLACES.len()),
                FIRST
                    .iter()
                    .position(|f| id.starts_with(f))
                    .unwrap_or(FIRST.len()),
                order.get(id).copied().unwrap_or(usize::MAX),
            )
        };
        rows.sort_by_cached_key(rank);
        let conflicts = mark_conflicts(km, &now, &mut rows, i18n);
        for r in &mut rows {
            let mut hay = r.title.to_lowercase();
            for c in &r.target.cmds {
                hay.push(' ');
                hay.push_str(&c.to_lowercase());
            }
            for c in &r.chips {
                hay.push(' ');
                hay.push_str(&c.label.to_lowercase());
            }
            r.hay = hay;
        }
        Catalog { rows, conflicts }
    }

    /// Строки по запросу: все слова запроса — в названии, id или клавишах.
    pub fn filter(&self, query: &str) -> Vec<u32> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| words.iter().all(|w| r.hay.contains(w)))
            .filter_map(|(i, _)| u32::try_from(i).ok())
            .collect()
    }
}

/// Конфликт: разные команды на одном сочетании там, где слои и точность контекста не
/// определяют победителя заранее — равная специфичность и один слой или один контекст.
fn mark_conflicts(km: &Keymap, now: &[&Binding], rows: &mut [KeyRow], i18n: &I18n) -> u32 {
    let mut by_chord: FxHashMap<&Chord, SmallVec<[&Binding; 2]>> = FxHashMap::default();
    for b in now {
        by_chord.entry(&b.chord).or_default().push(b);
    }
    let mut count = 0;
    let mut groups: Vec<_> = by_chord.into_values().filter(|g| g.len() > 1).collect();
    groups.sort_by_key(|g| g[0].chord.display(km.os()));
    for group in groups {
        let mut found = false;
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                if a.layer != b.layer || a.cmds == b.cmds || !a.when.ambiguous_with(&b.when) {
                    continue;
                }
                found = true;
                let find = |x: &Binding| {
                    let key = Target::of(x).key();
                    let at = place(&x.src.context);
                    rows.iter()
                        .position(|r| r.place == at && r.target.key() == key)
                };
                let (Some(ra), Some(rb)) = (find(a), find(b)) else {
                    continue;
                };
                let (ta, tb) = (title_for(&a.cmds, km, i18n), title_for(&b.cmds, km, i18n));
                rows[ra].conflicts.push(tb);
                rows[rb].conflicts.push(ta);
                let first = ra.min(rb);
                rows[first].conflict_first = true;
            }
        }
        count += u32::from(found);
    }
    count
}

/// С кем спорит сочетание `chord`, если дать его `target` в контексте `context`: другие
/// команды в пересекающихся контекстах. В `viewer` клавиша `panel` не мешает.
pub fn clashes(km: &Keymap, chord: &Chord, context: &str, target: &Target) -> Vec<Clash> {
    let want = norm(context);
    let parsed;
    let when: &When = match km
        .bindings()
        .iter()
        .find(|b| norm(&b.src.context) == want)
        .map(|b| b.when.as_ref())
    {
        Some(w) => w,
        None => match When::parse(context, &FlagRegistry::with_builtin()) {
            Ok(w) => {
                parsed = w;
                &parsed
            }
            Err(_) => return Vec::new(),
        },
    };
    let i18n = urfin_i18n::current();
    live(km.bindings())
        .into_iter()
        .filter(|b| b.chord == *chord && Target::of(b) != *target && b.when.overlaps(when))
        .map(|b| Clash {
            title: title_for(&b.cmds, km, &i18n),
            context: b.src.context.clone(),
            target: Target::of(b),
        })
        .collect()
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
