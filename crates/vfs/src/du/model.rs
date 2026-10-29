//! Модель анализа места (F01 §2.1): узлы каталогов и записи крупных файлов —
//! фиксированного размера (секции кэша на диске пишутся как есть), снимок, фильтры,
//! группы дубликатов. Живое дерево на атомиках (`DuTree`) — со сканом (F01-03).

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::volume::FsKind;

/// Индекс каталога в снимке; 0 — корень.
pub type DirId = u32;

/// Имя в арене UTF-8 снимка: 6 Б (`packed(2)`), чтобы [`DuNode`] уложился в 48 Б.
#[repr(C, packed(2))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct NameRef {
    pub off: u32,
    /// Длина в байтах; старший бит — [`NameRef::ESCAPED`].
    pub len: u16,
}

impl NameRef {
    /// Имя с экранированными не-UTF-8 юнитами (как `Attrs::ESCAPED_NAME`, S4 §0).
    pub const ESCAPED: u16 = 1 << 15;

    pub fn new(off: u32, len: u16, escaped: bool) -> NameRef {
        NameRef {
            off,
            len: (len & !Self::ESCAPED) | if escaped { Self::ESCAPED } else { 0 },
        }
    }

    /// Байты имени в арене.
    pub fn range(self) -> Range<usize> {
        let off = self.off as usize;
        off..off + usize::from(self.len & !Self::ESCAPED)
    }

    pub fn escaped(self) -> bool {
        self.len & Self::ESCAPED != 0
    }
}

/// Флаги каталога.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct NodeFlags(pub u16);

impl NodeFlags {
    /// Поддерево досчитано.
    pub const DONE: u16 = 1;
    /// Каталог не прочитан: нет доступа.
    pub const DENIED: u16 = 1 << 1;
    /// В поддереве есть каталоги без доступа.
    pub const PARTIAL_DENIED: u16 = 1 << 2;
    /// mtime сменился после скана («●»).
    pub const CHANGED: u16 = 1 << 3;
    /// В каталоге пропущены symlink/junction/mount point.
    pub const LINK_SKIPPED: u16 = 1 << 4;

    /// Установлены все биты `f`.
    pub fn has(self, f: u16) -> bool {
        self.0 & f == f
    }
}

/// Каталог снимка: 48 Б, `repr(C)` без выравнивающих дыр.
///
/// Файлов в самом каталоге в узле нет (не влезает в 48 Б): это `files` минус
/// сумма `files` подкаталогов.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DuNode {
    /// `u32::MAX` у корня.
    pub parent: DirId,
    pub name: NameRef,
    pub flags: NodeFlags,
    /// Файлов в поддереве.
    pub files: u32,
    /// `FileId` каталога (сопоставление с USN), 0 — неизвестен.
    pub file_id: u64,
    /// Размер поддерева на диске.
    pub disk: u64,
    /// Логический размер поддерева.
    pub logical: u64,
    /// `LastWriteTime` каталога (перепроверка mtime).
    pub mtime: i64,
}

/// Тип файла для фильтра «Типы» и цвета карты.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DuType {
    Video,
    Audio,
    Image,
    Doc,
    Archive,
    DiskImage,
    Exec,
    Code,
    Temp,
    Other,
}

impl DuType {
    pub const COUNT: usize = 10;
}

/// Крупный файл (`disk ≥ track_min`): 48 Б, `repr(C)`; 4 Б выравнивания после `attrs`.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FileRec {
    pub dir: DirId,
    pub name: NameRef,
    pub ty: DuType,
    /// [`FileRec::OFFLINE`] | [`FileRec::COMPRESSED`] | [`FileRec::HIDDEN`] | [`FileRec::SYSTEM`].
    pub attrs: u8,
    pub disk: u64,
    pub logical: u64,
    pub mtime: i64,
    pub file_id: u64,
}

impl FileRec {
    pub const OFFLINE: u8 = 1;
    pub const COMPRESSED: u8 = 1 << 1;
    pub const HIDDEN: u8 = 1 << 2;
    pub const SYSTEM: u8 = 1 << 3;
}

/// Суммы по типам — по всем файлам, включая мелкие; индекс — `DuType as usize`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeAgg {
    pub disk: [u64; DuType::COUNT],
    pub files: [u64; DuType::COUNT],
}

/// Том снимка: серийный номер и ФС.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VolumeKey {
    pub serial: u32,
    pub fs: FsKind,
}

/// Позиция в USN-журнале NTFS на момент снимка.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UsnMark {
    pub journal_id: u64,
    pub next_usn: i64,
}

/// Замороженный результат скана (кэш на диске — F01 §2.2).
#[derive(Clone, Debug)]
pub struct DuSnapshot {
    pub root: PathBuf,
    pub volume: VolumeKey,
    pub scanned_at: i64,
    pub took_ms: u32,
    pub generation: u64,
    pub usn: Option<UsnMark>,
    pub dirs: Vec<DuNode>,
    pub names: Vec<u8>,
    /// Только `disk ≥ track_min`, по убыванию `disk`.
    pub files: Vec<FileRec>,
    pub types: TypeAgg,
    pub denied: Vec<DirId>,
    pub size_mode: SizeMode,
}

/// Фильтр-чип (Ctrl+1…6).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DuFilter {
    Dirs,
    Large,
    Types,
    Old,
    Dups,
    Temp,
}

/// Какой размер показывать (`[du] size`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeMode {
    #[default]
    Disk,
    Logical,
}

/// Группа возможных дубликатов.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DupGroup {
    pub size: u64,
    /// Индексы в `DuSnapshot::files`.
    pub files: SmallVec<[u32; 4]>,
    pub level: DupLevel,
}

/// Насколько проверено совпадение: «только размер» / частичный хэш / полный xxh3.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DupLevel {
    Size,
    Partial,
    Full,
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    /// Размеры фиксированы: секции кэша пишутся как есть (F01 §2.2).
    #[test]
    fn fixed_sizes() {
        assert_eq!(size_of::<NameRef>(), 6);
        assert_eq!(size_of::<DuNode>(), 48);
        assert_eq!(size_of::<FileRec>(), 48);
        assert_eq!(offset_of!(DuNode, file_id), 16);
        assert_eq!(offset_of!(FileRec, disk), 16);
    }

    #[test]
    fn name_ref_escaped_bit() {
        let n = NameRef::new(10, 5, true);
        assert!(n.escaped());
        assert_eq!(n.range(), 10..15);
        assert!(!NameRef::new(0, 3, false).escaped());
    }
}
