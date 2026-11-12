//! Адрес каталога `Location` и его строковая uri-форма. Контракт с S4/S8.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use compact_str::CompactString;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const LOCAL: &str = "local";
const SEP: &str = ">>";
/// Максимальная глубина цепочки `outer` (S0 v5 №8).
const MAX_DEPTH: usize = 8;

/// Адрес каталога. Контракт с S4/S8.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Location {
    /// `"local"` | `"zip"` | `"sftp"` | …
    pub backend: CompactString,
    /// Сервер сетевой схемы: у `sftp` — имя профиля (`sftp://<профиль>/путь`, F03 №1).
    /// `None` — локальные пути и контейнеры. [`Location::parent`] не выходит за корень
    /// сервера.
    pub authority: Option<CompactString>,
    /// Для `local` — нормализованный (см. [`Location::local`]).
    pub path: PathBuf,
    /// Контейнер (архив, образ), S8. Глубина цепочки — не больше 8.
    pub outer: Option<Arc<Location>>,
}

/// Ошибка разбора uri [`Location`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LocError {
    #[error("empty location uri")]
    Empty,
    #[error("no backend in location segment '{0}'")]
    NoBackend(String),
    #[error("invalid backend in location segment '{0}'")]
    BadBackend(String),
    #[error("invalid '%' escape or stray '>' in location segment '{0}'")]
    BadEscape(String),
    #[error("path in location segment '{0}' is not representable on this OS")]
    BadPath(String),
    #[error("container chain is deeper than {MAX_DEPTH}")]
    TooDeep,
}

impl Location {
    /// Локальный путь с нормализацией (S0 v2 №6), без обращения к ФС:
    /// - `.` и `..` разрешаются лексически (`..` выше корня отбрасывается);
    /// - хвостовой разделитель убирается, кроме корня (`C:\`, `/`);
    /// - Windows: `/` → `\`, буква диска в верхнем регистре, `\\?\C:\…` → `C:\…`,
    ///   `\\?\UNC\s\sh\…` → `\\s\sh\…`; прочие `\\?\`- и `\\.\`-пути не меняются;
    /// - регистр имён и symlink не трогаются;
    /// - относительный путь остаётся относительным (абсолютным его делает вызывающий).
    pub fn local(p: impl Into<PathBuf>) -> Self {
        Location {
            backend: LOCAL.into(),
            authority: None,
            path: normalize_local(&p.into()),
            outer: None,
        }
    }

    /// Обычный путь ОС: backend `local` вне контейнера.
    pub fn is_local(&self) -> bool {
        self.backend == LOCAL && self.outer.is_none()
    }

    /// Родительский каталог; у корня контейнера — сам контейнер (`outer`).
    /// Есть ли [`Location::parent`] — без построения пути (строки списка, каждый кадр).
    pub fn has_parent(&self) -> bool {
        match self.path.parent() {
            Some(_) if self.is_view_root() => false,
            Some(p) if !p.as_os_str().is_empty() => true,
            _ => self.outer.is_some(),
        }
    }

    pub fn parent(&self) -> Option<Location> {
        match self.path.parent() {
            Some(_) if self.is_view_root() => None,
            Some(p) if !p.as_os_str().is_empty() => Some(Location {
                backend: self.backend.clone(),
                authority: self.authority.clone(),
                path: p.to_path_buf(),
                outer: self.outer.clone(),
            }),
            _ => self.outer.as_deref().cloned(),
        }
    }

    /// Корень вкладки анализа места `du:/<snap_id>` и фильтр вкладки пакетов
    /// `pkg:/installed` (S0 v6 №5), панели результатов поиска `search:/<id>` и сравнения
    /// `cmp:/<id>` (S8): выше него — не каталог вкладки, `..` нет.
    fn is_view_root(&self) -> bool {
        matches!(self.backend.as_str(), "du" | "pkg" | "search" | "cmp")
            && self.outer.is_none()
            && self
                .path
                .components()
                .filter(|c| matches!(c, std::path::Component::Normal(_)))
                .count()
                <= 1
    }

    pub fn file_name(&self) -> Option<&OsStr> {
        self.path.file_name()
    }

    /// Строковая форма: сегменты `backend:path` от внешнего к внутреннему через `>>`.
    ///
    /// `"local:C:\x"`, `"local:C:\a.zip>>zip:/in"`; с [`Location::authority`] —
    /// `backend://authority/path`: `"sftp://srv1/var/www"`.
    ///
    /// Экранирование пути (ADR-0009): `%` → `%25`, `>` → `%3E`, байты не-UTF-8
    /// (unix) → `%HH`, hex в верхнем регистре. Поэтому `>>` в uri всегда
    /// разделитель, а имена с `>>`, `>` и `%` переживают roundtrip. Backend не
    /// экранируется: это идентификатор `[a-z0-9][a-z0-9+.-]+`.
    pub fn to_uri(&self) -> String {
        let mut chain = Vec::new();
        let mut cur = Some(self);
        while let Some(l) = cur {
            chain.push(l);
            cur = l.outer.as_deref();
        }
        let mut s = String::new();
        for (i, l) in chain.iter().rev().enumerate() {
            debug_assert!(valid_backend(&l.backend), "backend {:?}", l.backend);
            if i > 0 {
                s.push_str(SEP);
            }
            s.push_str(&l.backend);
            s.push(':');
            let bytes = l.path.as_os_str().as_encoded_bytes();
            if let Some(a) = &l.authority {
                s.push_str("//");
                escape_authority(a, &mut s);
                escape_bytes(bytes, &mut s);
            } else if l.backend != LOCAL && bytes.starts_with(b"//") {
                // Без authority путь `//x` не должен читаться как сервер `x`.
                s.push_str("%2F");
                escape_bytes(&bytes[1..], &mut s);
            } else {
                escape_bytes(bytes, &mut s);
            }
        }
        s
    }

    /// Разбор формы [`Location::to_uri`].
    ///
    /// Backend — минимум два символа, чтобы `C:\x` не читался как backend `C`.
    /// Пути backend `local` нормализуются как в [`Location::local`]. Принимаются
    /// любые `%HH`; одиночный `>` и неполный `%` — ошибка `BadEscape`. Цепочка
    /// глубже 8 контейнеров — `TooDeep`. На Windows путь, который после
    /// декодирования не UTF-8 (непарные суррогаты), — `BadPath`.
    pub fn parse_uri(s: &str) -> Result<Self, LocError> {
        if s.is_empty() {
            return Err(LocError::Empty);
        }
        if s.split(SEP).count() > MAX_DEPTH + 1 {
            return Err(LocError::TooDeep);
        }
        let mut outer: Option<Arc<Location>> = None;
        for seg in s.split(SEP) {
            let (backend, raw) = seg
                .split_once(':')
                .ok_or_else(|| LocError::NoBackend(seg.into()))?;
            if !valid_backend(backend) {
                return Err(LocError::BadBackend(seg.into()));
            }
            let (authority, raw) = match raw.strip_prefix("//") {
                Some(rest) if backend != LOCAL => {
                    let (a, p) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
                    let a = unescape(a)
                        .and_then(|b| String::from_utf8(b).ok())
                        .ok_or_else(|| LocError::BadEscape(seg.into()))?;
                    (Some(CompactString::from(a)), p)
                }
                _ => (None, raw),
            };
            let bytes = unescape(raw).ok_or_else(|| LocError::BadEscape(seg.into()))?;
            let path = path_from_bytes(bytes).ok_or_else(|| LocError::BadPath(seg.into()))?;
            let path = if backend == LOCAL {
                normalize_local(&path)
            } else {
                path
            };
            outer = Some(Arc::new(Location {
                backend: backend.into(),
                authority,
                path,
                outer: outer.take(),
            }));
        }
        Ok(Arc::unwrap_or_clone(
            outer.expect("split yields at least one segment"),
        ))
    }
}

impl Serialize for Location {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_uri())
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Location::parse_uri(&s).map_err(serde::de::Error::custom)
    }
}

fn valid_backend(b: &str) -> bool {
    let mut it = b.bytes();
    b.len() >= 2
        && it
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && it.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'+' | b'.' | b'-')
        })
}

fn escape_bytes(b: &[u8], out: &mut String) {
    use std::fmt::Write;
    for chunk in b.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '%' => out.push_str("%25"),
                '>' => out.push_str("%3E"),
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "%{b:02X}");
        }
    }
}

/// Как [`escape_bytes`], плюс `/`: authority кончается на первом `/`.
fn escape_authority(a: &str, out: &mut String) {
    for c in a.chars() {
        match c {
            '%' => out.push_str("%25"),
            '>' => out.push_str("%3E"),
            '/' => out.push_str("%2F"),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = b.get(i + 1..i + 3)?;
                let hex = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'>' => return None,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps, reason = "общая сигнатура с не-unix")]
fn path_from_bytes(b: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(b)))
}

#[cfg(not(unix))]
fn path_from_bytes(b: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(b).ok().map(PathBuf::from)
}

#[cfg(windows)]
fn normalize_local(p: &Path) -> PathBuf {
    // Путь с непарными суррогатами оставляем как есть: лексическая нормализация
    // ниже работает со строкой.
    match p.to_str() {
        Some(s) => PathBuf::from(normalize_windows(s)),
        None => p.to_path_buf(),
    }
}

#[cfg(not(windows))]
fn normalize_local(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    let mut rooted = false;
    let mut normals = 0usize;
    for c in p.components() {
        match c {
            Component::RootDir => {
                out.push(c);
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if normals > 0 {
                    out.pop();
                    normals -= 1;
                } else if !rooted {
                    out.push("..");
                }
            }
            Component::Normal(n) => {
                out.push(n);
                normals += 1;
            }
            Component::Prefix(_) => out.push(c),
        }
    }
    if out.as_os_str().is_empty() && !p.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Нормализация Windows-пути как строки. Чистая функция: тестируется на любой ОС.
/// Идемпотентна: `//?/c:/x` и `\\?\UNC\?\c:\x` после разбора дают снова префикс
/// `\\?\` — такой результат разбирается ещё раз, пока не устоится (S9-15, fuzz).
#[cfg_attr(not(windows), allow(dead_code))]
fn normalize_windows(s: &str) -> String {
    let mut out = normalize_windows_once(s);
    while out.starts_with(r"\\?\") || out.starts_with(r"\\.\") {
        // Каждый шаг либо оставляет строку как есть, либо снимает префикс и
        // укорачивает её: цикл конечен.
        let next = normalize_windows_once(&out);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

#[cfg_attr(not(windows), allow(dead_code))]
fn normalize_windows_once(s: &str) -> String {
    enum Prefix {
        None,
        Drive(char),
        Unc,
    }

    let s = if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest
            .get(..4)
            .is_some_and(|h| h.eq_ignore_ascii_case(r"UNC\"))
        {
            format!(r"\\{}", &rest[4..])
        } else if is_drive(rest) && (rest.len() == 2 || rest.as_bytes()[2] == b'\\') {
            rest.to_owned()
        } else {
            return s.to_owned();
        }
    } else if s.starts_with(r"\\.\") {
        return s.to_owned();
    } else {
        s.to_owned()
    };
    // После снятия `\\?\` путь обычный: `/` тоже разделитель (иначе нормализация
    // не идемпотентна).
    let s = s.replace('/', "\\");

    let (prefix, head, rest, rooted) = if is_drive(&s) {
        let rest = &s[2..];
        let d = s.as_bytes()[0].to_ascii_uppercase() as char;
        (
            Prefix::Drive(d),
            String::new(),
            rest,
            rest.starts_with('\\'),
        )
    } else if let Some(unc) = s.strip_prefix(r"\\") {
        let mut it = unc.splitn(3, '\\');
        let (server, share) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if server.is_empty() || share.is_empty() {
            return s;
        }
        let head = format!(r"\\{server}\{share}");
        (Prefix::Unc, head, it.next().unwrap_or(""), true)
    } else {
        (Prefix::None, String::new(), s.as_str(), s.starts_with('\\'))
    };

    let mut stack: Vec<&str> = Vec::new();
    for c in rest.split('\\').filter(|c| !c.is_empty() && *c != ".") {
        if c == ".." {
            match stack.last() {
                Some(&top) if top != ".." => {
                    stack.pop();
                }
                _ if rooted => {}
                _ => stack.push(".."),
            }
        } else {
            stack.push(c);
        }
    }
    let body = stack.join("\\");
    match prefix {
        Prefix::Drive(d) => format!("{d}:{}{body}", if rooted { "\\" } else { "" }),
        Prefix::Unc if body.is_empty() => head,
        Prefix::Unc => format!(r"{head}\{body}"),
        Prefix::None if rooted => format!(r"\{body}"),
        Prefix::None if body.is_empty() && !s.is_empty() => ".".to_owned(),
        Prefix::None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::{LocError, Location, normalize_windows};
    use proptest::prelude::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn inside(outer: Location, backend: &str, path: &str) -> Location {
        Location {
            backend: backend.into(),
            authority: None,
            path: PathBuf::from(path),
            outer: Some(Arc::new(outer)),
        }
    }

    fn roundtrip(l: &Location) {
        let uri = l.to_uri();
        assert_eq!(Location::parse_uri(&uri).as_ref(), Ok(l), "uri = {uri}");
    }

    /// Вкладка анализа места: у корня `du:/<id>` родителя нет, глубже — есть.
    #[test]
    fn du_root_has_no_parent() {
        let du = |p: &str| Location {
            backend: "du".into(),
            authority: None,
            path: PathBuf::from(p),
            outer: None,
        };
        assert!(!du("/3").has_parent());
        assert_eq!(du("/3").parent(), None);
        assert!(du("/3/target").has_parent());
        assert_eq!(du("/3/target/debug").parent(), Some(du("/3/target")));
        assert_eq!(du("/3/target").parent(), Some(du("/3")));
        let other = Location {
            backend: "zip".into(),
            authority: None,
            path: PathBuf::from("/3"),
            outer: None,
        };
        assert!(other.has_parent());
        // Вкладка пакетов: фильтр — корень.
        let pkg = Location {
            backend: "pkg".into(),
            authority: None,
            path: PathBuf::from("/updates"),
            outer: None,
        };
        assert!(!pkg.has_parent());
        assert_eq!(pkg.parent(), None);
        roundtrip(&pkg);
        assert_eq!(pkg.to_uri(), "pkg:/updates");
        // Панель результатов поиска, её вид «без доступа» и панель сравнения.
        for (b, p) in [("search", "/3"), ("search", "/3?f=errors"), ("cmp", "/4")] {
            let view = Location {
                backend: b.into(),
                authority: None,
                path: PathBuf::from(p),
                outer: None,
            };
            assert!(!view.has_parent(), "{b}:{p}");
            assert_eq!(view.parent(), None);
        }
    }

    #[test]
    fn normalize_windows_rules() {
        let cases = [
            (r"c:/x/", r"C:\x"),
            (r"C:\x", r"C:\x"),
            (r"C:\", r"C:\"),
            ("c:", "C:"),
            (r"C:\..", r"C:\"),
            (r"C:\a\.\b\..\c\\", r"C:\a\c"),
            (r"a\.\b\..\c", r"a\c"),
            (r"..\a\..\..", r"..\.."),
            (r"a\..", "."),
            (r"\\?\C:\x\", r"C:\x"),
            (r"\\?\c:", "C:"),
            (r"\\?\UNC\srv\sh\x", r"\\srv\sh\x"),
            (r"\\?\unc\srv\sh", r"\\srv\sh"),
            (r"\\srv\sh\", r"\\srv\sh"),
            (r"\\srv\sh\x\..\..", r"\\srv\sh"),
            ("//srv/sh/x", r"\\srv\sh\x"),
            (r"\\?\Volume{1}\x\..", r"\\?\Volume{1}\x\.."),
            (r"\\.\COM1", r"\\.\COM1"),
            // Fuzz S9-15: `/`-форма префикса и `UNC\?\` дают снова `\\?\` —
            // разбор до устойчивого результата.
            (r"//?/l:/?/V:gs", r"L:\?\V:gs"),
            (r"\\?\UNC\?\c:\x", r"C:\x"),
            (r"//?/Volume{1}/x/..", r"\\?\Volume{1}"),
            (r"//./COM1/x/../y", r"\\.\COM1\y"),
            (r"\\?\UNя\x", r"\\?\UNя\x"),
            (r"\\?\C:\а\..\б", r"C:\б"),
            (r"\x\..\y", r"\y"),
            ("", ""),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_windows(input), want, "input {input:?}");
        }
    }

    /// Fuzz S9-15 (`loc_uri`): `to_uri` читается обратно тем же `Location`.
    #[cfg(windows)]
    #[test]
    fn uri_roundtrip_slash_verbatim_prefix() {
        for s in ["local://?/l:/?/V:gs%00%00oc", r"local:\\?\UNC\?\c:\x"] {
            let l = Location::parse_uri(s).unwrap();
            assert_eq!(Location::parse_uri(&l.to_uri()), Ok(l), "{s}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn local_normalizes_on_windows() {
        assert_eq!(Location::local("c:/x/"), Location::local(r"C:\x"));
        assert_eq!(Location::local(r"\\?\C:\x"), Location::local(r"C:\x"));
        assert_eq!(Location::local(r"a\.\b\..\c").path, PathBuf::from(r"a\c"));
        assert_eq!(Location::local(r"C:\").to_uri(), r"local:C:\");
        assert_eq!(Location::local(r"\\srv\sh\x").to_uri(), r"local:\\srv\sh\x");
        assert_eq!(
            Location::local(r"C:\x\y").parent(),
            Some(Location::local(r"C:\x"))
        );
        assert_eq!(Location::local(r"C:\").parent(), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn local_normalizes_on_unix() {
        assert_eq!(Location::local("/x/").path, PathBuf::from("/x"));
        assert_eq!(Location::local("//x/./y/../z").path, PathBuf::from("/x/z"));
        assert_eq!(Location::local("/..").path, PathBuf::from("/"));
        assert_eq!(Location::local("a/./b/../c").path, PathBuf::from("a/c"));
        assert_eq!(Location::local("../a/../..").path, PathBuf::from("../.."));
        assert_eq!(Location::local("a/..").path, PathBuf::from("."));
        assert_eq!(Location::local("/").to_uri(), "local:/");
        assert_eq!(
            Location::local("/x/y").parent(),
            Some(Location::local("/x"))
        );
        assert_eq!(Location::local("/").parent(), None);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_roundtrip_on_unix() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let l = Location::local(OsStr::from_bytes(b"/a/\xff\xfe%>b"));
        assert_eq!(l.to_uri(), "local:/a/%FF%FE%25%3Eb");
        roundtrip(&l);
    }

    #[test]
    fn roots_and_unc_roundtrip() {
        for p in [r"C:\", r"\\srv\sh\x", "/", r"C:\a b\c"] {
            roundtrip(&Location::local(p));
        }
    }

    #[test]
    fn container_chain() {
        let arc = Location::local(if cfg!(windows) { r"C:\a.zip" } else { "/a.zip" });
        let l = inside(arc.clone(), "zip", "/in/dir");
        let uri = l.to_uri();
        assert!(uri.ends_with(".zip>>zip:/in/dir"), "{uri}");
        roundtrip(&l);
        assert!(!l.is_local());
        assert!(arc.is_local());
        let root = l.parent().unwrap().parent().unwrap();
        assert_eq!(root.path, PathBuf::from("/"));
        assert_eq!(root.parent(), Some(arc));
        assert_eq!(l.file_name().unwrap(), "dir");
    }

    #[test]
    fn special_chars_in_names() {
        let base = Location::local(if cfg!(windows) { r"C:\x" } else { "/x" });
        for name in [
            "a>>b", ">", "a>", ">>>", "100%", "%25", "%3E", r"a\b", "a:b",
        ] {
            let l = inside(base.clone(), "zip", &format!("/{name}/{name}"));
            assert!(!l.to_uri()[base.to_uri().len() + 2..].contains('>'));
            roundtrip(&l);
        }
        assert_eq!(
            inside(base.clone(), "zip", "/a>>b%").to_uri(),
            format!("{}>>zip:/a%3E%3Eb%25", base.to_uri())
        );
    }

    #[test]
    fn depth_limit() {
        let mut l = Location::local(if cfg!(windows) { r"C:\a" } else { "/a" });
        for i in 0..8 {
            l = inside(l, "zip", &format!("/l{i}"));
        }
        roundtrip(&l);
        let too_deep = inside(l, "zip", "/l8");
        assert_eq!(
            Location::parse_uri(&too_deep.to_uri()),
            Err(LocError::TooDeep)
        );
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Location::parse_uri(""), Err(LocError::Empty));
        assert!(matches!(
            Location::parse_uri("nocolon"),
            Err(LocError::NoBackend(_))
        ));
        assert!(matches!(
            Location::parse_uri(r"C:\x"),
            Err(LocError::BadBackend(_))
        ));
        assert!(matches!(
            Location::parse_uri(r"c:\x"),
            Err(LocError::BadBackend(_))
        ));
        assert!(matches!(
            Location::parse_uri("Zip:/x"),
            Err(LocError::BadBackend(_))
        ));
        assert!(matches!(
            Location::parse_uri("zip:/a%2"),
            Err(LocError::BadEscape(_))
        ));
        assert!(matches!(
            Location::parse_uri("zip:/a%zz"),
            Err(LocError::BadEscape(_))
        ));
        assert!(matches!(
            Location::parse_uri("zip:/a>b"),
            Err(LocError::BadEscape(_))
        ));
        assert!(matches!(
            Location::parse_uri("local:/a>>>zip:/b"),
            Err(LocError::BadBackend(_))
        ));
        assert!(matches!(
            Location::parse_uri("local:/a>>zip:/b>"),
            Err(LocError::BadEscape(_))
        ));
        assert!(matches!(
            Location::parse_uri("local:/a>>"),
            Err(LocError::NoBackend(_))
        ));
    }

    #[test]
    fn serde_is_uri_string() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct S {
            loc: Location,
        }
        let base = Location::local(if cfg!(windows) { r"C:\a.zip" } else { "/a.zip" });
        let s = S {
            loc: inside(base, "zip", "/x>>y"),
        };
        let text = toml::to_string(&s).unwrap();
        assert!(text.contains(">>zip:/x%3E%3Ey"), "{text}");
        assert_eq!(toml::from_str::<S>(&text).unwrap(), s);
        assert!(toml::from_str::<S>("loc = 'C:/x'").is_err());
    }

    fn component() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-zA-Zа-я0-9 %>._-]{1,6}",
            Just(">>".to_owned()),
            Just("..".to_owned()),
            Just(".".to_owned()),
            "[a-z]{1,3}\\\\[a-z>]{1,3}",
        ]
    }

    fn local_path() -> impl Strategy<Value = String> {
        let roots: Vec<&'static str> = if cfg!(windows) {
            vec![r"C:\", r"\\srv\sh\", "d:/", r"\\?\C:\", ""]
        } else {
            vec!["/", ""]
        };
        (
            prop::sample::select(roots),
            prop::collection::vec(component(), 0..5),
        )
            .prop_map(|(root, comps)| format!("{root}{}", comps.join("/")))
    }

    fn segment() -> impl Strategy<Value = (String, String)> {
        (
            prop::sample::select(vec!["zip", "7z", "sftp", "search", "cmp", "iso"]),
            prop::collection::vec(component(), 0..4),
        )
            .prop_map(|(b, comps)| (b.to_owned(), format!("/{}", comps.join("/"))))
    }

    fn net(authority: &str, path: &str) -> Location {
        Location {
            backend: "sftp".into(),
            authority: Some(authority.into()),
            path: PathBuf::from(path),
            outer: None,
        }
    }

    #[test]
    fn authority_uri() {
        let l = net("srv1", "/var/www");
        assert_eq!(l.to_uri(), "sftp://srv1/var/www");
        roundtrip(&l);
        assert_eq!(net("srv1", "/").to_uri(), "sftp://srv1/");
        assert_eq!(Location::parse_uri("sftp://srv1"), Ok(net("srv1", "/")));
        // `/`, `%`, `>` в имени сервера экранируются.
        let odd = net("u@h:2222/a%>b", "/x>>y");
        assert_eq!(odd.to_uri(), "sftp://u@h:2222%2Fa%25%3Eb/x%3E%3Ey");
        roundtrip(&odd);
        // Без authority путь `//x` не становится сервером.
        let bare = Location {
            backend: "sftp".into(),
            authority: None,
            path: PathBuf::from("//x/y"),
            outer: None,
        };
        roundtrip(&bare);
        // У `local` `//` — путь (UNC и т. п.), не сервер.
        assert_eq!(
            Location::parse_uri(r"local:\srv\sh").unwrap().authority,
            None
        );
        assert_eq!(
            Location::parse_uri("sftp://%FF/").unwrap_err(),
            LocError::BadEscape("sftp://%FF/".into())
        );
    }

    #[test]
    fn authority_parent_stops_at_server_root() {
        let l = net("srv1", "/var/www");
        let p = l.parent().unwrap();
        assert_eq!(p, net("srv1", "/var"));
        let root = p.parent().unwrap();
        assert_eq!(root, net("srv1", "/"));
        assert!(!root.has_parent());
        assert_eq!(root.parent(), None);
    }

    #[test]
    fn has_parent_matches_parent() {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let nested = if cfg!(windows) { r"C:" } else { "/a/b" };
        let locs = [
            Location::local(PathBuf::from(root)),
            Location::local(PathBuf::from(nested)),
            inside(
                Location::local(PathBuf::from(nested).join("x.zip")),
                "zip",
                "/",
            ),
            inside(
                Location::local(PathBuf::from(nested).join("x.zip")),
                "zip",
                "/d",
            ),
        ];
        for l in locs {
            assert_eq!(l.has_parent(), l.parent().is_some(), "{l:?}");
        }
    }

    proptest! {
        #[test]
        fn uri_roundtrip(base in local_path(), inner in prop::collection::vec(segment(), 0..=8)) {
            let mut l = Location::local(base);
            for (b, p) in &inner {
                l = inside(l, b, p);
            }
            let uri = l.to_uri();
            prop_assert_eq!(Location::parse_uri(&uri), Ok(l), "uri = {}", uri);
        }

        #[test]
        fn normalize_windows_idempotent(
            p in "([\\\\/]{2}[?.][\\\\/](UNC[\\\\/])?)?[cC]?[:]?[\\\\/]{0,2}([a-z.?:]{1,3}[\\\\/]){0,5}"
        ) {
            let once = normalize_windows(&p);
            prop_assert_eq!(normalize_windows(&once), once.clone());
        }

        #[test]
        fn local_is_idempotent(p in local_path()) {
            let l = Location::local(&p);
            prop_assert_eq!(Location::local(&l.path), l);
        }
    }
}
