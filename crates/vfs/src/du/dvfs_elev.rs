//! Каталоги без доступа и досчёт с правами (F01-12, ADR-0021): какие пути досчитывать,
//! сколько недоступных под каталогом вкладки, итоги досчёта — в снимок. Под блокировкой
//! реестра, без I/O.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use urfin_core::{JobId, Location};

use super::{Banner, DuRegistry, Frozen, SnapId, Src, join_rel, parse};
use crate::du::elevated::{self, RootSum};
use crate::du::model::{DirId, DuType, NodeFlags, SizeMode};

impl Frozen {
    /// Каталог недоступен сейчас: `DENIED` и не удалён после скана.
    fn denied_now(&self, d: DirId) -> bool {
        self.snap
            .dirs
            .get(d as usize)
            .is_some_and(|n| n.flags.has(NodeFlags::DENIED))
            && !self.dead(d)
    }

    /// `d` — `dir` или внутри него.
    fn under(&self, mut d: DirId, dir: DirId) -> bool {
        loop {
            if d == dir {
                return true;
            }
            match self.snap.dirs.get(d as usize) {
                Some(n) if d != 0 => d = n.parent,
                _ => return false,
            }
        }
    }

    /// Каталог по точному пути от корня (`None` — нет такого).
    fn exact(&self, rel: &Path) -> Option<DirId> {
        let mut cur: DirId = 0;
        for c in rel.components() {
            let Component::Normal(name) = c else {
                return None;
            };
            cur = self.child(cur, name.to_str()?)?;
        }
        Some(cur)
    }

    /// `PARTIAL_DENIED` заново: у предков недоступных каталогов и у досчитанных с
    /// правами, внутри которых и с правами что-то не прочиталось (и их предков).
    fn repartial(&mut self, inner: &[DirId]) {
        let mut seeds: Vec<DirId> = (0u32..)
            .zip(&self.snap.dirs)
            .filter(|(_, n)| n.flags.has(NodeFlags::DENIED))
            .map(|(k, _)| k)
            .filter(|&k| !self.dead(k))
            .collect();
        seeds.extend(inner.iter().copied());
        for n in &mut self.snap.dirs {
            n.flags.0 &= !NodeFlags::PARTIAL_DENIED;
        }
        for d in seeds {
            let mut cur = if inner.contains(&d) {
                d
            } else {
                self.snap
                    .dirs
                    .get(d as usize)
                    .map_or(u32::MAX, |n| n.parent)
            };
            while let Some(n) = self.snap.dirs.get_mut(cur as usize) {
                n.flags.0 |= NodeFlags::PARTIAL_DENIED;
                if cur == 0 {
                    break;
                }
                cur = n.parent;
            }
        }
    }
}

impl DuRegistry {
    /// Недоступные каталоги анализа для досчёта — настоящие пути на дисках с буквой;
    /// не больше [`elevated::MAX_ROOTS`] и длины командной строки (остальные — следующим
    /// запуском). Во время скана — пусто.
    pub fn denied_paths(&self, id: SnapId) -> Vec<PathBuf> {
        let (Some(f), Some(root)) = (self.frozen(id), self.root(id)) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = Vec::new();
        for &d in &f.snap.denied {
            if !f.denied_now(d) {
                continue;
            }
            let p = join_rel(&root, &f.rel_names(d));
            if !p.to_str().is_some_and(elevated::drive_path) {
                continue;
            }
            if !elevated::fits(&out, &p) {
                break;
            }
            out.push(p);
        }
        out
    }

    /// Недоступных каталогов в каталоге вкладки `loc` и глубже (строка «нет доступа: N
    /// папок»); во время скана — 0.
    pub fn denied_under(&self, loc: &Location) -> usize {
        let Some((id, rel)) = parse(loc) else {
            return 0;
        };
        let slots = self.slots();
        let Some(s) = slots.get(id as usize) else {
            return 0;
        };
        let Src::Frozen(f) = &s.src else {
            return 0;
        };
        let Some(dir) = s.src.resolve(&rel) else {
            return 0;
        };
        f.snap
            .denied
            .iter()
            .filter(|&&d| f.denied_now(d) && f.under(d, dir))
            .count()
    }

    /// Задача досчёта с правами анализа `id`, пока идёт.
    pub fn elev_job(&self, id: SnapId) -> Option<JobId> {
        self.slots().get(id as usize).and_then(|s| s.elev_job)
    }

    pub fn set_elev_job(&self, id: SnapId, job: Option<JobId>) {
        if let Some(s) = self.slots().get_mut(id as usize) {
            s.elev_job = job;
        }
        self.bump();
    }

    /// Закрыть баннер «N папок без доступа» анализа (до пересчёта). `false` — нечего.
    pub fn close_banner(&self, id: SnapId) -> bool {
        let closed = {
            let mut slots = self.slots();
            let Some(s) = slots.get_mut(id as usize) else {
                return false;
            };
            let was = s.banner;
            s.banner = Banner::Closed;
            was == Banner::Shown
        };
        if closed {
            self.bump();
        }
        closed
    }

    /// Итоги досчёта — в снимок: недоступный каталог получает размер, число файлов и
    /// суммы по типам поддерева (с предками), флаг `ELEVATED` и уходит из «нет доступа»;
    /// не прочитанное и с правами — `PARTIAL_DENIED`. Корни — уже проверенные
    /// [`elevated::decode`]; `ok == false` — без изменений. Ответ — (каталогов, байт в
    /// режиме размера снимка).
    pub fn merge_elevated(&self, id: SnapId, sums: &[RootSum]) -> (usize, u64) {
        let Some(root) = self.root(id) else {
            return (0, 0);
        };
        let mut slots = self.slots();
        let Some(s) = slots.get_mut(id as usize) else {
            return (0, 0);
        };
        let Src::Frozen(f) = &mut s.src else {
            return (0, 0);
        };
        let f = Arc::make_mut(f);
        let mode = f.snap.size_mode;
        let (mut n, mut bytes) = (0usize, 0u64);
        let mut inner = Vec::new();
        for sum in sums.iter().filter(|s| s.ok) {
            let Ok(rel) = Path::new(&sum.path).strip_prefix(&root) else {
                continue;
            };
            let Some(d) = f.exact(rel).filter(|&d| d != 0 && f.denied_now(d)) else {
                continue;
            };
            let signed = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
            f.add(d, signed(sum.disk), signed(sum.logical), signed(sum.files));
            let build = f.snap.in_build(d);
            for t in 0..DuType::COUNT {
                f.snap.types.disk[t] += sum.types_disk[t];
                f.snap.types.files[t] += sum.types_files[t];
                if build {
                    f.snap.build.disk[t] += sum.types_disk[t];
                    f.snap.build.files[t] += sum.types_files[t];
                }
            }
            if let Some(node) = f.snap.dirs.get_mut(d as usize) {
                node.flags.0 =
                    (node.flags.0 & !NodeFlags::DENIED) | NodeFlags::ELEVATED | NodeFlags::DONE;
            }
            if sum.denied > 0 {
                inner.push(d);
            }
            n += 1;
            bytes += match mode {
                SizeMode::Disk => sum.disk,
                SizeMode::Logical => sum.logical,
            };
        }
        if n == 0 {
            return (0, 0);
        }
        let dirs = &f.snap.dirs;
        f.snap.denied.retain(|&d| {
            dirs.get(d as usize)
                .is_some_and(|x| x.flags.has(NodeFlags::DENIED))
        });
        f.repartial(&inner);
        s.dirty = true;
        s.edits += 1;
        drop(slots);
        self.bump();
        (n, bytes)
    }
}

#[cfg(test)]
#[path = "dvfs_elev_tests.rs"]
mod tests;
