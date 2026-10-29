//! Итог фоновой проверки `--pkg-check`: `state/pkg/check.json` (F02 §4). Читает баннер
//! при запуске.

use compact_str::CompactString;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::api::{Mgr, PkgId, Update};

/// Имя файла в `state_dir/pkg`.
pub const CHECK_FILE: &str = "check.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub checked_at: Timestamp,
    pub managers: Vec<MgrStatus>,
    pub updates: Vec<Update>,
    /// «Важные» из `updates`.
    pub important: Vec<PkgId>,
}

/// Ответ одного менеджера.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MgrStatus {
    pub mgr: Mgr,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CompactString>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Package;

    fn report() -> CheckReport {
        let firefox = PkgId {
            mgr: Mgr::Winget,
            id: "Mozilla.Firefox".into(),
        };
        CheckReport {
            checked_at: "2026-10-29T09:12:00Z".parse().unwrap(),
            managers: vec![
                MgrStatus {
                    mgr: Mgr::Winget,
                    ok: true,
                    error: None,
                },
                MgrStatus {
                    mgr: Mgr::Choco,
                    ok: false,
                    error: Some("timed out".into()),
                },
            ],
            updates: vec![
                Update {
                    pkg: Package {
                        id: firefox.clone(),
                        name: "Mozilla Firefox".into(),
                        version: "130.0".into(),
                        source: Some("winget".into()),
                        publisher: Some("Mozilla".into()),
                    },
                    available: "131.0.2".into(),
                    pinned: false,
                },
                Update {
                    pkg: Package {
                        id: "scoop:main/7zip".parse().unwrap(),
                        name: "7zip".into(),
                        version: "23.01".into(),
                        source: None,
                        publisher: None,
                    },
                    available: "24.08".into(),
                    pinned: true,
                },
            ],
            important: vec![firefox],
        }
    }

    #[test]
    fn roundtrip() {
        let r = report();
        let json = serde_json::to_string_pretty(&r).unwrap();
        let back: CheckReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    /// Формат на диске — снапшот: его читает следующий запуск, возможно другой версии.
    #[test]
    fn json_snapshot() {
        insta::assert_snapshot!(serde_json::to_string_pretty(&report()).unwrap());
    }
}
