//! Секреты в памяти (F03 §2.5, F03-03): пароли, фразы-пароли, ключи из менеджеров.
//! Память затирается (`zeroize`) при удалении; `Debug` значений не показывает; в логи и
//! трассировку значения не идут, паник со значениями здесь нет.
//!
//! Живут, пока открыт сеанс профиля; `[ssh].secret_ttl_min` > 0 — не дольше TTL.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// Строковый секрет.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: String) -> Secret {
        Secret(Zeroizing::new(s))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Двоичный секрет (закрытый ключ PEM).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn new(b: Vec<u8>) -> SecretBytes {
        SecretBytes(Zeroizing::new(b))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(***)")
    }
}

/// Секреты сеанса по ключу (`pw:<host>`, `pp:<путь ключа>`).
pub struct Secrets {
    ttl: Option<Duration>,
    items: HashMap<String, (Secret, Instant)>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("ttl", &self.ttl)
            .field("len", &self.items.len())
            .finish()
    }
}

impl Default for Secrets {
    fn default() -> Self {
        Secrets::new(0)
    }
}

impl Secrets {
    /// `ttl_min` = 0 — до отключения.
    pub fn new(ttl_min: u32) -> Secrets {
        Secrets {
            ttl: (ttl_min > 0).then(|| Duration::from_secs(u64::from(ttl_min) * 60)),
            items: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn with_ttl(ttl: Duration) -> Secrets {
        Secrets {
            ttl: Some(ttl),
            items: HashMap::new(),
        }
    }

    /// Живой секрет; истёкший удаляется.
    pub fn get(&mut self, key: &str) -> Option<Secret> {
        let ttl = self.ttl;
        let expired = self
            .items
            .get(key)
            .is_some_and(|(_, at)| ttl.is_some_and(|t| at.elapsed() >= t));
        if expired {
            self.items.remove(key);
            return None;
        }
        self.items.get(key).map(|(s, _)| s.clone())
    }

    pub fn put(&mut self, key: impl Into<String>, s: Secret) {
        self.items.insert(key.into(), (s, Instant::now()));
    }

    pub fn remove(&mut self, key: &str) {
        self.items.remove(key);
    }

    /// Отключение: всё затирается.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// Секреты сеанса профиля для обоих транспортов (F03 §0): SFTP и мост `ssh.exe` берут
/// пароль из одной памяти — он вводится один раз. Замок только на время вызова.
#[derive(Clone, Debug, Default)]
pub struct Shared(Arc<Mutex<Secrets>>);

impl Shared {
    pub fn new(ttl_min: u32) -> Shared {
        Shared(Arc::new(Mutex::new(Secrets::new(ttl_min))))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Secrets> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self, key: &str) -> Option<Secret> {
        self.lock().get(key)
    }

    pub fn put(&self, key: impl Into<String>, s: Secret) {
        self.lock().put(key, s);
    }

    pub fn remove(&self, key: &str) {
        self.lock().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_values() {
        let s = Secret::new("hunter2".into());
        assert_eq!(format!("{s:?}"), "Secret(***)");
        let b = SecretBytes::new(b"-----BEGIN".to_vec());
        assert!(!format!("{b:?}").contains("BEGIN"));
        let mut all = Secrets::new(0);
        all.put("pw:srv1", s);
        assert!(!format!("{all:?}").contains("hunter2"));
    }

    #[test]
    fn lives_until_cleared_without_ttl() {
        let mut all = Secrets::new(0);
        all.put("pw:srv1", Secret::new("a".into()));
        assert_eq!(all.get("pw:srv1").unwrap().expose(), "a");
        all.remove("pw:srv1");
        assert!(all.get("pw:srv1").is_none());
        all.put("pw:srv1", Secret::new("a".into()));
        all.clear();
        assert!(all.get("pw:srv1").is_none());
    }

    #[test]
    fn ttl_expires() {
        let mut all = Secrets::with_ttl(Duration::from_millis(20));
        all.put("pw:srv1", Secret::new("a".into()));
        assert!(all.get("pw:srv1").is_some());
        std::thread::sleep(Duration::from_millis(40));
        assert!(all.get("pw:srv1").is_none());
        assert_eq!(Secrets::new(5).ttl, Some(Duration::from_secs(300)));
    }
}
