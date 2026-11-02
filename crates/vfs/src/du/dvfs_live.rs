//! Правка снимка по живым изменениям и дочитке (F01 §3.3, F01-10/F01-11): каталог
//! перелистан — его файлы заново, исчезнувшие подкаталоги вычтены, новые поддеревья
//! (досчитанные отдельно) привиты; «●» (`CHANGED`) — отметка и снятие. Под блокировкой
//! реестра, без I/O: листинги и досчёт — у вызывающего ([`super::super::fresh`]).
//!
//! Суммы каталога точны: свои файлы каталога = узел минус подкаталоги (`DuNode` своих
//! файлов не хранит), новое — из листинга. Суммы по типам мелких файлов в снимке по
//! каталогам не хранятся: разница своих файлов вне крупных приписывается типу по
//! подсказке (имена из событий), иначе — пропорционально типам мелких файлов
//! листинга, иначе — «Прочее». Повторные жёсткие ссылки при перелистывании считаются
//! заново (дедуп — только полным сканом).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use compact_str::CompactString;
use rustc_hash::{FxHashMap, FxHashSet};

use super::{DuRegistry, Frozen, SnapId, Src, index, join_rel};
use crate::du::model::{DirId, DuNode, DuSnapshot, DuType, FileRec, NameRef, NodeFlags};

/// Файл листинга каталога.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub name: CompactString,
    pub escaped: bool,
    pub disk: u64,
    pub logical: u64,
    pub mtime: i64,
    pub file_id: u64,
    /// Флаги `RawEntry` (= `FileRec::attrs`).
    pub flags: u8,
}

/// Листинг одного каталога для перелистывания.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub files: Vec<Listed>,
    /// Подкаталоги: имя, `mtime`, `FileId`.
    pub dirs: Vec<(CompactString, i64, u64)>,
    /// `mtime` самого каталога (`None` — не узнали).
    pub mtime: Option<i64>,
    /// Пропущены ссылки (symlink/junction).
    pub link_skipped: bool,
}

/// Новое поддерево: имя подкаталога и его скан (корень скана — этот подкаталог).
pub type Graft = (CompactString, DuSnapshot);

fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Frozen {
    /// Пересобрать индексы детей и файлов (после прививки и смены крупных файлов).
    fn reindex(&mut self) {
        let n = self.snap.dirs.len();
        let (start, kids) = index(
            n,
            (0u32..)
                .zip(&self.snap.dirs)
                .skip(1)
                .map(|(id, d)| (d.parent, id)),
        );
        let (fstart, fidx) = index(n, (0u32..).zip(&self.snap.files).map(|(i, f)| (f.dir, i)));
        self.start = start;
        self.kids = kids;
        self.fstart = fstart;
        self.fidx = fidx;
    }

    /// Свои файлы каталога: узел минус живые подкаталоги.
    fn own(&self, d: DirId) -> (u64, u64, u32) {
        let Some(n) = self.snap.dirs.get(d as usize) else {
            return (0, 0, 0);
        };
        let (mut disk, mut logical, mut files) = (n.disk, n.logical, n.files);
        for k in self.children(d) {
            let c = &self.snap.dirs[k as usize];
            disk = disk.saturating_sub(c.disk);
            logical = logical.saturating_sub(c.logical);
            files = files.saturating_sub(c.files);
        }
        (disk, logical, files)
    }

    fn put_name(&mut self, s: &str, escaped: bool) -> NameRef {
        let off = u32::try_from(self.snap.names.len()).unwrap_or(u32::MAX);
        let len = u16::try_from(s.len()).unwrap_or(0x7FFF).min(0x7FFF);
        self.snap
            .names
            .extend_from_slice(&s.as_bytes()[..usize::from(len)]);
        NameRef::new(off, len, escaped)
    }

    /// Прибавить к суммам по типам (и сборок, если `build`).
    fn add_types(&mut self, build: bool, delta: &[(i64, i64); DuType::COUNT]) {
        for (i, &(disk, files)) in delta.iter().enumerate() {
            if disk == 0 && files == 0 {
                continue;
            }
            let t = &mut self.snap.types;
            t.disk[i] = t.disk[i].saturating_add_signed(disk);
            t.files[i] = t.files[i].saturating_add_signed(files);
            if build {
                let b = &mut self.snap.build;
                b.disk[i] = b.disk[i].saturating_add_signed(disk);
                b.files[i] = b.files[i].saturating_add_signed(files);
            }
        }
    }

    /// Привить скан нового подкаталога `name` к каталогу `d`. Подкаталог уже есть —
    /// ничего.
    fn graft(&mut self, d: DirId, name: &str, sub: &DuSnapshot) {
        if sub.dirs.is_empty() || self.child(d, name).is_some() {
            return;
        }
        // Всё внутри — сборка: родитель в сборке или сам подкаталог по правилу имени.
        let all_build = self.snap.in_build(d)
            || super::super::filters::build_rule(name).is_some_and(|r| {
                r.needs.is_empty()
                    || (1..sub.dirs.len()).any(|k| {
                        let k = u32::try_from(k).unwrap_or(u32::MAX);
                        sub.dirs[k as usize].parent == 0
                            && r.needs.iter().any(|w| w.eq_ignore_ascii_case(sub.name(k)))
                    })
            });
        let base = u32::try_from(self.snap.dirs.len()).unwrap_or(u32::MAX);
        for (j, n) in sub.dirs.iter().enumerate() {
            let (text, escaped) = if j == 0 {
                (name, false)
            } else {
                (
                    std::str::from_utf8(&sub.names[n.name.range()]).unwrap_or(""),
                    n.name.escaped(),
                )
            };
            let name_ref = self.put_name(text, escaped);
            let mut flags = n.flags;
            if all_build {
                flags.0 |= NodeFlags::BUILD;
            }
            self.snap.dirs.push(DuNode {
                parent: if j == 0 { d } else { base + n.parent },
                name: name_ref,
                flags,
                ..*n
            });
        }
        for f in &sub.files {
            let text = std::str::from_utf8(&sub.names[f.name.range()]).unwrap_or("");
            let name_ref = self.put_name(text, f.name.escaped());
            let mut attrs = f.attrs;
            if all_build {
                attrs |= FileRec::IN_BUILD;
            }
            self.snap.files.push(FileRec {
                dir: base + f.dir,
                name: name_ref,
                attrs,
                ..*f
            });
        }
        for i in 0..DuType::COUNT {
            self.snap.types.disk[i] += sub.types.disk[i];
            self.snap.types.files[i] += sub.types.files[i];
            let b = if all_build { &sub.types } else { &sub.build };
            self.snap.build.disk[i] += b.disk[i];
            self.snap.build.files[i] += b.files[i];
        }
        self.snap
            .denied
            .extend(sub.denied.iter().map(|&k| base + k));
        if !sub.denied.is_empty() {
            let mut cur = d;
            while let Some(n) = self.snap.dirs.get_mut(cur as usize) {
                n.flags.0 |= NodeFlags::PARTIAL_DENIED;
                cur = n.parent;
            }
        }
        let r = sub.dirs[0];
        self.add(d, signed(r.disk), signed(r.logical), i64::from(r.files));
    }

    /// Перелистанный каталог `d`: подкаталоги — исчезнувшие вычесть, новые привить
    /// (`grafts`), у оставшихся обновить `mtime`; свои файлы — разница с листингом в
    /// каталог и предков; крупные файлы — заново; «●» снимается. `hint` — типы файлов
    /// из событий. Возвращает: сменился список крупных файлов (индексы файлов другие).
    pub(in crate::du) fn relist(
        &mut self,
        d: DirId,
        l: &Listing,
        grafts: &[Graft],
        hint: &[DuType],
    ) -> bool {
        if d as usize >= self.snap.dirs.len() || self.dead(d) {
            return false;
        }
        let structural = self.relist_dirs(d, l, grafts);
        let files_changed = self.relist_files(d, l, hint);
        if let Some(n) = self.snap.dirs.get_mut(d as usize) {
            if let Some(m) = l.mtime {
                n.mtime = m;
            }
            n.flags.0 &= !NodeFlags::CHANGED;
            if l.link_skipped {
                n.flags.0 |= NodeFlags::LINK_SKIPPED;
            }
        }
        self.snap.generation += 1;
        self.recount();
        structural || files_changed
    }

    /// Подкаталоги перелистанного `d`: исчезнувшие — вычесть, новые — привить.
    fn relist_dirs(&mut self, d: DirId, l: &Listing, grafts: &[Graft]) -> bool {
        let on_disk: FxHashMap<&str, (i64, u64)> = l
            .dirs
            .iter()
            .map(|(n, m, id)| (n.as_str(), (*m, *id)))
            .collect();
        let kids: Vec<DirId> = self.children(d).collect();
        for k in kids {
            match on_disk.get(self.snap.name(k)) {
                None => self.drop_dir(k),
                Some(&(mtime, id)) => {
                    let n = &mut self.snap.dirs[k as usize];
                    n.mtime = mtime;
                    if id != 0 {
                        n.file_id = id;
                    }
                }
            }
        }
        let mut structural = false;
        for (name, sub) in grafts {
            if on_disk.contains_key(name.as_str()) && self.child(d, name).is_none() {
                self.graft(d, name, sub);
                structural = true;
            }
        }
        if structural {
            self.reindex();
        }
        structural
    }

    /// Свои файлы перелистанного `d`: разница — в каталог, предков и типы; крупные —
    /// заново. `true` — список крупных файлов сменился.
    fn relist_files(&mut self, d: DirId, l: &Listing, hint: &[DuType]) -> bool {
        let (old_disk, old_logical, old_files) = self.own(d);
        let track_min = self.snap.track_min.max(1);
        let new_disk: u64 = l.files.iter().map(|f| f.disk).sum();
        let new_logical: u64 = l.files.iter().map(|f| f.logical).sum();
        let new_files = u32::try_from(l.files.len()).unwrap_or(u32::MAX);
        self.add(
            d,
            signed(new_disk) - signed(old_disk),
            signed(new_logical) - signed(old_logical),
            i64::from(new_files) - i64::from(old_files),
        );
        // Крупные файлы: совпавшие по имени и размеру остаются, прочие — заново.
        let build = self.snap.in_build(d);
        let mut kept: FxHashSet<&str> = FxHashSet::default();
        let mut types = [(0i64, 0i64); DuType::COUNT];
        let mut large_delta = (0i64, 0i64);
        let mut files_changed = false;
        let now: FxHashMap<&str, &Listed> = l.files.iter().map(|f| (f.name.as_str(), f)).collect();
        let old: Vec<u32> = self.files_of(d).filter(|&i| self.live_file(i)).collect();
        for i in old {
            let r = self.snap.files[i as usize];
            let name = self.snap.file_name(&r);
            match now.get(name) {
                Some(f) if f.disk == r.disk && f.logical == r.logical => {
                    kept.insert(f.name.as_str());
                }
                _ => {
                    self.gone_files.insert(i);
                    types[r.ty as usize].0 -= signed(r.disk);
                    types[r.ty as usize].1 -= 1;
                    large_delta.0 -= signed(r.disk);
                    large_delta.1 -= 1;
                    files_changed = true;
                }
            }
        }
        let mut small_by_type = [0u64; DuType::COUNT];
        let mut added: Vec<FileRec> = Vec::new();
        for f in &l.files {
            let ty = super::super::filters::type_of(&crate::walk::RawName::Str {
                name: &f.name,
                escaped: f.escaped,
            });
            if f.disk.max(f.logical) < track_min {
                small_by_type[ty as usize] += f.disk;
                continue;
            }
            if kept.contains(f.name.as_str()) {
                continue;
            }
            types[ty as usize].0 += signed(f.disk);
            types[ty as usize].1 += 1;
            large_delta.0 += signed(f.disk);
            large_delta.1 += 1;
            let name = self.put_name(&f.name, f.escaped);
            added.push(FileRec {
                dir: d,
                name,
                ty,
                attrs: f.flags | if build { FileRec::IN_BUILD } else { 0 },
                disk: f.disk,
                logical: f.logical,
                mtime: f.mtime,
                file_id: f.file_id,
            });
            files_changed = true;
        }
        // Разница мелких файлов — по подсказке, пропорционально листингу или в «Прочее».
        let rest_disk = signed(new_disk) - signed(old_disk) - large_delta.0;
        let rest_files = i64::from(new_files) - i64::from(old_files) - large_delta.1;
        if rest_disk != 0 || rest_files != 0 {
            attribute(rest_disk, rest_files, hint, &small_by_type, &mut types);
        }
        self.add_types(build, &types);
        if files_changed {
            self.compact_files(added);
        }
        files_changed
    }

    /// Крупные файлы без удалённых и с новыми, по размеру ↓; индексы — заново.
    fn compact_files(&mut self, added: Vec<FileRec>) {
        let gone = std::mem::take(&mut self.gone_files);
        let mut files: Vec<FileRec> = (0u32..)
            .zip(std::mem::take(&mut self.snap.files))
            .filter(|(i, _)| !gone.contains(i))
            .map(|(_, f)| f)
            .collect();
        files.extend(added);
        files.sort_by(|a, b| b.disk.cmp(&a.disk).then(b.logical.cmp(&a.logical)));
        self.snap.files = files;
        self.reindex();
    }

    /// «●» у каталогов `dirs` (`None` — у всех живых).
    pub(in crate::du) fn mark_changed(&mut self, dirs: Option<&[DirId]>) -> usize {
        let mut n = 0;
        let ids: Vec<DirId> = if let Some(d) = dirs {
            d.to_vec()
        } else {
            (0..self.snap.dirs.len())
                .filter_map(|k| u32::try_from(k).ok())
                .collect()
        };
        for d in ids {
            if self.dead(d) {
                continue;
            }
            if let Some(node) = self.snap.dirs.get_mut(d as usize)
                && !node.flags.has(NodeFlags::CHANGED)
            {
                node.flags.0 |= NodeFlags::CHANGED;
                n += 1;
            }
        }
        if n > 0 {
            self.recount();
        }
        n
    }

    /// Пересчитать живые каталоги с «●» (после правок).
    pub(in crate::du) fn recount(&mut self) {
        self.changed = self
            .snap
            .dirs
            .iter()
            .enumerate()
            .filter(|(k, n)| {
                n.flags.has(NodeFlags::CHANGED) && !self.dead(u32::try_from(*k).unwrap_or(u32::MAX))
            })
            .count();
    }

    /// Каталоги с «●».
    pub(in crate::du) fn changed(&self) -> Vec<DirId> {
        (0u32..)
            .zip(&self.snap.dirs)
            .filter(|(k, n)| n.flags.has(NodeFlags::CHANGED) && !self.dead(*k))
            .map(|(k, _)| k)
            .collect()
    }

    /// Путь каталога от корня анализа (имена).
    pub(in crate::du) fn rel_names(&self, mut d: DirId) -> Vec<CompactString> {
        let mut out = Vec::new();
        while d != 0 {
            let Some(n) = self.snap.dirs.get(d as usize) else {
                break;
            };
            out.push(CompactString::from(self.snap.name(d)));
            d = n.parent;
        }
        out.reverse();
        out
    }

    /// Ближайший каталог снимка по пути от корня: первый отсутствующий сегмент —
    /// его родитель (регистр не важен, как в Windows).
    pub(in crate::du) fn nearest(&self, rel: &Path) -> DirId {
        let mut cur: DirId = 0;
        for c in rel.components() {
            let std::path::Component::Normal(name) = c else {
                continue;
            };
            let name = name.to_string_lossy();
            let next = self.child(cur, &name).or_else(|| {
                self.children(cur)
                    .find(|&k| self.snap.name(k).eq_ignore_ascii_case(&name))
                    .or_else(|| {
                        let low = name.to_lowercase();
                        self.children(cur)
                            .find(|&k| self.snap.name(k).to_lowercase() == low)
                    })
            });
            match next {
                Some(k) => cur = k,
                None => break,
            }
        }
        cur
    }
}

/// Разница мелких файлов (`disk`, `files`) — по типам: подсказка одного типа — ему;
/// иначе пропорционально мелким файлам листинга по типам; иначе — «Прочее».
fn attribute(
    disk: i64,
    files: i64,
    hint: &[DuType],
    small: &[u64; DuType::COUNT],
    out: &mut [(i64, i64); DuType::COUNT],
) {
    if let Some(&t) = hint.first()
        && hint.iter().all(|&h| h == t)
    {
        out[t as usize].0 += disk;
        out[t as usize].1 += files;
        return;
    }
    let total: u64 = small.iter().sum();
    if total == 0 {
        let o = DuType::Other as usize;
        out[o].0 += disk;
        out[o].1 += files;
        return;
    }
    let mut left = disk;
    let mut top = 0;
    for (i, &s) in small.iter().enumerate() {
        if s > small[top] {
            top = i;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "доля от суммы, в пределах disk"
        )]
        let part = (disk as f64 * s as f64 / total as f64) as i64;
        out[i].0 += part;
        left -= part;
    }
    out[top].0 += left;
    out[top].1 += files;
}

impl DuRegistry {
    /// Снимок анализа для чтения вне блокировки (`None` — идёт скан или анализа нет).
    pub(in crate::du) fn frozen(&self, id: SnapId) -> Option<Arc<Frozen>> {
        match &self.slots().get(id as usize)?.src {
            Src::Frozen(f) => Some(Arc::clone(f)),
            Src::Live(_) => None,
        }
    }

    /// Снимок-фикстура (`--screens`, тесты): не наблюдать и не дочитывать.
    pub fn fix(&self, id: SnapId) {
        self.fixed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id);
    }

    /// Анализ посчитан и не фикстура: его можно наблюдать и дочитывать.
    pub fn watchable(&self, id: SnapId) -> bool {
        let fixed = self
            .fixed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&id);
        !fixed
            && self
                .slots()
                .get(id as usize)
                .is_some_and(|s| matches!(s.src, Src::Frozen(_)))
    }

    /// Настоящий путь каталога `d` анализа `id`.
    pub fn dir_path(&self, id: SnapId, d: DirId) -> Option<PathBuf> {
        let f = self.frozen(id)?;
        let root = self.root(id)?;
        Some(join_rel(&root, &f.rel_names(d)))
    }

    /// Перелистанный каталог — в снимок (см. [`Frozen::relist`]). `false` — снимка
    /// нет (идёт пересчёт): ничего не изменено.
    pub fn apply_relist(
        &self,
        id: SnapId,
        d: DirId,
        l: &Listing,
        grafts: &[Graft],
        hint: &[DuType],
    ) -> bool {
        {
            let mut slots = self.slots();
            let Some(s) = slots.get_mut(id as usize) else {
                return false;
            };
            let Src::Frozen(f) = &mut s.src else {
                return false;
            };
            let f = Arc::make_mut(f);
            if f.relist(d, l, grafts, hint) {
                // Индексы крупных файлов сменились: группы дубликатов — заново.
                s.dups = None;
            }
            // Прошлый живой листинг каталога устарел: вычитание по перечитыванию
            // (F01-06) не должно вычесть то же второй раз.
            s.seen.remove(&d);
            s.dirty = true;
            s.edits += 1;
        }
        self.bump();
        true
    }

    /// «●» у каталогов (`None` — у всех: переполнение буфера наблюдения). Возвращает,
    /// сколько отмечено впервые.
    pub fn mark_changed(&self, id: SnapId, dirs: Option<&[DirId]>) -> usize {
        let n = {
            let mut slots = self.slots();
            let Some(s) = slots.get_mut(id as usize) else {
                return 0;
            };
            let Src::Frozen(f) = &mut s.src else {
                return 0;
            };
            let n = Arc::make_mut(f).mark_changed(dirs);
            if n > 0 {
                s.dirty = true;
                s.edits += 1;
            }
            n
        };
        if n > 0 {
            self.bump();
        }
        n
    }

    /// Позиция USN-журнала, до которой снимок актуален (после дочитки журнала).
    pub fn set_usn(&self, id: SnapId, usn: crate::du::model::UsnMark) {
        let mut slots = self.slots();
        let Some(s) = slots.get_mut(id as usize) else {
            return;
        };
        if let Src::Frozen(f) = &mut s.src {
            Arc::make_mut(f).snap.usn = Some(usn);
            s.dirty = true;
        }
    }

    /// Имена подкаталогов с «●» в каталоге вкладки `loc` («По папкам»): точка у
    /// размера (`screen-ducached`). Память снимка, без I/O.
    pub fn changed_names(&self, loc: &urfin_core::Location) -> FxHashSet<CompactString> {
        let Some((id, rel, v)) = super::view(loc) else {
            return FxHashSet::default();
        };
        if v.filter != crate::du::model::DuFilter::Dirs {
            return FxHashSet::default();
        }
        let slots = self.slots();
        let Some(Src::Frozen(f)) = slots.get(id as usize).map(|s| &s.src) else {
            return FxHashSet::default();
        };
        if f.changed == 0 {
            return FxHashSet::default();
        }
        let mut d: DirId = 0;
        for name in &rel {
            match f.child(d, name) {
                Some(k) => d = k,
                None => return FxHashSet::default(),
            }
        }
        f.children(d)
            .filter(|&k| f.snap.dirs[k as usize].flags.has(NodeFlags::CHANGED))
            .map(|k| CompactString::from(f.snap.name(k)))
            .collect()
    }

    /// Каталоги с «●» анализа `id`.
    pub fn changed(&self, id: SnapId) -> Vec<DirId> {
        self.frozen(id).map(|f| f.changed()).unwrap_or_default()
    }

    /// Имена подкаталогов `d` в снимке.
    pub fn subdir_names(&self, id: SnapId, d: DirId) -> FxHashSet<CompactString> {
        self.frozen(id)
            .map(|f| {
                f.children(d)
                    .map(|k| CompactString::from(f.snap.name(k)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ближайший каталог снимка к пути `rel` от корня (см. [`Frozen::nearest`]).
    pub fn nearest(&self, id: SnapId, rel: &Path) -> Option<DirId> {
        self.frozen(id).map(|f| f.nearest(rel))
    }
}

#[cfg(test)]
#[path = "dvfs_live_tests.rs"]
mod tests;
