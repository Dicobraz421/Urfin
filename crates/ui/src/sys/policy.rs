//! Политики организации (ADR-0020, F00-01): `%ProgramData%\Urfin\policy.toml` и
//! `HKLM\Software\Policies\Aahz\Urfin`, объединённые в [`PolicySet`] (GPO побеждает).
//! Читаются до окна и заново при изменении папки или ключа (поток `policy-watch`).
//! Только машинные источники: файлы профиля и `HKCU` не читаются.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use urfin_core::{GpoValue, PolicyError, PolicyKey, PolicySet};

/// Новый набор от `policy-watch` для UI-потока; берёт handler `ui` в pump.
pub type PolicyInbox = Arc<Mutex<Option<PolicySet>>>;

/// `%ProgramData%\Urfin` — папка `policy.toml`.
pub fn policy_dir() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|d| PathBuf::from(d).join("Urfin"))
}

/// Прочитать оба источника; ошибки разбора — в журнал, ключ не задан.
pub fn load() -> PolicySet {
    let file = policy_dir().map(|d| d.join("policy.toml"));
    let (set, errors) = load_from(file.as_deref(), super::os::policy_gpo());
    for e in &errors {
        tracing::warn!(error = %e, "policy value ignored");
    }
    if !set.is_empty() {
        tracing::info!(?set, "organization policy");
    }
    set
}

/// Файл (нет — не задано) и значения реестра в один набор.
pub(crate) fn load_from(
    file: Option<&Path>,
    gpo: Vec<(PolicyKey, GpoValue)>,
) -> (PolicySet, Vec<PolicyError>) {
    let text = file.and_then(|f| match std::fs::read_to_string(f) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(file = %f.display(), error = %e, "policy.toml unreadable");
            None
        }
    });
    let (mut set, mut errors) = text.map_or_else(
        || (PolicySet::default(), Vec::new()),
        |t| PolicySet::from_toml(&t),
    );
    errors.extend(set.merge_gpo(gpo));
    (set, errors)
}

/// Следить за папкой и ключом: изменился набор — в `inbox` и `wake`. Поток живёт до
/// выхода процесса.
pub fn watch(last: PolicySet, inbox: PolicyInbox, wake: impl Fn() + Send + 'static) {
    let mut last = last;
    super::os::watch_policy(
        policy_dir(),
        Box::new(move || {
            let now = load();
            if now != last {
                last = now.clone();
                if let Ok(mut slot) = inbox.lock() {
                    *slot = Some(now);
                }
                wake();
            }
            true
        }),
    );
}

/// Набор для `--screens`: как на макетах «Политики» и «Операции» (одинаков везде).
pub fn demo() -> PolicySet {
    let (mut set, _) = PolicySet::from_toml("[ssh]\nrecording = \"required\"");
    let _ = set.merge_gpo([
        (PolicyKey::DeleteToTrash, GpoValue::Dword(1)),
        (PolicyKey::PasswordCache, GpoValue::Dword(0)),
    ]);
    set
}

#[cfg(test)]
mod tests {
    use urfin_core::PolicySource;

    use super::*;

    #[test]
    fn file_and_registry_merge() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("policy.toml");
        std::fs::write(&file, "verify = \"size\"\ndelete_to_trash = false\n").unwrap();
        let (set, errors) = load_from(
            Some(&file),
            vec![(PolicyKey::DeleteToTrash, GpoValue::Dword(1))],
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(set.bool(PolicyKey::DeleteToTrash), Some(true));
        assert_eq!(
            set.get(PolicyKey::DeleteToTrash).map(|e| e.source),
            Some(PolicySource::Gpo)
        );
        assert_eq!(
            set.get(PolicyKey::Verify).map(|e| e.source),
            Some(PolicySource::Toml)
        );
    }

    #[test]
    fn missing_file_is_unset() {
        let dir = tempfile::tempdir().unwrap();
        let (set, errors) = load_from(Some(&dir.path().join("policy.toml")), Vec::new());
        assert!(set.is_empty() && errors.is_empty());
    }

    #[test]
    fn demo_matches_mockups() {
        let set = demo();
        assert_eq!(set.iter().count(), 3);
        assert_eq!(
            set.setting(&["ops", "delete_to_trash"]),
            Some(PolicySource::Gpo)
        );
    }
}
