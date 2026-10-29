//! `~/.ssh/config` и его `Include` (F03 §2.2) — подмножество: `Host` без шаблонов
//! (`*`, `?`, `!`), `HostName`, `User`, `Port`, `IdentityFile`, `ProxyJump`,
//! `ServerAliveInterval`. Блоки `Match` пропускаются с подписью «не поддерживается:
//! Match (строка N)»; прочие директивы — молча. Как у OpenSSH, побеждает первое
//! значение директивы для хоста.

use std::io;
use std::path::{Path, PathBuf};

use crate::config::NetError;
use crate::profile::{Auth, DEFAULT_PORT, Note, Profile, ProfileSource};

/// Глубина вложенных `Include`, как `READCONF_MAX_DEPTH` OpenSSH.
const MAX_DEPTH: u32 = 16;

/// Итог импорта.
#[derive(Clone, Debug, Default)]
pub struct SshConfig {
    pub profiles: Vec<Profile>,
    /// Прочитанные файлы (и `config`, даже если его нет) — для перечитывания по mtime.
    pub files: Vec<PathBuf>,
    pub errors: Vec<NetError>,
}

/// Читает `home/.ssh/config`. Нет файла — пусто, без ошибки.
pub fn load(home: &Path) -> SshConfig {
    let ssh_dir = home.join(".ssh");
    let mut p = Parser {
        home,
        ssh_dir: &ssh_dir,
        hosts: Vec::new(),
        notes: Vec::new(),
        out: SshConfig::default(),
    };
    p.file(&ssh_dir.join("config"), 0);
    p.finish()
}

/// Значения хоста; `None` — директива ещё не встречалась.
#[derive(Debug, Default)]
struct HostRec {
    name: String,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    proxy_jump: Option<String>,
    alive: Option<u32>,
}

/// К чему относятся директивы.
#[derive(Clone, Debug)]
enum Block {
    /// До первого `Host`/`Match` и в блоках без простых имён: не читаем.
    Skip,
    /// Индексы в `Parser::hosts`.
    Hosts(Vec<usize>),
}

struct Parser<'a> {
    home: &'a Path,
    ssh_dir: &'a Path,
    hosts: Vec<HostRec>,
    notes: Vec<Note>,
    out: SshConfig,
}

impl Parser<'_> {
    /// Файл целиком. Блок внешнего файла после `Include` восстанавливается (как у
    /// OpenSSH: включённый файл не меняет условие включившего). Петли `Include`
    /// обрывает глубина.
    fn file(&mut self, path: &Path, depth: u32) {
        self.out.files.push(path.to_path_buf());
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            // Отсутствующий файл OpenSSH пропускает молча.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                self.out.errors.push(NetError::io(path, &e));
                return;
            }
        };
        let mut block = Block::Skip;
        for (i, line) in text.lines().enumerate() {
            let Some((key, args)) = split(line) else {
                continue;
            };
            let line_no = u32::try_from(i + 1).unwrap_or(u32::MAX);
            match key.to_ascii_lowercase().as_str() {
                "host" => block = self.host_block(&args),
                "match" => {
                    block = Block::Skip;
                    self.notes.push(Note::Unsupported {
                        directive: key.to_owned(),
                        file: path.to_path_buf(),
                        line: line_no,
                    });
                }
                "include" if depth >= MAX_DEPTH => self
                    .out
                    .errors
                    .push(NetError::parse(path, "Include nested too deeply")),
                "include" => {
                    for arg in &args {
                        for inc in self.include_paths(arg) {
                            self.file(&inc, depth + 1);
                        }
                    }
                }
                other => {
                    if let (Block::Hosts(ids), Some(v)) = (&block, args.first()) {
                        for &id in ids {
                            set(&mut self.hosts[id], other, v);
                        }
                    }
                }
            }
        }
    }

    /// `Host a b *.lab !x`: профили — только простые имена.
    fn host_block(&mut self, patterns: &[String]) -> Block {
        let mut ids = Vec::new();
        for name in patterns.iter().filter(|p| !p.contains(['*', '?', '!'])) {
            let id = if let Some(id) = self.hosts.iter().position(|h| h.name == *name) {
                id
            } else {
                self.hosts.push(HostRec {
                    name: name.clone(),
                    ..HostRec::default()
                });
                self.hosts.len() - 1
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            Block::Skip
        } else {
            Block::Hosts(ids)
        }
    }

    /// Путь `Include`: `~` — домашний каталог, относительный — от `~/.ssh`; шаблон
    /// `*`/`?` — только в имени файла, совпадения по алфавиту.
    fn include_paths(&self, arg: &str) -> Vec<PathBuf> {
        let path = if arg == "~" {
            self.home.to_path_buf()
        } else if let Some(rest) = arg.strip_prefix("~/").or_else(|| arg.strip_prefix("~\\")) {
            self.home.join(rest)
        } else if Path::new(arg).is_absolute() {
            PathBuf::from(arg)
        } else {
            self.ssh_dir.join(arg)
        };
        let Some(pattern) = path.file_name().and_then(|n| n.to_str()) else {
            return vec![path];
        };
        if !pattern.contains(['*', '?']) {
            return vec![path];
        }
        let dir = path.parent().unwrap_or(self.ssh_dir);
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut found: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
            .filter(|e| e.file_name().to_str().is_some_and(|n| wildcard(pattern, n)))
            .map(|e| e.path())
            .collect();
        found.sort();
        found
    }

    fn finish(self) -> SshConfig {
        let Parser {
            hosts,
            notes,
            mut out,
            ..
        } = self;
        out.profiles = hosts
            .into_iter()
            .map(|h| {
                let host = h
                    .hostname
                    .map_or_else(|| h.name.clone(), |v| expand_host(&v, &h.name));
                Profile {
                    host,
                    port: h.port.unwrap_or(DEFAULT_PORT),
                    user: h.user.unwrap_or_default(),
                    auth: if h.identity.is_some() {
                        Auth::Key
                    } else {
                        Auth::Agent
                    },
                    key: h.identity.unwrap_or_default(),
                    via: h.proxy_jump.unwrap_or_default(),
                    source: ProfileSource::Config,
                    keepalive_s: h.alive,
                    // Блок `Match` мог относиться к любому хосту.
                    notes: notes.clone(),
                    ..Profile::new(h.name, String::new())
                }
            })
            .collect();
        out
    }
}

/// Первое значение директивы побеждает.
fn set(h: &mut HostRec, key: &str, v: &str) {
    fn first<T>(slot: &mut Option<T>, v: Option<T>) {
        if slot.is_none() {
            *slot = v;
        }
    }
    match key {
        "hostname" => first(&mut h.hostname, Some(v.to_owned())),
        "user" => first(&mut h.user, Some(v.to_owned())),
        "port" => first(&mut h.port, v.parse().ok()),
        "identityfile" => first(&mut h.identity, Some(v.to_owned())),
        // `none` — без jump, но значение занято.
        "proxyjump" => first(
            &mut h.proxy_jump,
            Some(if v.eq_ignore_ascii_case("none") {
                String::new()
            } else {
                v.to_owned()
            }),
        ),
        "serveraliveinterval" => first(&mut h.alive, v.parse().ok()),
        _ => {}
    }
}

/// `%h` в `HostName` — имя из `Host`, `%%` — `%`.
fn expand_host(v: &str, name: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(name),
            Some('%') | None => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
        }
    }
    out
}

/// Строка → ключевое слово и аргументы: `Key value`, `Key=value`, кавычки `"…"`,
/// `#` в начале строки или токена — комментарий.
fn split(line: &str) -> Option<(&str, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (key, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    Some((key, tokens(rest)))
}

fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '#' if !quoted && !started => break,
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Шаблон `*`/`?` по имени файла без учёта регистра ASCII.
fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_forms() {
        assert_eq!(split("  # comment"), None);
        assert_eq!(split(""), None);
        let (k, a) = split("HostName=10.0.0.1").unwrap();
        assert_eq!((k, a), ("HostName", vec!["10.0.0.1".to_owned()]));
        let (k, a) = split("Port = 2222 # ssh").unwrap();
        assert_eq!((k, a), ("Port", vec!["2222".to_owned()]));
        let (_, a) = split(r#"IdentityFile "C:\Users\a b\key" second"#).unwrap();
        assert_eq!(a, [r"C:\Users\a b\key", "second"]);
        let (_, a) = split("Host a  b\t*.lab").unwrap();
        assert_eq!(a, ["a", "b", "*.lab"]);
    }

    #[test]
    fn wildcard_matches() {
        assert!(wildcard("*.conf", "10-lab.conf"));
        assert!(wildcard("*.CONF", "x.conf"));
        assert!(wildcard("a?c*", "abcdef"));
        assert!(!wildcard("*.conf", "readme.txt"));
        assert!(wildcard("*", "anything"));
        assert!(!wildcard("a*b", "acbx"));
    }

    #[test]
    fn host_tokens() {
        assert_eq!(expand_host("%h.example.com", "web"), "web.example.com");
        assert_eq!(expand_host("a%%b%x", "web"), "a%b%x");
    }
}
