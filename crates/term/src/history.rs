//! История команд (S6-16, S6 §6.1): по shell-ключу (`pwsh`, `cmd`, `wsl:Ubuntu`) в
//! `state_dir/term/history/<ключ>.jsonl`, строка — `{"t":…,"cmd":"…","cwd":"…",
//! "exit":…,"lock":false}`.
//!
//! - Повтор переносится в конец (старая запись уходит), предел — `term.history.max`;
//!   закреплённые (`lock`) не вытесняются.
//! - Не пишутся строки с ведущим пробелом (`ignore_space`) и подходящие под маски
//!   `exclude` (`*password*`): ни в память, ни на диск.
//! - Диск: дописывание на полосе Bulk; при загрузке — дедуп (последняя запись
//!   побеждает) и компакция, если строк больше `2 × max`. Битая строка пропускается
//!   с предупреждением в лог. Удаление и закрепление переписывают файл целиком.
//! - Нативные истории (`PSReadLine`, bash, zsh, fish) — только чтение, с пометкой;
//!   идут раньше своих записей и в файл не пишутся.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use urfin_core::{Lane, Runtime};

use crate::config::HistoryConfig;

/// Запись истории.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub cmd: String,
    /// Время, секунды Unix.
    pub t: u64,
    pub cwd: Option<String>,
    pub exit: Option<i32>,
    /// Закреплена (Ins в Alt+F8): не вытесняется пределом.
    pub lock: bool,
    /// Из нативной истории shell: только чтение.
    pub native: bool,
    /// `cmd` в нижнем регистре — фильтр Alt+F8 без аллокаций на нажатие.
    lower: String,
}

impl Entry {
    pub fn new(cmd: &str, t: u64) -> Entry {
        Entry {
            cmd: cmd.to_owned(),
            t,
            cwd: None,
            exit: None,
            lock: false,
            native: false,
            lower: cmd.to_lowercase(),
        }
    }

    pub fn lower(&self) -> &str {
        &self.lower
    }
}

/// История одного shell: старые — в начале.
#[derive(Clone, Debug, Default)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Добавить; повтор переносится в конец (закрепление сохраняется).
    pub fn push(&mut self, mut e: Entry, max: usize) {
        if let Some(i) = self.entries.iter().position(|x| x.cmd == e.cmd) {
            let old = self.entries.remove(i);
            e.lock |= old.lock && !old.native;
        }
        self.entries.push(e);
        self.cap(max);
    }

    /// Записи с диска (своя история): до уже добавленных в этом запуске.
    pub fn load(&mut self, own: Vec<Entry>, max: usize) {
        let newer = std::mem::take(&mut self.entries);
        let (native, mine): (Vec<_>, Vec<_>) = newer.into_iter().partition(|e| e.native);
        self.entries = native;
        for e in own.into_iter().chain(mine) {
            self.push(e, max);
        }
    }

    /// Нативная история: в начало, без записей, которые уже есть свои.
    pub fn import(&mut self, native: Vec<Entry>, max: usize) {
        let mut seen: std::collections::HashSet<String> =
            self.entries.iter().map(|e| e.cmd.clone()).collect();
        let mut add: Vec<Entry> = Vec::new();
        // С конца: из повторов остаётся последний.
        for mut e in native.into_iter().rev() {
            if seen.insert(e.cmd.clone()) {
                e.native = true;
                add.push(e);
            }
            if add.len() >= max {
                break;
            }
        }
        add.reverse();
        add.append(&mut self.entries);
        self.entries = add;
        self.cap(max);
    }

    /// Предел: вытесняются старые незакреплённые (нативные — первыми).
    fn cap(&mut self, max: usize) {
        let max = max.max(1);
        while self.entries.len() > max {
            let i = self
                .entries
                .iter()
                .position(|e| e.native)
                .or_else(|| self.entries.iter().position(|e| !e.lock));
            match i {
                Some(i) => {
                    self.entries.remove(i);
                }
                None => break,
            }
        }
    }

    /// Удалить запись (Del в Alt+F8); `true` — была своя (файл переписать).
    pub fn remove(&mut self, i: usize) -> bool {
        if i < self.entries.len() {
            !self.entries.remove(i).native
        } else {
            false
        }
    }

    /// Закрепить или открепить (Ins в Alt+F8); нативные не закрепляются.
    pub fn toggle_lock(&mut self, i: usize) -> bool {
        match self.entries.get_mut(i) {
            Some(e) if !e.native => {
                e.lock = !e.lock;
                true
            }
            _ => false,
        }
    }

    /// Свои записи — для перезаписи файла.
    pub fn own(&self) -> Vec<Entry> {
        self.entries.iter().filter(|e| !e.native).cloned().collect()
    }

    /// Ctrl+E / ↑ (Far): предыдущая перед `before` команда, начинающаяся с `prefix`
    /// (без учёта регистра).
    pub fn prev(&self, prefix: &str, before: usize) -> Option<usize> {
        let p = prefix.to_lowercase();
        (0..before.min(self.entries.len()))
            .rev()
            .find(|&i| self.entries[i].lower.starts_with(&p) && self.entries[i].lower != p)
    }

    /// Ctrl+X / ↓: следующая после `after`.
    pub fn next(&self, prefix: &str, after: usize) -> Option<usize> {
        let p = prefix.to_lowercase();
        (after + 1..self.entries.len())
            .find(|&i| self.entries[i].lower.starts_with(&p) && self.entries[i].lower != p)
    }
}

/// Строку не сохранять: ведущий пробел (`ignore_space`) или маска `exclude`.
pub fn excluded(cfg: &HistoryConfig, cmd: &str) -> bool {
    if cmd.trim().is_empty() {
        return true;
    }
    if cfg.ignore_space && cmd.starts_with([' ', '\t']) {
        return true;
    }
    let lower = cmd.to_lowercase();
    cfg.exclude.iter().any(|m| glob(&m.to_lowercase(), &lower))
}

/// Маска `*` и `?` (вся строка).
pub fn glob(mask: &str, text: &str) -> bool {
    let (m, t): (Vec<char>, Vec<char>) = (mask.chars().collect(), text.chars().collect());
    let (mut mi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if mi < m.len() && (m[mi] == '?' || m[mi] == t[ti]) {
            mi += 1;
            ti += 1;
        } else if mi < m.len() && m[mi] == '*' {
            star = Some((mi, ti));
            mi += 1;
        } else if let Some((sm, st)) = star {
            mi = sm + 1;
            ti = st + 1;
            star = Some((sm, st + 1));
        } else {
            return false;
        }
    }
    m[mi..].iter().all(|&c| c == '*')
}

/// Имя файла по ключу shell: `wsl:Ubuntu` → `wsl%3AUbuntu.jsonl` (обратимо).
pub fn file_name(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 6);
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_') {
            out.push(char::from(b));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{b:02X}"));
        }
    }
    out.push_str(".jsonl");
    out
}

/// Ключ shell по имени файла без расширения.
pub fn key_of(stem: &str) -> String {
    let b = stem.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = stem
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- jsonl ----

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = std::fmt::Write::write_fmt(out, format_args!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Запись → строка jsonl (без перевода строки).
pub fn to_line(e: &Entry) -> String {
    let mut s = String::with_capacity(e.cmd.len() + 64);
    s.push_str("{\"t\":");
    s.push_str(&e.t.to_string());
    s.push_str(",\"cmd\":");
    json_str(&mut s, &e.cmd);
    s.push_str(",\"cwd\":");
    match &e.cwd {
        Some(c) => json_str(&mut s, c),
        None => s.push_str("null"),
    }
    s.push_str(",\"exit\":");
    match e.exit {
        Some(x) => s.push_str(&x.to_string()),
        None => s.push_str("null"),
    }
    s.push_str(",\"lock\":");
    s.push_str(if e.lock { "true" } else { "false" });
    s.push('}');
    s
}

/// Значение плоского JSON-объекта.
#[derive(Debug, PartialEq)]
enum Json {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

/// Разбор плоского объекта `{"k":v,…}` (строки, числа, bool, null).
fn parse_object(s: &str) -> Option<Vec<(String, Json)>> {
    let mut p = s.trim().chars().peekable();
    let ws = |p: &mut std::iter::Peekable<std::str::Chars<'_>>| {
        while p.next_if(|c| c.is_whitespace()).is_some() {}
    };
    let string = |p: &mut std::iter::Peekable<std::str::Chars<'_>>| -> Option<String> {
        if p.next()? != '"' {
            return None;
        }
        let mut out = String::new();
        loop {
            match p.next()? {
                '"' => return Some(out),
                '\\' => match p.next()? {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let hex: String = (0..4).filter_map(|_| p.next()).collect();
                        let mut code = u32::from_str_radix(&hex, 16).ok()?;
                        // Суррогатная пара.
                        if (0xD800..0xDC00).contains(&code) {
                            if p.next()? != '\\' || p.next()? != 'u' {
                                return None;
                            }
                            let lo: String = (0..4).filter_map(|_| p.next()).collect();
                            let lo = u32::from_str_radix(&lo, 16).ok()?;
                            code = 0x10000 + ((code - 0xD800) << 10) + (lo.checked_sub(0xDC00)?);
                        }
                        out.push(char::from_u32(code)?);
                    }
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
    };
    ws(&mut p);
    if p.next()? != '{' {
        return None;
    }
    let mut fields = Vec::new();
    ws(&mut p);
    if p.next_if_eq(&'}').is_some() {
        return Some(fields);
    }
    loop {
        ws(&mut p);
        let k = string(&mut p)?;
        ws(&mut p);
        if p.next()? != ':' {
            return None;
        }
        ws(&mut p);
        let v = match *p.peek()? {
            '"' => Json::Str(string(&mut p)?),
            't' | 'f' | 'n' => {
                let word: String =
                    std::iter::from_fn(|| p.next_if(char::is_ascii_alphabetic)).collect();
                match word.as_str() {
                    "true" => Json::Bool(true),
                    "false" => Json::Bool(false),
                    "null" => Json::Null,
                    _ => return None,
                }
            }
            _ => {
                let num: String = std::iter::from_fn(|| {
                    p.next_if(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
                })
                .collect();
                Json::Num(num.parse().ok()?)
            }
        };
        fields.push((k, v));
        ws(&mut p);
        match p.next()? {
            ',' => {}
            '}' => break,
            _ => return None,
        }
    }
    ws(&mut p);
    p.next().is_none().then_some(fields)
}

/// Строка jsonl → запись; `None` — битая (нет `cmd` или не JSON).
pub fn from_line(line: &str) -> Option<Entry> {
    let mut e = Entry::new("", 0);
    let mut has_cmd = false;
    for (k, v) in parse_object(line)? {
        match (k.as_str(), v) {
            ("cmd", Json::Str(s)) => {
                e.lower = s.to_lowercase();
                e.cmd = s;
                has_cmd = true;
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "секунды Unix"
            )]
            ("t", Json::Num(n)) if n >= 0.0 => e.t = n as u64,
            ("cwd", Json::Str(s)) => e.cwd = Some(s),
            #[allow(clippy::cast_possible_truncation, reason = "код выхода")]
            ("exit", Json::Num(n)) => e.exit = Some(n as i32),
            ("lock", Json::Bool(b)) => e.lock = b,
            _ => {}
        }
    }
    (has_cmd && !e.cmd.is_empty()).then_some(e)
}

/// Разобрать файл: записи (дедуп — побеждает последняя) и число строк в файле.
/// Битые строки пропускаются с предупреждением.
pub fn parse(text: &str, max: usize, file: &Path) -> (Vec<Entry>, usize) {
    let mut h = History::default();
    let mut lines = 0;
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        lines += 1;
        match from_line(line) {
            Some(e) => h.push(e, usize::MAX),
            None => {
                tracing::warn!(file = %file.display(), line = n + 1, "term: broken history line skipped");
            }
        }
    }
    h.cap(max);
    (h.entries, lines)
}

/// Загрузить файл истории; строк больше `2 × max` — переписать сжатым.
pub fn load_file(path: &Path, max: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let (entries, lines) = parse(&text, max, path);
    if lines > 2 * max.max(1)
        && let Err(e) = rewrite(path, &entries)
    {
        tracing::warn!(error = %e, file = %path.display(), "term: history compaction");
    }
    entries
}

/// Дописать запись.
pub fn append(path: &Path, e: &Entry) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = to_line(e);
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// Переписать файл целиком (через временный и rename).
pub fn rewrite(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut text = String::new();
    for e in entries.iter().filter(|e| !e.native) {
        text.push_str(&to_line(e));
        text.push('\n');
    }
    let tmp = path.with_extension("jsonl~");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

// ---- нативные истории ----

/// `PSReadLine` `ConsoleHost_history.txt`: строка на команду, обратный апостроф в конце —
/// продолжение.
pub fn parse_psreadline(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if let Some(head) = line.strip_suffix('`') {
            cur.push_str(head);
            cur.push('\n');
            continue;
        }
        cur.push_str(line);
        let cmd = std::mem::take(&mut cur);
        if !cmd.trim().is_empty() {
            out.push(cmd);
        }
    }
    out
}

/// `~/.bash_history`: строка на команду; `#время` — служебные строки.
pub fn parse_bash(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| {
            let stamp = l.starts_with('#') && l[1..].chars().all(|c| c.is_ascii_digit());
            !l.trim().is_empty() && !stamp
        })
        .map(str::to_owned)
        .collect()
}

/// `~/.zsh_history` (extended: `: время:длительность;команда`, `\` в конце —
/// продолжение).
pub fn parse_zsh(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let body = match cur.take() {
            Some(mut c) => {
                c.push('\n');
                c.push_str(line);
                c
            }
            None => match line.strip_prefix(": ").and_then(|r| r.split_once(';')) {
                Some((_, cmd)) => cmd.to_owned(),
                None => line.to_owned(),
            },
        };
        if let Some(head) = body.strip_suffix('\\') {
            cur = Some(head.to_owned());
        } else if !body.trim().is_empty() {
            out.push(body);
        }
    }
    out
}

/// fish `fish_history`: `- cmd: команда` (YAML-подобно, `\n` и `\\` экранированы).
pub fn parse_fish(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("- cmd: "))
        .map(|c| c.replace("\\n", "\n").replace("\\\\", "\\"))
        .filter(|c| !c.trim().is_empty())
        .collect()
}

/// Разбор нативной истории: текст файла → команды.
pub type NativeParse = fn(&str) -> Vec<String>;

/// Где лежит нативная история shell и как её читать.
pub fn native_source(kind: &crate::shells::ShellKind) -> Option<(PathBuf, NativeParse)> {
    use crate::shells::ShellKind as K;
    let home = || std::env::var_os("USERPROFILE").map(PathBuf::from);
    match kind {
        K::Pwsh | K::WinPowerShell => {
            let appdata = std::env::var_os("APPDATA").map(PathBuf::from)?;
            Some((
                appdata.join(r"Microsoft\Windows\PowerShell\PSReadLine\ConsoleHost_history.txt"),
                parse_psreadline,
            ))
        }
        K::Bash => Some((home()?.join(".bash_history"), parse_bash)),
        K::Zsh => Some((home()?.join(".zsh_history"), parse_zsh)),
        K::Fish => Some((home()?.join(r".local\share\fish\fish_history"), parse_fish)),
        // cmd истории не хранит; WSL — внутри дистрибутива (не читаем).
        _ => None,
    }
}

// ---- состояние в TermState ----

/// Истории всех shell и каталог файлов.
#[derive(Debug, Default)]
pub struct Store {
    pub by_shell: HashMap<CompactString, History>,
    pub dir: Option<PathBuf>,
    /// Файлы прочитаны (иначе загрузка ещё идёт).
    pub loaded: bool,
    /// Нативные истории уже импортированы.
    pub(crate) imported: bool,
}

impl Store {
    pub fn get(&self, shell: &str) -> Option<&History> {
        self.by_shell.get(shell)
    }

    fn path(&self, shell: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(file_name(shell)))
    }

    /// Добавить строку (submit, `633;E`), если её можно хранить; дописать в файл.
    pub fn record(&mut self, cfg: &HistoryConfig, rt: &Runtime, shell: &str, e: Entry) {
        if excluded(cfg, &e.cmd) {
            return;
        }
        let max = cfg.max as usize;
        self.by_shell
            .entry(shell.into())
            .or_default()
            .push(e.clone(), max);
        if let Some(path) = self.path(shell) {
            rt.spawn(Lane::Bulk, "term.history.append", rt.token(), move |_| {
                if let Err(err) = append(&path, &e) {
                    tracing::warn!(error = %err, file = %path.display(), "term: history append");
                }
            });
        }
    }

    /// Переписать файл shell (удаление, закрепление).
    pub fn save(&self, rt: &Runtime, shell: &str) {
        let (Some(path), Some(h)) = (self.path(shell), self.by_shell.get(shell)) else {
            return;
        };
        let own = h.own();
        rt.spawn(Lane::Bulk, "term.history.save", rt.token(), move |_| {
            if let Err(err) = rewrite(&path, &own) {
                tracing::warn!(error = %err, file = %path.display(), "term: history save");
            }
        });
    }

    /// Прочитать все файлы каталога (полоса Bulk) → [`LoadedMsg`].
    pub(crate) fn spawn_load(&self, rt: &Runtime, max: usize) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        rt.spawn(Lane::Bulk, "term.history.load", rt.token(), move |t| {
            let mut out = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "jsonl")
                        && let Some(key) = p.file_stem().and_then(|s| s.to_str())
                    {
                        out.push((key_of(key).into(), load_file(&p, max)));
                    }
                }
            }
            let _ = t.sink.send(
                Box::new(LoadedMsg {
                    own: out,
                    native: false,
                }),
                &t.cancel,
            );
        });
    }

    /// Нативные истории shells (полоса Bulk).
    pub(crate) fn spawn_import(
        &mut self,
        rt: &Runtime,
        shells: &[crate::shells::ShellSpec],
        max: usize,
    ) {
        if self.imported {
            return;
        }
        self.imported = true;
        let jobs: Vec<(CompactString, PathBuf, NativeParse)> = shells
            .iter()
            .filter_map(|s| native_source(&s.kind).map(|(p, f)| (s.id.clone(), p, f)))
            .collect();
        if jobs.is_empty() {
            return;
        }
        rt.spawn(Lane::Bulk, "term.history.import", rt.token(), move |t| {
            let mut out = Vec::new();
            for (id, path, parse) in jobs {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let text = String::from_utf8_lossy(&bytes);
                let cmds = parse(&text);
                let skip = cmds.len().saturating_sub(max);
                let entries = cmds[skip..].iter().map(|c| Entry::new(c, 0)).collect();
                out.push((id, entries));
            }
            let _ = t.sink.send(
                Box::new(LoadedMsg {
                    own: out,
                    native: true,
                }),
                &t.cancel,
            );
        });
    }
}

/// Файлы истории прочитаны.
struct LoadedMsg {
    own: Vec<(CompactString, Vec<Entry>)>,
    native: bool,
}

impl urfin_core::Msg for LoadedMsg {
    fn name(&self) -> &'static str {
        "term.history"
    }

    fn apply(self: Box<Self>, cx: &mut urfin_core::Cx<'_>) {
        let Some(st) = cx.state.ext.get_mut::<crate::TermState>() else {
            return;
        };
        let max = st.config.history.max as usize;
        for (key, entries) in self.own {
            let h = st.history.by_shell.entry(key.clone()).or_default();
            if self.native {
                h.import(entries, max);
            } else {
                h.load(entries, max);
            }
        }
        if !self.native {
            st.history.loaded = true;
        }
    }
}

/// Записать строку команды в историю shell (submit, `633;E`).
pub(crate) fn record(st: &mut crate::TermState, rt: &Runtime, shell: &str, line: &str) {
    let mut e = Entry::new(line, now());
    e.cwd = st
        .sessions
        .active()
        .and_then(|s| s.cwd.as_ref())
        .map(|p| p.display().to_string());
    let crate::TermState {
        config, history, ..
    } = st;
    history.record(&config.history, rt, shell, e);
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
