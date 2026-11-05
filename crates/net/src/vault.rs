//! Кэш паролей (F03 §2.5, F03-03): Диспетчер учётных данных Windows через `keyring`,
//! цель `Urfin/ssh/<user>@<host>:<port>`. Только при `password_cache` (политика сильнее
//! флажка `[ssh]`) и флажке профиля; иначе хранилище не вызывается ни разу, а «Запомнить»
//! показан с замком. Выключенная политикой — записи Урфина удаляются при следующем запуске
//! (по префиксу, решение владельца №2).

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use urfin_core::{PolicyKey, PolicySet};

use crate::config::SshCfg;
use crate::secrets::Secret;

/// Префикс целей Урфина.
pub const PREFIX: &str = "Urfin/ssh/";

/// Цель записи.
pub fn target(user: &str, host: &str, port: u16) -> String {
    format!("{PREFIX}{user}@{host}:{port}")
}

/// Ошибка хранилища: текст — для лога, без значений.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("password cache is disabled")]
    Locked,
    #[error("credential store: {0}")]
    Store(String),
}

/// Хранилище учётных данных. В тестах — мок.
pub trait CredStore: Send + Sync {
    fn get(&self, target: &str) -> Result<Option<Secret>, VaultError>;
    fn set(&self, target: &str, secret: &Secret) -> Result<(), VaultError>;
    fn delete(&self, target: &str) -> Result<(), VaultError>;
    /// Цели с префиксом.
    fn list(&self, prefix: &str) -> Result<Vec<String>, VaultError>;
}

/// `password_cache` политики в [`AtomicU8`].
const POLICY_UNSET: u8 = 0;
const POLICY_OFF: u8 = 1;
const POLICY_ON: u8 = 2;

fn policy_bits(policies: &PolicySet) -> u8 {
    match policies.bool(PolicyKey::PasswordCache) {
        None => POLICY_UNSET,
        Some(false) => POLICY_OFF,
        Some(true) => POLICY_ON,
    }
}

/// Кэш с политикой. Политика и флажок `[ssh]` меняются на лету ([`Vault::update`]):
/// сеансы держат общий `Arc`.
pub struct Vault {
    /// Политика `password_cache`: [`POLICY_OFF`] — запрещено организацией.
    policy: AtomicU8,
    user: AtomicBool,
    store: Box<dyn CredStore>,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("policy", &self.policy())
            .field("user", &self.user.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Vault {
    pub fn new(policies: &PolicySet, cfg: &SshCfg, store: Box<dyn CredStore>) -> Vault {
        Vault {
            policy: AtomicU8::new(policy_bits(policies)),
            user: AtomicBool::new(cfg.password_cache),
            store,
        }
    }

    /// Новые политика и флажок `[ssh]` (настройки, F03-14): действуют со следующего
    /// обращения. Хранилище здесь не трогается: уборка — только на старте (решение
    /// 2026-11-01 «password-cache-cleanup»).
    pub fn update(&self, policies: &PolicySet, cfg: &SshCfg) {
        self.policy.store(policy_bits(policies), Ordering::Relaxed);
        self.user.store(cfg.password_cache, Ordering::Relaxed);
    }

    fn policy(&self) -> Option<bool> {
        match self.policy.load(Ordering::Relaxed) {
            POLICY_OFF => Some(false),
            POLICY_ON => Some(true),
            _ => None,
        }
    }

    /// Кэш разрешён: политика, а без неё — `[ssh].password_cache`.
    pub fn enabled(&self) -> bool {
        self.policy()
            .unwrap_or_else(|| self.user.load(Ordering::Relaxed))
    }

    /// «Запомнить» под замком: запрещено политикой организации.
    pub fn locked(&self) -> bool {
        self.policy() == Some(false)
    }

    /// Пароль из кэша; `remember` — флажок профиля.
    pub fn get(&self, target: &str, remember: bool) -> Option<Secret> {
        if !self.enabled() || !remember {
            return None;
        }
        self.store
            .get(target)
            .inspect_err(|e| tracing::warn!(target: "net", error = %e, "vault get"))
            .ok()
            .flatten()
    }

    /// «Запомнить».
    pub fn remember(&self, target: &str, secret: &Secret) -> Result<(), VaultError> {
        if !self.enabled() {
            return Err(VaultError::Locked);
        }
        self.store.set(target, secret)
    }

    /// Неверный пароль из кэша — удалить.
    pub fn forget(&self, target: &str) -> Result<(), VaultError> {
        if !self.enabled() {
            return Err(VaultError::Locked);
        }
        self.store.delete(target)
    }

    /// Старт при политике `password_cache = false`: удалить записи Урфина. Без такой
    /// политики хранилище не трогается. Возвращает число удалённых.
    pub fn purge_if_forbidden(&self) -> usize {
        if !self.locked() {
            return 0;
        }
        let Ok(list) = self.store.list(PREFIX) else {
            return 0;
        };
        list.iter()
            .filter(|t| t.starts_with(PREFIX))
            .filter(|t| self.store.delete(t).is_ok())
            .count()
    }
}

/// Диспетчер учётных данных Windows (`keyring`); перечисление — `CredEnumerateW`.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct WinCredStore;

#[cfg(windows)]
impl WinCredStore {
    fn entry(target: &str) -> Result<keyring::Entry, VaultError> {
        let user = target.strip_prefix(PREFIX).unwrap_or(target);
        keyring::Entry::new_with_target(target, "Urfin", user)
            .map_err(|e| VaultError::Store(e.to_string()))
    }
}

#[cfg(windows)]
impl CredStore for WinCredStore {
    fn get(&self, target: &str) -> Result<Option<Secret>, VaultError> {
        match Self::entry(target)?.get_password() {
            Ok(p) => Ok(Some(Secret::new(p))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(VaultError::Store(e.to_string())),
        }
    }

    fn set(&self, target: &str, secret: &Secret) -> Result<(), VaultError> {
        Self::entry(target)?
            .set_password(secret.expose())
            .map_err(|e| VaultError::Store(e.to_string()))
    }

    fn delete(&self, target: &str) -> Result<(), VaultError> {
        match Self::entry(target)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(VaultError::Store(e.to_string())),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, VaultError> {
        crate::sys::cred_targets(prefix).map_err(|e| VaultError::Store(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Мок `keyring`: считает вызовы.
    #[derive(Clone, Default)]
    struct Mock {
        calls: Arc<Mutex<Vec<String>>>,
        items: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Mock {
        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl CredStore for Mock {
        fn get(&self, target: &str) -> Result<Option<Secret>, VaultError> {
            self.calls.lock().unwrap().push(format!("get {target}"));
            let items = self.items.lock().unwrap();
            Ok(items
                .iter()
                .find(|(t, _)| t == target)
                .map(|(_, p)| Secret::new(p.clone())))
        }

        fn set(&self, target: &str, secret: &Secret) -> Result<(), VaultError> {
            self.calls.lock().unwrap().push(format!("set {target}"));
            let mut items = self.items.lock().unwrap();
            items.retain(|(t, _)| t != target);
            items.push((target.into(), secret.expose().into()));
            Ok(())
        }

        fn delete(&self, target: &str) -> Result<(), VaultError> {
            self.calls.lock().unwrap().push(format!("delete {target}"));
            self.items.lock().unwrap().retain(|(t, _)| t != target);
            Ok(())
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>, VaultError> {
            self.calls.lock().unwrap().push(format!("list {prefix}"));
            let items = self.items.lock().unwrap();
            Ok(items
                .iter()
                .map(|(t, _)| t.clone())
                .filter(|t| t.starts_with(prefix))
                .collect())
        }
    }

    fn policy(v: Option<bool>) -> PolicySet {
        let text = v.map_or(String::new(), |b| format!("password_cache = {b}\n"));
        let (p, errs) = PolicySet::from_toml(&text);
        assert!(errs.is_empty(), "{errs:?}");
        p
    }

    fn cfg(user: bool) -> SshCfg {
        SshCfg {
            password_cache: user,
            ..SshCfg::default()
        }
    }

    #[test]
    fn target_format() {
        assert_eq!(target("admin", "srv1", 22), "Urfin/ssh/admin@srv1:22");
    }

    #[test]
    fn policy_off_never_calls_store() {
        let mock = Mock::default();
        let v = Vault::new(&policy(Some(false)), &cfg(true), Box::new(mock.clone()));
        let t = target("admin", "srv1", 22);
        assert!(v.locked());
        assert!(!v.enabled());
        assert!(v.get(&t, true).is_none());
        assert_eq!(
            v.remember(&t, &Secret::new("pw".into())),
            Err(VaultError::Locked)
        );
        assert_eq!(v.forget(&t), Err(VaultError::Locked));
        assert_eq!(mock.calls(), 0);
    }

    #[test]
    fn user_flag_off_never_calls_store_but_not_locked() {
        let mock = Mock::default();
        let v = Vault::new(&policy(None), &cfg(false), Box::new(mock.clone()));
        assert!(!v.locked());
        assert!(v.get("Urfin/ssh/a@b:22", true).is_none());
        assert_eq!(v.purge_if_forbidden(), 0);
        assert_eq!(mock.calls(), 0);
    }

    #[test]
    fn enabled_roundtrip_and_profile_flag() {
        let mock = Mock::default();
        // Политика сильнее выключенного флажка `[ssh]`.
        let v = Vault::new(&policy(Some(true)), &cfg(false), Box::new(mock.clone()));
        let t = target("admin", "srv1", 22);
        v.remember(&t, &Secret::new("pw".into())).unwrap();
        assert_eq!(v.get(&t, true).unwrap().expose(), "pw");
        // Без флажка профиля — без хранилища.
        let before = mock.calls();
        assert!(v.get(&t, false).is_none());
        assert_eq!(mock.calls(), before);
    }

    /// Настройки и политика меняются на лету; хранилище при этом не вызывается.
    #[test]
    fn update_applies_without_touching_store() {
        let mock = Mock::default();
        let v = Vault::new(&policy(None), &cfg(false), Box::new(mock.clone()));
        assert!(!v.enabled());
        v.update(&policy(None), &cfg(true));
        assert!(v.enabled() && !v.locked());
        v.update(&policy(Some(false)), &cfg(true));
        assert!(!v.enabled() && v.locked());
        assert_eq!(mock.calls(), 0);
    }

    #[test]
    fn purge_deletes_only_urfin_entries() {
        let mock = Mock::default();
        mock.items.lock().unwrap().extend([
            ("Urfin/ssh/a@b:22".into(), "1".into()),
            ("Urfin/ssh/c@d:2222".into(), "2".into()),
            ("Other/x".into(), "3".into()),
        ]);
        let v = Vault::new(&policy(Some(false)), &cfg(true), Box::new(mock.clone()));
        assert_eq!(v.purge_if_forbidden(), 2);
        assert_eq!(mock.items.lock().unwrap().len(), 1);
    }
}
