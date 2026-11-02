//! История хостов `state/ssh/history.toml` (F03 §2.2): хост, пользователь, время.
//! Источник автодополнения поля «Хост» наряду с `config` и `known_hosts` (№70).

use std::io;
use std::path::Path;

use crate::config::{NetError, write_atomic};

// Данные истории — в core (модель подключений для UI, F03-05a).
pub use urfin_core::net::{HISTORY_MAX, History, HistoryEntry};

/// Чтение; нет файла — пусто. I/O — только задачами Runtime.
pub fn load(path: &Path) -> Result<History, NetError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| NetError::parse(path, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(History::default()),
        Err(e) => Err(NetError::io(path, &e)),
    }
}

/// Атомарная запись (temp + rename).
pub fn save(h: &History, path: &Path) -> io::Result<()> {
    let text = toml::to_string(h).expect("history.toml serializes");
    write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml::value::Datetime;

    fn t(s: &str) -> Datetime {
        s.parse().unwrap()
    }

    #[test]
    fn roundtrip_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh").join("history.toml");
        assert_eq!(load(&path).unwrap(), History::default());
        let mut h = History::default();
        h.touch("10.20.1.11", "deploy", t("2026-10-23T09:14:00Z"));
        h.touch("bastion", "", t("2026-10-24T08:00:00Z"));
        save(&h, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("time = 2026-10-23T09:14:00Z"), "{text}");
        assert_eq!(load(&path).unwrap(), h);
    }
}
