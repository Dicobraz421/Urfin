//! Объединение ответов менеджеров (F02 §2.4): сводный список установленных с
//! «вне менеджера» и издателем, группы поиска с вариантами, «рекомендуется» и
//! «новейшая». Ответы приходят по одному: [`InstalledMerge::set`],
//! [`SearchMerge::add`].

use std::cmp::Reverse;
use std::collections::HashMap;

use compact_str::CompactString;
use icu_normalizer::ComposingNormalizerBorrowed;

use crate::api::{Hit, MatchKind, Mgr, Package, PkgId};
use crate::arp::ArpEntry;
use crate::version::Version;

/// Разрядность в имени — не часть имени программы.
const ARCH: [&str; 9] = [
    "x64", "x86", "x86_64", "amd64", "arm64", "64-bit", "32-bit", "64bit", "32bit",
];

/// Хвостовое слово, без которого имя — то же: `Telegram Desktop` → `telegram`,
/// `FileZilla Client` → `filezilla`, `RipGrep MSVC` → `ripgrep`.
const GENERIC_TAIL: [&str; 5] = ["desktop", "client", "app", "msvc", "gnu"];

/// Суффиксы пакетов choco: `git.install`, `7zip.portable` — та же программа.
const CHOCO_SUFFIX: [&str; 4] = [".install", ".portable", ".app", ".commandline"];

/// Известные расхождения имён у менеджеров (нормализованные): псевдоним → ключ.
const KNOWN: [(&str, &str); 7] = [
    ("vscode", "visualstudiocode"),
    ("pwsh", "powershell"),
    ("powershellcore", "powershell"),
    ("golang", "go"),
    ("libreofficefresh", "libreoffice"),
    ("libreofficestill", "libreoffice"),
    ("notepadpp", "notepadplusplus"),
];

/// Слова имени после нормализации (§2.4): NFKC, нижний регистр, без скобок
/// (`(x64)`, `(User)`), разрядности и хвостовых версий; в словах — только буквы и
/// цифры, `+` → `plus` (`Notepad++` → `notepadplusplus`).
fn words(s: &str) -> Vec<String> {
    let s = ComposingNormalizerBorrowed::new_nfkc()
        .normalize(s)
        .to_lowercase();
    let mut flat = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let mut out: Vec<String> = flat
        .split_whitespace()
        .filter(|t| !ARCH.contains(t))
        .flat_map(|t| t.split(['.', '-', '_', '/', ',', ':', '\\']))
        .filter(|w| !w.is_empty() && !ARCH.contains(w))
        .map(str::to_owned)
        .collect();
    let is_version = |w: &str| {
        let d = w.trim_end_matches('+');
        let d = d.strip_prefix('v').unwrap_or(d);
        !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
    };
    let keep = out
        .iter()
        .rposition(|w| !is_version(w))
        .map_or(0, |i| i + 1);
    if keep > 0 {
        out.truncate(keep);
    }
    out.into_iter()
        .map(|w| {
            let mut a = String::with_capacity(w.len());
            for c in w.chars() {
                if c == '+' {
                    a.push_str("plus");
                } else if c.is_alphanumeric() {
                    a.push(c);
                }
            }
            a
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Нормализованное имя (§2.4): `Mozilla Firefox (x64 ru)` → `mozillafirefox`,
/// `7-Zip 24.08 (x64)` → `7zip`, `Python 3.12.7 (64-bit)` → `python`.
pub fn norm(s: &str) -> CompactString {
    words(s).concat().into()
}

/// Без хвостовых цифр: `python312` → `python`, `vcredist2022` → `vcredist`; `None`,
/// если цифр нет или букв остаётся меньше двух.
fn without_digits(a: &str) -> Option<&str> {
    let t = a.trim_end_matches(|c: char| c.is_ascii_digit());
    (t.len() < a.len() && t.chars().count() >= 2).then_some(t)
}

/// Набор псевдонимов: дубликаты и пустые не добавляются, известные расхождения
/// ([`KNOWN`]) дают ещё и ключ.
#[derive(Default)]
struct Aliases(Vec<CompactString>);

impl Aliases {
    fn push(&mut self, a: &str) {
        if a.is_empty() {
            return;
        }
        if !self.0.iter().any(|x| x == a) {
            self.0.push(a.into());
        }
        if let Some((_, key)) = KNOWN.iter().find(|(k, _)| *k == a)
            && !self.0.iter().any(|x| x == key)
        {
            self.0.push((*key).into());
        }
    }

    /// Имя целиком, без хвостового общего слова и без ведущего слова издателя
    /// (`Mozilla Firefox` у `Mozilla` → `firefox`).
    fn name(&mut self, name: &str, publisher: Option<&str>) {
        let w = words(name);
        self.push(&w.concat());
        if w.len() > 1 && GENERIC_TAIL.contains(&w[w.len() - 1].as_str()) {
            self.push(&w[..w.len() - 1].concat());
        }
        let first = publisher.and_then(|p| words(p).into_iter().next());
        if let Some(p) = first
            && w.len() > 1
            && w[0] == p
        {
            self.push(&w[1..].concat());
        }
    }

    /// Id менеджера пакетов scoop/choco: само имя и без хвостовых цифр.
    fn short_id(&mut self, id: &str) {
        let n = norm(id);
        self.push(&n);
        if let Some(t) = without_digits(&n) {
            self.push(t);
        }
    }

    fn meets(&self, other: &[CompactString]) -> bool {
        self.0.iter().any(|a| other.contains(a))
    }
}

/// Псевдонимы пакета для группировки (§2.4): имя и хвост Id. winget —
/// `Mozilla.Firefox` → `firefox` и `mozillafirefox`; scoop `extras/firefox` и choco
/// `firefox.install` → `firefox`.
fn aliases(pkg: &Package) -> Aliases {
    let mut a = Aliases::default();
    a.name(&pkg.name, pkg.publisher.as_deref());
    let id = pkg.id.id.as_str();
    match pkg.id.mgr {
        Mgr::Winget if crate::winget::is_local(id) => {}
        Mgr::Winget => {
            if let Some((_, tail)) = id.split_once('.') {
                a.push(&norm(tail));
            }
            a.push(&norm(id));
        }
        Mgr::Scoop => a.short_id(crate::scoop::app_name(id)),
        Mgr::Choco => {
            let lower = id.to_ascii_lowercase();
            let base = CHOCO_SUFFIX
                .iter()
                .find_map(|s| lower.strip_suffix(s))
                .unwrap_or(&lower);
            a.short_id(base);
        }
    }
    a
}

/// Ключи программы для поиска дублей между менеджерами: псевдонимы имени и Id (§2.4).
pub fn program_keys(pkg: &Package) -> Vec<CompactString> {
    aliases(pkg).0
}

/// Издатель winget по Id (`Mozilla.Firefox` → `mozilla`), для деления групп.
fn winget_publisher(pkg: &Package) -> Option<CompactString> {
    if pkg.id.mgr != Mgr::Winget || crate::winget::is_local(&pkg.id.id) {
        return None;
    }
    crate::winget::publisher_of(&pkg.id.id).map(|p| p.to_lowercase())
}

// ---------------------------------------------------------------- установленные

/// Строка сводного списка установленных.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRow {
    /// Пакет; `publisher` — по правилам §2.4.
    pub pkg: Package,
    /// «Вне менеджера»: строка winget без источника и без пары у scoop/choco
    /// (`secondary`, обновлять не предлагаем).
    pub outside: bool,
    /// Записи winget ARP/MSIX, сведённые в эту строку (удаление — через менеджер строки).
    pub twins: Vec<PkgId>,
}

/// Сводный список установленных. Ответ каждого менеджера — [`InstalledMerge::set`];
/// строка «вне менеджера» может обрести пару, когда ответит scoop или choco.
#[derive(Clone, Debug, Default)]
pub struct InstalledMerge {
    lists: Vec<(Mgr, Vec<Package>)>,
    arp: Vec<ArpEntry>,
}

impl InstalledMerge {
    /// `arp` — записи «Программ» ([`crate::Arp::load`]) для издателей.
    pub fn new(arp: Vec<ArpEntry>) -> InstalledMerge {
        InstalledMerge {
            lists: Vec::new(),
            arp,
        }
    }

    /// Ответ менеджера; повторный ответ заменяет прежний.
    pub fn set(&mut self, mgr: Mgr, pkgs: Vec<Package>) {
        match self.lists.iter_mut().find(|(m, _)| *m == mgr) {
            Some((_, list)) => *list = pkgs,
            None => self.lists.push((mgr, pkgs)),
        }
    }

    /// Менеджеры, которые уже ответили, в порядке ответов.
    pub fn answered(&self) -> impl Iterator<Item = Mgr> + '_ {
        self.lists.iter().map(|(m, _)| *m)
    }

    fn list(&self, mgr: Mgr) -> &[Package] {
        self.lists
            .iter()
            .find(|(m, _)| *m == mgr)
            .map_or(&[], |(_, l)| l)
    }

    /// Строки по имени (без учёта регистра), затем менеджеру и Id.
    ///
    /// - choco/scoop и запись winget без источника с общим псевдонимом имени — одна
    ///   строка менеджера choco/scoop (сначала ищется пара у choco); пара — одна на пакет;
    /// - winget без источника и без пары — «вне менеджера»;
    /// - издатель: winget — ARP `Publisher` записи с тем же именем, иначе префикс Id;
    ///   scoop/choco — ARP при совпадении имени, иначе пусто.
    pub fn rows(&self) -> Vec<InstalledRow> {
        let arp: Vec<(Aliases, &ArpEntry)> = self
            .arp
            .iter()
            .map(|e| {
                let mut a = Aliases::default();
                a.name(&e.name, e.publisher.as_deref());
                (a, e)
            })
            .collect();
        let arp_publisher = |a: &Aliases| {
            arp.iter()
                .find(|(x, e)| e.publisher.is_some() && x.meets(&a.0))
                .and_then(|(_, e)| e.publisher.clone())
        };

        let mut rows: Vec<InstalledRow> = Vec::new();
        // Псевдоним → строки choco, затем scoop.
        let mut index: HashMap<CompactString, Vec<usize>> = HashMap::new();
        for mgr in [Mgr::Choco, Mgr::Scoop] {
            for pkg in self.list(mgr) {
                let a = aliases(pkg);
                let mut pkg = pkg.clone();
                if pkg.publisher.is_none() {
                    pkg.publisher = arp_publisher(&a);
                }
                for alias in a.0 {
                    index.entry(alias).or_default().push(rows.len());
                }
                rows.push(InstalledRow {
                    pkg,
                    outside: false,
                    twins: Vec::new(),
                });
            }
        }
        let mut paired = vec![false; rows.len()];
        for pkg in self.list(Mgr::Winget) {
            let mut pkg = pkg.clone();
            if pkg.source.is_some() {
                let name = norm(&pkg.name);
                if let Some(p) = self
                    .arp
                    .iter()
                    .find(|e| e.publisher.is_some() && norm(&e.name) == name)
                    .and_then(|e| e.publisher.clone())
                {
                    pkg.publisher = Some(p);
                }
                rows.push(InstalledRow {
                    pkg,
                    outside: false,
                    twins: Vec::new(),
                });
                continue;
            }
            let a = aliases(&pkg);
            let pair =
                a.0.iter()
                    .filter_map(|x| index.get(x))
                    .flatten()
                    .copied()
                    .filter(|&i| !paired[i])
                    .min();
            match pair {
                Some(i) => {
                    paired[i] = true;
                    let row = &mut rows[i];
                    row.twins.push(pkg.id);
                    if row.pkg.publisher.is_none() {
                        row.pkg.publisher = pkg.publisher;
                    }
                }
                None => rows.push(InstalledRow {
                    pkg,
                    outside: true,
                    twins: Vec::new(),
                }),
            }
        }
        rows.sort_by_cached_key(|r| (r.pkg.name.to_lowercase(), r.pkg.id.mgr, r.pkg.id.id.clone()));
        rows
    }
}

// ---------------------------------------------------------------- поиск

/// Вариант программы в группе поиска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub hit: Hit,
    /// Новейшая версия в группе (`accent`); равных новейших может быть несколько.
    pub newest: bool,
}

/// Группа поиска: одна программа у разных менеджеров (§2.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Постоянный номер группы в пределах [`SearchMerge`]: курсор UI держится за него.
    pub id: u32,
    /// Имя программы: из winget, если он есть в группе.
    pub title: CompactString,
    /// Лучшее совпадение среди вариантов.
    pub matched: MatchKind,
    /// Первый — «рекомендуется»: менеджер по `[pkg].order`, у winget источник `winget`
    /// раньше `msstore`.
    pub variants: Vec<Variant>,
    /// Издатель winget (префикс Id, нижний регистр): разные издатели — разные группы.
    publisher: Option<CompactString>,
    aliases: Vec<CompactString>,
}

impl Group {
    /// «Рекомендуется» (решение владельца №4).
    pub fn recommended(&self) -> &Hit {
        &self.variants[0].hit
    }

    /// Число разных менеджеров с программой.
    pub fn managers(&self) -> usize {
        let mut m: Vec<Mgr> = self.variants.iter().map(|v| v.hit.pkg.id.mgr).collect();
        m.sort_unstable();
        m.dedup();
        m.len()
    }

    /// Новейшая версия есть только у не рекомендуемого варианта: её подсвечивают
    /// отдельно (решение владельца №4).
    pub fn newest_elsewhere(&self) -> bool {
        !self.variants[0].newest && self.variants.iter().any(|v| v.newest)
    }
}

/// Группы поиска одного запроса. Ответы менеджеров — [`SearchMerge::add`] по мере
/// готовности.
#[derive(Clone, Debug)]
pub struct SearchMerge {
    order: Vec<Mgr>,
    groups: Vec<Group>,
    answered: Vec<Mgr>,
    next_id: u32,
}

fn source_rank(hit: &Hit) -> u8 {
    match (hit.pkg.id.mgr, hit.pkg.source.as_deref()) {
        (Mgr::Winget, Some("winget")) | (Mgr::Scoop | Mgr::Choco, _) => 0,
        (Mgr::Winget, Some("msstore")) => 1,
        (Mgr::Winget, _) => 2,
    }
}

impl SearchMerge {
    /// `order` — `[pkg].order`; не названные менеджеры — после, в порядке [`Mgr::ALL`].
    pub fn new(order: &[Mgr]) -> SearchMerge {
        let mut o: Vec<Mgr> = Vec::with_capacity(Mgr::ALL.len());
        for m in order.iter().chain(&Mgr::ALL) {
            if !o.contains(m) {
                o.push(*m);
            }
        }
        SearchMerge {
            order: o,
            groups: Vec::new(),
            answered: Vec::new(),
            next_id: 0,
        }
    }

    /// Группы в порядке показа.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Ответившие менеджеры (пустой ответ — тоже ответ).
    pub fn answered(&self) -> &[Mgr] {
        &self.answered
    }

    /// Ответ менеджера `mgr`. Первые `keep` групп (показанные выше курсора и сама
    /// группа под курсором) остаются на местах — в них только добавляются варианты;
    /// остальные и новые сортируются: совпадение (`Exact` > `IdTail` > `Prefix` > `Contains`),
    /// затем больше менеджеров, затем имя. Повторный ответ менеджера заменяет прежний.
    pub fn add(&mut self, mgr: Mgr, mut hits: Vec<Hit>, keep: usize) {
        if self.answered.contains(&mgr) {
            for g in &mut self.groups {
                g.variants.retain(|v| v.hit.pkg.id.mgr != mgr);
            }
            self.groups.retain(|g| !g.variants.is_empty());
            for g in &mut self.groups {
                g.aliases = g
                    .variants
                    .iter()
                    .flat_map(|v| aliases(&v.hit.pkg).0)
                    .collect();
                g.publisher = g.variants.iter().find_map(|v| winget_publisher(&v.hit.pkg));
            }
        } else {
            self.answered.push(mgr);
        }
        let keep = keep.min(self.groups.len());
        hits.retain(|h| h.pkg.id.mgr == mgr);
        hits.sort_by(|a, b| {
            a.matched
                .cmp(&b.matched)
                .then_with(|| a.pkg.id.cmp(&b.pkg.id))
        });
        let mut touched = vec![false; self.groups.len()];
        for hit in hits {
            let a = aliases(&hit.pkg);
            let publisher = winget_publisher(&hit.pkg);
            let fits = |g: &Group| {
                a.meets(&g.aliases)
                    && match (&publisher, &g.publisher) {
                        (Some(p), Some(q)) => p == q,
                        _ => true,
                    }
            };
            let matching: Vec<usize> = (0..self.groups.len())
                .filter(|&i| fits(&self.groups[i]))
                .collect();
            let at = matching.first().copied();
            // Вариант связал несколько групп: незакреплённые сливаются в первую
            // (Windows Terminal: scoop `windows-terminal` и choco
            // `microsoft-windows-terminal` сходятся на `Microsoft.WindowsTerminal`).
            if let Some(first) = at {
                for &j in matching[1..].iter().rev() {
                    if j < keep || !same_publisher(&self.groups[first], &self.groups[j]) {
                        continue;
                    }
                    let gone = self.groups.remove(j);
                    touched.remove(j);
                    let g = &mut self.groups[first];
                    for x in gone.aliases {
                        if !g.aliases.contains(&x) {
                            g.aliases.push(x);
                        }
                    }
                    if g.publisher.is_none() {
                        g.publisher = gone.publisher;
                    }
                    g.variants.extend(gone.variants);
                    touched[first] = true;
                }
            }
            let i = at.unwrap_or_else(|| {
                self.groups.push(Group {
                    id: self.next_id,
                    title: CompactString::default(),
                    matched: hit.matched,
                    variants: Vec::new(),
                    publisher: None,
                    aliases: Vec::new(),
                });
                self.next_id += 1;
                touched.push(false);
                self.groups.len() - 1
            });
            let g = &mut self.groups[i];
            for x in a.0 {
                if !g.aliases.contains(&x) {
                    g.aliases.push(x);
                }
            }
            if g.publisher.is_none() {
                g.publisher = publisher;
            }
            g.variants.push(Variant { hit, newest: false });
            touched[i] = true;
        }
        let order = self.order.clone();
        for (g, _) in self.groups.iter_mut().zip(&touched).filter(|(_, t)| **t) {
            settle(g, &order);
        }
        self.groups[keep..].sort_by_cached_key(|g| {
            (
                g.matched,
                Reverse(g.managers()),
                g.title.to_lowercase(),
                g.recommended().pkg.id.clone(),
            )
        });
    }
}

/// Издатели winget групп не спорят.
fn same_publisher(a: &Group, b: &Group) -> bool {
    match (&a.publisher, &b.publisher) {
        (Some(p), Some(q)) => p == q,
        _ => true,
    }
}

/// Порядок вариантов, «новейшая», имя и совпадение группы.
fn settle(g: &mut Group, order: &[Mgr]) {
    let rank = |m: Mgr| order.iter().position(|x| *x == m).unwrap_or(order.len());
    g.variants.sort_by_cached_key(|v| {
        let p = &v.hit.pkg;
        (
            rank(p.id.mgr),
            source_rank(&v.hit),
            v.hit.matched,
            Reverse(Version::parse(&p.version)),
            p.id.id.len(),
            p.id.id.clone(),
        )
    });
    let max = g
        .variants
        .iter()
        .filter_map(|v| Version::parse(&v.hit.pkg.version))
        .max();
    for v in &mut g.variants {
        v.newest = max.is_some() && Version::parse(&v.hit.pkg.version) == max;
    }
    g.matched = g
        .variants
        .iter()
        .map(|v| v.hit.matched)
        .min()
        .unwrap_or(MatchKind::Contains);
    let named = g
        .variants
        .iter()
        .find(|v| v.hit.pkg.id.mgr == Mgr::Winget)
        .unwrap_or(&g.variants[0]);
    g.title = named.hit.pkg.name.clone();
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
