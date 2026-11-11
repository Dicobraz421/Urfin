//! Urfin — генераторы тестовых данных. Только `[dev-dependencies]`, в граф `urfin-app` не входит.
//!
//! [`gen_tree`] (S4 §7, S4-11; по S0 v5 №13 живёт здесь): дерево для бенчей листинга.
//! Состав: пустые и мелкие файлы, имена «слово + число» (natural-кейсы), 10% длиннее
//! 24 байт, 3% Unicode (кириллица, эмодзи), 2% скрытых, 1% каталогов, 0,5% ссылок.
//! Детерминирован по `seed`: одинаковые аргументы — одинаковое дерево. Внутри —
//! [`plan()`] (дерево без диска) и [`materialize`] (запись), [`tree_hash`] — хэш плана.
//!
//! Датасеты (H-01, гигиена диска): [`dataset`] кладёт дерево в
//! `%LOCALAPPDATA%\Pets\testdata\urfin\<имя>` (не в `%TEMP%`) и переиспользует его
//! при том же сиде, размере и раскладке; [`clean`] удаляет все датасеты приложения
//! (`cargo run -p urfin-testdata --example testdata -- clean`).
//!
//! Наборы S9 §1 (S9-01…03): B1–B6, L1–L4, M1, D1 — [`Set`], [`build`] в
//! `<--root>\<ID>` с манифестом [`Manifest`] (`.urfin-dataset.json`), [`verify`];
//! U1 — [`gen_u1`] по эталону `fixtures/u1.expect.json`, сверка —
//! [`assert_listing_matches`]. Запуск —
//! `cargo run -p urfin-testdata --example testdata --release -- b1…d1|all|u1 --root <каталог>`.
//!
//! Файлы вьювера и редактора (S7 §9, S7-00a): V1–V4 — [`dataset_file`] (файл
//! `v1-10g-s1.log` и т. п. там же, метка рядом), V5 — корпус кодировок [`V5`] в
//! [`v5_dir`], V6 — растущий лог [`grow_log`]. Запуск —
//! `cargo run -p urfin-testdata --example testdata --release -- v1|v2|v3|v4|v5|v6|all`.

mod files;
mod plan;
mod sets;
mod sys;
#[cfg(windows)]
mod u1;
mod v5;

pub use files::{
    FileSet, FileSpec, allocated, dataset_file, edge_hash, gen_file, grow_log, parse_size,
};
pub use plan::{
    Content, Entry, GEN_VER, Kind, MANIFEST, Manifest, Plan, SPARSE_UNIT, Scanned, join,
    materialize, mtime, plan, scan, scan_hash, tree_hash, verbatim,
};
pub use sets::{Built, Set, SetSpec, build, verify};
#[cfg(windows)]
pub use u1::{
    Fs, Listed, ListedKind, Listing, U1Expect, U1Report, assert_listing_matches, gen_u1,
    listing_mismatches, u1_expect,
};
pub use v5::{V5, V5Case, V5Eol};

use std::io;
use std::path::{Path, PathBuf};

/// Каталог корпуса V5 в репозитории (`crates/testdata/fixtures/v5`).
pub fn v5_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("v5")
}

/// Раскладка дерева.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Один каталог на `n` записей (кейсы 1 × 100k, 1 × 1M).
    Flat,
    /// `dirs` каталогов по `n / dirs` записей (навигация: 1000 × 100).
    Nav { dirs: usize },
}

#[derive(Copy, Clone, Debug)]
pub struct TreeSpec {
    pub n: usize,
    pub seed: u64,
    pub layout: Layout,
}

/// Что создано.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub files: usize,
    pub dirs: usize,
    pub hidden: usize,
    pub links: usize,
    /// Ссылки, которые ОС не дала создать (Windows без режима разработчика).
    pub links_skipped: usize,
    pub bytes: u64,
    /// Пути ссылок из плана, которые не создались (в `tree_hash` манифеста их нет).
    pub skipped: Vec<String>,
}

/// Детерминированный генератор (SplitMix64): без внешних зависимостей.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

pub(crate) const WORDS: [&str; 24] = [
    "report", "file", "photo", "invoice", "backup", "notes", "draft", "image", "data", "log",
    "build", "main", "test", "readme", "config", "index", "summary", "archive", "scan", "budget",
    "plan", "video", "track", "chapter",
];
const WORDS_RU: [&str; 8] = [
    "отчёт",
    "договор",
    "фото",
    "счёт",
    "заметки",
    "черновик",
    "архив",
    "смета",
];
const EMOJI: [&str; 4] = ["📁", "🎉", "✅", "🚀"];
const EXTS: [&str; 10] = [
    "txt", "md", "rs", "jpg", "png", "pdf", "docx", "zip", "log", "json",
];

/// Имя записи `i`: уникально (содержит `i`), с числом для natural-сортировки.
pub fn name(rng: &mut Rng, i: usize, dir: bool) -> String {
    let pick = |rng: &mut Rng, list: &[&str]| -> String {
        list[usize::try_from(rng.below(list.len() as u64)).unwrap_or(0)].to_owned()
    };
    let roll = rng.below(1000);
    let mut stem = if roll < 30 {
        // 3% Unicode.
        if roll < 22 {
            pick(rng, &WORDS_RU)
        } else {
            format!("{}{}", pick(rng, &EMOJI), pick(rng, &WORDS))
        }
    } else {
        pick(rng, &WORDS)
    };
    if rng.below(10) == 0 {
        // 10% длиннее 24 байт.
        for _ in 0..3 {
            stem.push('_');
            stem.push_str(&pick(rng, &WORDS));
        }
    }
    let num = i;
    if dir {
        format!("{stem} {num}")
    } else {
        format!("{stem}{num}.{}", pick(rng, &EXTS))
    }
}

/// Создать дерево в `root` (каталог создаётся; существующие файлы перезаписываются):
/// [`materialize`] плана [`plan`].
pub fn gen_tree(root: &Path, spec: &TreeSpec) -> io::Result<Report> {
    materialize(&plan(spec), root)
}

/// Корень датасетов приложения `app`: Windows — `%LOCALAPPDATA%\Pets\testdata\<app>`,
/// иначе `$XDG_CACHE_HOME/pets/testdata/<app>` (или `~/.cache/pets/…`).
pub fn datasets_root(app: &str) -> PathBuf {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("LOCALAPPDATA").map(|p| p.join("Pets"))
    } else {
        var("XDG_CACHE_HOME")
            .map(|p| p.join("pets"))
            .or_else(|| var("HOME").map(|h| h.join(".cache").join("pets")))
    };
    base.unwrap_or_else(|| std::env::temp_dir().join("pets"))
        .join("testdata")
        .join(app)
}

/// Дерево [`gen_tree`] как датасет `name` приложения `app`: готовый каталог, если он
/// собран с теми же `n`, `seed` и раскладкой, иначе пересоздаётся.
pub fn dataset(app: &str, name: &str, spec: &TreeSpec) -> io::Result<PathBuf> {
    dataset_with(app, name, &signature(spec), |dir| {
        gen_tree(dir, spec).map(|_| ())
    })
}

/// Датасет со своим наполнением: `build` заполняет пустой каталог, `signature`
/// описывает содержимое (сид, размер, версию) — при совпадении каталог берётся как есть.
pub fn dataset_with(
    app: &str,
    name: &str,
    signature: &str,
    build: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    dataset_in(&datasets_root(app), name, signature, build)
}

/// Метка датасета — рядом с каталогом (`<имя>.pets-dataset`), а не внутри: листинг
/// каталога не видит лишней записи.
pub(crate) fn marker(root: &Path, name: &str) -> PathBuf {
    root.join(format!("{name}.pets-dataset"))
}

fn dataset_in(
    root: &Path,
    name: &str,
    signature: &str,
    build: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let dir = root.join(name);
    let mark = marker(root, name);
    if dir.is_dir() && std::fs::read_to_string(&mark).is_ok_and(|s| s == signature) {
        return Ok(dir);
    }
    let _ = std::fs::remove_file(&mark);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    build(&dir)?;
    std::fs::write(&mark, signature)?;
    Ok(dir)
}

fn signature(spec: &TreeSpec) -> String {
    format!(
        "gen_tree v{GEN_VER} n={} seed={} layout={:?}",
        spec.n, spec.seed, spec.layout
    )
}

/// Удалить все датасеты приложения; возвращает освобождённые байты.
pub fn clean(app: &str) -> io::Result<u64> {
    let root = datasets_root(app);
    if !root.exists() {
        return Ok(0);
    }
    let bytes = size_of_dir(&root);
    std::fs::remove_dir_all(&root)?;
    Ok(bytes)
}

fn size_of_dir(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => size_of_dir(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

/// Версия крейта из `Cargo.toml`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V5: все файлы на месте; UTF-8 — байт в байт (BOM + текст); у однобайтовых
    /// кодировок байт столько же, сколько символов; окончания строк — как заявлено.
    #[test]
    fn v5_corpus() {
        for c in V5 {
            let b = std::fs::read(v5_dir().join(c.file)).unwrap();
            let body = if c.bom {
                assert_eq!(&b[..3], b"\xEF\xBB\xBF", "{}", c.file);
                &b[3..]
            } else {
                &b[..]
            };
            match c.encoding {
                "UTF-8" => assert_eq!(body, c.text.as_bytes(), "{}", c.file),
                "Shift_JIS" => assert!(body.len() < c.text.len(), "{}", c.file),
                _ => assert_eq!(body.len(), c.text.chars().count(), "{}", c.file),
            }
            let crlf = c.text.matches("\r\n").count();
            let (lf, cr) = (c.text.matches('\n').count(), c.text.matches('\r').count());
            let got = match (crlf, lf - crlf, cr - crlf) {
                (0, _, 0) => V5Eol::Lf,
                (_, 0, 0) => V5Eol::CrLf,
                (0, 0, _) => V5Eol::Cr,
                _ => V5Eol::Mixed,
            };
            assert_eq!(got, c.eol, "{}", c.file);
        }
        let n = std::fs::read_dir(v5_dir()).unwrap().count();
        assert_eq!(n, V5.len());
    }

    #[test]
    fn version_smoke() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert_ne!(super::version(), "");
    }

    /// Детерминирован и даёт заявленный состав (на 2000 записей — с допуском).
    #[test]
    fn composition() {
        let dir = tempfile::Builder::new()
            .prefix("urfin-gen-tree-")
            .tempdir()
            .unwrap();
        let tmp = dir.path();
        let spec = TreeSpec {
            n: 2000,
            seed: 1,
            layout: Layout::Flat,
        };
        let r = gen_tree(tmp, &spec).unwrap();
        assert_eq!(r.files + r.dirs + r.links + r.links_skipped, 2000);
        assert!((8..=40).contains(&r.dirs), "{r:?}");
        assert!(r.hidden > 10 && r.hidden < 90, "{r:?}");
        let names: Vec<String> = std::fs::read_dir(tmp)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.len() > 24));
        assert!(names.iter().any(|n| !n.is_ascii()));
        // Те же аргументы — те же имена.
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(name(&mut a, 5, false), name(&mut b, 5, false));
    }

    /// Та же подпись — каталог переиспользуется; другая — пересоздаётся; метка — снаружи.
    #[test]
    fn dataset_is_reused_by_signature() {
        let root = tempfile::Builder::new()
            .prefix("urfin-datasets-")
            .tempdir()
            .unwrap();
        let mut builds = 0;
        let mut make = |sig: &str| {
            dataset_in(root.path(), "d", sig, |d| {
                builds += 1;
                std::fs::write(d.join("f"), b"x")
            })
            .unwrap()
        };
        let a = make("v1");
        let b = make("v1");
        assert_eq!(a, b);
        make("v2");
        assert_eq!(builds, 2);
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 1, "marker outside");
        assert!(marker(root.path(), "d").exists());
    }
}
