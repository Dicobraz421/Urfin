//! Раскраска имён по маскам, атрибутам и типам (S2 §1.3, S0-decisions №32).
//! Пользовательские правила `[[ui.colors]]` — первыми, затем встроенные:
//! битые ссылки → `critical.fg`, облачные → `text.secondary`, скрытые и системные →
//! `type.muted`, временные и резервные (`~$*`, `*.tmp`, `*.bak`) → `type.temp`, архивы →
//! `type.archive`, исполняемые → `type.exec`.

use crate::color::Color32;
use rustc_hash::FxHashMap;
use urfin_core::{Attrs, ConfigWarning, Entry, EntryKind, Mask};

use crate::config::ColorRule;
use crate::tokens::{Role, Tokens, parse_color};

/// Цвет группы: роль токенов или свой.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fg {
    Role(Role),
    Rgb(Color32),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum KindSel {
    Dir,
    File,
    Link,
    Broken,
}

impl KindSel {
    fn matches(self, k: EntryKind) -> bool {
        match self {
            KindSel::Dir => k.is_dir_like(),
            KindSel::File => matches!(k, EntryKind::File | EntryKind::LinkFile),
            KindSel::Link => matches!(k, EntryKind::LinkFile | EntryKind::LinkDir),
            KindSel::Broken => k == EntryKind::LinkBroken,
        }
    }
}

#[derive(Debug)]
struct Rule {
    mask: Option<Mask>,
    attrs: u32,
    kind: Option<KindSel>,
    fg: Fg,
}

impl Rule {
    fn matches(&self, e: &Entry) -> bool {
        e.attrs.has(self.attrs)
            && self.kind.is_none_or(|k| k.matches(e.kind))
            && self.mask.as_ref().is_none_or(|m| m.matches(&e.name))
    }
}

/// Встроенные группы (индексы после пользовательских).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Builtin {
    Broken,
    Offline,
    Muted,
    Temp,
    Archive,
    Exec,
}

const BUILTINS: [(Builtin, Role); 6] = [
    (Builtin::Broken, Role::CriticalFg),
    (Builtin::Offline, Role::TextSecondary),
    (Builtin::Muted, Role::TypeMuted),
    (Builtin::Temp, Role::TypeTemp),
    (Builtin::Archive, Role::TypeArchive),
    (Builtin::Exec, Role::TypeExec),
];

const ARCHIVES: &[&str] = &[
    "7z", "zip", "rar", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "lz", "lzma", "cab",
    "arj", "lzh", "iso", "jar", "war", "apk", "cpio", "rpm", "deb", "z",
];
const EXECUTABLES: &[&str] = &[
    "exe", "msi", "ps1", "bat", "cmd", "sh", "com", "scr", "msix",
];
const TEMP: &[&str] = &["tmp", "bak", "temp", "swp", "old"];

/// Раскраска: [`Highlighter::classify`] даёт номер группы (0 — без цвета),
/// [`Highlighter::color`] — цвет группы в текущей теме.
#[derive(Debug)]
pub struct Highlighter {
    rules: Vec<Rule>,
    /// Расширение в нижнем регистре → встроенная группа.
    ext: FxHashMap<&'static str, Builtin>,
    /// Скрытые dot-файлы приглушать (Unix; на Windows dot-файл — обычное имя).
    mute_dotfiles: bool,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::build(&[]).0
    }
}

/// Длина буфера расширения: длиннее не бывает у встроенных.
const EXT_MAX: usize = 8;

impl Highlighter {
    /// Правила конфига; неверные пропускаются с предупреждением.
    pub fn build(cfg: &[ColorRule]) -> (Self, Vec<ConfigWarning>) {
        let mut warnings = Vec::new();
        let mut rules = Vec::new();
        let warn = |i: usize, msg: String| ConfigWarning {
            key: format!("ui.colors[{i}]"),
            msg,
        };
        for (i, r) in cfg.iter().enumerate() {
            let Some(fg) = parse_fg(&r.fg) else {
                warnings.push(warn(i, format!("bad fg {:?}", r.fg)));
                continue;
            };
            let mask = match r.mask.as_deref().map(Mask::parse).transpose() {
                Ok(m) => m,
                Err(e) => {
                    warnings.push(warn(i, format!("bad mask: {e}")));
                    continue;
                }
            };
            let mut attrs = 0;
            let mut bad = None;
            for a in &r.attrs {
                match attr_bit(a) {
                    Some(b) => attrs |= b,
                    None => bad = Some(a.clone()),
                }
            }
            if let Some(a) = bad {
                warnings.push(warn(i, format!("unknown attribute {a:?}")));
                continue;
            }
            let kind = match r.kind.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None => None,
                Some("dir") => Some(KindSel::Dir),
                Some("file") => Some(KindSel::File),
                Some("link") => Some(KindSel::Link),
                Some("broken") => Some(KindSel::Broken),
                Some(k) => {
                    warnings.push(warn(i, format!("unknown kind {k:?}")));
                    continue;
                }
            };
            rules.push(Rule {
                mask,
                attrs,
                kind,
                fg,
            });
        }
        // Номер группы — u8: 0 — нет, затем правила, затем встроенные.
        rules.truncate(usize::from(u8::MAX) - BUILTINS.len() - 1);
        let mut ext = FxHashMap::default();
        for (list, group) in [
            (ARCHIVES, Builtin::Archive),
            (EXECUTABLES, Builtin::Exec),
            (TEMP, Builtin::Temp),
        ] {
            for e in list {
                ext.insert(*e, group);
            }
        }
        let hl = Highlighter {
            rules,
            ext,
            mute_dotfiles: cfg!(not(windows)),
        };
        (hl, warnings)
    }

    /// Группа элемента: 0 — без раскраски.
    pub fn classify(&self, e: &Entry) -> u8 {
        if let Some(i) = self.rules.iter().position(|r| r.matches(e)) {
            return (i + 1) as u8;
        }
        self.builtin(e)
            .map_or(0, |b| (self.rules.len() + 1 + b as usize) as u8)
    }

    fn builtin(&self, e: &Entry) -> Option<Builtin> {
        if e.kind == EntryKind::LinkBroken {
            return Some(Builtin::Broken);
        }
        let a = e.attrs;
        if a.has(Attrs::OFFLINE) {
            return Some(Builtin::Offline);
        }
        if a.has(Attrs::HIDDEN)
            || a.has(Attrs::SYSTEM)
            || (self.mute_dotfiles && a.has(Attrs::DOTFILE))
        {
            return Some(Builtin::Muted);
        }
        if e.kind.is_dir_like() {
            return None;
        }
        let name = e.name.as_str();
        if name.starts_with("~$") {
            return Some(Builtin::Temp);
        }
        let by_ext = name
            .rfind('.')
            .filter(|&dot| dot > 0)
            .and_then(|dot| lower_ext(&name[dot + 1..]))
            .and_then(|(buf, len)| {
                let ext = std::str::from_utf8(&buf[..len]).ok()?;
                self.ext.get(ext).copied()
            });
        by_ext.or_else(|| a.has(Attrs::EXEC).then_some(Builtin::Exec))
    }

    /// Цвет группы `g` в текущей теме `t`.
    pub fn color(&self, g: u8, t: &Tokens) -> Option<Color32> {
        let i = usize::from(g).checked_sub(1)?;
        let fg = match self.rules.get(i) {
            Some(r) => r.fg,
            None => Fg::Role(BUILTINS.get(i - self.rules.len())?.1),
        };
        Some(match fg {
            Fg::Role(r) => t.c(r),
            Fg::Rgb(c) => c,
        })
    }
}

/// Расширение в нижнем регистре на стеке; длиннее [`EXT_MAX`] или не ASCII — `None`.
fn lower_ext(ext: &str) -> Option<([u8; EXT_MAX], usize)> {
    let b = ext.as_bytes();
    if b.is_empty() || b.len() > EXT_MAX || !b.is_ascii() {
        return None;
    }
    let mut buf = [0u8; EXT_MAX];
    for (d, s) in buf.iter_mut().zip(b) {
        *d = s.to_ascii_lowercase();
    }
    Some((buf, b.len()))
}

fn parse_fg(s: &str) -> Option<Fg> {
    match s.trim().strip_prefix('@') {
        Some(role) => Role::from_name(role).map(Fg::Role),
        None => parse_color(s.trim()).map(Fg::Rgb),
    }
}

fn attr_bit(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "hidden" => Attrs::HIDDEN,
        "system" => Attrs::SYSTEM,
        "readonly" => Attrs::READONLY,
        "archive" => Attrs::ARCHIVE,
        "reparse" => Attrs::REPARSE,
        "exec" => Attrs::EXEC,
        "compressed" => Attrs::COMPRESSED,
        "encrypted" => Attrs::ENCRYPTED,
        "offline" => Attrs::OFFLINE,
        "dotfile" => Attrs::DOTFILE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, attrs: u32, kind: EntryKind) -> Entry {
        Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: name.into(),
            size: 0,
            mtime: 0,
            attrs: Attrs(attrs),
            kind,
        }
    }

    fn file(name: &str) -> Entry {
        entry(name, 0, EntryKind::File)
    }

    fn role_of(h: &Highlighter, e: &Entry) -> Option<Color32> {
        h.color(h.classify(e), &Tokens::default())
    }

    #[test]
    fn builtin_groups() {
        let h = Highlighter::default();
        let t = Tokens::default();
        let c = |r| Some(t.c(r));
        assert_eq!(role_of(&h, &file("a.ZIP")), c(Role::TypeArchive));
        assert_eq!(role_of(&h, &file("setup.exe")), c(Role::TypeExec));
        assert_eq!(role_of(&h, &file("~$doc.docx")), c(Role::TypeTemp));
        assert_eq!(role_of(&h, &file("x.bak")), c(Role::TypeTemp));
        assert_eq!(
            role_of(&h, &entry("run", Attrs::EXEC, EntryKind::File)),
            c(Role::TypeExec)
        );
        assert_eq!(
            role_of(&h, &entry("h.zip", Attrs::HIDDEN, EntryKind::File)),
            c(Role::TypeMuted)
        );
        assert_eq!(
            role_of(&h, &entry("l", 0, EntryKind::LinkBroken)),
            c(Role::CriticalFg)
        );
        assert_eq!(
            role_of(&h, &entry("o", Attrs::OFFLINE, EntryKind::File)),
            c(Role::TextSecondary)
        );
        assert_eq!(h.classify(&file("main.rs")), 0);
        assert_eq!(
            h.classify(&file(".zip")),
            0,
            "dot-файл без имени — не архив"
        );
        assert_eq!(h.classify(&entry("dir.zip", 0, EntryKind::Dir)), 0);
        assert_eq!(h.color(0, &t), None);
    }

    #[test]
    fn user_rules_first_and_warnings() {
        let rules = vec![
            ColorRule {
                mask: Some("*.rs".into()),
                fg: "@accent.text".into(),
                ..ColorRule::default()
            },
            ColorRule {
                kind: Some("dir".into()),
                attrs: vec!["readonly".into()],
                fg: "#112233".into(),
                ..ColorRule::default()
            },
            ColorRule {
                mask: Some("*.zip".into()),
                fg: "@no.such".into(),
                ..ColorRule::default()
            },
            ColorRule {
                attrs: vec!["shiny".into()],
                fg: "#000000".into(),
                ..ColorRule::default()
            },
        ];
        let (h, warnings) = Highlighter::build(&rules);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let t = Tokens::default();
        assert_eq!(role_of(&h, &file("main.rs")), Some(t.c(Role::AccentText)));
        let ro = entry("d", Attrs::READONLY, EntryKind::Dir);
        assert_eq!(role_of(&h, &ro), Some(Color32::from_rgb(0x11, 0x22, 0x33)));
        // Плохое правило пропущено — архив остаётся встроенным.
        assert_eq!(role_of(&h, &file("a.zip")), Some(t.c(Role::TypeArchive)));
    }
}
