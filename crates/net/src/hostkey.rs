//! Ключи серверов (F03 §2.4, F03-02): общий `~/.ssh/known_hosts` (и `known_hosts2`),
//! временный файл сеанса для «Только в этот раз». Хэшированные имена (`|1|`) находятся,
//! `@cert-authority` и `@revoked` учитываются, остальные маркеры — нет.
//!
//! Запись: «Доверять» дописывает одну строку в конец файла, остальные байты не трогаются;
//! «Заменить» меняет только строку N, файл переписывается атомарно с копией
//! `known_hosts.urfin-bak`.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit as _, Mac as _};
use russh::keys::ssh_key::known_hosts::{Entry, HostPatterns, Marker};
use russh::keys::{Certificate, HashAlg, PublicKey};
use sha1::Sha1;

/// Имя для сопоставления: `[host]:port`, если порт ≠22, иначе `host`.
pub fn host_name(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

/// `SHA256:…` — как у `ssh-keygen -l`.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Итог проверки ключа сервера.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Known,
    Unknown,
    /// Для имени есть другой ключ того же типа: где он записан.
    Changed {
        file: PathBuf,
        line: usize,
        old: PublicKey,
    },
    /// Ключ (или CA сертификата) помечен `@revoked`.
    Revoked,
}

/// Одна разобранная строка.
#[derive(Clone, Debug)]
struct Record {
    file: PathBuf,
    /// С 1, как в `known_hosts:N`.
    line: usize,
    marker: Option<Marker>,
    patterns: HostPatterns,
    key: PublicKey,
}

/// Записи всех файлов `known_hosts` по порядку. Битые строки пропускаются.
#[derive(Clone, Debug, Default)]
pub struct KnownHosts {
    lines: Vec<Record>,
}

impl KnownHosts {
    /// Читает файлы; отсутствующие пропускаются.
    pub fn load(files: &[PathBuf]) -> KnownHosts {
        let mut kh = KnownHosts::default();
        for f in files {
            match fs::read_to_string(f) {
                Ok(text) => kh.parse(f, &text),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(target: "net", file = %f.display(), error = %e, "known_hosts");
                }
            }
        }
        kh
    }

    fn parse(&mut self, file: &Path, text: &str) {
        for (i, raw) in text.lines().enumerate() {
            let s = raw.trim();
            if s.is_empty() || s.starts_with('#') {
                continue;
            }
            // Битые строки и чужие маркеры не разбираются.
            let Ok(e) = s.parse::<Entry>() else {
                continue;
            };
            self.lines.push(Record {
                file: file.to_path_buf(),
                line: i + 1,
                marker: e.marker().copied(),
                patterns: e.host_patterns().clone(),
                key: e.public_key().clone(),
            });
        }
    }

    /// Проверка ключа сервера `name` (см. [`host_name`]).
    pub fn check(&self, name: &str, key: &PublicKey) -> Verdict {
        let same = |a: &PublicKey| a.key_data() == key.key_data();
        let lines = || self.lines.iter().filter(|l| matches(&l.patterns, name));
        if lines().any(|l| l.marker == Some(Marker::Revoked) && same(&l.key)) {
            return Verdict::Revoked;
        }
        let plain = || lines().filter(|l| l.marker.is_none());
        if plain().any(|l| same(&l.key)) {
            return Verdict::Known;
        }
        match plain().find(|l| l.key.algorithm() == key.algorithm()) {
            Some(l) => Verdict::Changed {
                file: l.file.clone(),
                line: l.line,
                old: l.key.clone(),
            },
            None => Verdict::Unknown,
        }
    }

    /// Имена хостов для автодополнения «Хоста» (`screen-sshnew`): открытые записи без
    /// шаблонов; `[host]:port` — как `host`. Хэшированные (`|1|`) не читаются.
    pub fn hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in self.lines.iter().filter(|l| l.marker.is_none()) {
            let HostPatterns::Patterns(list) = &l.patterns else {
                continue;
            };
            for p in list {
                if p.starts_with('!') || p.contains(['*', '?']) {
                    continue;
                }
                let h = p
                    .strip_prefix('[')
                    .and_then(|r| r.split_once(']'))
                    .map_or(p.as_str(), |(h, _)| h);
                if !h.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(h)) {
                    out.push(h.to_owned());
                }
            }
        }
        out
    }

    /// Ключ сервера `name` из `known_hosts` для «Свойств» профиля: алгоритм и отпечаток
    /// первой подходящей записи без маркера.
    pub fn key_of(&self, name: &str) -> Option<(String, String)> {
        self.lines
            .iter()
            .find(|l| l.marker.is_none() && matches(&l.patterns, name))
            .map(|l| (l.key.algorithm().to_string(), fingerprint(&l.key)))
    }

    /// Сертификат хоста: подписан ли он CA из `@cert-authority` для `name`.
    pub fn check_cert(&self, name: &str, cert: &Certificate) -> Verdict {
        let ca = cert.signature_key();
        let lines = || self.lines.iter().filter(|l| matches(&l.patterns, name));
        if lines().any(|l| l.marker == Some(Marker::Revoked) && l.key.key_data() == ca) {
            return Verdict::Revoked;
        }
        if lines().any(|l| l.marker == Some(Marker::CertAuthority) && l.key.key_data() == ca) {
            Verdict::Known
        } else {
            Verdict::Unknown
        }
    }
}

/// Список шаблонов OpenSSH: совпал отрицательный — нет; иначе — любой положительный.
fn matches(patterns: &HostPatterns, name: &str) -> bool {
    match patterns {
        HostPatterns::HashedName { salt, hash } => {
            let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(salt) else {
                return false;
            };
            mac.update(name.as_bytes());
            mac.verify_slice(hash).is_ok()
        }
        HostPatterns::Patterns(list) => {
            let name = name.to_ascii_lowercase();
            let mut hit = false;
            for p in list {
                let p = p.to_ascii_lowercase();
                if let Some(neg) = p.strip_prefix('!') {
                    if glob(neg.as_bytes(), name.as_bytes()) {
                        return false;
                    }
                } else if glob(p.as_bytes(), name.as_bytes()) {
                    hit = true;
                }
            }
            hit
        }
    }
}

/// `*` и `?` как в OpenSSH `match_pattern`.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && glob(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

fn entry_line(name: &str, key: &PublicKey) -> io::Result<String> {
    let mut key = key.clone();
    key.set_comment("");
    let text = key.to_openssh().map_err(io::Error::other)?;
    Ok(format!("{name} {}", text.trim_end()))
}

/// «Доверять»: одна строка в конец файла; прежние байты не меняются. Нет файла — создаётся
/// (с папкой).
pub fn append(file: &Path, name: &str, key: &PublicKey) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let line = entry_line(name, key)?;
    let tail_newline = match fs::read(file) {
        Ok(b) => b.is_empty() || b.ends_with(b"\n"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    let sep = if tail_newline { "" } else { "\n" };
    f.write_all(format!("{sep}{line}\n").as_bytes())?;
    f.sync_all()
}

/// «Заменить ключ»: строка `line` (с 1) получает новый ключ; копия — `<файл>.urfin-bak`,
/// запись — через временный файл и переименование.
pub fn replace(file: &Path, line: usize, name: &str, key: &PublicKey) -> io::Result<()> {
    let old = fs::read_to_string(file)?;
    let new_line = entry_line(name, key)?;
    let mut out = String::with_capacity(old.len() + new_line.len());
    let mut found = false;
    for (i, l) in old.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            found = true;
            out.push_str(&new_line);
            out.push_str(if l.ends_with("\r\n") {
                "\r\n"
            } else if l.ends_with('\n') {
                "\n"
            } else {
                ""
            });
        } else {
            out.push_str(l);
        }
    }
    if !found {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("no line {line}"),
        ));
    }
    let mut bak = file.as_os_str().to_owned();
    bak.push(".urfin-bak");
    fs::copy(file, PathBuf::from(bak))?;
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(".urfin-tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, out)?;
    fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use russh::keys::PrivateKey;
    use russh::keys::ssh_key::private::Ed25519Keypair;

    use super::*;

    fn key(seed: u8) -> PublicKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
            .public_key()
            .clone()
    }

    fn line(name: &str, k: &PublicKey) -> String {
        entry_line(name, k).unwrap()
    }

    fn hashed(name: &str, k: &PublicKey) -> String {
        let salt = [7u8; 20];
        let mut mac = Hmac::<Sha1>::new_from_slice(&salt).unwrap();
        mac.update(name.as_bytes());
        let hash = mac.finalize().into_bytes();
        let text = line("x", k);
        let key = text.split_once(' ').unwrap().1;
        format!(
            "|1|{}|{} {key}",
            STANDARD.encode(salt),
            STANDARD.encode(hash)
        )
    }

    fn known(text: &str) -> KnownHosts {
        let mut kh = KnownHosts::default();
        kh.parse(Path::new("known_hosts"), text);
        kh
    }

    #[test]
    fn name_has_port_only_when_not_22() {
        assert_eq!(host_name("srv1", 22), "srv1");
        assert_eq!(host_name("srv1", 2222), "[srv1]:2222");
    }

    #[test]
    fn fingerprint_is_sha256() {
        assert!(fingerprint(&key(1)).starts_with("SHA256:"));
    }

    #[test]
    fn known_unknown_changed() {
        let a = key(1);
        let b = key(2);
        let kh = known(&format!("# comment\n\n{}\n", line("srv1,10.0.0.1", &a)));
        assert_eq!(kh.check("srv1", &a), Verdict::Known);
        assert_eq!(kh.check("10.0.0.1", &a), Verdict::Known);
        assert_eq!(kh.check("srv2", &a), Verdict::Unknown);
        assert_eq!(
            kh.check("srv1", &b),
            Verdict::Changed {
                file: PathBuf::from("known_hosts"),
                line: 3,
                old: a.clone(),
            }
        );
        // Порт ≠22 — другое имя.
        assert_eq!(kh.check("[srv1]:2222", &a), Verdict::Unknown);
    }

    #[test]
    fn hosts_for_completion() {
        let a = key(1);
        let kh = known(&format!(
            "{}
{}
{}
{}
",
            line("srv1.corp.local,10.0.0.1", &a),
            line("[srv10.corp.local]:2222,*.lab,!bad", &a),
            hashed("secret", &a),
            line("SRV1.corp.local", &a),
        ));
        assert_eq!(
            kh.hosts(),
            ["srv1.corp.local", "10.0.0.1", "srv10.corp.local"]
        );
        let (algo, fp) = kh.key_of("10.0.0.1").unwrap();
        assert_eq!((algo.as_str(), fp), ("ssh-ed25519", fingerprint(&a)));
        assert!(kh.key_of("[srv10.corp.local]:2222").is_some());
        assert_eq!(kh.key_of("nope"), None);
    }

    #[test]
    fn hashed_names_are_found() {
        let a = key(1);
        let kh = known(&format!("{}\n", hashed("[srv1]:2222", &a)));
        assert_eq!(kh.check("[srv1]:2222", &a), Verdict::Known);
        assert_eq!(kh.check("srv1", &a), Verdict::Unknown);
    }

    #[test]
    fn wildcards_and_negation() {
        let a = key(1);
        let kh = known(&format!("{}\n", line("*.corp,!bad.corp", &a)));
        assert_eq!(kh.check("web.corp", &a), Verdict::Known);
        assert_eq!(kh.check("WEB.CORP", &a), Verdict::Known);
        assert_eq!(kh.check("bad.corp", &a), Verdict::Unknown);
        assert!(glob(b"srv?", b"srv1"));
        assert!(!glob(b"srv?", b"srv12"));
    }

    #[test]
    fn revoked_and_other_markers() {
        let a = key(1);
        let kh = known(&format!(
            "{}\n@revoked {}\n@unknown {}\n",
            line("srv1", &a),
            line("srv1", &a),
            line("srv2", &a)
        ));
        assert_eq!(kh.check("srv1", &a), Verdict::Revoked);
        assert_eq!(kh.check("srv2", &a), Verdict::Unknown);
        // `@cert-authority` — не ключ хоста.
        let kh = known(&format!("@cert-authority {}\n", line("*", &a)));
        assert_eq!(kh.check("srv1", &a), Verdict::Unknown);
    }

    #[test]
    fn append_keeps_other_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("known_hosts");
        // Без перевода строки в конце, с CRLF и комментарием.
        let before = format!("# mine\r\n{}\r\nbroken line", line("srv1", &key(1)));
        fs::write(&f, &before).unwrap();
        append(&f, "[srv2]:2222", &key(2)).unwrap();
        let after = fs::read_to_string(&f).unwrap();
        assert!(after.starts_with(&before));
        let added = &after[before.len()..];
        assert_eq!(added, format!("\n{}\n", line("[srv2]:2222", &key(2))));
        assert_eq!(
            KnownHosts::load(&[f]).check("[srv2]:2222", &key(2)),
            Verdict::Known
        );
    }

    #[test]
    fn append_creates_file_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join(".ssh").join("known_hosts");
        append(&f, "srv1", &key(1)).unwrap();
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            format!("{}\n", line("srv1", &key(1)))
        );
    }

    #[test]
    fn replace_changes_only_line_n() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("known_hosts");
        let before = format!(
            "{}\r\n{}\r\n# tail",
            line("srv0", &key(9)),
            line("srv1", &key(1))
        );
        fs::write(&f, &before).unwrap();
        replace(&f, 2, "srv1", &key(2)).unwrap();
        let after = fs::read_to_string(&f).unwrap();
        assert_eq!(
            after,
            format!(
                "{}\r\n{}\r\n# tail",
                line("srv0", &key(9)),
                line("srv1", &key(2))
            )
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("known_hosts.urfin-bak")).unwrap(),
            before
        );
        assert!(replace(&f, 9, "srv1", &key(2)).is_err());
    }
}
