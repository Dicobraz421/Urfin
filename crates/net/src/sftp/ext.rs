//! Колонки «Права» и «Владелец» (F03 §2.6, v6 №1, №21): данные `Entry.ext` схемы `sftp`.
//! Листинг кладёт права и владельца из `longname` в память по каталогу; ячейки панель
//! берёт отсюда же в кадре — без запросов к серверу.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use urfin_vfs::{ColumnSpec, ColumnView, ExtCell};

use super::longname::Owner;

/// Колонки панели `sftp://` (`screen-sftppanel`): Имя · Размер · Изменён · Права · Владелец.
pub static COLUMNS: [ColumnSpec; 4] = [
    ColumnSpec::SIZE,
    ColumnSpec::MTIME,
    ColumnSpec {
        id: "perms",
        title: "net-col-perms",
        width: 90.0,
        right: false,
        view: ColumnView::Text,
        mono: true,
    },
    ColumnSpec {
        id: "owner",
        title: "net-col-owner",
        width: 82.0,
        right: false,
        view: ColumnView::Text,
        mono: false,
    },
];

/// Каталогов в памяти: открытые табы и история «назад».
const DIRS: usize = 16;

/// Права и владелец одного имени.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Detail {
    pub perms: String,
    pub owner: String,
}

impl Detail {
    pub(crate) fn new(perms: String, owner: &Owner) -> Detail {
        Detail {
            perms,
            owner: owner.user.clone(),
        }
    }
}

type Dir = HashMap<String, Detail>;

/// Последние каталоги: ключ — профиль в нижнем регистре и путь.
#[derive(Debug, Default)]
pub(crate) struct Details {
    dirs: Mutex<VecDeque<(String, Dir)>>,
}

impl Details {
    pub(crate) fn key(profile: &str, path: &str) -> String {
        format!("{}\n{path}", profile.to_ascii_lowercase())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, Dir)>> {
        self.dirs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Начало листинга: прежние записи каталога забываются.
    pub(crate) fn reset(&self, key: &str) {
        let mut d = self.lock();
        d.retain(|(k, _)| k != key);
        d.push_front((key.to_owned(), Dir::new()));
        d.truncate(DIRS);
    }

    /// Батч листинга.
    pub(crate) fn add(&self, key: &str, batch: impl IntoIterator<Item = (String, Detail)>) {
        let mut d = self.lock();
        if let Some((_, dir)) = d.iter_mut().find(|(k, _)| k == key) {
            dir.extend(batch);
        }
    }

    /// Ячейка колонки `col` для имени `name`.
    pub(crate) fn cell(&self, key: &str, name: &str, col: &str) -> Option<ExtCell> {
        let d = self.lock();
        let v = d.iter().find(|(k, _)| k == key)?.1.get(name)?;
        let text = match col {
            "perms" => &v.perms,
            "owner" => &v.owner,
            _ => return None,
        };
        (!text.is_empty()).then(|| ExtCell::Text(text.as_str().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(perms: &str, user: &str) -> Detail {
        Detail::new(
            perms.to_owned(),
            &Owner {
                user: user.to_owned(),
                group: "g".to_owned(),
            },
        )
    }

    #[test]
    fn cells_by_dir_and_name() {
        let d = Details::default();
        let k = Details::key("Srv1", "/var/www");
        assert_eq!(k, Details::key("srv1", "/var/www"));
        d.add(&k, [("lost".to_owned(), detail("x", "y"))]);
        assert_eq!(d.cell(&k, "lost", "perms"), None, "без начала листинга");
        d.reset(&k);
        d.add(&k, [("a".to_owned(), detail("drwxr-xr-x", "www-data"))]);
        assert_eq!(
            d.cell(&k, "a", "perms"),
            Some(ExtCell::Text("drwxr-xr-x".into()))
        );
        assert_eq!(
            d.cell(&k, "a", "owner"),
            Some(ExtCell::Text("www-data".into()))
        );
        assert_eq!(d.cell(&k, "a", "size"), None);
        assert_eq!(d.cell(&k, "b", "owner"), None);
        d.reset(&k);
        assert_eq!(
            d.cell(&k, "a", "owner"),
            None,
            "новый листинг забывает прежний"
        );
    }

    #[test]
    fn keeps_last_dirs() {
        let d = Details::default();
        for i in 0..=DIRS {
            let k = Details::key("s", &format!("/{i}"));
            d.reset(&k);
            d.add(&k, [("f".to_owned(), detail("-rw-r--r--", "u"))]);
        }
        assert_eq!(d.cell(&Details::key("s", "/0"), "f", "owner"), None);
        assert!(
            d.cell(&Details::key("s", &format!("/{DIRS}")), "f", "owner")
                .is_some()
        );
    }
}
