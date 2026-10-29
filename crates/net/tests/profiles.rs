//! Профили F03-01: `connections.toml` v1, импорт `~/.ssh/config` и `PuTTY` по фикстурам,
//! приоритет источников, секреты не попадают в файл.

use std::path::{Path, PathBuf};

use urfin_net::import::{putty, ssh_config};
use urfin_net::profile::{self, DEFAULT_PORT};
use urfin_net::{Auth, ConnectionsFile, Note, Profile, ProfileOverride, ProfileSource, Sources};

fn data(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(rel)
}

fn fixture_file() -> ConnectionsFile {
    let path = data("connections.toml");
    ConnectionsFile::parse(&std::fs::read_to_string(&path).unwrap(), &path).unwrap()
}

fn putty_fixture() -> Vec<Profile> {
    let text = putty::decode_reg(&std::fs::read(data("putty/sessions.reg")).unwrap());
    putty::parse_reg(&text)
        .iter()
        .filter_map(putty::PuttySession::to_profile)
        .collect()
}

fn ru() -> urfin_i18n::I18n {
    urfin_i18n::Catalog::embedded()
        .build(&"ru".parse().unwrap())
        .0
}

/// Без изоляции Fluent (U+2068/2069 вокруг подстановок).
fn plain(s: &str) -> String {
    s.replace(['\u{2068}', '\u{2069}'], "")
}

#[test]
fn connections_roundtrip() {
    let file = fixture_file();
    assert_eq!(file.version, 1);
    let web = &file.profiles[0];
    assert_eq!(web.name, "web-prod-01");
    assert_eq!(web.auth, Auth::Key);
    assert_eq!(web.via, "bastion");
    assert_eq!(web.bookmarks, ["/var/log/nginx", "/etc/nginx"]);
    assert_eq!(
        web.last_used.map(|d| d.to_string()).as_deref(),
        Some("2026-10-23T09:14:00Z")
    );
    assert_eq!(web.source, ProfileSource::Urfin);
    assert_eq!(file.profiles[1].auth, Auth::Command);
    assert_eq!(file.overrides[0].password_id.as_deref(), Some("srv-siem01"));

    let text = file.to_toml();
    // Дата — TOML-датой, не строкой; пустые поля не пишутся.
    assert!(text.contains("last_used = 2026-10-23T09:14:00Z"), "{text}");
    assert!(!text.contains("password_cmd = \"\""), "{text}");
    let again = ConnectionsFile::parse(&text, Path::new("again.toml")).unwrap();
    assert_eq!(again, file);
    assert_eq!(again.to_toml(), text);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connections.toml");
    file.save(&path).unwrap();
    assert_eq!(ConnectionsFile::load(&path).unwrap(), file);
}

#[test]
fn secrets_never_reach_the_file() {
    // Руками дописанные секреты при чтении отбрасываются и обратно не пишутся.
    let text = r#"
version = 1

[[profile]]
name = "a"
host = "h"
auth = "password"
password = "hunter2"
passphrase = "s3cret-phrase"
private_key = "-----BEGIN OPENSSH PRIVATE KEY-----"

[[override]]
name = "b"
password = "hunter3"
"#;
    let file = ConnectionsFile::parse(text, Path::new("c.toml")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("connections.toml");
    file.save(&path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    for secret in ["hunter2", "hunter3", "s3cret-phrase", "PRIVATE KEY"] {
        assert!(!written.contains(secret), "{secret} in:\n{written}");
    }

    // Полностью заполненный профиль пишет только ключи формата v1.
    let full = Profile {
        user: "u".into(),
        auth: Auth::Command,
        key: "~/.ssh/k".into(),
        via: "j".into(),
        start_dir: "/".into(),
        bookmarks: vec!["/etc".into()],
        password_cmd: "pass {id}".into(),
        password_id: "id".into(),
        last_used: Some("2026-10-23T09:14:00Z".parse().unwrap()),
        keepalive_s: Some(5),
        notes: vec![Note::KeyViaPageant],
        ..Profile::new("n", "h")
    };
    let over = ProfileOverride {
        name: "o".into(),
        password_cmd: Some("pass".into()),
        ..ProfileOverride::default()
    };
    let file = ConnectionsFile {
        profiles: vec![full],
        overrides: vec![over],
        ..ConnectionsFile::default()
    };
    let table: toml::Table = toml::from_str(&file.to_toml()).unwrap();
    let allowed = [
        "name",
        "host",
        "port",
        "user",
        "auth",
        "key",
        "via",
        "start_dir",
        "bookmarks",
        "password_cmd",
        "password_id",
        "last_used",
    ];
    for section in ["profile", "override"] {
        for rec in table[section].as_array().unwrap() {
            for k in rec.as_table().unwrap().keys() {
                assert!(allowed.contains(&k.as_str()), "{section}.{k}");
            }
        }
    }
}

#[test]
fn ssh_config_fixture() {
    let home = data("home");
    let cfg = ssh_config::load(&home);
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let names: Vec<_> = cfg.profiles.iter().map(|p| p.name.as_str()).collect();
    // Include — на месте директивы; шаблоны `*`, `!` профилей не дают.
    assert_eq!(
        names,
        [
            "lab",
            "siem-collector",
            "web-prod-01",
            "web-prod-01.lab",
            "bastion",
            "db-01"
        ]
    );
    let get = |n: &str| cfg.profiles.iter().find(|p| p.name == n).unwrap();

    let lab = get("lab");
    assert_eq!(
        (lab.host.as_str(), lab.user.as_str()),
        ("192.168.0.10", "root")
    );
    // После Include — снова блок `Host lab`.
    assert_eq!(lab.key, "~/.ssh/lab_rsa");
    assert_eq!(lab.auth, Auth::Key);
    assert_eq!(get("siem-collector").via, "");

    let web = get("web-prod-01");
    assert_eq!(web.host, "10.20.1.11");
    assert_eq!(web.user, "deploy");
    assert_eq!(web.port, 22);
    assert_eq!(web.key, "~/.ssh/deploy_ed25519");
    assert_eq!(web.via, "bastion");
    assert_eq!(web.keepalive_s, Some(15));
    assert_eq!(web.source, ProfileSource::Config);
    assert_eq!(get("web-prod-01.lab").host, "10.20.1.11");

    let bastion = get("bastion");
    assert_eq!(bastion.host, "bastion.example.com");
    assert_eq!(bastion.user, "jump user");
    assert_eq!(bastion.port, 2222);
    assert_eq!(bastion.auth, Auth::Agent);
    assert_eq!(bastion.keepalive_s, None);

    // Директивы блока `Match` не применились.
    let db = get("db-01");
    assert_eq!(db.host, "db-01.db.example.com");
    assert_eq!(db.port, 5022);
    assert_eq!(db.user, "");

    let match_note = Note::Unsupported {
        directive: "Match".into(),
        file: home.join(".ssh").join("config"),
        line: 25,
    };
    assert_eq!(db.notes, std::slice::from_ref(&match_note));
    assert_eq!(
        plain(&match_note.text(&ru())),
        "не поддерживается: Match (строка 25)"
    );

    // Прочитанные файлы — для перечитывания по mtime, включая отсутствующий.
    let ssh = home.join(".ssh");
    assert_eq!(
        cfg.files,
        [
            ssh.join("config"),
            ssh.join("config.d").join("10-lab.conf"),
            ssh.join("extra.conf"),
            ssh.join("missing.conf"),
        ]
    );
}

#[test]
fn missing_ssh_config_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ssh_config::load(dir.path());
    assert!(cfg.profiles.is_empty() && cfg.errors.is_empty());
}

#[test]
fn putty_reg_fixture() {
    let sessions = putty_fixture();
    let names: Vec<_> = sessions.iter().map(|p| p.name.as_str()).collect();
    // Default Settings — шаблон, router — telnet.
    assert_eq!(names, ["web prod (putty)", "lab", "web-prod-01"]);

    let web = &sessions[0];
    assert_eq!(web.host, "10.20.1.12");
    assert_eq!(web.user, "admin");
    assert_eq!(web.port, 22);
    assert_eq!(web.source, ProfileSource::Putty);
    // `.ppk` — через Pageant, путь в профиль не идёт.
    assert_eq!(web.auth, Auth::Agent);
    assert_eq!(web.key, "");
    assert_eq!(web.notes, [Note::KeyViaPageant]);

    let lab = &sessions[1];
    assert_eq!(
        (lab.user.as_str(), lab.host.as_str()),
        ("root", "192.168.0.99")
    );
    assert_eq!(lab.port, 2082);
    assert_eq!(sessions[2].port, DEFAULT_PORT);
}

#[test]
fn putty_reg_utf16_export() {
    // regedit экспортирует UTF-16LE с BOM.
    let text = std::fs::read_to_string(data("putty/sessions.reg")).unwrap();
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    let decoded = putty::decode_reg(&bytes);
    assert_eq!(putty::parse_reg(&decoded), putty::parse_reg(&text));
}

#[test]
fn priority_urfin_config_putty() {
    let file = fixture_file();
    let config = ssh_config::load(&data("home")).profiles;
    let (list, conflicts) = profile::merge(file, config, putty_fixture());

    let get = |n: &str| list.iter().find(|p| p.name == n).unwrap();
    let web = get("web-prod-01");
    assert_eq!(web.source, ProfileSource::Urfin);
    assert_eq!(web.start_dir, "/var/www/portal");
    assert_eq!(get("lab").source, ProfileSource::Config);
    assert_eq!(get("lab").host, "192.168.0.10");
    assert_eq!(get("web prod (putty)").source, ProfileSource::Putty);
    // `[[override]]` — поверх импортированного.
    let siem = get("siem-collector");
    assert_eq!(siem.auth, Auth::Command);
    assert_eq!(siem.password_id, "srv-siem01");
    assert_eq!(siem.host, "10.30.0.5");

    let shadow: Vec<_> = conflicts
        .iter()
        .map(|c| (c.name.as_str(), c.used, c.shadowed.clone()))
        .collect();
    assert_eq!(
        shadow,
        [
            (
                "web-prod-01",
                ProfileSource::Urfin,
                vec![ProfileSource::Config, ProfileSource::Putty]
            ),
            ("lab", ProfileSource::Config, vec![ProfileSource::Putty]),
        ]
    );
}

#[test]
fn load_all_sources_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let conn = dir.path().join("connections.toml");
    std::fs::copy(data("connections.toml"), &conn).unwrap();
    let profiles = profile::load(&Sources {
        connections: Some(conn),
        home: Some(data("home")),
        putty: false,
    });
    assert!(profiles.errors.is_empty(), "{:?}", profiles.errors);
    assert_eq!(profiles.file, fixture_file());
    assert_eq!(
        profiles.get("web-prod-01").map(|p| p.source),
        Some(ProfileSource::Urfin)
    );
    assert_eq!(profiles.get("nas").map(|p| p.port), Some(2222));
    assert_eq!(
        profiles.get("bastion").map(|p| p.source),
        Some(ProfileSource::Config)
    );
    assert!(!profiles.is_stale());
}
