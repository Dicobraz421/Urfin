//! Пути картинок и ссылок (S7 §3.10): относительно папки `.md`, `/путь` — от корня
//! git-репозитория (если `.md` в нём), иначе от корня тома; абсолютный путь Windows — как
//! есть; `%20` декодируется. Только локальные файлы: сеть (`http(s)://`, `//`, UNC) —
//! причина для плашки без единого обращения к ФС и сети. Зовётся из задачи разбора —
//! не в UI-потоке.

use std::path::{Component, Path, PathBuf};

use super::{ImageIssue, ImageState, LinkTarget, MdDoc};

/// Что ФС знает о файле.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FileMeta {
    pub len: u64,
    pub dir: bool,
}

/// Доступ к ФС для разрешения путей (в тестах — подмена со счётчиком обращений).
pub trait MdFs {
    fn meta(&self, p: &Path) -> Option<FileMeta>;
}

/// Локальная ФС.
#[derive(Copy, Clone, Debug, Default)]
pub struct RealFs;

impl MdFs for RealFs {
    fn meta(&self, p: &Path) -> Option<FileMeta> {
        let m = std::fs::metadata(p).ok()?;
        Some(FileMeta {
            len: m.len(),
            dir: m.is_dir(),
        })
    }
}

/// Окружение разрешения.
pub struct Resolver<'a> {
    /// Папка `.md`; `None` — документ не на локальном диске (архив, SFTP).
    pub dir: Option<PathBuf>,
    /// Картинки больше — `TooLarge`.
    pub image_max: u64,
    pub fs: &'a dyn MdFs,
}

impl std::fmt::Debug for Resolver<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("dir", &self.dir)
            .field("image_max", &self.image_max)
            .finish_non_exhaustive()
    }
}

/// Форматы, которые декодирует GPUI.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];
const IMAGE_MIME: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/svg+xml",
];

/// Где лежит цель.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Where {
    Net,
    Data,
    Local(PathBuf),
    NotLocal,
    /// Прочая схема.
    Other,
}

/// Разрешает картинки и пути ссылок документа.
/// Корень для `/путь`, найденный один раз на документ.
#[derive(Debug, Default)]
enum Root {
    #[default]
    Unknown,
    Found(Option<PathBuf>),
}

pub fn resolve(doc: &mut MdDoc, r: &Resolver<'_>) {
    let mut root = Root::Unknown;
    for img in &mut doc.images {
        img.state = image_state(&img.src, r, &mut root);
    }
    for l in &mut doc.links {
        if let LinkTarget::Path { path, .. } = &mut l.target {
            *path = match locate(&l.dest, r, &mut root) {
                Where::Local(p) => Some(p),
                _ => None,
            };
        }
    }
}

fn image_state(src: &str, r: &Resolver<'_>, root: &mut Root) -> ImageState {
    match locate(src, r, root) {
        Where::Net => ImageState::Blocked(ImageIssue::Network),
        Where::NotLocal => ImageState::Blocked(ImageIssue::NotLocal),
        Where::Other => ImageState::Blocked(ImageIssue::Unsupported),
        Where::Data => {
            let mime = src
                .trim()
                .get(5..)
                .and_then(|s| s.split([';', ',']).next())
                .unwrap_or("")
                .to_ascii_lowercase();
            if IMAGE_MIME.contains(&mime.as_str()) {
                ImageState::Data
            } else {
                ImageState::Blocked(ImageIssue::Unsupported)
            }
        }
        Where::Local(p) => {
            let Some(m) = r.fs.meta(&p) else {
                return ImageState::Blocked(ImageIssue::NotFound);
            };
            if m.dir {
                return ImageState::Blocked(ImageIssue::NotFound);
            }
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !IMAGE_EXTS.contains(&ext.as_str()) {
                return ImageState::Blocked(ImageIssue::Unsupported);
            }
            if m.len > r.image_max {
                return ImageState::Blocked(ImageIssue::TooLarge);
            }
            ImageState::File(p)
        }
    }
}

/// Схема URI (`http`, `data`, `file`…) в нижнем регистре; буква диска — не схема.
pub(crate) fn scheme(s: &str) -> Option<String> {
    let (head, _) = s.split_once(':')?;
    let ok = head.len() > 1
        && head.as_bytes()[0].is_ascii_alphabetic()
        && head
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'));
    ok.then(|| head.to_ascii_lowercase())
}

fn locate(raw: &str, r: &Resolver<'_>, root: &mut Root) -> Where {
    let s = raw.trim();
    let s = s
        .strip_prefix('<')
        .and_then(|x| x.strip_suffix('>'))
        .unwrap_or(s);
    if s.starts_with("//") || s.starts_with("\\\\") {
        return Where::Net;
    }
    let mut path = s;
    match scheme(s).as_deref() {
        Some("data") => return Where::Data,
        Some("file") => {
            let rest = &s[5..];
            let Some(p) = rest.strip_prefix("///") else {
                // `file://host/…` — сеть.
                return Where::Net;
            };
            path = p;
        }
        Some("http" | "https" | "ftp" | "ftps") => return Where::Net,
        Some(_) => return Where::Other,
        None => {}
    }
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if path.is_empty() {
        return Where::Other;
    }
    let path = percent_decode(path);
    if path.starts_with("\\\\") || path.starts_with("//") {
        return Where::Net;
    }
    let b = path.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    if drive {
        return Where::Local(normalize(Path::new(&path)));
    }
    let Some(dir) = &r.dir else {
        return Where::NotLocal;
    };
    if let Some(rest) = path.strip_prefix(['/', '\\']) {
        if let Root::Unknown = root {
            *root = Root::Found(repo_root(dir, r.fs));
        }
        let Root::Found(base) = root else {
            return Where::NotLocal;
        };
        return match base {
            Some(b) => Where::Local(normalize(&b.join(rest))),
            None => Where::NotLocal,
        };
    }
    Where::Local(normalize(&dir.join(&path)))
}

/// Корень git-репозитория над `dir` (папка с `.git`), иначе корень тома.
fn repo_root(dir: &Path, fs: &dyn MdFs) -> Option<PathBuf> {
    for a in dir.ancestors() {
        if fs.meta(&a.join(".git")).is_some() {
            return Some(a.to_path_buf());
        }
    }
    dir.ancestors().last().map(Path::to_path_buf)
}

/// Лексическая нормализация: `.` убирается, `..` поднимается (без обращения к ФС).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// `%XX` → байты (UTF-8, битые — U+FFFD).
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
