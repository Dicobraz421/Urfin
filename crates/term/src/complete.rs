//! Автокомплит строки ввода: свои источники (S6-17, S6 §6.2).
//!
//! - **Пути** — относительно каталога сеанса; листинг каталога читается на полосе
//!   Interactive, последний каталог в кэше (повторный Tab — без диска). Регистр не
//!   важен.
//! - **Команды PATH** — для первого слова: файлы с расширениями `PATHEXT` из каталогов
//!   `PATH`; кэш по mtime каталогов.
//! - **История** — строки, начинающиеся с набранного (вся строка).
//!
//! Слияние: пути > PATH > история, без повторов. Вариант вставляется в кавычках по
//! правилам shell ([`crate::quote`]). Ghost-текст — хвост последней подходящей строки
//! истории, принимается → или End.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::history::History;
use crate::quote::{self, Style};

/// Откуда вариант.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Path,
    Command,
    History,
}

/// Вариант: что поставить на место `range` строки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Показать в списке.
    pub label: String,
    /// Вставить (уже в кавычках).
    pub insert: String,
    pub range: std::ops::Range<usize>,
    pub source: Source,
    pub dir: bool,
}

/// Имя в листинге каталога.
#[derive(Clone, Debug)]
pub struct Name {
    pub name: String,
    lower: String,
    pub dir: bool,
}

impl Name {
    pub fn new(name: &str, dir: bool) -> Name {
        Name {
            name: name.to_owned(),
            lower: name.to_lowercase(),
            dir,
        }
    }
}

/// Кэш последнего каталога.
#[derive(Clone, Debug, Default)]
pub struct DirCache {
    pub dir: PathBuf,
    pub names: Vec<Name>,
}

/// Прочитать каталог (вне UI-потока): имена по алфавиту без учёта регистра.
pub fn read_dir(dir: &Path) -> Vec<Name> {
    let mut out: Vec<Name> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_owned();
                let dir = e.file_type().is_ok_and(|t| t.is_dir())
                    || (e.file_type().is_ok_and(|t| t.is_symlink())
                        && e.path().metadata().is_ok_and(|m| m.is_dir()));
                Some(Name::new(&name, dir))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by(|a, b| a.lower.cmp(&b.lower));
    out
}

/// Что дополнять в слове пути: каталог для листинга, набранная часть каталога (как
/// есть) и начало имени.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathQuery {
    pub dir: PathBuf,
    pub typed_dir: String,
    pub prefix: String,
}

/// Разобрать слово пути относительно `cwd`.
pub fn path_query(word: &str, cwd: &Path) -> PathQuery {
    let cut = word.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let (typed_dir, prefix) = word.split_at(cut);
    // `C:` без разделителя — текущий каталог диска; берём корень.
    let (typed_dir, prefix) = if cut == 0
        && prefix.len() >= 2
        && prefix.as_bytes()[1] == b':'
        && prefix.as_bytes()[0].is_ascii_alphabetic()
    {
        (&word[..2], &word[2..])
    } else {
        (typed_dir, prefix)
    };
    let dir = if typed_dir.is_empty() {
        cwd.to_path_buf()
    } else {
        let p = Path::new(typed_dir);
        let p = if typed_dir.len() == 2 && typed_dir.ends_with(':') {
            PathBuf::from(format!("{typed_dir}\\"))
        } else {
            p.to_path_buf()
        };
        if p.is_absolute() || typed_dir.starts_with(['\\', '/']) {
            p
        } else {
            cwd.join(p)
        }
    };
    PathQuery {
        dir,
        typed_dir: typed_dir.to_owned(),
        prefix: prefix.to_owned(),
    }
}

/// Варианты путей из листинга каталога запроса.
pub fn paths(q: &PathQuery, names: &[Name], dirs_only: bool) -> Vec<(String, bool)> {
    let p = q.prefix.to_lowercase();
    names
        .iter()
        .filter(|n| (!dirs_only || n.dir) && n.lower.starts_with(&p))
        .map(|n| (format!("{}{}", q.typed_dir, n.name), n.dir))
        .collect()
}

/// Команды из PATH: каталог, его mtime и исполняемые имена.
#[derive(Clone, Debug, Default)]
pub struct PathCommands {
    pub dirs: Vec<(PathBuf, Option<SystemTime>, Vec<Name>)>,
}

/// Расширения `PATHEXT` в нижнем регистре (`.exe`, …) и `.ps1`.
pub fn pathext() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let mut v: Vec<String> = raw
        .split(';')
        .filter(|e| e.starts_with('.'))
        .map(str::to_lowercase)
        .collect();
    if !v.iter().any(|e| e == ".ps1") {
        v.push(".ps1".into());
    }
    v
}

fn mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

impl PathCommands {
    /// Пересканировать каталоги PATH, у которых сменился mtime (вне UI-потока).
    pub fn refresh(&mut self, path_var: &str, exts: &[String]) {
        let dirs: Vec<PathBuf> = std::env::split_paths(path_var)
            .filter(|d| !d.as_os_str().is_empty())
            .collect();
        let mut out = Vec::with_capacity(dirs.len());
        for d in dirs {
            let m = mtime(&d);
            let old = self
                .dirs
                .iter()
                .position(|(p, t, _)| *p == d && *t == m && m.is_some());
            if let Some(i) = old {
                out.push(self.dirs.swap_remove(i));
                continue;
            }
            let names = read_dir(&d)
                .into_iter()
                .filter(|n| !n.dir && exts.iter().any(|e| n.lower.ends_with(e.as_str())))
                .collect();
            out.push((d, m, names));
        }
        self.dirs = out;
    }

    /// Команды, начинающиеся с `prefix` (без расширения у `.exe`/`.com`, как набирают).
    pub fn matching(&self, prefix: &str) -> Vec<String> {
        let p = prefix.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (_, _, names) in &self.dirs {
            for n in names.iter().filter(|n| n.lower.starts_with(&p)) {
                let short = n
                    .name
                    .rsplit_once('.')
                    .filter(|(_, e)| matches!(e.to_lowercase().as_str(), "exe" | "com"))
                    .map_or(n.name.as_str(), |(s, _)| s);
                if seen.insert(short.to_lowercase()) {
                    out.push(short.to_owned());
                }
            }
        }
        out.sort_by_key(|s| s.to_lowercase());
        out
    }
}

/// Ghost-текст: хвост последней строки истории, начинающейся с `line` (с учётом
/// регистра набранного — хвост из истории).
pub fn ghost<'a>(history: &'a History, line: &str) -> Option<&'a str> {
    if line.trim().is_empty() {
        return None;
    }
    history
        .entries()
        .iter()
        .rev()
        .find(|e| e.cmd.len() > line.len() && e.cmd.starts_with(line) && !e.cmd.contains('\n'))
        .map(|e| &e.cmd[line.len()..])
}

/// Вход комплита.
#[derive(Debug)]
pub struct Query<'a> {
    pub line: &'a str,
    pub cursor: usize,
    pub style: Style,
    pub cwd: &'a Path,
}

/// Собрать варианты: пути (из листинга `names` каталога `q`) > PATH > история, без
/// повторов. `names` — листинг каталога [`path_query`] этого слова.
pub fn candidates(
    q: &Query<'_>,
    names: &[Name],
    commands: Option<&PathCommands>,
    history: Option<&History>,
) -> Vec<Candidate> {
    let (n, word) = quote::word_at(q.line, q.cursor, q.style);
    let range = word.start..word.end;
    let pq = path_query(&word.text, q.cwd);
    // Первое слово `cd` — только каталоги.
    let first = quote::words(q.line, q.style)
        .first()
        .map(|w| w.text.to_lowercase());
    let dirs_only = n > 0
        && first
            .as_deref()
            .is_some_and(|f| matches!(f, "cd" | "chdir" | "pushd" | "set-location" | "sl"));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (text, dir) in paths(&pq, names, dirs_only) {
        if seen.insert(text.to_lowercase()) {
            out.push(Candidate {
                label: text.clone(),
                insert: quote::quote(&text, q.style),
                range: range.clone(),
                source: Source::Path,
                dir,
            });
        }
    }
    if n == 0
        && pq.typed_dir.is_empty()
        && let Some(c) = commands
    {
        for name in c.matching(&word.text) {
            if seen.insert(name.to_lowercase()) {
                out.push(Candidate {
                    label: name.clone(),
                    insert: quote::quote(&name, q.style),
                    range: range.clone(),
                    source: Source::Command,
                    dir: false,
                });
            }
        }
    }
    if let Some(h) = history
        && q.cursor == q.line.len()
        && !q.line.trim().is_empty()
    {
        let typed = q.line.to_lowercase();
        let mut hist_seen = std::collections::HashSet::new();
        for e in h.entries().iter().rev() {
            if e.lower().starts_with(&typed)
                && e.cmd.len() > q.line.len()
                && !e.cmd.contains('\n')
                && hist_seen.insert(e.cmd.clone())
            {
                out.push(Candidate {
                    label: e.cmd.clone(),
                    insert: e.cmd.clone(),
                    range: 0..q.line.len(),
                    source: Source::History,
                    dir: false,
                });
                if hist_seen.len() >= 50 {
                    break;
                }
            }
        }
    }
    out
}

/// Поставить вариант в строку: новый текст и позиция курсора.
pub fn apply(line: &str, c: &Candidate) -> (String, usize) {
    let mut s = String::with_capacity(line.len() + c.insert.len());
    s.push_str(&line[..c.range.start]);
    s.push_str(&c.insert);
    let cur = s.len();
    s.push_str(&line[c.range.end..]);
    (s, cur)
}

#[cfg(test)]
#[path = "complete_tests.rs"]
mod tests;
