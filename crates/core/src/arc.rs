//! Архивы: сводка для экрана архива (S8 §3.1, решение 2026-11-06 «s8-answers»).
//!
//! Чистые данные, без разбора форматов и I/O. `urfin-arc` публикует сводку, когда
//! строит индекс; UI читает её для метки формата у пути и статуса «архив · сжатие».
//! `urfin-ui` от `urfin-arc` не зависит.

use compact_str::CompactString;
use rustc_hash::FxHashMap;

use crate::Location;

/// Сводка одного архива.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArcSummary {
    /// Метка формата у пути: `zip`, `7z`, `tar.gz`, `rar`.
    pub format: CompactString,
    /// Размер файла архива, байты.
    pub size: u64,
    /// Сумма размеров файлов внутри.
    pub unpacked: u64,
    /// Сумма сжатых размеров файлов внутри.
    pub packed: u64,
    /// Файлов внутри, без каталогов.
    pub files: u64,
    /// Есть зашифрованные записи или имена.
    pub encrypted: bool,
    /// Сплошное сжатие: файл нельзя достать, не распаковав соседние.
    pub solid: bool,
    /// Индекс дочитан. Tar.* и rar строятся по ходу чтения: пока `false`, суммы растут.
    pub complete: bool,
}

impl ArcSummary {
    /// «Сжатие N %» — сжатое от исходного, как в 7-Zip и Far. `None` — внутри пусто.
    pub fn ratio_pct(&self) -> Option<u8> {
        if self.unpacked == 0 {
            return None;
        }
        let pct = (u128::from(self.packed) * 100 + u128::from(self.unpacked) / 2)
            / u128::from(self.unpacked);
        Some(u8::try_from(pct.min(100)).unwrap_or(100))
    }
}

/// Модель архивов в `AppState.ext`: сводки по адресу файла архива. Мутации — в Handler
/// и Msg, UI только читает.
#[derive(Debug, Default)]
pub struct ArcModel {
    /// Ключ — адрес файла архива: `outer` адреса внутри него.
    open: FxHashMap<Location, ArcSummary>,
    /// Ключ перерисовки UI.
    rev: u64,
}

impl ArcModel {
    /// Сводка архива `at` (адрес файла архива); прежняя заменяется.
    pub fn set(&mut self, at: Location, s: ArcSummary) {
        if self.open.get(&at) == Some(&s) {
            return;
        }
        self.open.insert(at, s);
        self.bump();
    }

    /// Сводка архива `at` (адрес файла архива).
    pub fn get(&self, at: &Location) -> Option<&ArcSummary> {
        self.open.get(at)
    }

    /// Сводка архива, внутри которого каталог `dir` (по его `outer`). Вне архива — `None`.
    pub fn of_dir(&self, dir: &Location) -> Option<&ArcSummary> {
        self.get(dir.outer.as_deref()?)
    }

    /// Убирает сводку: индекс вышел из кэша или архив закрыт.
    pub fn remove(&mut self, at: &Location) -> Option<ArcSummary> {
        let s = self.open.remove(at);
        if s.is_some() {
            self.bump();
        }
        s
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Растёт на каждое изменение модели.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;

    fn zip_file() -> Location {
        Location::local(r"C:\Dev\urfin\urfin-0.4.2.zip")
    }

    fn inside(file: &Location, path: &str) -> Location {
        Location {
            backend: "zip".into(),
            authority: None,
            path: PathBuf::from(path),
            outer: Some(Arc::new(file.clone())),
        }
    }

    fn summary() -> ArcSummary {
        ArcSummary {
            format: "zip".into(),
            size: 9_332_326,
            unpacked: 24_000_000,
            packed: 9_120_000,
            files: 412,
            encrypted: false,
            solid: false,
            complete: true,
        }
    }

    #[test]
    fn arc_model_finds_summary_by_dir_inside() {
        let mut m = ArcModel::default();
        let file = zip_file();
        let rev = m.rev();
        m.set(file.clone(), summary());
        assert!(m.rev() > rev);
        assert_eq!(m.len(), 1);
        let deep = inside(&file, "/urfin-0.4.2/src");
        assert_eq!(m.of_dir(&deep).map(|s| s.format.as_str()), Some("zip"));
        assert_eq!(m.of_dir(&inside(&file, "/")), m.get(&file));
        assert!(m.of_dir(&file).is_none(), "сам файл архива — не внутри");
        let other = inside(&Location::local(r"C:\x.zip"), "/");
        assert!(m.of_dir(&other).is_none());
    }

    #[test]
    fn arc_model_rev_moves_only_on_change() {
        let mut m = ArcModel::default();
        let file = zip_file();
        m.set(file.clone(), summary());
        let rev = m.rev();
        m.set(file.clone(), summary());
        assert_eq!(m.rev(), rev, "та же сводка не будит UI");
        let mut grown = summary();
        grown.files += 1;
        m.set(file.clone(), grown);
        assert!(m.rev() > rev);
        let rev = m.rev();
        assert!(m.remove(&Location::local(r"C:\x.zip")).is_none());
        assert_eq!(m.rev(), rev);
        assert!(m.remove(&file).is_some());
        assert!(m.rev() > rev);
        assert!(m.is_empty());
    }

    #[test]
    fn arc_summary_ratio_rounds_and_clamps() {
        let s = summary();
        assert_eq!(s.ratio_pct(), Some(38));
        let empty = ArcSummary::default();
        assert_eq!(empty.ratio_pct(), None);
        let stored_worse = ArcSummary {
            unpacked: 10,
            packed: 12,
            ..ArcSummary::default()
        };
        assert_eq!(stored_worse.ratio_pct(), Some(100));
    }
}
