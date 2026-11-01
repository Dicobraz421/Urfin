//! «Установка и удаление программ» (ARP, F02 §0, §2.4): ключи Uninstall — HKLM
//! (64 и 32 бит, `WOW6432Node`) и HKCU. Издатель и имя программ — локально, без
//! процессов и сети.

use compact_str::CompactString;

/// Где лежит ключ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArpScope {
    /// HKLM, 64 бит.
    Machine,
    /// HKLM, `WOW6432Node`.
    Machine32,
    /// HKCU.
    User,
}

/// Программа из ARP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArpEntry {
    pub scope: ArpScope,
    /// Имя подраздела (GUID MSI или имя продукта): из него winget строит `ARP\…` Id.
    pub key: CompactString,
    /// `DisplayName`.
    pub name: CompactString,
    /// `Publisher`.
    pub publisher: Option<CompactString>,
    /// `DisplayVersion`.
    pub version: Option<CompactString>,
}

impl ArpEntry {
    /// Запись из значений ключа (`string`, `dword` — чтение по имени). Как в окне
    /// «Программы»: без имени, системные компоненты и обновления других программ
    /// (`ParentKeyName`) — `None`.
    pub fn from_values(
        scope: ArpScope,
        key: &str,
        string: impl Fn(&str) -> Option<String>,
        dword: impl Fn(&str) -> Option<u32>,
    ) -> Option<ArpEntry> {
        let text = |v: &str| {
            string(v)
                .map(|s| CompactString::from(s.trim()))
                .filter(|s| !s.is_empty())
        };
        let name = text("DisplayName")?;
        if dword("SystemComponent") == Some(1) || text("ParentKeyName").is_some() {
            return None;
        }
        Some(ArpEntry {
            scope,
            key: key.into(),
            name,
            publisher: text("Publisher"),
            version: text("DisplayVersion"),
        })
    }
}

/// Чтение ARP.
#[derive(Debug)]
pub struct Arp;

#[cfg(windows)]
impl Arp {
    /// Все видимые записи трёх веток; недоступная ветка или ключ пропускаются.
    pub fn load() -> Vec<ArpEntry> {
        const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
        const UNINSTALL_32: &str =
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";
        let roots = [
            (
                windows_registry::LOCAL_MACHINE,
                UNINSTALL,
                ArpScope::Machine,
            ),
            (
                windows_registry::LOCAL_MACHINE,
                UNINSTALL_32,
                ArpScope::Machine32,
            ),
            (windows_registry::CURRENT_USER, UNINSTALL, ArpScope::User),
        ];
        let mut out = Vec::new();
        for (root, path, scope) in roots {
            let Ok(base) = root.open(path) else {
                continue;
            };
            let Ok(keys) = base.keys() else {
                continue;
            };
            for name in keys {
                let Ok(k) = base.open(&name) else {
                    continue;
                };
                out.extend(ArpEntry::from_values(
                    scope,
                    &name,
                    |v| k.get_string(v).ok(),
                    |v| k.get_u32(v).ok(),
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn entry(values: &[(&str, &str)], dwords: &[(&str, u32)]) -> Option<ArpEntry> {
        let s: HashMap<&str, &str> = values.iter().copied().collect();
        let d: HashMap<&str, u32> = dwords.iter().copied().collect();
        ArpEntry::from_values(
            ArpScope::User,
            "Mozilla Firefox 131.0.2 (x64 ru)",
            |v| s.get(v).map(|x| (*x).to_owned()),
            |v| d.get(v).copied(),
        )
    }

    #[test]
    fn visible_entry() {
        let e = entry(
            &[
                ("DisplayName", " Mozilla Firefox (x64 ru) "),
                ("Publisher", "Mozilla"),
                ("DisplayVersion", "131.0.2"),
            ],
            &[("SystemComponent", 0)],
        )
        .unwrap();
        assert_eq!(e.name, "Mozilla Firefox (x64 ru)");
        assert_eq!(e.publisher.as_deref(), Some("Mozilla"));
        assert_eq!(e.version.as_deref(), Some("131.0.2"));
        assert_eq!(e.key, "Mozilla Firefox 131.0.2 (x64 ru)");
        let bare = entry(&[("DisplayName", "Tool"), ("Publisher", "")], &[]).unwrap();
        assert_eq!((bare.publisher, bare.version), (None, None));
    }

    #[test]
    fn hidden_entries() {
        assert_eq!(entry(&[("Publisher", "Microsoft")], &[]), None);
        assert_eq!(entry(&[("DisplayName", "  ")], &[]), None);
        assert_eq!(
            entry(
                &[("DisplayName", "VC++ runtime")],
                &[("SystemComponent", 1)]
            ),
            None
        );
        assert_eq!(
            entry(
                &[
                    ("DisplayName", "Update for Office"),
                    ("ParentKeyName", "Office16")
                ],
                &[]
            ),
            None
        );
    }

    /// Стенд: `Arp::load` ≤30 мс холодно (первый вызов в процессе).
    #[cfg(windows)]
    #[test]
    #[ignore = "стенд: настоящий реестр"]
    fn stand_load_cold() {
        let t = std::time::Instant::now();
        let all = Arp::load();
        let took = t.elapsed();
        eprintln!("{took:?}: {} entries", all.len());
        assert_ne!(all.len(), 0);
        assert!(took <= std::time::Duration::from_millis(30), "{took:?}");
    }
}
