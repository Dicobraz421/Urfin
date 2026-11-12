//! Типы операций (S5 §2.1–2.3): запрос, опции и политики, проблемы (конфликты и
//! ошибки), отчёт. Движок, обход и платформенные ускорения — S5-04 и дальше.

use std::num::{NonZeroU8, NonZeroU64};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use urfin_core::{JobSummary, Location};

/// Запрос операции: что, откуда, куда и как.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpRequest {
    pub kind: OpKind,
    /// Каталог источников (`Targets::base`).
    pub base: Location,
    /// Имена в `base` (`Targets::items`); пусто для `Mkdir`.
    pub items: Vec<CompactString>,
    /// Назначение: Copy, Move, Link, Sync.
    pub dest: Option<Location>,
    #[serde(default)]
    pub opts: OpOptions,
}

/// Вид операции.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    Copy,
    Move,
    /// Насовсем (Shift+F8).
    Delete,
    /// В корзину (F8, S0-decisions №11).
    Trash,
    /// `a/b/c` и `x;y` — как в Far.
    Mkdir {
        path: CompactString,
    },
    Rename {
        to: CompactString,
    },
    MultiRename(RenameSpec),
    SetAttrs(AttrChange),
    Link(LinkKind),
    Compare(CompareMode),
    Sync(SyncSpec),
}

/// Групповое переименование (S5-23).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSpec {
    pub pattern: CompactString,
    pub regex: bool,
}

/// Смена атрибутов и времени (S5-22).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttrChange {
    /// Буквы атрибутов `RHSA…` для установки и снятия.
    pub set: CompactString,
    pub clear: CompactString,
    /// Время изменения, текстом (разбирает S5-22).
    pub mtime: Option<CompactString>,
}

/// Вид ссылки.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkKind {
    Hard,
    Symbolic,
    /// Только Windows.
    Junction,
}

/// Сравнение каталогов (S5-24).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareMode {
    /// Размер и время.
    Quick,
    Content,
}

/// Синхронизация каталогов (S5-24).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncSpec {
    LeftToRight,
    RightToLeft,
    Both,
}

/// Куда ложат элементы (Far): внутрь каталога или под новым именем.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DestMode {
    /// Внутрь существующего или создаваемого каталога.
    Into(Location),
    /// Один элемент под новым именем.
    RenameTo(Location),
}

/// Опции операции; значения по умолчанию — `[ops.defaults]` конфига.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpOptions {
    pub conflict: ConflictPolicy,
    pub dirs: DirPolicy,
    /// После авто-ретраев.
    pub errors: ErrorPolicy,
    pub verify: Verify,
    pub preserve: Preserve,
    pub links: LinkMode,
    /// Маска файлов (`core::Mask`, текстом).
    pub filter: Option<CompactString>,
    pub priority: Priority,
    pub engine: EngineOverride,
    pub durability: Durability,
    pub limit_bps: Option<NonZeroU64>,
    /// `None` — по носителю (§3.3).
    pub workers: Option<NonZeroU8>,
    pub cloud: CloudPolicy,
    pub queue: QueueMode,
}

impl Default for OpOptions {
    fn default() -> Self {
        OpOptions {
            conflict: ConflictPolicy::Ask,
            dirs: DirPolicy::Merge,
            errors: ErrorPolicy::Ask,
            verify: Verify::Off,
            preserve: Preserve::TIMES | Preserve::ATTRS | Preserve::STREAMS | Preserve::XATTRS,
            links: LinkMode::Preserve,
            filter: None,
            priority: Priority::Normal,
            engine: EngineOverride::Auto,
            durability: Durability::Fast,
            limit_bps: None,
            workers: None,
            cloud: CloudPolicy::Hydrate,
            queue: QueueMode::Auto,
        }
    }
}

/// Что делать, если файл назначения существует.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictPolicy {
    Ask,
    Overwrite,
    Skip,
    /// mtime источника новее (с учётом точности ФС).
    OverwriteIfNewer,
    /// Размер или mtime различаются.
    OverwriteIfDiffers,
    /// Размер и mtime равны — пропустить, иначе спросить (восстановление, sync).
    SkipIdentical,
    /// `name (2).ext`.
    RenameNew(RenameScheme),
    /// Старый → `name.bak~N`.
    RenameExisting(RenameScheme),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenameScheme {
    Counter,
    Timestamp,
}

/// Каталог назначения существует.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirPolicy {
    Merge,
    Ask,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorPolicy {
    Ask,
    Skip,
    Abort,
}

/// Проверка копии (S0-decisions №12: copy — выкл).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verify {
    Off,
    /// Размер и успешный fsync.
    Size,
    Hash(HashAlgo),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HashAlgo {
    Xxh3_128,
    Blake3,
}

bitflags::bitflags! {
    /// Что переносить вместе с данными.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Preserve: u8 {
        const TIMES = 1;
        const ATTRS = 1 << 1;
        /// Альтернативные потоки NTFS (ADS).
        const STREAMS = 1 << 2;
        const XATTRS = 1 << 3;
        const ACL = 1 << 4;
        const OWNER = 1 << 5;
        const BIRTHTIME = 1 << 6;
    }
}

/// Ссылки в источнике.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkMode {
    Preserve,
    Follow,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    Low,
    Normal,
    High,
}

/// Ручной выбор движка (бенчи, обход проблем).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineOverride {
    Auto,
    NoClone,
    Os,
    Stream,
    Unbuffered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Durability {
    /// Устойчиво к падению приложения.
    Fast,
    /// fsync на файл: устойчиво к потере питания.
    Safe,
}

/// Облачные плейсхолдеры (`OneDrive` и т.п.).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloudPolicy {
    Hydrate,
    Skip,
}

/// Очередь устройства (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    Auto,
    Now,
}

/// Номер проблемы внутри job.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct IssueId(pub u32);

/// Сведения об элементе для текста вопроса.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemMeta {
    pub name: CompactString,
    pub size: u64,
    /// Наносекунды от эпохи Unix.
    pub mtime: i64,
    pub attrs: u32,
}

/// Над чем шла работа (для текста вопроса и отчёта).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemOp {
    Copy,
    Move,
    Delete,
    Trash,
    Mkdir,
    Rename,
    SetAttrs,
    Link,
}

/// Проблема с элементом: конфликт или ошибка; паркуется, job идёт дальше (§5).
#[derive(Clone, Debug)]
pub struct Issue {
    pub id: IssueId,
    pub kind: IssueKind,
    pub src: Option<ItemMeta>,
    pub dst: Option<ItemMeta>,
    pub op: ItemOp,
    /// Данные уже скопированы во временный файл (гонка при rename).
    pub temp: Option<PathBuf>,
    /// Текст ошибки ОС (`IssueKind::Io`, `LowSpace`).
    pub error: Option<Arc<str>>,
    /// Полный путь элемента с ошибкой и код ошибки ОС — для строки-вопроса (D-04) и
    /// поиска процесса, держащего файл (№69).
    pub path: Option<PathBuf>,
    pub code: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IssueKind {
    /// Файл назначения существует.
    Exists,
    /// Файл ↔ каталог.
    TypeMismatch,
    /// Источник и назначение — один объект.
    SameFile,
    ReadOnlyTarget,
    /// Недопустимо на ФС назначения (`:`, `CON`, коллизия регистра).
    InvalidName,
    /// Больше 4 ГБ на `FAT32`.
    TooLarge,
    LowSpace,
    /// Элемент нельзя в корзину (сеть, слишком большой, длинный путь).
    NoTrash,
    /// Облачный плейсхолдер будет скачан.
    Cloud,
    Io(IoClass),
}

/// Класс ошибки ввода-вывода (S5-11 классифицирует коды ОС).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IoClass {
    Permission,
    Sharing,
    Busy,
    Network,
    NotFound,
    Other,
}

impl IoClass {
    /// Может пройти сама: повторяется автоматически (0,2 / 1 / 4 с, S5-11).
    pub fn transient(self) -> bool {
        matches!(self, IoClass::Sharing | IoClass::Busy | IoClass::Network)
    }
}

/// Ответ на проблему (id выбора в `PromptSpec.choices`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Overwrite,
    /// Заменить, если источник новее, иначе пропустить (S5 §2.2, вариант W).
    OverwriteIfNewer,
    Skip,
    RenameNew,
    RenameExisting,
    Retry,
    NewName(String),
    DeletePermanently,
    /// Вместо symlink без прав — junction (S5-22, только каталог).
    Junction,
    Abort,
}

/// Движок копирования файла (§0, таблица стратегий — S5-19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Engine {
    /// `std`: буфер 1 МБ.
    Stream,
    /// Системный (`CopyFile2`).
    Os,
    /// Block clone (`ReFS`, Dev Drive).
    Clone,
    /// Небуферизованный overlapped-поток для крупных.
    Unbuffered,
    /// Быстрый путь мелких файлов.
    Small,
    /// Не-локальный источник через `Vfs` (S0 v5 №16).
    Vfs,
}

/// Итог job.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub summary: JobSummary,
    pub cloned: u64,
    pub renamed_fast: u64,
    pub verified: u64,
    /// Не больше [`Report::FAILED_MAX`], остальные — в `summary.errors`.
    pub failed: Vec<FailedItem>,
    /// Пропущенное с адресом и причиной (`ops.issues`); не больше
    /// [`Report::FAILED_MAX`], всего — `summary.skipped`.
    pub skipped: Vec<SkippedItem>,
    pub elapsed: Duration,
    pub peak_bps: u64,
    /// Файлов по движкам — для бенчей и отладки.
    pub engines: SmallVec<[(Engine, u64); 6]>,
}

impl Report {
    pub const FAILED_MAX: usize = 10_000;
}

/// Пропущенный элемент: конфликт или ошибка, на которые ответили «Пропустить»
/// (сами или политикой), и вопросы без ответа при отмене (S5 §5.5).
#[derive(Clone, Debug)]
pub struct SkippedItem {
    pub path: PathBuf,
    pub op: ItemOp,
    /// Из-за чего пропущен.
    pub kind: IssueKind,
    /// Текст ошибки, если причина — ошибка.
    pub error: Option<Arc<str>>,
}

/// Состояние строки списка проблем.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IssueState {
    /// Ошибка: элемент не обработан.
    Failed,
    /// Пропущен (ответом, политикой или отменой).
    Skipped,
    /// Вопрос ждёт ответа: job идёт дальше.
    Waiting,
}

/// Строка списка проблем: адрес и причина.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRow {
    pub path: PathBuf,
    pub state: IssueState,
    pub op: ItemOp,
    /// Причина для показа (текст ошибки или вид проблемы).
    pub reason: Arc<str>,
    /// Ошибку исправит `ops.retry`.
    pub retry: bool,
}

/// Список проблем задачи (S5 §5.5, запрос к S0 №4): лежит в `AppState.ext`, кладёт его
/// `ops.issues {job}` — и у идущей задачи (снимок на момент команды), и по отчёту
/// завершённой. Окно рисует его по событию `UiEvent::Ext{"ops.issues"}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobIssues {
    pub job: urfin_core::JobId,
    /// Ошибки, затем пропуски, затем ждущие ответа.
    pub rows: Arc<[IssueRow]>,
    /// Проблем без строки: сверх [`Report::FAILED_MAX`] и пропуски без адреса.
    pub more: u64,
}

impl JobIssues {
    /// Строк в состоянии `state`.
    pub fn count(&self, state: IssueState) -> usize {
        self.rows.iter().filter(|r| r.state == state).count()
    }
}

/// Элемент, который не удалось обработать (для `ops.retry`).
#[derive(Clone, Debug)]
pub struct FailedItem {
    pub path: PathBuf,
    pub op: ItemOp,
    pub error: Arc<str>,
    pub class: IoClass,
    /// Источник и назначение для `ops.retry`; `None` — повтор не поможет
    /// (копия в себя, нет источника и т.п.).
    pub pair: Option<(PathBuf, PathBuf)>,
}
