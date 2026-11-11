//! Датасеты S9 §1 (S9-02): B1–B6, L1–L4, M1, D1 — план [`Set::plan`], сборка в
//! `<корень>/<ID>` с манифестом ([`build`]), проверка дерева по манифесту ([`verify`]).
//!
//! Сид плана — `seed ^ hash(ID)`; `--scale` умножает число файлов или объём (B6 и M1 —
//! 0,25 по умолчанию). Содержимое — несжимаемые блоки, у B3-кода — текст. mtime —
//! 2020-01-01 плюс номер записи в секундах: `verify` ловит и правку без смены размера.

use std::io;
use std::path::{Path, PathBuf};

use crate::plan::{
    Content, Entry, GEN_VER, MANIFEST, Manifest, Plan, SPARSE_UNIT, materialize, mtime, plan, scan,
    scan_hash, str_hash, verbatim,
};
use crate::{Kind, Layout, Report, Rng, TreeSpec, WORDS};

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// Набор §1.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Set {
    /// 1 × 20 ГБ.
    B1,
    /// 100k × 1–64 КБ (лог-нормаль), глубина 6, ветвление как `node_modules`.
    B2,
    /// Смесь ≈ 10 ГБ: по числу 70% код 1–50 КБ (текст), 25% медиа 1–20 МБ, 5% 50–500 МБ.
    B3,
    /// 10k файлов, пути 300–1000 символов UTF-16, компоненты до 255.
    B4,
    /// Разреженный образ 50 ГБ, 5 ГБ данных в 100 полосах.
    B5,
    /// 1k × 100 МБ (`--scale` 0,25 по умолчанию — 250 файлов).
    B6,
    /// 1 каталог × 100k, состав S4 §7.
    L1,
    /// 1 каталог × 1M, состав S4 §7.
    L2,
    /// 1000 каталогов × 100 (навигация).
    L3,
    /// 100k файлов в 5k каталогах, ≈ 1 ГБ (размер папок).
    L4,
    /// 10k средних 64 КБ–8 МБ (`--scale` 0,25 по умолчанию).
    M1,
    /// Глубина 256 плюс «гребёнка» 64 × 64.
    D1,
}

impl Set {
    pub const ALL: [Set; 12] = [
        Set::B1,
        Set::B2,
        Set::B3,
        Set::B4,
        Set::B5,
        Set::B6,
        Set::L1,
        Set::L2,
        Set::L3,
        Set::L4,
        Set::M1,
        Set::D1,
    ];

    pub const fn tag(self) -> &'static str {
        match self {
            Set::B1 => "B1",
            Set::B2 => "B2",
            Set::B3 => "B3",
            Set::B4 => "B4",
            Set::B5 => "B5",
            Set::B6 => "B6",
            Set::L1 => "L1",
            Set::L2 => "L2",
            Set::L3 => "L3",
            Set::L4 => "L4",
            Set::M1 => "M1",
            Set::D1 => "D1",
        }
    }

    pub fn parse(s: &str) -> Option<Set> {
        Set::ALL
            .into_iter()
            .find(|x| x.tag().eq_ignore_ascii_case(s))
    }

    /// Масштаб по умолчанию: B6 и M1 — 0,25 (ноутбук), остальные — 1.
    pub const fn default_scale(self) -> f64 {
        match self {
            Set::B6 | Set::M1 => 0.25,
            _ => 1.0,
        }
    }

    /// План набора; `seed` — уже смешанный с ID (см. [`SetSpec::plan`]).
    fn plan(self, seed: u64, scale: f64) -> Plan {
        let mut r = Rng::new(seed);
        let n = |base: u64| scaled(base, scale);
        let entries = match self {
            Set::B1 => vec![Entry::file("b1.bin", n(20 * GIB), Content::Random)],
            Set::B2 => b2(&mut r, n(100_000)),
            Set::B3 => b3(&mut r, n(10 * GIB)),
            Set::B4 => b4(&mut r, n(10_000)),
            Set::B5 => {
                let size = n(50 * GIB);
                let extents = 100u32;
                let extent = n(5 * GIB)
                    .div_ceil(u64::from(extents))
                    .div_ceil(SPARSE_UNIT)
                    * SPARSE_UNIT;
                vec![Entry::file(
                    "b5.img",
                    size.max(u64::from(extents) * extent * 2),
                    Content::Sparse { extents, extent },
                )]
            }
            Set::B6 => (0..n(1000))
                .map(|i| Entry::file(format!("f{i:04}.bin"), 100 * MIB, Content::Random))
                .collect(),
            Set::L1 => tree(seed, n(100_000), Layout::Flat),
            Set::L2 => tree(seed, n(1_000_000), Layout::Flat),
            Set::L3 => {
                let dirs = n(1000);
                tree(
                    seed,
                    dirs * 100,
                    Layout::Nav {
                        dirs: as_usize(dirs),
                    },
                )
            }
            Set::L4 => l4(&mut r, n(100_000), n(5000)),
            Set::M1 => (0..n(10_000))
                .map(|i| {
                    let size = log_uniform(&mut r, 64 * KIB, 8 * MIB);
                    Entry::file(format!("m{i:05}.bin"), size, Content::Random)
                })
                .collect(),
            Set::D1 => d1(),
        };
        Plan { seed, entries }
    }
}

/// Набор, сид, масштаб.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SetSpec {
    pub set: Set,
    pub seed: u64,
    pub scale: f64,
}

impl SetSpec {
    /// Сид 1, масштаб по умолчанию.
    pub const fn new(set: Set) -> Self {
        SetSpec {
            set,
            seed: 1,
            scale: set.default_scale(),
        }
    }

    /// План: сид — `seed ^ hash(ID)`.
    pub fn plan(&self) -> Plan {
        self.set
            .plan(self.seed ^ str_hash(self.set.tag()), self.scale)
    }

    /// Каталог набора под `root`.
    pub fn dir(&self, root: &Path) -> PathBuf {
        root.join(self.set.tag())
    }

    fn matches(&self, m: &Manifest) -> bool {
        m.id == self.set.tag()
            && m.seed == self.seed
            && m.gen_ver == GEN_VER
            && (m.scale - self.scale).abs() < 1e-9
    }
}

/// Итог [`build`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Built {
    /// Манифест совпал — ничего не делали.
    Ready,
    /// Создан заново.
    Created(Report),
}

/// Собрать набор в `<root>/<ID>`. Совпадающий манифест — no-op (если не `force`).
/// Чужой или другой датасет в каталоге — ошибка, с `force` — удаляется и пересоздаётся.
/// Место проверяется до записи (для планов от 64 МБ).
pub fn build(root: &Path, spec: &SetSpec, force: bool) -> io::Result<(PathBuf, Built)> {
    let dir = spec.dir(root);
    if !force && Manifest::read(&dir).is_ok_and(|m| spec.matches(&m)) {
        return Ok((dir, Built::Ready));
    }
    let plan = spec.plan();
    let busy = std::fs::read_dir(&dir).is_ok_and(|mut rd| rd.next().is_some());
    if busy && !force {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{}: в каталоге другой датасет или чужие файлы — --force пересоздаст",
                dir.display()
            ),
        ));
    }
    let need = plan.disk_bytes();
    if need >= 64 * MIB {
        std::fs::create_dir_all(root)?;
        if let Ok(vol) = crate::sys::volume(root) {
            let freed = if busy { dir_bytes(&dir) } else { 0 };
            if vol.free + freed < need + 256 * MIB {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    format!(
                        "{}: нужно {} МиБ, свободно {} МиБ",
                        root.display(),
                        need / MIB,
                        (vol.free + freed) / MIB
                    ),
                ));
            }
        }
    }
    if busy {
        std::fs::remove_dir_all(verbatim(&dir)?)?;
    }
    let report = materialize(&plan, &dir)?;
    Manifest {
        id: spec.set.tag().to_owned(),
        seed: spec.seed,
        gen_ver: GEN_VER,
        scale: spec.scale,
        files: report.files as u64,
        dirs: report.dirs as u64,
        bytes: report.bytes,
        tree_hash: plan.tree_hash_without(&report.skipped),
    }
    .write(&dir)?;
    Ok((dir, Built::Created(report)))
}

/// Перепроверить `<root>/<ID>`: манифест, каждая запись плана (удалена, изменена —
/// размер или mtime), лишние записи, `tree_hash`. Пустой список — дерево цело.
pub fn verify(root: &Path, spec: &SetSpec) -> io::Result<Vec<String>> {
    let dir = spec.dir(root);
    let m = match Manifest::read(&dir) {
        Ok(m) => m,
        Err(e) => return Ok(vec![format!("{}: {MANIFEST}: {e}", dir.display())]),
    };
    let mut out = Vec::new();
    if !spec.matches(&m) {
        out.push(format!(
            "манифест другого датасета: {} seed={} gen_ver={} scale={}",
            m.id, m.seed, m.gen_ver, m.scale
        ));
        return Ok(out);
    }
    let plan = spec.plan();
    let disk = scan(&dir)?;
    let mut on_disk: std::collections::HashMap<&str, &crate::Scanned> =
        disk.iter().map(|s| (s.path.as_str(), s)).collect();
    for (i, e) in plan.entries.iter().enumerate() {
        let got = on_disk.remove(e.path.as_str());
        match (&e.kind, got) {
            (Kind::Link { .. }, _) => {}
            (_, None) => out.push(format!("удалён: {}", e.path)),
            (Kind::Dir, Some(s)) if s.kind != 'd' => out.push(format!("изменён: {}", e.path)),
            (Kind::File { size, .. }, Some(s))
                if s.kind != 'f' || s.size != *size || s.mtime != Some(mtime(i)) =>
            {
                out.push(format!("изменён: {}", e.path));
            }
            _ => {}
        }
    }
    let mut extra: Vec<&str> = on_disk.keys().copied().collect();
    extra.sort_unstable();
    out.extend(extra.into_iter().map(|p| format!("лишний: {p}")));
    let hash = scan_hash(&disk);
    if hash != m.tree_hash {
        out.push(format!(
            "tree_hash: на диске {hash:#018x}, в манифесте {:#018x}",
            m.tree_hash
        ));
    }
    Ok(out)
}

fn dir_bytes(dir: &Path) -> u64 {
    scan(dir).map_or(0, |v| v.iter().map(|s| s.size).sum())
}

fn as_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// `base × scale`, не меньше 1.
fn scaled(base: u64, scale: f64) -> u64 {
    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
    let v = (base as f64 * scale).round().max(1.0) as u64;
    v
}

/// Равномерное `[0, 1)`.
fn unit(r: &mut Rng) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let v = (r.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    v
}

/// Лог-нормаль с медианой `median` и σ, обрезанная до `[lo, hi]`.
fn log_normal(r: &mut Rng, median: f64, sigma: f64, lo: u64, hi: u64) -> u64 {
    let (u1, u2) = (unit(r).max(f64::MIN_POSITIVE), unit(r));
    let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
    #[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let v = (median.ln() + sigma * z).exp() as u64;
    v.clamp(lo, hi)
}

/// Лог-равномерное `[lo, hi]`.
fn log_uniform(r: &mut Rng, lo: u64, hi: u64) -> u64 {
    #[allow(clippy::cast_precision_loss)]
    let (a, b) = ((lo as f64).ln(), (hi as f64).ln());
    #[allow(clippy::cast_sign_loss)]
    let v = (a + (b - a) * unit(r)).exp() as u64;
    v.clamp(lo, hi)
}

fn word(r: &mut Rng) -> &'static str {
    WORDS[as_usize(r.below(WORDS.len() as u64))]
}

fn tree(seed: u64, n: u64, layout: Layout) -> Vec<Entry> {
    plan(&TreeSpec {
        n: as_usize(n),
        seed,
        layout,
    })
    .entries
}

/// B2: пакеты по ≈ 12 файлов; у трети пакетов — свой `node_modules` (до глубины 6).
fn b2(r: &mut Rng, n: u64) -> Vec<Entry> {
    const EXTS: [&str; 5] = ["js", "js", "json", "ts", "map"];
    let mut out = vec![Entry::dir("node_modules")];
    let mut nm: Vec<(String, u32)> = vec![("node_modules".into(), 1)];
    let mut pkg = String::new();
    for i in 0..n {
        if i % 12 == 0 {
            let (parent, depth) = nm[as_usize(r.below(nm.len() as u64))].clone();
            pkg = format!("{parent}/{}-{i}", word(r));
            out.push(Entry::dir(pkg.clone()));
            if depth < 6 && r.below(3) == 0 {
                let sub = format!("{pkg}/node_modules");
                out.push(Entry::dir(sub.clone()));
                nm.push((sub, depth + 1));
            }
        }
        let ext = EXTS[as_usize(r.below(EXTS.len() as u64))];
        let size = log_normal(r, 8192.0, 1.0, KIB, 64 * KIB);
        out.push(Entry::file(
            format!("{pkg}/{}{i}.{ext}", word(r)),
            size,
            Content::Random,
        ));
    }
    out
}

/// B3: классы по числу 70/25/5, пока объём не достигнет `volume`.
fn b3(r: &mut Rng, volume: u64) -> Vec<Entry> {
    let mut out = vec![Entry::dir("src"), Entry::dir("media"), Entry::dir("big")];
    let (mut bytes, mut code) = (0u64, 0u64);
    let mut i = 0u64;
    while bytes < volume {
        let roll = r.below(100);
        let e = if roll < 70 {
            if code.is_multiple_of(200) {
                out.push(Entry::dir(format!("src/m{:03}", code / 200)));
            }
            let size = KIB + r.below(49 * KIB + 1);
            let e = Entry::file(
                format!("src/m{:03}/{}{i}.rs", code / 200, word(r)),
                size,
                Content::Text,
            );
            code += 1;
            e
        } else if roll < 95 {
            let size = MIB + r.below(19 * MIB + 1);
            Entry::file(format!("media/{}{i}.jpg", word(r)), size, Content::Random)
        } else {
            let size = 50 * MIB + r.below(450 * MIB + 1);
            Entry::file(format!("big/{}{i}.mkv", word(r)), size, Content::Random)
        };
        bytes += e.size();
        out.push(e);
        i += 1;
    }
    out
}

/// B4: группы по 100 файлов в цепочке каталогов; путь от корня — 300–1000 символов
/// UTF-16, компонент — до 255.
fn b4(r: &mut Rng, n: u64) -> Vec<Entry> {
    const ABC: &str = "abcdefghijklmnopqrstuvwxyz0123456789_абвгдежзийклмнопрстуфхцчшщэюя";
    let abc: Vec<char> = ABC.chars().collect();
    let text = |r: &mut Rng, len: usize| -> String {
        (0..len)
            .map(|_| abc[as_usize(r.below(abc.len() as u64))])
            .collect()
    };
    let mut out = Vec::new();
    for g in 0..n.div_ceil(100) {
        let mut chain = format!("g{g:03}");
        out.push(Entry::dir(chain.clone()));
        let target = 250 + as_usize(r.below(351));
        let mut k = 0;
        while chain.chars().count() < target {
            let len = 40 + as_usize(r.below(216));
            chain = format!("{chain}/{k:02}_{}", text(r, len - 3));
            out.push(Entry::dir(chain.clone()));
            k += 1;
        }
        let have = chain.chars().count();
        for i in (g * 100)..((g + 1) * 100).min(n) {
            let want = 300 + as_usize(r.below(701));
            let len = want
                .saturating_sub(have + 1)
                .clamp(20, 255.min(1000 - have - 1));
            let stem = format!("{i:05}_");
            let fill = text(r, len - stem.len() - 4);
            let size = 8 * KIB + r.below(4 * KIB + 1);
            out.push(Entry::file(
                format!("{chain}/{stem}{fill}.txt"),
                size,
                Content::Random,
            ));
        }
    }
    out
}

/// L4: `dirs` каталогов (50 верхних, остальные — внутри), `n` файлов 0–20 КБ.
fn l4(r: &mut Rng, n: u64, dirs: u64) -> Vec<Entry> {
    let top = dirs.clamp(1, 50);
    let mut paths: Vec<String> = (0..top).map(|t| format!("t{t:02}")).collect();
    for k in 0..dirs.saturating_sub(top) {
        paths.push(format!("t{:02}/s{k:04}", k % top));
    }
    let mut out: Vec<Entry> = paths.iter().map(Entry::dir).collect();
    for i in 0..n {
        let d = &paths[as_usize(r.below(paths.len() as u64))];
        let size = r.below(20 * KIB + 1);
        out.push(Entry::file(
            format!("{d}/{}{i}.dat", word(r)),
            size,
            Content::Random,
        ));
    }
    out
}

/// D1: цепочка `d000/…/d255` (файл каждые 32 уровня и на дне) и гребёнка
/// `comb/t00…t63`, в каждом зубце — цепочка `c00/…/c63` с файлом на дне.
fn d1() -> Vec<Entry> {
    let mut out = Vec::new();
    let mut p = String::new();
    for k in 0..256 {
        p = if p.is_empty() {
            format!("d{k:03}")
        } else {
            format!("{p}/d{k:03}")
        };
        out.push(Entry::dir(p.clone()));
        if k % 32 == 31 {
            out.push(Entry::file(
                format!("{p}/at{k:03}.txt"),
                100,
                Content::Random,
            ));
        }
    }
    out.push(Entry::file(format!("{p}/leaf.txt"), 100, Content::Random));
    out.push(Entry::dir("comb"));
    for t in 0..64 {
        let mut p = format!("comb/t{t:02}");
        out.push(Entry::dir(p.clone()));
        for c in 0..64 {
            p = format!("{p}/c{c:02}");
            out.push(Entry::dir(p.clone()));
        }
        out.push(Entry::file(format!("{p}/leaf.txt"), 100, Content::Random));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("urfin-sets-")
            .tempdir()
            .unwrap()
    }

    fn small(set: Set, scale: f64) -> SetSpec {
        SetSpec {
            set,
            seed: 1,
            scale,
        }
    }

    /// Планы всех наборов (малый масштаб): родитель раньше потомков, нет совпадений без
    /// учёта регистра, тот же сид — тот же хэш.
    #[test]
    fn plans_are_well_formed() {
        for set in Set::ALL {
            let spec = small(set, 0.01);
            let p = spec.plan();
            assert_eq!(p, spec.plan(), "{set:?}");
            assert_eq!(p.case_collisions(), vec![], "{set:?}");
            let mut dirs = std::collections::HashSet::new();
            for e in &p.entries {
                if let Some((parent, _)) = e.path.rsplit_once('/') {
                    assert!(dirs.contains(parent), "{set:?}: {} раньше родителя", e.path);
                }
                if matches!(e.kind, Kind::Dir) {
                    dirs.insert(e.path.as_str());
                }
            }
            assert!(p.files() > 0, "{set:?}");
        }
        assert_ne!(
            small(Set::B2, 0.01).plan().tree_hash(),
            SetSpec {
                seed: 2,
                ..small(Set::B2, 0.01)
            }
            .plan()
            .tree_hash()
        );
    }

    /// Состав по §1 на полном масштабе (только план, без диска).
    #[test]
    fn full_scale_composition() {
        let b4 = SetSpec::new(Set::B4).plan();
        assert_eq!(b4.files(), 10_000);
        for e in &b4.entries {
            assert!(e.path.split('/').all(|c| c.encode_utf16().count() <= 255));
            if matches!(e.kind, Kind::File { .. }) {
                let len = e.path.encode_utf16().count();
                assert!((300..=1000).contains(&len), "{len}");
            }
        }
        let b6 = SetSpec::new(Set::B6).plan();
        assert_eq!(b6.files(), 250);
        let b2 = SetSpec::new(Set::B2).plan();
        assert_eq!(b2.files(), 100_000);
        assert!(b2.entries.iter().all(|e| e.size() <= 64 * KIB));
        let depth = b2
            .entries
            .iter()
            .map(|e| e.path.matches("node_modules").count())
            .max();
        assert_eq!(depth, Some(6));
        let b3 = SetSpec::new(Set::B3).plan();
        assert!((10 * GIB..11 * GIB).contains(&b3.bytes()), "{}", b3.bytes());
        let l4 = SetSpec::new(Set::L4).plan();
        assert_eq!((l4.files(), l4.dirs()), (100_000, 5000));
        let d1 = SetSpec::new(Set::D1).plan();
        let deepest = d1.entries.iter().map(|e| e.path.split('/').count()).max();
        assert_eq!(deepest, Some(257));
    }

    /// Повторный запуск при совпадающем манифесте — no-op быстрее секунды.
    #[test]
    fn rebuild_is_noop() {
        let root = tmp();
        let spec = small(Set::L1, 0.01);
        let (dir, built) = build(root.path(), &spec, false).unwrap();
        assert!(matches!(built, Built::Created(_)));
        assert!(dir.join(MANIFEST).is_file());
        let t = Instant::now();
        let (_, again) = build(root.path(), &spec, false).unwrap();
        assert_eq!(again, Built::Ready);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert_eq!(verify(root.path(), &spec).unwrap(), Vec::<String>::new());
        // Другой масштаб — каталог занят: без --force ошибка, с --force пересоздаётся.
        let other = small(Set::L1, 0.02);
        assert!(build(root.path(), &other, false).is_err());
        let (_, b) = build(root.path(), &other, true).unwrap();
        assert!(matches!(b, Built::Created(_)));
        assert_eq!(verify(root.path(), &other).unwrap(), Vec::<String>::new());
    }

    /// `--verify` ловит удалённый файл и правку без смены размера.
    #[test]
    fn verify_catches_deleted_and_changed() {
        let root = tmp();
        let spec = small(Set::B2, 0.002);
        let (dir, _) = build(root.path(), &spec, false).unwrap();
        assert_eq!(verify(root.path(), &spec).unwrap(), Vec::<String>::new());
        let p = spec.plan();
        let files: Vec<&Entry> = p
            .entries
            .iter()
            .filter(|e| matches!(e.kind, Kind::File { .. }))
            .collect();
        std::fs::remove_file(crate::join(&dir, &files[0].path)).unwrap();
        let path = crate::join(&dir, &files[1].path);
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::write(&path, vec![0u8; as_usize(len)]).unwrap();
        let problems = verify(root.path(), &spec).unwrap();
        assert!(
            problems.contains(&format!("удалён: {}", files[0].path)),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!("изменён: {}", files[1].path)),
            "{problems:?}"
        );
        std::fs::write(dir.join("junk.txt"), b"x").unwrap();
        let problems = verify(root.path(), &spec).unwrap();
        assert!(
            problems.contains(&"лишний: junk.txt".to_owned()),
            "{problems:?}"
        );
    }

    /// B5 на NTFS занимает объём данных, а не логический размер.
    #[test]
    fn b5_sparse_takes_data_only() {
        let root = tmp();
        let spec = small(Set::B5, 0.002);
        let (dir, _) = build(root.path(), &spec, false).unwrap();
        let Kind::File {
            size,
            content: Content::Sparse { extents, extent },
        } = spec.plan().entries[0].kind
        else {
            panic!("B5 — разреженный файл");
        };
        let data = u64::from(extents) * extent;
        let path = dir.join("b5.img");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        let alloc = crate::allocated(&path).unwrap();
        assert!(
            alloc >= data && alloc <= data + data / 10,
            "{alloc} vs {data}"
        );
        assert!(size >= 8 * data);
        assert_eq!(verify(root.path(), &spec).unwrap(), Vec::<String>::new());
    }

    /// D1: глубина 256 (пути за `MAX_PATH`) создаётся и проверяется.
    #[test]
    fn d1_deep_tree() {
        let root = tmp();
        let spec = SetSpec::new(Set::D1);
        let (_, built) = build(root.path(), &spec, false).unwrap();
        let Built::Created(r) = built else {
            panic!("создан")
        };
        assert_eq!(r.dirs, 256 + 1 + 64 * 65);
        assert_eq!(verify(root.path(), &spec).unwrap(), Vec::<String>::new());
    }
}
