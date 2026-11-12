//! Шаблоны политик (S9-28, ADR-0020): `assets/policy/urfin.admx`, ADML en-US и ru-RU,
//! `policy.example.toml` — каждый `PolicyKey` есть везде, лишних нет.

use std::collections::BTreeSet;
use std::path::PathBuf;

use urfin_core::policy::{PolicyKey, PolicySet};

const KEY: &str = r"Software\Policies\Aahz\Urfin";

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/policy")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Значения между `start` и следующим `end` по всему тексту.
fn between(text: &str, start: &str, end: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(start) {
        rest = &rest[i + start.len()..];
        let j = rest.find(end).expect("unterminated");
        out.push(rest[..j].to_owned());
        rest = &rest[j..];
    }
    out
}

fn set(v: Vec<String>) -> BTreeSet<String> {
    let n = v.len();
    let s: BTreeSet<String> = v.into_iter().collect();
    assert_eq!(s.len(), n, "duplicates: {s:?}");
    s
}

fn registry_names() -> BTreeSet<String> {
    PolicyKey::ALL
        .iter()
        .map(|k| k.registry().to_owned())
        .collect()
}

/// ADMX: политика на каждый ключ, имя значения — `PolicyKey::registry`, только HKLM.
#[test]
fn admx_has_every_policy_key() {
    let admx = read("urfin.admx");
    let names = registry_names();
    assert_eq!(set(between(&admx, "<policy name=\"", '"')), names);
    assert_eq!(set(between(&admx, "valueName=\"", '"')), names);
    let policies = names.len();
    assert_eq!(admx.matches("class=\"Machine\"").count(), policies);
    assert!(!admx.contains("class=\"User\"") && !admx.contains("class=\"Both\""));
    let keys = between(&admx, " key=\"", '"');
    assert_eq!(keys.len(), policies);
    assert!(keys.iter().all(|k| k == KEY), "{keys:?}");
}

/// ADML: все строки и представления, на которые ссылается ADMX, и ничего сверх.
#[test]
fn adml_match_admx() {
    let admx = read("urfin.admx");
    let strings: BTreeSet<String> = between(&admx, "$(string.", ')').into_iter().collect();
    let presentations = set(between(&admx, "$(presentation.", ')'));
    for k in PolicyKey::ALL {
        let name = k.registry();
        assert!(strings.contains(name), "{name}");
        assert!(strings.contains(&format!("{name}_Help")), "{name}_Help");
    }
    for lang in ["en-US", "ru-RU"] {
        let res = read(&format!("{lang}/urfin.adml"));
        assert_eq!(
            set(between(&res, "<string id=\"", '"')),
            strings,
            "{lang}: strings"
        );
        assert_eq!(
            set(between(&res, "<presentation id=\"", '"')),
            presentations,
            "{lang}: presentations"
        );
        for k in PolicyKey::ALL {
            // Справка называет имя в реестре и ключ TOML (вложенные — `[ssh] имя`).
            let toml = k.toml().replace("ssh.", "[ssh] ");
            let help = between(&res, &format!("<string id=\"{}_Help\">", k.registry()), '<');
            let help = help.first().map_or("", String::as_str);
            assert!(
                help.contains(&format!("{} (REG_", k.registry()))
                    && help.contains(&format!("policy.toml: {toml}.")),
                "{lang}: {}: {help}",
                k.registry()
            );
        }
    }
}

/// Пример: как есть ничего не задаёт; с раскомментированными ключами — все ключи без
/// ошибок (лишний или опечатка — `UnknownKey`).
#[test]
fn example_toml_covers_every_key() {
    let text = read("policy.example.toml");
    let (as_is, errors) = PolicySet::from_toml(&text);
    assert!(errors.is_empty() && as_is.is_empty(), "{errors:?}");
    let uncommented: String = text
        .lines()
        .map(|l| match l.strip_prefix('#') {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_lowercase() || c == '[') => rest,
            _ => l,
        })
        .flat_map(|l| [l, "\n"])
        .collect();
    let (all, errors) = PolicySet::from_toml(&uncommented);
    assert!(errors.is_empty(), "{errors:?}");
    for k in PolicyKey::ALL {
        assert!(all.get(k).is_some(), "{}", k.toml());
    }
    assert_eq!(all.iter().count(), PolicyKey::ALL.len());
}
