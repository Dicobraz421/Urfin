//! Пакеты грамматик (S7-21a): `grammars\<язык>.dll` рядом с `urfin.exe`, формат —
//! `assets/syntax/pack` (ABI [`ABI`]). Папка читается один раз; DLL грузится по
//! требованию и не выгружается (язык tree-sitter — её статическая таблица). Только папка
//! установки: путь абсолютный, имя — из своей таблицы языков, без поиска по PATH.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rustc_hash::FxHashSet;
use tree_sitter::Language;

use super::{Grammar, GrammarError};

/// Версия формата пакета, которую понимает этот exe.
pub(super) const ABI: u32 = 1;

/// Папка пакетов: `grammars` рядом с exe.
fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("grammars"))
    })
    .as_deref()
}

/// Имена пакетов в папке (`rust.dll` → `rust`), нижний регистр.
fn names() -> &'static FxHashSet<String> {
    static NAMES: OnceLock<FxHashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut out = FxHashSet::default();
        let Some(rd) = dir().and_then(|d| std::fs::read_dir(d).ok()) else {
            return out;
        };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if let Some(stem) = n.strip_suffix(".dll")
                && valid(stem)
            {
                out.insert(stem.to_owned());
            }
        }
        out
    })
}

/// Имя языка — только `[a-z0-9_]`: путь пакета не уходит из папки.
fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub(super) fn has(name: &str) -> bool {
    valid(name) && names().contains(name)
}

/// Грамматика из пакета; `None` — пакета нет.
pub(super) fn load(name: &str) -> Option<Result<Grammar, GrammarError>> {
    if !has(name) {
        return None;
    }
    let path = dir()?.join(format!("{name}.dll"));
    let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
    Some(open(name, &path))
}

fn open(name: &'static str, path: &Path) -> Result<Grammar, GrammarError> {
    let p = crate::sys::load_grammar(path, ABI).map_err(|e| GrammarError::Pack(e.to_string()))?;
    let lang = Language::new(p.lang);
    Grammar::new(name, lang, &p.query)
}
