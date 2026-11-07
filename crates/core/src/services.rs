//! Сервисы, которые внедряют другие крейты (S1 §2.6): протокол листинга с S4
//! (+ S0 v2 №1–№4) и хук открытия файлов (S0 v5 №9).

use std::fmt;
use std::sync::Arc;

use crate::cancel::CancelToken;
use crate::cmd::{Args, Command};
use crate::entry::Entry;
use crate::ids::{Gen, TabId};
use crate::loc::Location;
use crate::mask::Mask;
use crate::msg::{Msg, msg};
use crate::runtime::{Lane, Runtime};
use crate::tab::SortSpec;

/// Листинг каталогов. Реализует S4. Методы не блокируют: только spawn в `rt`,
/// результаты — только через [`listing_msg`].
pub trait ListingService: Send + Sync + 'static {
    fn load(&self, req: LoadReq, rt: &Runtime);
    fn arrange(&self, req: ArrangeReq, rt: &Runtime);
    /// Жизненный цикл табов для watch и кэша (S0 v2 №4).
    fn tab_event(&self, _ev: TabEvent) {}
}

/// Как показывать: сортировка, фильтр, скрытые.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrangeSpec {
    pub sort: SortSpec,
    pub filter: Option<Mask>,
    pub show_hidden: bool,
}

/// Загрузить каталог.
#[derive(Clone, Debug)]
pub struct LoadReq {
    pub tab: TabId,
    pub generation: Gen,
    /// `Listing.arrange_seq` на момент запроса (S0 v2 №3).
    pub seq: u32,
    pub loc: Location,
    pub cancel: CancelToken,
    pub view: ArrangeSpec,
}

/// Пересчитать порядок уже загруженных записей.
#[derive(Clone, Debug)]
pub struct ArrangeReq {
    pub tab: TabId,
    pub generation: Gen,
    pub seq: u32,
    pub entries: Arc<Vec<Entry>>,
    pub cancel: CancelToken,
    pub view: ArrangeSpec,
}

/// Инкрементальное изменение загруженного каталога (S0 v2 №2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    /// Ожидаемое `entries.len()` до патча.
    pub base_len: u32,
    pub seq: u32,
    pub update: Vec<(u32, Entry)>,
    /// По убыванию; применяется как `swap_remove`.
    pub remove: Vec<u32>,
    pub append: Vec<Entry>,
    pub order: Arc<Vec<u32>>,
}

/// Порция результата.
#[derive(Clone, Debug)]
pub enum ListingUpdate {
    /// Дописать `entries`; `visible` — абсолютные индексы новых записей, прошедших
    /// фильтр и hidden, в порядке прихода (S0 v2 №1).
    Chunk {
        entries: Vec<Entry>,
        visible: Vec<u32>,
    },
    /// Полный порядок; отбрасывается, если `entries.len() != for_len` или `seq`
    /// устарел (S0 v2 №3).
    Order {
        order: Arc<Vec<u32>>,
        for_len: u32,
        seq: u32,
    },
    Patch(Patch),
    /// Все записи пришли, порядок окончательный.
    Done,
    /// Все записи пришли, но финальный порядок не посчитан (сервис не держит копию
    /// записей, ADR-0017): core сам запрашивает `ListingService::arrange` по своим
    /// записям и отмечает `Complete` (при refresh — подменяет листинг), когда придёт
    /// `Order` на полную длину.
    DoneUnsorted,
    Failed(Arc<str>),
}

/// События табов для S4 (S0 v2 №4).
#[derive(Clone, Debug)]
pub enum TabEvent {
    Shown(TabId),
    Hidden(TabId),
    Closed(TabId),
    /// Таб уходит из `loc` (навигация или замена при refresh) — до замены листинга.
    Left {
        tab: TabId,
        loc: Location,
        entries: Arc<Vec<Entry>>,
    },
}

/// Готовый `Msg`: core сам проверит поколение, применит обновление, восстановит
/// курсор и выделение.
pub fn listing_msg(tab: TabId, generation: Gen, u: ListingUpdate) -> Box<dyn Msg> {
    msg("listing", move |cx| {
        crate::nav::apply_update(cx, tab, generation, u);
    })
}

/// Кто открывает файл по Enter (S0 v3 №9, v5 №9). Опрашиваются по порядку
/// регистрации (S8 первым); первый `Some` выполняется вместо `file.open`.
pub trait OpenResolver: Send + Sync + 'static {
    fn resolve(&self, dir: &Location, e: &Entry, args: &Args) -> Option<Command>;

    /// Enter по каталогу `e` в `dir`: виртуальная схема (`search:`, `cmp:`) ведёт в
    /// настоящий каталог; первый `Some` выполняется вместо входа в `dir\e`.
    fn enter_dir(&self, _dir: &Location, _e: &Entry, _args: &Args) -> Option<Command> {
        None
    }
}

/// Листинг без ФС: каждый каталог пуст. Для `--smoke` и тестов шины.
#[derive(Debug, Default)]
pub struct NullListing;

impl ListingService for NullListing {
    fn load(&self, req: LoadReq, rt: &Runtime) {
        rt.spawn(
            Lane::Interactive,
            "null.list",
            req.cancel.clone(),
            move |cx| {
                let _ = cx.sink.send(
                    listing_msg(req.tab, req.generation, ListingUpdate::Done),
                    &cx.cancel,
                );
            },
        );
    }

    fn arrange(&self, req: ArrangeReq, rt: &Runtime) {
        rt.spawn(
            Lane::Interactive,
            "null.arrange",
            req.cancel.clone(),
            move |cx| {
                let u = ListingUpdate::Order {
                    order: Arc::default(),
                    for_len: req.entries.len() as u32,
                    seq: req.seq,
                };
                let _ = cx
                    .sink
                    .send(listing_msg(req.tab, req.generation, u), &cx.cancel);
            },
        );
    }
}

/// Внешние сервисы Core.
pub struct Services {
    pub listing: Arc<dyn ListingService>,
    pub open_resolvers: Vec<Arc<dyn OpenResolver>>,
}

impl Services {
    /// Без внешних сервисов (`NullListing`, без резолверов) — для тестов и `--smoke`.
    pub fn null() -> Self {
        Services {
            listing: Arc::new(NullListing),
            open_resolvers: Vec::new(),
        }
    }
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Services")
            .field("open_resolvers", &self.open_resolvers.len())
            .finish_non_exhaustive()
    }
}
