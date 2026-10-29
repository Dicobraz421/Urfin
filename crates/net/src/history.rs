//! История хостов `state/ssh/history.toml` (F03 §2.2): хост, пользователь, время.
//! Источник автодополнения поля «Хост» наряду с `config` и `known_hosts` (№70).

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::value::Datetime;

use crate::config::{NetError, write_atomic};

/// Записей в истории максимум; старые вытесняются.
pub const HISTORY_MAX: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    pub time: Datetime,
}

/// Новые — первыми; пара хост + пользователь — одна запись.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    #[serde(default, rename = "host", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    /// Подключение к `host` под `user` в `time`: запись — в начало.
    pub fn touch(&mut self, host: &str, user: &str, time: Datetime) {
        self.entries
            .retain(|e| !(e.host.eq_ignore_ascii_case(host) && e.user == user));
        self.entries.insert(
            0,
            HistoryEntry {
                host: host.to_owned(),
                user: user.to_owned(),
                time,
            },
        );
        self.entries.truncate(HISTORY_MAX);
    }

    /// Чтение; нет файла — пусто. I/O — только задачами Runtime.
    pub fn load(path: &Path) -> Result<History, NetError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| NetError::parse(path, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(History::default()),
            Err(e) => Err(NetError::io(path, &e)),
        }
    }

    /// Атомарная запись (temp + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string(self).expect("history.toml serializes");
        write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Datetime {
        s.parse().unwrap()
    }

    #[test]
    fn touch_moves_to_front_and_caps() {
        let mut h = History::default();
        h.touch("a", "root", t("2026-10-23T09:00:00Z"));
        h.touch("b", "", t("2026-10-23T09:01:00Z"));
        h.touch("A", "root", t("2026-10-23T09:02:00Z"));
        let hosts: Vec<_> = h.entries.iter().map(|e| e.host.as_str()).collect();
        assert_eq!(hosts, ["A", "b"]);
        for i in 0..HISTORY_MAX + 5 {
            h.touch(&format!("h{i}"), "", t("2026-10-23T10:00:00Z"));
        }
        assert_eq!(h.entries.len(), HISTORY_MAX);
    }

    #[test]
    fn roundtrip_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh").join("history.toml");
        assert_eq!(History::load(&path).unwrap(), History::default());
        let mut h = History::default();
        h.touch("10.20.1.11", "deploy", t("2026-10-23T09:14:00Z"));
        h.touch("bastion", "", t("2026-10-24T08:00:00Z"));
        h.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("time = 2026-10-23T09:14:00Z"), "{text}");
        assert_eq!(History::load(&path).unwrap(), h);
    }
}
