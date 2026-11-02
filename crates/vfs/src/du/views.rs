//! Виды вкладки `du:` по снимку (F01 §3.5, §3.4): плоские списки фильтров «Крупные»,
//! «Типы», «Старые», «Временные» и данные карты. Только память снимка, без I/O.

use compact_str::{CompactString, format_compact};
use rustc_hash::{FxHashMap, FxHashSet};
use urfin_core::{Attrs, Entry, EntryKind};

use super::dvfs::Frozen;
use super::filters::{self, Effect, Expanded, NAME_RULES, TempGroup, TypeGroup, View};
use super::model::{DirId, DuFilter, DuType, DupGroup, FileRec, NodeFlags, SizeMode};

/// Файлов раскрытой группы «Типов» (крупнейшие); остальное — строкой «ещё N файлов».
pub const GROUP_SHOW: usize = 20;

/// Что проверить на диске при перечитывании вида (вычитание удалённого, F01 §3.7).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Check {
    None,
    File(u32),
    Dir(DirId),
}

/// Строка вида и что за ней на диске.
pub(super) struct Row {
    pub entry: Entry,
    pub check: Check,
}

/// Подсказка у строки «Временных»: «что это», у каталогов без доступа — предупреждение.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// Кто создаёт / что это («сборки Cargo»); у каталога без доступа — «нет доступа».
    pub text: String,
    pub warn: bool,
    /// Понятное имя строки («%TEMP%», «Кэш Microsoft Edge»): крупно, путь — серым
    /// рядом (решение 2026-11-02 «du-filters» №4); `None` — имя строки как есть.
    pub label: Option<String>,
    /// Что будет после удаления: метка и пояснение (решение «du-filters» №3).
    pub effect: Effect,
    pub after: String,
    /// Корзина — в очистку не идёт.
    pub recycle: bool,
}

/// Совпадение правила «Временных».
#[derive(Clone, Debug)]
pub(super) struct TempHit {
    pub group: TempGroup,
    pub dir: Option<DirId>,
    pub file: Option<u32>,
    /// Ключ «что это»; `None` — имена крупнейших подкаталогов (`.cache`).
    pub what: Option<&'static str>,
    /// Ключ понятного имени строки (правила по пути).
    pub label: Option<&'static str>,
    pub effect: Effect,
    /// Ключ «что будет после удаления».
    pub after: &'static str,
}

fn size_of(mode: SizeMode, disk: u64, logical: u64) -> u64 {
    match mode {
        SizeMode::Disk => disk,
        SizeMode::Logical => logical,
    }
}

fn text(key: &str, args: &[(&str, urfin_i18n::FluentValue<'static>)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, v.clone());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn file_attrs(r: &FileRec) -> Attrs {
    let mut a = 0;
    for (from, to) in [
        (FileRec::HIDDEN, Attrs::HIDDEN),
        (FileRec::SYSTEM, Attrs::SYSTEM),
        (FileRec::OFFLINE, Attrs::OFFLINE),
        (FileRec::COMPRESSED, Attrs::COMPRESSED),
    ] {
        if r.attrs & from != 0 {
            a |= to;
        }
    }
    Attrs(a)
}

/// Строка без файла за ней: группа «Типов», «ещё N файлов», раздел «Временных».
fn virtual_row(name: impl Into<CompactString>, size: u64) -> Row {
    Row {
        entry: Entry {
            name: name.into(),
            size,
            mtime: i64::MIN,
            ctime: i64::MIN,
            atime: i64::MIN,
            attrs: Attrs(Attrs::SIZE_KNOWN),
            kind: EntryKind::Other,
        },
        check: Check::None,
    }
}

impl Frozen {
    /// Путь каталога от корня через `\` (корень — пусто).
    pub(super) fn rel_path(
        &self,
        d: DirId,
        memo: &mut FxHashMap<DirId, CompactString>,
    ) -> CompactString {
        if d == 0 {
            return CompactString::default();
        }
        if let Some(p) = memo.get(&d) {
            return p.clone();
        }
        let parent = self.snap.dirs.get(d as usize).map_or(0, |n| n.parent);
        let mut p = self.rel_path(parent, memo);
        if !p.is_empty() {
            p.push('\\');
        }
        p.push_str(self.snap.name(d));
        memo.insert(d, p.clone());
        p
    }

    /// Каталог или кто-то из предков удалён после скана.
    pub(super) fn dead(&self, mut d: DirId) -> bool {
        loop {
            if self.gone.contains(&d) {
                return true;
            }
            match self.snap.dirs.get(d as usize) {
                Some(n) if d != 0 => d = n.parent,
                _ => return false,
            }
        }
    }

    /// Крупный файл ещё в снимке.
    pub(super) fn live_file(&self, i: u32) -> bool {
        !self.gone_files.contains(&i)
    }

    /// Путь крупного файла от корня (`dir\name`).
    pub(super) fn file_rel(
        &self,
        i: u32,
        memo: &mut FxHashMap<DirId, CompactString>,
    ) -> CompactString {
        let r = &self.snap.files[i as usize];
        let dir = self.rel_path(r.dir, memo);
        let name = self.snap.file_name(r);
        if dir.is_empty() {
            CompactString::from(name)
        } else {
            format_compact!("{dir}\\{name}")
        }
    }

    /// Группы дубликатов по размеру (F01 §3.6, уровень `Size`) — без удалённого.
    pub(super) fn dup_candidates(&self, min: u64) -> Vec<DupGroup> {
        super::dups::candidates(&self.snap, min, |i| {
            self.live_file(i) && !self.dead(self.snap.files[i as usize].dir)
        })
    }

    /// Файлы группы, что ещё на диске.
    pub(super) fn live_members(&self, g: &DupGroup) -> smallvec::SmallVec<[u32; 4]> {
        g.files
            .iter()
            .copied()
            .filter(|&i| self.live_file(i) && !self.dead(self.snap.files[i as usize].dir))
            .collect()
    }

    fn file_row(&self, i: u32, memo: &mut FxHashMap<DirId, CompactString>) -> Row {
        let r = &self.snap.files[i as usize];
        let name = self.file_rel(i, memo);
        Row {
            entry: Entry {
                name,
                size: size_of(self.snap.size_mode, r.disk, r.logical),
                mtime: r.mtime,
                ctime: i64::MIN,
                atime: i64::MIN,
                attrs: file_attrs(r),
                kind: EntryKind::File,
            },
            check: Check::File(i),
        }
    }

    fn dir_row(&self, d: DirId, memo: &mut FxHashMap<DirId, CompactString>) -> Row {
        let n = self.snap.dirs[d as usize];
        let partial = if n.flags.has(NodeFlags::DONE) {
            0
        } else {
            Attrs::SIZE_PARTIAL
        };
        Row {
            entry: Entry {
                name: self.rel_path(d, memo),
                size: size_of(self.snap.size_mode, n.disk, n.logical),
                mtime: n.mtime,
                ctime: i64::MIN,
                atime: i64::MIN,
                attrs: Attrs(Attrs::SIZE_KNOWN | partial),
                kind: EntryKind::Dir,
            },
            check: Check::Dir(d),
        }
    }

    /// Строки вида `view` (не «По папкам»); `now` — нс от Unix epoch («Старые»),
    /// `open` — раскрытые группы «Типов» (бит `DuType as u8`), `rules` — «Временные».
    /// Подсказки «Временных» — в `hints`.
    pub(super) fn view_rows(
        &self,
        view: &View,
        now: i64,
        open: u16,
        rules: &[Expanded],
        dups: &[DupGroup],
        hints: &mut FxHashMap<CompactString, Hint>,
    ) -> Vec<Row> {
        let mut memo = FxHashMap::default();
        let live = (0u32..)
            .zip(&self.snap.files)
            .filter(|(i, r)| self.live_file(*i) && !self.dead(r.dir));
        match view.filter {
            DuFilter::Large => live.map(|(i, _)| self.file_row(i, &mut memo)).collect(),
            DuFilter::Old => {
                let before = now.saturating_sub(view.age_nanos());
                live.filter(|(_, r)| r.mtime != i64::MIN && r.mtime < before)
                    .map(|(i, _)| self.file_row(i, &mut memo))
                    .collect()
            }
            DuFilter::Types => self.types_rows(open, &mut memo),
            DuFilter::Temp => self.temp_rows(rules, hints, &mut memo),
            DuFilter::Dups => self.dups_rows(dups, &mut memo),
            DuFilter::Dirs => Vec::new(),
        }
    }

    /// «Дубликаты» (`screen-dudups`): заголовок группы — имя первого файла и размер
    /// одной копии (`ctime` строки — номер группы: имена групп могут совпасть), под
    /// ним — копии, самая старая первой. Группы, где на диске меньше двух копий, —
    /// не показываются.
    fn dups_rows(
        &self,
        groups: &[DupGroup],
        memo: &mut FxHashMap<DirId, CompactString>,
    ) -> Vec<Row> {
        let mut out = Vec::new();
        for (gi, g) in groups.iter().enumerate() {
            let live = self.live_members(g);
            let Some(&first) = live.first().filter(|_| live.len() > 1) else {
                continue;
            };
            let r = &self.snap.files[first as usize];
            let mut head = virtual_row(
                self.snap.file_name(r),
                size_of(self.snap.size_mode, r.disk, r.logical),
            );
            head.entry.ctime = i64::try_from(gi).unwrap_or(i64::MAX);
            out.push(head);
            for i in live {
                out.push(self.file_row(i, memo));
            }
        }
        out
    }

    /// «Типы» (`screen-dutypes`): группы по размеру ↓ («Прочее» — последней), «Сборки
    /// и кэши» — всё внутри сборок проекта, типы — вне их; у раскрытой — крупнейшие
    /// файлы и «ещё N файлов».
    fn types_rows(&self, open: u16, memo: &mut FxHashMap<DirId, CompactString>) -> Vec<Row> {
        let mut groups: Vec<(TypeGroup, u64, u64)> = TypeGroup::ALL
            .into_iter()
            .map(|g| {
                let (disk, count) = self.group_sums(g);
                (g, disk, count)
            })
            .filter(|(_, _, count)| *count > 0)
            .collect();
        groups.sort_by_key(|(g, disk, _)| {
            (
                *g == TypeGroup::Type(DuType::Other),
                std::cmp::Reverse(*disk),
            )
        });
        let mut out = Vec::new();
        for (g, disk, count) in groups {
            out.push(virtual_row(g.label(), disk));
            if open & g.bit() == 0 {
                continue;
            }
            let mut shown = (0u64, 0u64);
            for (i, _) in (0u32..)
                .zip(&self.snap.files)
                .filter(|(i, r)| {
                    TypeGroup::of_file(r) == g && self.live_file(*i) && !self.dead(r.dir)
                })
                .take(GROUP_SHOW)
            {
                let row = self.file_row(i, memo);
                shown.0 += self.snap.files[i as usize].disk;
                shown.1 += 1;
                out.push(row);
            }
            let rest = count.saturating_sub(shown.1);
            if rest > 0 {
                let name = text(
                    "du-types-more",
                    &[("count", rest.into()), ("type", g.label().into())],
                );
                out.push(virtual_row(name, disk.saturating_sub(shown.0)));
            }
        }
        out
    }

    /// Совпадения правил по пути (`%TEMP%` и т. п.) внутри корня анализа — в `out`;
    /// совпавшие каталоги — в `dirs`.
    fn path_hits(&self, rules: &[Expanded], out: &mut Vec<TempHit>, dirs: &mut FxHashSet<DirId>) {
        let root: Vec<String> = self
            .snap
            .root
            .components()
            .filter(|c| !matches!(c, std::path::Component::RootDir))
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        for r in rules {
            if r.comps.len() <= root.len()
                || !root
                    .iter()
                    .zip(&r.comps)
                    .all(|(have, pat)| filters::comp_match(pat, have))
            {
                continue;
            }
            let rest = &r.comps[root.len()..];
            let mut at: Vec<DirId> = vec![0];
            for pat in &rest[..rest.len() - 1] {
                at = at
                    .iter()
                    .flat_map(|d| self.children(*d))
                    .filter(|k| filters::comp_match(pat, self.snap.name(*k)))
                    .collect();
            }
            let last = &rest[rest.len() - 1];
            for d in &at {
                for k in self.children(*d) {
                    if filters::comp_match(last, self.snap.name(k)) && dirs.insert(k) {
                        out.push(TempHit {
                            group: TempGroup::System,
                            dir: Some(k),
                            file: None,
                            what: Some(r.what),
                            label: r.label,
                            effect: r.effect,
                            after: r.after,
                        });
                    }
                }
                for i in self.files_of(*d) {
                    let f = &self.snap.files[i as usize];
                    if self.live_file(i) && filters::comp_match(last, self.snap.file_name(f)) {
                        out.push(TempHit {
                            group: TempGroup::System,
                            dir: None,
                            file: Some(i),
                            what: Some(r.what),
                            label: r.label,
                            effect: r.effect,
                            after: r.after,
                        });
                    }
                }
            }
        }
    }

    /// Совпадения правил «Временных» (F01 §3.5) — внешние: внутри совпавшего не ищем.
    pub(super) fn temp_hits(&self, rules: &[Expanded]) -> Vec<TempHit> {
        let mut out = Vec::new();
        let mut path_dirs: FxHashSet<DirId> = FxHashSet::default();
        self.path_hits(rules, &mut out, &mut path_dirs);
        // Сборки и кэши по имени каталога — обход в глубину.
        let mut hit_dirs = path_dirs.clone();
        let mut stack: Vec<DirId> = self.children(0).collect();
        while let Some(d) = stack.pop() {
            if path_dirs.contains(&d) {
                continue;
            }
            let name = self.snap.name(d);
            let rule = NAME_RULES.iter().find(|r| {
                r.name.eq_ignore_ascii_case(name)
                    && (r.needs.is_empty()
                        || self.children(d).any(|k| {
                            r.needs
                                .iter()
                                .any(|n| n.eq_ignore_ascii_case(self.snap.name(k)))
                        }))
            });
            match rule {
                Some(r) => {
                    hit_dirs.insert(d);
                    out.push(TempHit {
                        group: TempGroup::Project,
                        dir: Some(d),
                        file: None,
                        what: r.what,
                        label: None,
                        effect: r.effect,
                        after: r.after,
                    });
                }
                None => stack.extend(self.children(d)),
            }
        }
        // Одиночные временные файлы вне совпавших каталогов.
        for (i, f) in (0u32..).zip(&self.snap.files) {
            if !self.live_file(i) || !filters::temp_file(self.snap.file_name(f)) {
                continue;
            }
            let mut d = f.dir;
            let inside = loop {
                if hit_dirs.contains(&d) {
                    break true;
                }
                match self.snap.dirs.get(d as usize) {
                    Some(n) if d != 0 => d = n.parent,
                    _ => break false,
                }
            };
            if !inside {
                out.push(TempHit {
                    group: TempGroup::Project,
                    dir: None,
                    file: Some(i),
                    what: Some("du-temp-what-file"),
                    label: None,
                    effect: filters::FILE_EFFECT.0,
                    after: filters::FILE_EFFECT.1,
                });
            }
        }
        out.retain(|h| h.dir.is_none_or(|d| !self.dead(d)));
        out
    }

    /// «Временные» (`screen-dutemp`): разделы «В <корень>» и «Система и профиль», в
    /// каждом — по размеру ↓; у строки — «что это».
    fn temp_rows(
        &self,
        rules: &[Expanded],
        hints: &mut FxHashMap<CompactString, Hint>,
        memo: &mut FxHashMap<DirId, CompactString>,
    ) -> Vec<Row> {
        let found = self.temp_hits(rules);
        let mut out = Vec::new();
        for (group, label) in [
            (
                TempGroup::Project,
                text(
                    "du-temp-in",
                    &[("root", self.snap.root.display().to_string().into())],
                ),
            ),
            (TempGroup::System, text("du-temp-system", &[])),
        ] {
            let mut rows: Vec<(Row, Hint)> = found
                .iter()
                .filter(|h| h.group == group)
                .map(|h| {
                    let row = match (h.dir, h.file) {
                        (Some(d), _) => self.dir_row(d, memo),
                        (None, Some(i)) => self.file_row(i, memo),
                        (None, None) => virtual_row("", 0),
                    };
                    (row, self.hint(h))
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            rows.sort_by_key(|(r, _)| std::cmp::Reverse(r.entry.size));
            let total = rows.iter().map(|(r, _)| r.entry.size).sum();
            out.push(virtual_row(label, total));
            for (row, hint) in rows {
                hints.insert(row.entry.name.clone(), hint);
                out.push(row);
            }
        }
        out
    }

    fn hint(&self, h: &TempHit) -> Hint {
        let i18n = urfin_i18n::current();
        let base = Hint {
            text: String::new(),
            warn: false,
            label: h.label.map(|k| i18n.get(k)),
            effect: h.effect,
            after: i18n.get(h.after),
            recycle: h.what == Some(filters::RECYCLE),
        };
        if let Some(d) = h.dir {
            let n = self.snap.dirs[d as usize];
            if n.flags.0 & (NodeFlags::DENIED | NodeFlags::PARTIAL_DENIED) != 0 {
                let count = self
                    .snap
                    .denied
                    .iter()
                    .filter(|&&x| {
                        let mut c = x;
                        loop {
                            if c == d {
                                break true;
                            }
                            match self.snap.dirs.get(c as usize) {
                                Some(n) if c != 0 => c = n.parent,
                                _ => break false,
                            }
                        }
                    })
                    .count()
                    .max(1);
                return Hint {
                    text: text("du-temp-denied", &[("count", count.into())]),
                    warn: true,
                    ..base
                };
            }
        }
        let text = match (h.what, h.dir) {
            (Some(key), _) => urfin_i18n::current().get(key),
            // `.cache`: что внутри — три крупнейших подкаталога.
            (None, Some(d)) => {
                let mut kids: Vec<DirId> = self.children(d).collect();
                kids.sort_by_key(|k| std::cmp::Reverse(self.snap.dirs[*k as usize].disk));
                kids.iter()
                    .take(3)
                    .map(|k| self.snap.name(*k))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            (None, None) => String::new(),
        };
        Hint { text, ..base }
    }

    /// Каталоги «Временных» (для карты: плитки вне фильтра — бледнее).
    pub(super) fn temp_dirs(&self, rules: &[Expanded]) -> FxHashSet<DirId> {
        self.temp_hits(rules).iter().filter_map(|h| h.dir).collect()
    }

    /// Карта каталога `d` (F01 §3.4): дети и, у подкаталогов, их дети; файлы меньше
    /// порога — одним блоком «мелкие файлы».
    pub(super) fn map_items(&self, d: DirId, temp: &FxHashSet<DirId>) -> Vec<MapItem> {
        let under = {
            let mut c = d;
            loop {
                if temp.contains(&c) {
                    break true;
                }
                match self.snap.dirs.get(c as usize) {
                    Some(n) if c != 0 => c = n.parent,
                    _ => break false,
                }
            }
        };
        self.level(d, temp, under, true)
    }

    fn level(&self, d: DirId, temp: &FxHashSet<DirId>, under: bool, deep: bool) -> Vec<MapItem> {
        let mode = self.snap.size_mode;
        let Some(node) = self.snap.dirs.get(d as usize).copied() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let (mut used, mut files) = (0u64, 0u64);
        for k in self.children(d) {
            let n = self.snap.dirs[k as usize];
            let size = size_of(mode, n.disk, n.logical);
            used += size;
            files += u64::from(n.files);
            if size == 0 {
                continue;
            }
            let t = under || temp.contains(&k);
            out.push(MapItem {
                name: self.snap.name(k).into(),
                size,
                kind: MapKind::Dir,
                temp: t,
                kids: if deep {
                    self.level(k, temp, t, false)
                } else {
                    Vec::new()
                },
            });
        }
        for i in self.files_of(d) {
            if !self.live_file(i) {
                continue;
            }
            let r = &self.snap.files[i as usize];
            let size = size_of(mode, r.disk, r.logical);
            used += size;
            files += 1;
            out.push(MapItem {
                name: self.snap.file_name(r).into(),
                size,
                kind: MapKind::File(r.ty),
                temp: under || filters::temp_file(self.snap.file_name(r)),
                kids: Vec::new(),
            });
        }
        let small = size_of(mode, node.disk, node.logical).saturating_sub(used);
        if small > 0 {
            out.push(MapItem {
                name: CompactString::default(),
                size: small,
                kind: MapKind::Small {
                    files: u32::try_from(u64::from(node.files).saturating_sub(files))
                        .unwrap_or(u32::MAX),
                },
                temp: under,
                kids: Vec::new(),
            });
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.size));
        out
    }
}

/// Элемент карты.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapItem {
    /// Имя в каталоге (у «мелких файлов» — пусто).
    pub name: CompactString,
    pub size: u64,
    pub kind: MapKind,
    /// Внутри совпадения «Временных».
    pub temp: bool,
    /// Дети подкаталога (только у первого уровня).
    pub kids: Vec<MapItem>,
}

/// Вид плитки: каталог, крупный файл своего типа, мелкие файлы каталога одним блоком.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MapKind {
    Dir,
    File(DuType),
    Small { files: u32 },
}

/// Карта каталога вкладки: ключ раскладки и элементы по размеру ↓.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDir {
    /// Анализ и каталог: меняется — раскладка другая.
    pub key: (u32, DirId),
    /// Номер изменения реестра (вычтено удалённое — раскладка заново).
    pub rev: u64,
    pub items: Vec<MapItem>,
}

#[cfg(test)]
#[path = "views_tests.rs"]
mod tests;
