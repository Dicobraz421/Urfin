//! Модель анализа места (F01 §2.1): узлы каталогов и записи крупных файлов —
//! фиксированного размера (секции кэша на диске пишутся как есть), снимок, фильтры,
//! группы дубликатов. Живое дерево на атомиках — [`DuTree`] (F01 §3.2).

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::volume::FsKind;
use crate::walk::RawName;

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

    /// Тип по расширению (таблица иконок S4 §5.7; временные — как подсветка №32).
    pub fn of(name: &RawName<'_>) -> DuType {
        use crate::icons::IconClass as C;
        let mut buf = [0u8; 16];
        let Some(ext) = name.ext_lower(&mut buf) else {
            return DuType::Other;
        };
        if matches!(ext, "tmp" | "temp" | "bak" | "swp" | "old") {
            return DuType::Temp;
        }
        match crate::icons::class_by_ext(ext) {
            Some(C::Video) => DuType::Video,
            Some(C::Audio) => DuType::Audio,
            Some(C::Image) => DuType::Image,
            Some(C::Doc | C::Text) => DuType::Doc,
            Some(C::Archive) => DuType::Archive,
            Some(C::Disk) => DuType::DiskImage,
            Some(C::Exec) => DuType::Exec,
            Some(C::Code) => DuType::Code,
            _ => DuType::Other,
        }
    }
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
    /// [`DuSnapshot::PARTIAL`] | [`DuSnapshot::APPROX`].
    pub flags: u8,
}

impl DuSnapshot {
    /// Скан прерван: размеры недосчитанных каталогов — «≥ X».
    pub const PARTIAL: u8 = 1;
    /// Запасной листер (`FindFirstFileExW`): размер на диске приблизителен, ссылки
    /// не дедуплицированы.
    pub const APPROX: u8 = 1 << 1;

    /// Имя каталога из арены (корень — полный путь не хранит: пусто).
    pub fn name(&self, id: DirId) -> &str {
        self.dirs
            .get(id as usize)
            .and_then(|d| std::str::from_utf8(&self.names[d.name.range()]).ok())
            .unwrap_or("")
    }

    /// Имя крупного файла из арены.
    pub fn file_name(&self, f: &FileRec) -> &str {
        std::str::from_utf8(&self.names[f.name.range()]).unwrap_or("")
    }
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

// ---------------------------------------------------------------------------
// DuTree: живое дерево скана (F01 §3.2)

/// Узлов в сегменте.
const SEG: usize = 4096;
/// Сегментов максимум: 16M каталогов.
const MAX_SEGS: usize = 4096;
const NO_PARENT: DirId = u32::MAX;

struct LiveNode {
    /// `NameRef`: `off | len << 32`.
    name: AtomicU64,
    file_id: AtomicU64,
    disk: AtomicU64,
    logical: AtomicU64,
    mtime: AtomicI64,
    parent: AtomicU32,
    flags: AtomicU32,
    files: AtomicU32,
    /// Свой листинг (1) + недосчитанные подкаталоги.
    pending: AtomicU32,
}

impl LiveNode {
    fn new() -> LiveNode {
        LiveNode {
            name: AtomicU64::new(0),
            file_id: AtomicU64::new(0),
            disk: AtomicU64::new(0),
            logical: AtomicU64::new(0),
            mtime: AtomicI64::new(0),
            parent: AtomicU32::new(NO_PARENT),
            flags: AtomicU32::new(0),
            files: AtomicU32::new(0),
            pending: AtomicU32::new(1),
        }
    }
}

fn pack(n: NameRef) -> u64 {
    let NameRef { off, len } = n;
    u64::from(off) | (u64::from(len) << 32)
}

#[allow(clippy::cast_possible_truncation, reason = "распаковка pack()")]
fn unpack(v: u64) -> NameRef {
    NameRef {
        off: v as u32,
        len: (v >> 32) as u16,
    }
}

/// Живое дерево скана: сегменты по 4096 узлов, агрегаты — атомики; UI читает
/// без блокировок (F01 §3.2). Узел появляется при листинге родителя; файлы
/// каталога одной прибавкой идут в узел и цепочку предков; когда досчитаны он и все
/// подкаталоги — `DONE`.
pub struct DuTree {
    segs: Box<[OnceLock<Box<[LiveNode]>>]>,
    len: AtomicU32,
    names: Mutex<Vec<u8>>,
    /// Крупные файлы (≥ `track_min`), в порядке поступления.
    files: Mutex<Vec<FileRec>>,
    types_disk: [AtomicU64; DuType::COUNT],
    types_files: [AtomicU64; DuType::COUNT],
    denied: Mutex<Vec<DirId>>,
}

impl std::fmt::Debug for DuTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuTree")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for DuTree {
    fn default() -> Self {
        Self::new()
    }
}

/// Поля снимка, которых нет в дереве.
#[derive(Clone, Debug)]
pub struct SnapMeta {
    pub root: PathBuf,
    pub volume: VolumeKey,
    pub scanned_at: i64,
    pub took_ms: u32,
    pub size_mode: SizeMode,
    pub flags: u8,
}

impl DuTree {
    /// Дерево с корнем (`DirId` 0, имя пустое).
    pub fn new() -> DuTree {
        let t = DuTree {
            segs: (0..MAX_SEGS).map(|_| OnceLock::new()).collect(),
            len: AtomicU32::new(0),
            names: Mutex::new(Vec::new()),
            files: Mutex::new(Vec::new()),
            types_disk: std::array::from_fn(|_| AtomicU64::new(0)),
            types_files: std::array::from_fn(|_| AtomicU64::new(0)),
            denied: Mutex::new(Vec::new()),
        };
        let root = t.alloc().expect("root");
        debug_assert_eq!(root, 0);
        t
    }

    fn alloc(&self) -> Option<DirId> {
        let id = self.len.fetch_add(1, Ordering::AcqRel);
        let seg = self.segs.get(id as usize / SEG)?;
        seg.get_or_init(|| (0..SEG).map(|_| LiveNode::new()).collect());
        Some(id)
    }

    fn node(&self, id: DirId) -> Option<&LiveNode> {
        let id = id as usize;
        self.segs.get(id / SEG)?.get()?.get(id % SEG)
    }

    /// Узлов выделено (часть последних может ещё заполняться).
    pub fn len(&self) -> u32 {
        self.len
            .load(Ordering::Acquire)
            .min(u32::try_from(SEG * MAX_SEGS).unwrap_or(u32::MAX))
    }

    /// Всегда есть корень.
    pub fn is_empty(&self) -> bool {
        false
    }

    fn put_name(&self, name: &RawName<'_>) -> NameRef {
        let (s, escaped) = name.to_name();
        self.put_str(&s, escaped)
    }

    fn put_str(&self, s: &str, escaped: bool) -> NameRef {
        let mut names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        let off = u32::try_from(names.len()).unwrap_or(u32::MAX);
        let len = u16::try_from(s.len()).unwrap_or(0x7FFF).min(0x7FFF);
        names.extend_from_slice(&s.as_bytes()[..usize::from(len)]);
        NameRef::new(off, len, escaped)
    }

    /// Подкаталог `name` у `parent` (`pending` родителя растёт). `None` — дерево
    /// переполнено (каталог не обходится).
    pub fn add_dir(
        &self,
        parent: DirId,
        name: &RawName<'_>,
        mtime: i64,
        file_id: u64,
    ) -> Option<DirId> {
        let p = self.node(parent)?;
        let id = self.alloc()?;
        let n = self.node(id)?;
        n.name.store(pack(self.put_name(name)), Ordering::Relaxed);
        n.mtime.store(mtime, Ordering::Relaxed);
        n.file_id.store(file_id, Ordering::Relaxed);
        n.parent.store(parent, Ordering::Release);
        p.pending.fetch_add(1, Ordering::AcqRel);
        Some(id)
    }

    /// `mtime` и `FileId` корня (из stat).
    pub fn set_root_meta(&self, mtime: i64, file_id: u64) {
        if let Some(n) = self.node(0) {
            n.mtime.store(mtime, Ordering::Relaxed);
            n.file_id.store(file_id, Ordering::Relaxed);
        }
    }

    /// Прибавить файлы каталога `id` к нему и всем предкам.
    pub fn add_sizes(&self, id: DirId, disk: u64, logical: u64, files: u32) {
        if disk == 0 && logical == 0 && files == 0 {
            return;
        }
        let mut cur = id;
        while let Some(n) = self.node(cur) {
            n.disk.fetch_add(disk, Ordering::Relaxed);
            n.logical.fetch_add(logical, Ordering::Relaxed);
            n.files.fetch_add(files, Ordering::Relaxed);
            cur = n.parent.load(Ordering::Acquire);
        }
    }

    /// Суммы по типам (по всем файлам).
    pub fn add_types(&self, disk: &[u64; DuType::COUNT], files: &[u64; DuType::COUNT]) {
        for i in 0..DuType::COUNT {
            if files[i] != 0 {
                self.types_disk[i].fetch_add(disk[i], Ordering::Relaxed);
                self.types_files[i].fetch_add(files[i], Ordering::Relaxed);
            }
        }
    }

    /// Крупные файлы каталога: имена — в арену, записи — в общий список.
    pub fn add_large(&self, recs: &[(FileRec, compact_str::CompactString, bool)]) {
        if recs.is_empty() {
            return;
        }
        let named: SmallVec<[FileRec; 8]> = recs
            .iter()
            .map(|(r, name, escaped)| FileRec {
                name: self.put_str(name, *escaped),
                ..*r
            })
            .collect();
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(&named);
    }

    pub fn set_flag(&self, id: DirId, f: u16) {
        if let Some(n) = self.node(id) {
            n.flags.fetch_or(u32::from(f), Ordering::AcqRel);
        }
    }

    /// Каталог не прочитан (нет доступа): `DENIED`, у предков `PARTIAL_DENIED`.
    pub fn deny(&self, id: DirId) {
        self.set_flag(id, NodeFlags::DENIED);
        let mut cur = self
            .node(id)
            .map_or(NO_PARENT, |n| n.parent.load(Ordering::Acquire));
        while let Some(n) = self.node(cur) {
            n.flags
                .fetch_or(u32::from(NodeFlags::PARTIAL_DENIED), Ordering::AcqRel);
            cur = n.parent.load(Ordering::Acquire);
        }
        self.denied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
    }

    /// Листинг `id` (или досчёт подкаталога) завершён: `pending -= 1`; ноль —
    /// `DONE` и то же для родителя.
    pub fn complete(&self, id: DirId) {
        let mut cur = id;
        while let Some(n) = self.node(cur) {
            if n.pending.fetch_sub(1, Ordering::AcqRel) != 1 {
                return;
            }
            n.flags
                .fetch_or(u32::from(NodeFlags::DONE), Ordering::AcqRel);
            cur = n.parent.load(Ordering::Acquire);
        }
    }

    /// Узел как в снимке (значения на момент чтения).
    pub fn get(&self, id: DirId) -> Option<DuNode> {
        let n = self.node(id)?;
        #[allow(clippy::cast_possible_truncation, reason = "флаги — 16 бит")]
        let flags = NodeFlags(n.flags.load(Ordering::Acquire) as u16);
        Some(DuNode {
            parent: n.parent.load(Ordering::Acquire),
            name: unpack(n.name.load(Ordering::Relaxed)),
            flags,
            files: n.files.load(Ordering::Relaxed),
            file_id: n.file_id.load(Ordering::Relaxed),
            disk: n.disk.load(Ordering::Relaxed),
            logical: n.logical.load(Ordering::Relaxed),
            mtime: n.mtime.load(Ordering::Relaxed),
        })
    }

    /// Итог корня.
    pub fn total(&self) -> DuNode {
        self.get(0).expect("root")
    }

    /// Имя каталога (копия из арены).
    pub fn name(&self, id: DirId) -> String {
        let Some(n) = self.get(id) else {
            return String::new();
        };
        let names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        names
            .get(n.name.range())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    /// Текущий топ крупных файлов по размеру на диске (для «Крупные» во время скана).
    pub fn top_files(&self, n: usize) -> Vec<FileRec> {
        let mut v = self
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        v.sort_unstable_by(|a, b| b.disk.cmp(&a.disk).then(b.logical.cmp(&a.logical)));
        v.truncate(n);
        v
    }

    /// Каталогов без доступа.
    pub fn denied_count(&self) -> usize {
        self.denied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Заморозить в снимок (O(n), без I/O). Зовётся после обхода.
    pub fn freeze(&self, meta: SnapMeta) -> DuSnapshot {
        let n = self.len();
        let dirs: Vec<DuNode> = (0..n).filter_map(|id| self.get(id)).collect();
        let names = self
            .names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        files.sort_unstable_by(|a, b| b.disk.cmp(&a.disk).then(b.logical.cmp(&a.logical)));
        let mut denied = self
            .denied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        denied.sort_unstable();
        let types = TypeAgg {
            disk: std::array::from_fn(|i| self.types_disk[i].load(Ordering::Relaxed)),
            files: std::array::from_fn(|i| self.types_files[i].load(Ordering::Relaxed)),
        };
        DuSnapshot {
            root: meta.root,
            volume: meta.volume,
            scanned_at: meta.scanned_at,
            took_ms: meta.took_ms,
            generation: 1,
            usn: None,
            dirs,
            names,
            files,
            types,
            denied,
            size_mode: meta.size_mode,
            flags: meta.flags,
        }
    }
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

    fn s(name: &str) -> RawName<'_> {
        RawName::Str {
            name,
            escaped: false,
        }
    }

    /// Агрегаты идут вверх; `DONE` — когда досчитаны каталог и все подкаталоги.
    #[test]
    fn tree_aggregates_and_done() {
        let t = DuTree::new();
        let a = t.add_dir(0, &s("a"), 5, 7).unwrap();
        let b = t.add_dir(a, &s("b"), 0, 0).unwrap();
        t.add_sizes(0, 10, 11, 1);
        t.complete(0);
        assert!(!t.total().flags.has(NodeFlags::DONE), "a is pending");
        t.add_sizes(a, 100, 101, 2);
        t.complete(a);
        assert!(
            !t.get(a).unwrap().flags.has(NodeFlags::DONE),
            "b is pending"
        );
        t.deny(b);
        t.complete(b);
        let root = t.total();
        assert!(root.flags.has(NodeFlags::DONE | NodeFlags::PARTIAL_DENIED));
        assert_eq!((root.disk, root.logical, root.files), (110, 112, 3));
        let an = t.get(a).unwrap();
        assert_eq!((an.disk, an.files, an.mtime, an.file_id), (100, 2, 5, 7));
        assert!(
            t.get(b)
                .unwrap()
                .flags
                .has(NodeFlags::DENIED | NodeFlags::DONE)
        );
        assert_eq!(t.name(a), "a");

        let f = FileRec {
            dir: a,
            name: NameRef::default(),
            ty: DuType::Video,
            attrs: 0,
            disk: 5 << 20,
            logical: 5 << 20,
            mtime: 0,
            file_id: 0,
        };
        t.add_large(&[
            (f, "small.mkv".into(), false),
            (FileRec { disk: 9 << 20, ..f }, "big.mkv".into(), false),
        ]);
        let snap = t.freeze(SnapMeta {
            root: PathBuf::from("C:\\x"),
            volume: VolumeKey {
                serial: 1,
                fs: FsKind::Ntfs,
            },
            scanned_at: 0,
            took_ms: 1,
            size_mode: SizeMode::Disk,
            flags: 0,
        });
        assert_eq!(snap.dirs.len(), 3);
        assert_eq!(snap.name(b), "b");
        assert_eq!(snap.denied, vec![b]);
        assert_eq!(snap.file_name(&snap.files[0]), "big.mkv");
        assert_eq!(snap.files[1].disk, 5 << 20);
    }

    #[test]
    fn types_by_extension() {
        let w: Vec<u16> = "Movie.MKV".encode_utf16().collect();
        assert_eq!(DuType::of(&RawName::Wide(&w)), DuType::Video);
        assert_eq!(DuType::of(&s("a.tmp")), DuType::Temp);
        assert_eq!(DuType::of(&s("notes.txt")), DuType::Doc);
        assert_eq!(DuType::of(&s("disk.vhdx")), DuType::DiskImage);
        assert_eq!(DuType::of(&s(".bashrc")), DuType::Other);
        assert_eq!(DuType::of(&s("noext")), DuType::Other);
    }
}
