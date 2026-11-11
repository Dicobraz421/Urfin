//! План дерева и манифест датасета (S9-01, S9 §1).
//!
//! [`plan`] раскладывает [`TreeSpec`] в список записей без обращения к диску,
//! [`materialize`] пишет план в каталог (каталоги — по порядку, файлы — параллельно,
//! ссылки — последними). [`tree_hash`] — свой 64-битный хэш (FNV-1a) по путям, видам и
//! размерам: одинаковый план — одинаковый хэш, тот же хэш даёт [`scan`] по диску.
//!
//! Манифест `<корень>/.urfin-dataset.json` ([`Manifest`]) пишется руками, без serde;
//! читается своим мини-разбором JSON ([`json`], он же — для эталона U1).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use crate::{Layout, Report, Rng, TreeSpec, name};

/// Версия генераторов планов: смена алгоритма — новая версия (манифест не совпадёт).
pub const GEN_VER: u32 = 1;

/// Имя манифеста в корне датасета.
pub const MANIFEST: &str = ".urfin-dataset.json";

/// 2020-01-01T00:00:00Z: mtime записи `i` — эта дата плюс `i` секунд (S9 §1).
const MTIME_BASE: u64 = 1_577_836_800;

/// Единица выделения разреженного файла NTFS (16 кластеров по 4 КБ).
pub const SPARSE_UNIT: u64 = 64 << 10;

/// Содержимое файла.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// Байты `x` (дерево листинга S4 §7).
    Fill,
    /// Несжимаемые псевдослучайные блоки (сид — от сида плана и пути).
    Random,
    /// Текст «кода»: сжимаемый (B3).
    Text,
    /// Разреженный файл: `extents` полос данных по `extent` байт (кратно
    /// [`SPARSE_UNIT`]) через равные промежутки, остальное — дыры.
    Sparse { extents: u32, extent: u64 },
}

/// Вид записи.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File {
        size: u64,
        content: Content,
    },
    /// Символьная ссылка на `target` (путь от корня датасета; на диске — абсолютный).
    Link {
        target: String,
        dir: bool,
    },
}

impl Kind {
    fn tag(&self) -> char {
        match self {
            Kind::Dir => 'd',
            Kind::File { .. } => 'f',
            Kind::Link { .. } => 'l',
        }
    }
}

/// Запись плана: путь от корня через `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub hidden: bool,
}

impl Entry {
    pub fn dir(path: impl Into<String>) -> Self {
        Entry {
            path: path.into(),
            kind: Kind::Dir,
            hidden: false,
        }
    }

    pub fn file(path: impl Into<String>, size: u64, content: Content) -> Self {
        Entry {
            path: path.into(),
            kind: Kind::File { size, content },
            hidden: false,
        }
    }

    /// Логический размер (у каталогов и ссылок — 0).
    pub fn size(&self) -> u64 {
        match self.kind {
            Kind::File { size, .. } => size,
            _ => 0,
        }
    }
}

/// План дерева. Родитель всегда раньше потомков.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Сид содержимого файлов.
    pub seed: u64,
    pub entries: Vec<Entry>,
}

impl Plan {
    pub fn files(&self) -> usize {
        self.count(|k| matches!(k, Kind::File { .. }))
    }

    pub fn dirs(&self) -> usize {
        self.count(|k| matches!(k, Kind::Dir))
    }

    fn count(&self, f: impl Fn(&Kind) -> bool) -> usize {
        self.entries.iter().filter(|e| f(&e.kind)).count()
    }

    /// Сумма логических размеров файлов.
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(Entry::size).sum()
    }

    /// Оценка места на диске: файлы — с округлением до 4 КБ, разреженные — данные.
    pub fn disk_bytes(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| match e.kind {
                Kind::File {
                    content: Content::Sparse { extents, extent },
                    ..
                } => u64::from(extents) * extent,
                Kind::File { size, .. } => size.div_ceil(4096) * 4096,
                _ => 4096,
            })
            .sum()
    }

    /// [`tree_hash`] всех записей.
    pub fn tree_hash(&self) -> u64 {
        self.tree_hash_without(&[])
    }

    /// [`tree_hash`] без записей `skipped` (ссылки, которые ОС не дала создать).
    pub fn tree_hash_without(&self, skipped: &[String]) -> u64 {
        tree_hash(
            self.entries
                .iter()
                .filter(|e| !skipped.contains(&e.path))
                .map(|e| (e.path.clone(), e.kind.tag(), e.size())),
        )
    }

    /// Пары путей, совпадающих без учёта регистра (на NTFS это одна запись).
    pub fn case_collisions(&self) -> Vec<(String, String)> {
        let mut keys: Vec<(String, &str)> = self
            .entries
            .iter()
            .map(|e| (e.path.to_lowercase(), e.path.as_str()))
            .collect();
        keys.sort_unstable();
        keys.windows(2)
            .filter(|w| w[0].0 == w[1].0)
            .map(|w| (w[0].1.to_owned(), w[1].1.to_owned()))
            .collect()
    }
}

/// Хэш дерева: FNV-1a 64 по отсортированным `(путь, вид, размер)`; вид — `d`, `f`, `l`.
pub fn tree_hash(items: impl IntoIterator<Item = (String, char, u64)>) -> u64 {
    let mut v: Vec<(String, char, u64)> = items.into_iter().collect();
    v.sort_unstable();
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    for (path, kind, size) in &v {
        feed(path.as_bytes());
        feed(&[0, kind.to_ascii_lowercase() as u8]);
        feed(&size.to_le_bytes());
    }
    h
}

/// FNV-1a строки: сид набора и сид содержимого файла.
pub(crate) fn str_hash(s: &str) -> u64 {
    tree_hash([(s.to_owned(), 's', 0)])
}

/// План дерева листинга S4 §7 (тот же состав и порядок сида, что у прежнего `gen_tree`).
pub fn plan(spec: &TreeSpec) -> Plan {
    let mut rng = Rng::new(spec.seed);
    let mut entries = Vec::with_capacity(spec.n + 1);
    let dirs: Vec<String> = match spec.layout {
        Layout::Flat => vec![String::new()],
        Layout::Nav { dirs } => (0..dirs.max(1)).map(|d| format!("dir {d:04}")).collect(),
    };
    entries.extend(dirs.iter().filter(|d| !d.is_empty()).map(Entry::dir));
    let per = spec.n.div_ceil(dirs.len()).max(1);
    let at = |dir: &str, leaf: String| {
        if dir.is_empty() {
            leaf
        } else {
            format!("{dir}/{leaf}")
        }
    };
    let mut last_file: Option<String> = None;
    for i in 0..spec.n {
        let dir = &dirs[i / per];
        let roll = rng.below(1000);
        if roll < 10 {
            // 1% каталогов.
            entries.push(Entry::dir(at(dir, name(&mut rng, i, true))));
            continue;
        }
        if roll < 15
            && let Some(target) = &last_file
        {
            // 0,5% ссылок на файл.
            entries.push(Entry {
                path: at(dir, format!("link {i}")),
                kind: Kind::Link {
                    target: target.clone(),
                    dir: false,
                },
                hidden: false,
            });
            continue;
        }
        let hidden = rng.below(50) == 0;
        let mut file_name = name(&mut rng, i, false);
        if hidden && !cfg!(windows) {
            file_name.insert(0, '.');
        }
        let path = at(dir, file_name);
        // 70% пустые, остальные — до 512 байт.
        let size = if rng.below(10) < 7 {
            0
        } else {
            rng.below(512) + 1
        };
        entries.push(Entry {
            hidden,
            ..Entry::file(path.clone(), size, Content::Fill)
        });
        last_file = Some(path);
    }
    Plan {
        seed: spec.seed,
        entries,
    }
}

/// Путь записи `rel` (`a/b/c`) под `root` — по компонентам (годится и для `\\?\`).
pub fn join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel.split('/').filter(|c| !c.is_empty()));
    p
}

/// Абсолютный путь с префиксом `\\?\` (Windows): глубина 256 и пути длиннее `MAX_PATH`.
pub fn verbatim(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    #[cfg(windows)]
    {
        let Some(s) = abs.to_str() else {
            return Ok(abs);
        };
        if s.starts_with(r"\\?\") {
            return Ok(abs);
        }
        if let Some(unc) = s.strip_prefix(r"\\") {
            return Ok(PathBuf::from(format!(r"\\?\UNC\{unc}")));
        }
        Ok(PathBuf::from(format!(r"\\?\{s}")))
    }
    #[cfg(not(windows))]
    Ok(abs)
}

/// mtime записи `i` плана.
pub fn mtime(i: usize) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(MTIME_BASE + i as u64)
}

/// Записать план в `root` (каталог создаётся; существующие файлы перезаписываются).
/// Ссылки, которые ОС не дала создать, — в [`Report::skipped`].
pub fn materialize(plan: &Plan, root: &Path) -> io::Result<Report> {
    let root = verbatim(root)?;
    std::fs::create_dir_all(&root)?;
    let mut report = Report::default();
    let mut files = Vec::new();
    for (i, e) in plan.entries.iter().enumerate() {
        match &e.kind {
            Kind::Dir => {
                match std::fs::create_dir(join(&root, &e.path)) {
                    Err(err) if err.kind() != io::ErrorKind::AlreadyExists => return Err(err),
                    _ => {}
                }
                report.dirs += 1;
            }
            Kind::File { size, .. } => {
                files.push(i);
                report.files += 1;
                report.bytes += size;
                report.hidden += usize::from(e.hidden);
            }
            Kind::Link { .. } => {}
        }
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(8)
        .min(files.len());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| -> io::Result<()> {
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = files.get(k) else {
                            return Ok(());
                        };
                        write_entry(&root, plan.seed, i, &plan.entries[i])?;
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .try_for_each(|w| w.join().unwrap_or_else(|_| Err(io::Error::other("panic"))))
    })?;
    for e in &plan.entries {
        if let Kind::Link { target, dir } = &e.kind {
            if make_link(&join(&root, target), &join(&root, &e.path), *dir).is_ok() {
                report.links += 1;
            } else {
                report.links_skipped += 1;
                report.skipped.push(e.path.clone());
            }
        }
    }
    Ok(report)
}

fn write_entry(root: &Path, seed: u64, index: usize, entry: &Entry) -> io::Result<()> {
    let Kind::File { size, content } = entry.kind else {
        return Ok(());
    };
    let path = join(root, &entry.path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(windows)]
    if entry.hidden {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 2;
        opts.attributes(FILE_ATTRIBUTE_HIDDEN);
    }
    let mut file = opts.open(&path)?;
    let mut rng = Rng::new(seed ^ str_hash(&entry.path));
    match content {
        Content::Fill => write_with(&mut file, size, |buf| buf.fill(b'x'))?,
        Content::Random => write_with(&mut file, size, |buf| fill_random(&mut rng, buf))?,
        Content::Text => {
            let mut text = TextGen::default();
            write_with(&mut file, size, |buf| text.fill(&mut rng, buf))?;
        }
        Content::Sparse { extents, extent } => {
            crate::sys::set_sparse(&path)?;
            file.set_len(size)?;
            let stride = (size / u64::from(extents.max(1))) & !(SPARSE_UNIT - 1);
            let mut buf = vec![0u8; 1 << 20];
            for k in 0..u64::from(extents) {
                let at = k * stride;
                let len = extent.min(size.saturating_sub(at));
                io::Seek::seek(&mut file, io::SeekFrom::Start(at))?;
                let mut left = len;
                while left > 0 {
                    let m = usize::try_from(left.min(buf.len() as u64)).unwrap_or(0);
                    fill_random(&mut rng, &mut buf[..m]);
                    file.write_all(&buf[..m])?;
                    left -= m as u64;
                }
            }
        }
    }
    file.set_modified(mtime(index))
}

/// Писать `size` байт кусками по 1 МБ, наполняя кусок `fill`.
fn write_with(f: &mut std::fs::File, size: u64, mut fill: impl FnMut(&mut [u8])) -> io::Result<()> {
    if size == 0 {
        return Ok(());
    }
    let mut buf = vec![0u8; usize::try_from(size.min(1 << 20)).unwrap_or(1 << 20)];
    let mut left = size;
    while left > 0 {
        let m = usize::try_from(left.min(buf.len() as u64)).unwrap_or(0);
        fill(&mut buf[..m]);
        f.write_all(&buf[..m])?;
        left -= m as u64;
    }
    Ok(())
}

fn fill_random(rng: &mut Rng, buf: &mut [u8]) {
    let (chunks, rest) = buf.as_chunks_mut::<8>();
    for c in chunks {
        *c = rng.next_u64().to_le_bytes();
    }
    let last = rng.next_u64().to_le_bytes();
    rest.copy_from_slice(&last[..rest.len()]);
}

/// Строки «кода» для B3: сжимаемый текст, перенос строки на границе куска сохраняется.
#[derive(Debug, Default)]
struct TextGen {
    pending: Vec<u8>,
}

impl TextGen {
    fn fill(&mut self, rng: &mut Rng, buf: &mut [u8]) {
        let mut at = 0;
        while at < buf.len() {
            if self.pending.is_empty() {
                let w =
                    |rng: &mut Rng| crate::WORDS[(rng.below(crate::WORDS.len() as u64)) as usize];
                let line = format!(
                    "    let {}_{} = {}({}, {});\n",
                    w(rng),
                    rng.below(100),
                    w(rng),
                    w(rng),
                    rng.below(10_000)
                );
                self.pending = line.into_bytes();
                self.pending.reverse();
            }
            while at < buf.len()
                && let Some(b) = self.pending.pop()
            {
                buf[at] = b;
                at += 1;
            }
        }
    }
}

#[cfg(windows)]
fn make_link(target: &Path, link: &Path, dir: bool) -> io::Result<()> {
    if dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(not(windows))]
fn make_link(target: &Path, link: &Path, _dir: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Запись на диске: путь от корня через `/`, вид, размер, mtime (у файлов).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    pub path: String,
    pub kind: char,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

/// Обойти дерево `root` (ссылки не разыменовываются; манифест в корне пропускается).
pub fn scan(root: &Path) -> io::Result<Vec<Scanned>> {
    let root = verbatim(root)?;
    let mut out = Vec::new();
    let mut stack = vec![(root, String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let leaf = e.file_name().to_string_lossy().into_owned();
            if rel.is_empty() && leaf == MANIFEST {
                continue;
            }
            let path = if rel.is_empty() {
                leaf
            } else {
                format!("{rel}/{leaf}")
            };
            let m = std::fs::symlink_metadata(e.path())?;
            let t = m.file_type();
            let (kind, size, mtime) = if t.is_symlink() {
                ('l', 0, None)
            } else if t.is_dir() {
                stack.push((e.path(), path.clone()));
                ('d', 0, None)
            } else {
                ('f', m.len(), m.modified().ok())
            };
            out.push(Scanned {
                path,
                kind,
                size,
                mtime,
            });
        }
    }
    Ok(out)
}

/// [`tree_hash`] дерева на диске.
pub fn scan_hash(scanned: &[Scanned]) -> u64 {
    tree_hash(scanned.iter().map(|s| (s.path.clone(), s.kind, s.size)))
}

// ---------------------------------------------------------------- манифест

/// Манифест датасета `<корень>/.urfin-dataset.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub seed: u64,
    pub gen_ver: u32,
    /// Масштаб `--scale` (у наборов без масштаба — 1).
    pub scale: f64,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub tree_hash: u64,
}

impl Manifest {
    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \"id\": {},\n  \"seed\": {},\n  \"gen_ver\": {},\n  \"scale\": {},\n  \"files\": {},\n  \"dirs\": {},\n  \"bytes\": {},\n  \"tree_hash\": \"{:#018x}\"\n}}\n",
            json::quote(&self.id),
            self.seed,
            self.gen_ver,
            self.scale,
            self.files,
            self.dirs,
            self.bytes,
            self.tree_hash
        )
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let j = json::Json::parse(text)?;
        let num = |k: &str| {
            j.get(k)
                .and_then(json::Json::as_u64)
                .ok_or_else(|| format!("манифест: нет {k}"))
        };
        let hash = j
            .get("tree_hash")
            .and_then(json::Json::as_string)
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or("манифест: нет tree_hash")?;
        Ok(Manifest {
            id: j
                .get("id")
                .and_then(json::Json::as_string)
                .ok_or("манифест: нет id")?,
            seed: num("seed")?,
            gen_ver: u32::try_from(num("gen_ver")?).map_err(|e| e.to_string())?,
            scale: j.get("scale").and_then(json::Json::as_f64).unwrap_or(1.0),
            files: num("files")?,
            dirs: num("dirs")?,
            bytes: num("bytes")?,
            tree_hash: hash,
        })
    }

    pub fn read(dir: &Path) -> io::Result<Manifest> {
        let text = std::fs::read_to_string(dir.join(MANIFEST))?;
        Manifest::parse(&text).map_err(io::Error::other)
    }

    pub fn write(&self, dir: &Path) -> io::Result<()> {
        std::fs::write(dir.join(MANIFEST), self.to_json())
    }
}

/// Мини-разбор JSON без зависимостей: манифест и эталон U1. Строки хранятся в UTF-16,
/// чтобы `\uD800` (непарный суррогат в имени U1) доживал до имени файла.
pub(crate) mod json {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Json {
        Null,
        Bool(bool),
        /// Число как в тексте: `u64` и `f64` разбираются по запросу.
        Num(String),
        Str(Vec<u16>),
        Arr(Vec<Json>),
        Obj(Vec<(String, Json)>),
    }

    impl Json {
        pub fn parse(text: &str) -> Result<Json, String> {
            let mut p = Parser {
                s: text.as_bytes(),
                at: 0,
            };
            let v = p.value()?;
            p.ws();
            if p.at != p.s.len() {
                return Err(p.err("лишнее после значения"));
            }
            Ok(v)
        }

        pub fn get(&self, key: &str) -> Option<&Json> {
            match self {
                Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        pub fn as_u64(&self) -> Option<u64> {
            match self {
                Json::Num(n) => n.parse().ok(),
                _ => None,
            }
        }

        pub fn as_f64(&self) -> Option<f64> {
            match self {
                Json::Num(n) => n.parse().ok(),
                _ => None,
            }
        }

        pub fn as_bool(&self) -> Option<bool> {
            match self {
                Json::Bool(b) => Some(*b),
                _ => None,
            }
        }

        pub fn as_wide(&self) -> Option<&[u16]> {
            match self {
                Json::Str(w) => Some(w),
                _ => None,
            }
        }

        /// Строка без непарных суррогатов.
        pub fn as_string(&self) -> Option<String> {
            String::from_utf16(self.as_wide()?).ok()
        }

        pub fn as_arr(&self) -> Option<&[Json]> {
            match self {
                Json::Arr(a) => Some(a),
                _ => None,
            }
        }
    }

    /// Строка в кавычках с экранированием.
    pub fn quote(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => {
                    let _ =
                        std::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    struct Parser<'a> {
        s: &'a [u8],
        at: usize,
    }

    impl Parser<'_> {
        fn err(&self, what: &str) -> String {
            format!("JSON, байт {}: {what}", self.at)
        }

        fn ws(&mut self) {
            while self.s.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                self.at += 1;
            }
        }

        fn eat(&mut self, lit: &str) -> bool {
            if self.s[self.at..].starts_with(lit.as_bytes()) {
                self.at += lit.len();
                true
            } else {
                false
            }
        }

        fn value(&mut self) -> Result<Json, String> {
            self.ws();
            match self.s.get(self.at) {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => self.string().map(Json::Str),
                Some(b'-' | b'0'..=b'9') => {
                    let start = self.at;
                    while self
                        .s
                        .get(self.at)
                        .is_some_and(|b| b"+-.eE0123456789".contains(b))
                    {
                        self.at += 1;
                    }
                    let n = std::str::from_utf8(&self.s[start..self.at]).unwrap_or_default();
                    Ok(Json::Num(n.to_owned()))
                }
                _ if self.eat("true") => Ok(Json::Bool(true)),
                _ if self.eat("false") => Ok(Json::Bool(false)),
                _ if self.eat("null") => Ok(Json::Null),
                _ => Err(self.err("ожидалось значение")),
            }
        }

        fn object(&mut self) -> Result<Json, String> {
            self.at += 1;
            let mut kv = Vec::new();
            self.ws();
            if self.eat("}") {
                return Ok(Json::Obj(kv));
            }
            loop {
                self.ws();
                if self.s.get(self.at) != Some(&b'"') {
                    return Err(self.err("ожидался ключ"));
                }
                let k = String::from_utf16_lossy(&self.string()?);
                self.ws();
                if !self.eat(":") {
                    return Err(self.err("ожидалось «:»"));
                }
                kv.push((k, self.value()?));
                self.ws();
                if self.eat("}") {
                    return Ok(Json::Obj(kv));
                }
                if !self.eat(",") {
                    return Err(self.err("ожидалось «,» или «}»"));
                }
            }
        }

        fn array(&mut self) -> Result<Json, String> {
            self.at += 1;
            let mut a = Vec::new();
            self.ws();
            if self.eat("]") {
                return Ok(Json::Arr(a));
            }
            loop {
                a.push(self.value()?);
                self.ws();
                if self.eat("]") {
                    return Ok(Json::Arr(a));
                }
                if !self.eat(",") {
                    return Err(self.err("ожидалось «,» или «]»"));
                }
            }
        }

        fn string(&mut self) -> Result<Vec<u16>, String> {
            self.at += 1;
            let mut out = Vec::new();
            loop {
                let rest = std::str::from_utf8(&self.s[self.at..]).map_err(|e| e.to_string())?;
                let Some(c) = rest.chars().next() else {
                    return Err(self.err("строка не закрыта"));
                };
                self.at += c.len_utf8();
                match c {
                    '"' => return Ok(out),
                    '\\' => {
                        let e = self.s.get(self.at).copied().ok_or("строка не закрыта")?;
                        self.at += 1;
                        match e {
                            b'"' => out.push(u16::from(b'"')),
                            b'\\' => out.push(u16::from(b'\\')),
                            b'/' => out.push(u16::from(b'/')),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            b'n' => out.push(10),
                            b'r' => out.push(13),
                            b't' => out.push(9),
                            b'u' => {
                                let hex = self
                                    .s
                                    .get(self.at..self.at + 4)
                                    .and_then(|h| std::str::from_utf8(h).ok())
                                    .and_then(|h| u16::from_str_radix(h, 16).ok())
                                    .ok_or_else(|| self.err("плохое \\u"))?;
                                self.at += 4;
                                out.push(hex);
                            }
                            _ => return Err(self.err("плохое экранирование")),
                        }
                    }
                    c => {
                        let mut b = [0u16; 2];
                        out.extend_from_slice(c.encode_utf16(&mut b));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(n: usize, seed: u64, layout: Layout) -> TreeSpec {
        TreeSpec { n, seed, layout }
    }

    /// Одинаковый сид — одинаковый план и `tree_hash`; другой сид — другой хэш.
    #[test]
    fn same_seed_same_tree_hash() {
        let a = plan(&spec(5000, 7, Layout::Flat));
        let b = plan(&spec(5000, 7, Layout::Flat));
        assert_eq!(a, b);
        assert_eq!(a.tree_hash(), b.tree_hash());
        assert_ne!(
            a.tree_hash(),
            plan(&spec(5000, 8, Layout::Flat)).tree_hash()
        );
        let nav = spec(5000, 7, Layout::Nav { dirs: 50 });
        assert_eq!(plan(&nav).tree_hash(), plan(&nav).tree_hash());
        // Порядок записей на хэш не влияет.
        let mut c = a.clone();
        c.entries.reverse();
        assert_eq!(c.tree_hash(), a.tree_hash());
    }

    /// В плане нет имён, совпадающих без учёта регистра (десять сидов, обе раскладки).
    #[test]
    fn plan_has_no_case_collisions() {
        for seed in 0..10 {
            for layout in [Layout::Flat, Layout::Nav { dirs: 20 }] {
                let p = plan(&spec(20_000, seed, layout));
                assert_eq!(p.case_collisions(), vec![], "seed {seed} {layout:?}");
            }
        }
        let dup = Plan {
            seed: 0,
            entries: vec![
                Entry::file("Report1.txt", 0, Content::Fill),
                Entry::file("report1.TXT", 0, Content::Fill),
            ],
        };
        assert_eq!(dup.case_collisions().len(), 1);
    }

    /// План на диске: `scan` даёт тот же `tree_hash`, mtime — по индексу записи.
    #[test]
    fn materialize_matches_plan() {
        let dir = tempfile::Builder::new()
            .prefix("urfin-plan-")
            .tempdir()
            .unwrap();
        let p = plan(&spec(3000, 3, Layout::Nav { dirs: 7 }));
        let report = materialize(&p, dir.path()).unwrap();
        assert_eq!(report.files, p.files());
        assert_eq!(report.bytes, p.bytes());
        let disk = scan(dir.path()).unwrap();
        assert_eq!(scan_hash(&disk), p.tree_hash_without(&report.skipped));
        let (index, first) = p
            .entries
            .iter()
            .enumerate()
            .find(|(_, x)| matches!(x.kind, Kind::File { .. }))
            .unwrap();
        let got = disk.iter().find(|x| x.path == first.path).unwrap();
        assert_eq!(got.mtime, Some(mtime(index)));
    }

    #[test]
    fn manifest_round_trip() {
        let m = Manifest {
            id: "L1".into(),
            seed: u64::MAX,
            gen_ver: GEN_VER,
            scale: 0.25,
            files: 99_000,
            dirs: 1000,
            bytes: 12_345,
            tree_hash: 0xDEAD_BEEF_0000_0001,
        };
        assert_eq!(Manifest::parse(&m.to_json()).unwrap(), m);
        assert!(Manifest::parse("{\"id\": \"x\"}").is_err());
    }

    #[test]
    fn json_strings_keep_lone_surrogates() {
        let j =
            json::Json::parse(r#"{"a": ["x\ud800y", "caf\u00e9", "é\n"], "n": -1.5e3, "t": true}"#)
                .unwrap();
        let a = j.get("a").and_then(json::Json::as_arr).unwrap();
        assert_eq!(a[0].as_wide().unwrap(), &[0x78, 0xD800, 0x79]);
        assert_eq!(a[0].as_string(), None);
        assert_eq!(a[1].as_string().unwrap(), "café");
        assert_eq!(a[2].as_string().unwrap(), "é\n");
        assert_eq!(j.get("n").and_then(json::Json::as_f64), Some(-1500.0));
        assert_eq!(j.get("t").and_then(json::Json::as_bool), Some(true));
        assert!(json::Json::parse("[1, 2").is_err());
    }
}
