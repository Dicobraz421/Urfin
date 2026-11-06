//! История запросов на диске: `state_dir/search_history.toml`. Только из фоновых задач.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use urfin_core::paths::write_atomic;
use urfin_core::search::PastQuery;

pub(crate) const FILE: &str = "search_history.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct History {
    /// Свежие сверху.
    query: Vec<PastQuery>,
}

/// Запросы из файла; файла нет или он испорчен — пусто: история не стоит ошибки на старте.
pub(crate) fn load(dir: &Path) -> Vec<PastQuery> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    match toml::from_str::<History>(&text) {
        Ok(h) => h.query,
        Err(e) => {
            tracing::warn!(target: "search", error = %e, "history ignored");
            Vec::new()
        }
    }
}

pub(crate) fn save(dir: &Path, query: Vec<PastQuery>) -> io::Result<()> {
    let text = toml::to_string(&History { query }).map_err(io::Error::other)?;
    write_atomic(&dir.join(FILE), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_broken_file() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(d.path()), [] as [urfin_core::search::PastQuery; 0]);
        let q = PastQuery {
            name: "*.rs".into(),
            text: "Привет".into(),
            place: r"C:\a;D:\b".into(),
            case: true,
            regex: false,
            recursive: true,
        };
        save(d.path(), vec![q.clone()]).unwrap();
        assert_eq!(load(d.path()), [q]);
        std::fs::write(d.path().join(FILE), "query = 1").unwrap();
        assert_eq!(load(d.path()), [] as [urfin_core::search::PastQuery; 0]);
    }
}
