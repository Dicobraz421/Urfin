//! Политики организации (F00-01, ADR-0020): разбор `policy.toml`, значения GPO,
//! объединение (GPO побеждает) и замки настроек.

use crate::policy::{
    GpoValue, MinVerify, PolicyError, PolicyKey, PolicySet, PolicySource, PolicyValue, SshRecording,
};

fn toml(text: &str) -> PolicySet {
    let (set, errors) = PolicySet::from_toml(text);
    assert!(errors.is_empty(), "{errors:?}");
    set
}

fn gpo(values: impl IntoIterator<Item = (PolicyKey, GpoValue)>) -> PolicySet {
    let mut set = PolicySet::default();
    let errors = set.merge_gpo(values);
    assert!(errors.is_empty(), "{errors:?}");
    set
}

#[test]
fn adr_example() {
    let set = toml(
        r#"
# Политика организации для Урфина. Писать — только администратор.
delete_to_trash = true
verify = "size"

[ssh]
recording = "required"
"#,
    );
    assert_eq!(set.bool(PolicyKey::DeleteToTrash), Some(true));
    assert_eq!(set.verify(), Some(MinVerify::Size));
    assert_eq!(set.ssh_recording(), Some(SshRecording::Required));
    assert_eq!(set.bool(PolicyKey::ExperimentalFastTrash), None);
    assert!(set.iter().all(|(_, e)| e.source == PolicySource::Toml));
    let keys: Vec<_> = set.iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        [
            PolicyKey::DeleteToTrash,
            PolicyKey::Verify,
            PolicyKey::SshRecording
        ]
    );
}

#[test]
fn every_key_from_toml() {
    let set = toml(
        r#"
theme = " dark "
theme_locked = true
delete_to_trash = false
experimental_fast_trash = false
verify = "hash"
du_elevated_helper = false
du_elevated_mft = true
password_cache = false

[ssh]
recording = "forbidden"
multi_exec = false
"#,
    );
    assert_eq!(set.iter().count(), PolicyKey::ALL.len());
    assert_eq!(set.theme(), Some("dark"));
    assert_eq!(set.bool(PolicyKey::ThemeLocked), Some(true));
    assert_eq!(set.bool(PolicyKey::DuElevatedMft), Some(true));
    assert_eq!(set.bool(PolicyKey::SshMultiExec), Some(false));
    assert_eq!(set.verify(), Some(MinVerify::Hash));
    assert_eq!(set.ssh_recording(), Some(SshRecording::Forbidden));
    // Нет булевого значения у небулева ключа.
    assert_eq!(set.bool(PolicyKey::Verify), None);
}

#[test]
fn toml_keys_match_registry_names() {
    for k in PolicyKey::ALL {
        let camel: String = k
            .toml()
            .split(['_', '.'])
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(camel, k.registry(), "{k:?}");
    }
}

#[test]
fn bad_toml_values_are_skipped_with_errors() {
    let (set, errors) = PolicySet::from_toml(
        r#"
delete_to_trash = 1
verify = "full"
theme = ""
password_cache = false
unknown = true

[ssh]
recording = true
extra = 1
"#,
    );
    assert_eq!(set.iter().count(), 1);
    assert_eq!(set.bool(PolicyKey::PasswordCache), Some(false));
    assert_eq!(set.theme(), None, "empty theme is not set");
    let got: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        got,
        [
            "TOML delete_to_trash: expected true or false",
            "TOML verify: expected none, size or hash",
            "TOML unknown: unknown key",
            "TOML ssh.recording: expected required, forbidden or user",
            "TOML ssh.extra: unknown key",
        ]
    );
    assert_eq!(
        errors[1],
        PolicyError::BadValue {
            from: PolicySource::Toml,
            name: "verify",
            expected: "expected none, size or hash",
        }
    );
}

#[test]
fn broken_toml_is_one_error() {
    let (set, errors) = PolicySet::from_toml("verify = ");
    assert!(set.is_empty());
    assert!(matches!(errors[..], [PolicyError::Toml(_)]), "{errors:?}");
}

#[test]
fn empty_sources_nothing_set() {
    let (mut set, errors) = PolicySet::from_toml("");
    assert_eq!(errors, []);
    assert_eq!(set.merge_gpo([]), []);
    assert!(set.is_empty());
    assert_eq!(set, PolicySet::default());
    assert_eq!(set.setting(&["ops", "delete_to_trash"]), None);
}

#[test]
fn gpo_values() {
    let set = gpo([
        (PolicyKey::DeleteToTrash, GpoValue::Dword(1)),
        (PolicyKey::ExperimentalFastTrash, GpoValue::Dword(0)),
        (PolicyKey::ThemeLocked, GpoValue::Dword(7)),
        (PolicyKey::Theme, GpoValue::Sz("light".into())),
        (PolicyKey::Verify, GpoValue::Sz("size".into())),
        (PolicyKey::SshRecording, GpoValue::Sz("user".into())),
    ]);
    assert!(set.iter().all(|(_, e)| e.source == PolicySource::Gpo));
    assert_eq!(set.bool(PolicyKey::DeleteToTrash), Some(true));
    assert_eq!(set.bool(PolicyKey::ExperimentalFastTrash), Some(false));
    assert_eq!(set.bool(PolicyKey::ThemeLocked), Some(true));
    assert_eq!(set.theme(), Some("light"));
    assert_eq!(set.verify(), Some(MinVerify::Size));
    assert_eq!(set.ssh_recording(), Some(SshRecording::User));
}

#[test]
fn gpo_wrong_types_are_skipped_with_errors() {
    let mut set = PolicySet::default();
    let errors = set.merge_gpo([
        (PolicyKey::DeleteToTrash, GpoValue::Sz("1".into())),
        (PolicyKey::Verify, GpoValue::Dword(1)),
        (PolicyKey::PasswordCache, GpoValue::Other),
        (PolicyKey::SshRecording, GpoValue::Sz("always".into())),
    ]);
    assert!(set.is_empty());
    let got: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        got,
        [
            "GPO DeleteToTrash: expected REG_DWORD",
            "GPO Verify: expected REG_SZ",
            "GPO PasswordCache: expected REG_DWORD",
            "GPO SshRecording: expected required, forbidden or user",
        ]
    );
}

#[test]
fn gpo_wins_over_toml() {
    let mut set = toml(
        r#"
verify = "hash"
delete_to_trash = true
theme = "dark"
[ssh]
multi_exec = true
"#,
    );
    let errors = set.merge_gpo([
        (PolicyKey::Verify, GpoValue::Sz("none".into())),
        (PolicyKey::DeleteToTrash, GpoValue::Dword(0)),
        // Неверное значение GPO не перекрывает TOML.
        (PolicyKey::Theme, GpoValue::Dword(1)),
        (PolicyKey::PasswordCache, GpoValue::Dword(0)),
    ]);
    assert_eq!(errors.len(), 1);
    let src = |k| set.get(k).map(|e| e.source);
    assert_eq!(set.verify(), Some(MinVerify::None));
    assert_eq!(src(PolicyKey::Verify), Some(PolicySource::Gpo));
    assert_eq!(set.bool(PolicyKey::DeleteToTrash), Some(false));
    assert_eq!(src(PolicyKey::DeleteToTrash), Some(PolicySource::Gpo));
    assert_eq!(set.theme(), Some("dark"));
    assert_eq!(src(PolicyKey::Theme), Some(PolicySource::Toml));
    assert_eq!(set.bool(PolicyKey::SshMultiExec), Some(true));
    assert_eq!(src(PolicyKey::SshMultiExec), Some(PolicySource::Toml));
    assert_eq!(src(PolicyKey::PasswordCache), Some(PolicySource::Gpo));
    assert_eq!(set.iter().count(), 5);
}

#[test]
fn settings_under_policy() {
    let mut set = toml("delete_to_trash = true\ntheme = \"dark\"\nverify = \"size\"");
    let errors = set.merge_gpo([(PolicyKey::ExperimentalFastTrash, GpoValue::Dword(0))]);
    assert_eq!(errors, []);
    assert_eq!(
        set.setting(&["ops", "delete_to_trash"]),
        Some(PolicySource::Toml)
    );
    assert_eq!(
        set.setting(&["ops", "experimental_fast_trash"]),
        Some(PolicySource::Gpo)
    );
    assert_eq!(
        set.setting(&["ops", "defaults", "verify"]),
        Some(PolicySource::Toml)
    );
    // Тема без `theme_locked` — только значение по умолчанию.
    assert_eq!(set.setting(&["ui", "theme"]), None);
    assert_eq!(set.setting(&["ui", "zoom"]), None);
    assert_eq!(set.setting(&["ops"]), None);

    let mut locked = toml("theme_locked = true");
    assert_eq!(locked.setting(&["ui", "theme"]), Some(PolicySource::Toml));
    let errors = locked.merge_gpo([(PolicyKey::ThemeLocked, GpoValue::Dword(1))]);
    assert_eq!(errors, []);
    assert_eq!(locked.setting(&["ui", "theme"]), Some(PolicySource::Gpo));
    let unlocked = toml("theme_locked = false");
    assert_eq!(unlocked.setting(&["ui", "theme"]), None);
}

#[test]
fn values_display_as_in_toml() {
    let shown = |v: PolicyValue| v.to_string();
    assert_eq!(shown(PolicyValue::Bool(false)), "false");
    assert_eq!(shown(PolicyValue::Verify(MinVerify::Hash)), "hash");
    assert_eq!(
        shown(PolicyValue::SshRecording(SshRecording::Forbidden)),
        "forbidden"
    );
    assert_eq!(shown(PolicyValue::Theme("dark".into())), "dark");
    assert_eq!(PolicySource::Gpo.label(), "GPO");
    assert_eq!(PolicySource::Toml.to_string(), "TOML");
}

/// Конфиг с секциями других крейтов из текста.
fn config(ext: &str) -> crate::Config {
    crate::Config {
        ext: ext.parse().unwrap(),
        ..crate::Config::default()
    }
}

#[test]
fn apply_overlays_ops_keys() {
    let mut cfg = config(
        r#"
[ops]
delete_to_trash = false
experimental_fast_trash = true
[ops.defaults]
verify = "Off"
conflict = "Skip"
"#,
    );
    let mut set = toml(
        "verify = \"size\"
experimental_fast_trash = false",
    );
    set.merge_gpo([(PolicyKey::DeleteToTrash, GpoValue::Dword(1))]);
    set.apply(&mut cfg);
    let ops = cfg.ext["ops"].as_table().unwrap();
    assert_eq!(ops["delete_to_trash"].as_bool(), Some(true));
    assert_eq!(ops["experimental_fast_trash"].as_bool(), Some(false));
    assert_eq!(ops["defaults"]["verify"].as_str(), Some("Size"));
    assert_eq!(ops["defaults"]["conflict"].as_str(), Some("Skip"));
}

#[test]
fn apply_verify_is_a_minimum() {
    let mut cfg = config(
        "[ops.defaults]
verify = { Hash = \"Blake3\" }",
    );
    toml("verify = \"size\"").apply(&mut cfg);
    assert_eq!(
        cfg.ext["ops"]["defaults"]["verify"]["Hash"].as_str(),
        Some("Blake3")
    );
    // Ключа нет — по умолчанию выкл., поднимается до минимума.
    let mut cfg = crate::Config::default();
    toml("verify = \"hash\"").apply(&mut cfg);
    assert_eq!(
        cfg.ext["ops"]["defaults"]["verify"]["Hash"].as_str(),
        Some("Xxh3_128")
    );
}

#[test]
fn apply_without_ops_keys_keeps_config() {
    let mut cfg = crate::Config::default();
    toml(
        "theme_locked = true
password_cache = false
verify = \"none\"",
    )
    .apply(&mut cfg);
    assert_eq!(cfg, crate::Config::default());
    PolicySet::default().apply(&mut cfg);
    assert_eq!(cfg, crate::Config::default());
}
