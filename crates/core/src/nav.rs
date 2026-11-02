//! Политика навигации (S1 §2.6): поколения и отмена, double-buffer refresh,
//! `pending_focus`, восстановление курсора и выделения.

use std::sync::Arc;
use std::time::{Duration, Instant};

use compact_str::CompactString;

use crate::entry::Entry;
use crate::history::HistEntry;
use crate::ids::{Gen, TabId};
use crate::loc::Location;
use crate::msg::Cx;
use crate::selection::Selection;
use crate::services::{ArrangeReq, ArrangeSpec, ListingUpdate, LoadReq, Patch, TabEvent};
use crate::state::Level;
use crate::tab::{Incoming, Listing, ListingStats, ListingStatus, Row, Tab};

/// Долгий refresh показывает новый листинг, не дожидаясь `Done`.
const REFRESH_SWAP_AFTER: Duration = Duration::from_millis(150);

pub(crate) fn arrange_spec(t: &Tab) -> ArrangeSpec {
    ArrangeSpec {
        sort: t.sort,
        filter: t.filter.clone(),
        show_hidden: t.show_hidden,
    }
}

/// Каталог `name` внутри `loc` (тот же backend и контейнер).
///
/// Имена с `Attrs::ESCAPED_NAME` здесь не декодируются: путь для ОС строит
/// `vfs::name::join_os` (S4).
pub(crate) fn child_loc(loc: &Location, name: &str) -> Location {
    let path = loc.path.join(name);
    if loc.is_local() {
        Location::local(path)
    } else {
        Location {
            backend: loc.backend.clone(),
            authority: loc.authority.clone(),
            path,
            outer: loc.outer.clone(),
        }
    }
}

/// Имя элемента под курсором (`None` на `..` и в пустом листинге).
pub(crate) fn current_name(t: &Tab) -> Option<CompactString> {
    match t.current()? {
        Row::Entry { entry, .. } => Some(entry.name.clone()),
        Row::Parent => None,
    }
}

fn cursor_idx(t: &Tab) -> Option<u32> {
    match t.current()? {
        Row::Entry { idx, .. } => Some(idx),
        Row::Parent => None,
    }
}

fn parent_offset(t: &Tab) -> u32 {
    u32::from(t.has_parent_row())
}

fn row_where(t: &Tab, f: impl Fn(u32, &Entry) -> bool) -> Option<u32> {
    let l = &t.listing;
    l.order
        .iter()
        .position(|&i| f(i, &l.entries[i as usize]))
        .map(|p| p as u32 + parent_offset(t))
}

pub(crate) fn row_of_name(t: &Tab, name: &str) -> Option<u32> {
    row_where(t, |_, e| e.name == name)
}

fn next_gen(t: &Tab) -> Gen {
    let inc = t.incoming.as_ref().map_or(0, |i| i.listing.generation.0);
    Gen(t.listing.generation.0.max(inc) + 1)
}

fn loading(generation: Gen) -> Listing {
    let mut l = Listing::new(generation);
    l.status = ListingStatus::Loading {
        since: Instant::now(),
    };
    l
}

/// Куда смотрел таб: для истории.
pub(crate) fn hist_entry(t: &Tab) -> HistEntry {
    HistEntry {
        loc: t.loc.clone(),
        focus: current_name(t),
        top: t.cursor.top,
    }
}

/// Перейти табом `id` в `loc`: новое поколение, отмена старой загрузки, листинг
/// стримится сразу. `focus` — имя, на которое встать, когда оно придёт; `top` —
/// прокрутка (для `back`/`forward`).
pub(crate) fn navigate(
    cx: &mut Cx<'_>,
    id: TabId,
    loc: Location,
    focus: Option<CompactString>,
    top: u32,
    push_history: bool,
) {
    if let Some(t) = cx.state.tab_mut(id) {
        t.restore_up = 0;
    }
    load_into(cx, id, loc, focus, top, push_history);
}

/// Загрузка таба из сессии: при ошибке таб поднимается к существующему родителю.
pub(crate) fn navigate_restored(
    cx: &mut Cx<'_>,
    id: TabId,
    loc: Location,
    focus: Option<CompactString>,
    top: u32,
) {
    load_into(cx, id, loc, focus, top, false);
}

fn load_into(
    cx: &mut Cx<'_>,
    id: TabId,
    loc: Location,
    focus: Option<CompactString>,
    top: u32,
    push_history: bool,
) {
    let (rt, services) = (cx.rt, cx.services);
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    if !tab.listing.entries.is_empty() {
        services.listing.tab_event(TabEvent::Left {
            tab: id,
            loc: tab.loc.clone(),
            entries: tab.listing.entries.clone(),
        });
    }
    if push_history {
        let h = hist_entry(tab);
        tab.history.push(h);
    }
    if let Some(c) = tab.cancel.take() {
        c.cancel();
    }
    let generation = next_gen(tab);
    let cancel = rt.token();
    tab.incoming = None;
    tab.loc = loc.clone();
    tab.listing = loading(generation);
    tab.final_order = false;
    tab.selection.clear();
    tab.cursor.row = 0;
    tab.cursor.top = top;
    tab.pending_focus = focus;
    tab.load_seq = 0;
    tab.cancel = Some(cancel.clone());
    tab.touch();
    let view = arrange_spec(tab);
    cx.state.visited.touch(&loc);
    cx.state.mark_session_dirty();
    let req = LoadReq {
        tab: id,
        generation,
        seq: 0,
        loc,
        cancel,
        view,
    };
    services.listing.load(req, rt);
}

/// Перечитать каталог в фоне (double-buffer): текущий листинг остаётся на экране,
/// новый подменяет его на `Done` или через 150 мс; курсор и выделение
/// восстанавливаются по именам. Если текущая загрузка не закончена или упала —
/// обычная навигация на то же место.
pub(crate) fn refresh(cx: &mut Cx<'_>, id: TabId) {
    let (rt, services) = (cx.rt, cx.services);
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    if !matches!(tab.listing.status, ListingStatus::Complete) {
        let (loc, focus, top) = (tab.loc.clone(), current_name(tab), tab.cursor.top);
        navigate(cx, id, loc, focus, top, false);
        return;
    }
    if let Some(c) = tab.cancel.take() {
        c.cancel();
    }
    let generation = next_gen(tab);
    let cancel = rt.token();
    let mut listing = loading(generation);
    listing.arrange_seq = tab.listing.arrange_seq;
    tab.load_seq = listing.arrange_seq;
    tab.incoming = Some(Incoming {
        listing,
        started: Instant::now(),
        final_order: false,
    });
    tab.cancel = Some(cancel.clone());
    let req = LoadReq {
        tab: id,
        generation,
        seq: tab.load_seq,
        loc: tab.loc.clone(),
        cancel,
        view: arrange_spec(tab),
    };
    cx.keep_rev();
    services.listing.load(req, rt);
    // Если за 150 мс не придёт ничего, swap сделает `swap_overdue` (S1 §2.6).
    rt.waker().wake_after(REFRESH_SWAP_AFTER);
}

/// Перечитывание идёт дольше 150 мс — показать новый листинг как есть (в том
/// числе ещё пустой: S2 покажет загрузку), не дожидаясь `Done` или чанка.
/// Вызывается в конце каждого pump.
pub(crate) fn swap_overdue(cx: &mut Cx<'_>) {
    let overdue: Vec<TabId> = cx
        .state
        .panels
        .iter()
        .flat_map(|p| p.tabs.iter())
        .filter(|t| {
            t.incoming
                .as_ref()
                .is_some_and(|i| i.started.elapsed() >= REFRESH_SWAP_AFTER)
        })
        .map(|t| t.id)
        .collect();
    for id in overdue {
        swap_incoming(cx, id);
    }
}

/// Пересчитать порядок после смены сортировки, фильтра или hidden.
pub(crate) fn arrange(cx: &mut Cx<'_>, id: TabId) {
    let (rt, services) = (cx.rt, cx.services);
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    tab.listing.arrange_seq += 1;
    tab.touch();
    cx.state.mark_session_dirty();
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    if matches!(tab.listing.status, ListingStatus::Loading { .. }) && tab.listing.entries.is_empty()
    {
        // Порядок для уже пришедших записей пересчитается на `Done`.
        return;
    }
    let req = ArrangeReq {
        tab: id,
        generation: tab.listing.generation,
        seq: tab.listing.arrange_seq,
        entries: tab.listing.entries.clone(),
        cancel: tab.cancel.clone().unwrap_or_else(|| rt.token()),
        view: arrange_spec(tab),
    };
    services.listing.arrange(req, rt);
}

/// Применение `listing_msg`.
pub(crate) fn apply_update(cx: &mut Cx<'_>, id: TabId, generation: Gen, u: ListingUpdate) {
    let Some(tab) = cx.state.tab_mut(id) else {
        cx.keep_rev();
        return;
    };
    if tab
        .incoming
        .as_ref()
        .is_some_and(|i| i.listing.generation == generation)
    {
        apply_incoming(cx, id, u);
    } else if tab.listing.generation == generation {
        match apply_shown(tab, u) {
            Follow::Nothing => {}
            Follow::Arrange => arrange(cx, id),
            Follow::Up => {
                let up = tab.loc.parent();
                tab.restore_up -= 1;
                if let Some(parent) = up {
                    navigate_restored(cx, id, parent, None, 0);
                }
            }
        }
    } else {
        // Устаревшее поколение: таб уже ушёл или перечитан.
        cx.keep_rev();
    }
}

/// Обновление перечитываемого листинга; на экране ничего не меняется до замены.
fn apply_incoming(cx: &mut Cx<'_>, id: TabId, u: ListingUpdate) {
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    let inc = tab.incoming.as_mut().expect("checked by caller");
    let swap = match u {
        ListingUpdate::Chunk { entries, visible } => {
            append_chunk(&mut inc.listing, entries, &visible);
            inc.started.elapsed() >= REFRESH_SWAP_AFTER
        }
        ListingUpdate::Order {
            order,
            for_len,
            seq,
        } => {
            let l = &mut inc.listing;
            let fits = l.entries.len() as u32 == for_len && seq == l.arrange_seq;
            if fits {
                l.order = order;
            }
            if fits && inc.final_order {
                inc.final_order = false;
                inc.listing.status = ListingStatus::Complete;
                true
            } else {
                false
            }
        }
        ListingUpdate::DoneUnsorted => {
            inc.final_order = true;
            arrange_incoming(cx, id);
            return;
        }
        // Патч к ещё не показанному листингу не применяется: следующий flush S4
        // сделает refresh.
        ListingUpdate::Patch(_) => false,
        ListingUpdate::Done => {
            inc.listing.status = ListingStatus::Complete;
            true
        }
        ListingUpdate::Failed(e) => {
            tab.incoming = None;
            cx.toast(Level::Error, e.to_string());
            return;
        }
    };
    if swap {
        swap_incoming(cx, id);
    } else {
        cx.keep_rev();
    }
}

fn swap_incoming(cx: &mut Cx<'_>, id: TabId) {
    let services = cx.services;
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    let Some(inc) = tab.incoming.take() else {
        return;
    };
    services.listing.tab_event(TabEvent::Left {
        tab: id,
        loc: tab.loc.clone(),
        entries: tab.listing.entries.clone(),
    });
    let names = tab.selection.names(&tab.listing);
    let on_parent = matches!(tab.current(), Some(Row::Parent));
    let cur = current_name(tab);
    let old_row = tab.cursor.row;
    let shown_seq = tab.listing.arrange_seq;
    tab.listing = inc.listing;
    tab.listing.arrange_seq = shown_seq;
    tab.final_order = inc.final_order;
    tab.selection = Selection::restore(&names, &tab.listing);
    tab.cursor.row = if on_parent {
        0
    } else {
        cur.and_then(|n| row_of_name(tab, &n)).unwrap_or(old_row)
    };
    tab.clamp_cursor();
    tab.touch();
    let done = matches!(tab.listing.status, ListingStatus::Complete);
    // Подменили до финального порядка (прошло 150 мс): досчитать его по уже
    // показанному листингу, прежний запрос отбросится по `seq`.
    if tab.final_order || (done && tab.load_seq != tab.listing.arrange_seq) {
        arrange(cx, id);
    }
}

/// Финальный порядок перечитываемого листинга по его записям (ADR-0017): `seq` тот
/// же — `Order` примет `apply_incoming`.
fn arrange_incoming(cx: &mut Cx<'_>, id: TabId) {
    let (rt, services) = (cx.rt, cx.services);
    let Some(tab) = cx.state.tab_mut(id) else {
        return;
    };
    let view = arrange_spec(tab);
    let cancel = tab.cancel.clone().unwrap_or_else(|| rt.token());
    let Some(inc) = tab.incoming.as_ref() else {
        return;
    };
    let req = ArrangeReq {
        tab: id,
        generation: inc.listing.generation,
        seq: inc.listing.arrange_seq,
        entries: inc.listing.entries.clone(),
        cancel,
        view,
    };
    cx.keep_rev();
    services.listing.arrange(req, rt);
}

fn add_stats(s: &mut ListingStats, e: &Entry, sign: i8) {
    if e.kind.is_dir_like() {
        s.dirs = s.dirs.wrapping_add_signed(sign.into());
    } else {
        s.files = s.files.wrapping_add_signed(sign.into());
        if sign > 0 {
            s.bytes += e.size;
        } else {
            s.bytes -= e.size;
        }
    }
}

fn append_chunk(l: &mut Listing, entries: Vec<Entry>, visible: &[u32]) {
    for e in &entries {
        add_stats(&mut l.stats, e, 1);
    }
    Arc::make_mut(&mut l.entries).extend(entries);
    let len = l.entries.len() as u32;
    debug_assert!(
        visible.iter().all(|&i| i < len),
        "visible index out of range"
    );
    Arc::make_mut(&mut l.order).extend(visible.iter().copied().filter(|&i| i < len));
}

/// Что сделать после обновления показанного листинга.
enum Follow {
    Nothing,
    /// Загрузка закончилась, а сортировка менялась по ходу.
    Arrange,
    /// Каталог из сессии не открылся: подняться к родителю.
    Up,
}

/// Обновление показанного листинга.
fn apply_shown(tab: &mut Tab, u: ListingUpdate) -> Follow {
    match u {
        ListingUpdate::Chunk { entries, visible } => {
            let base_pos = tab.listing.order.len();
            append_chunk(&mut tab.listing, entries, &visible);
            if let Some(name) = tab.pending_focus.clone() {
                let off = parent_offset(tab);
                let found = tab.listing.order[base_pos..]
                    .iter()
                    .position(|&i| tab.listing.entries[i as usize].name == name);
                if let Some(p) = found {
                    tab.cursor.row = (base_pos + p) as u32 + off;
                    tab.pending_focus = None;
                }
            }
            tab.clamp_cursor();
        }
        ListingUpdate::Order {
            order,
            for_len,
            seq,
        } => {
            if tab.listing.entries.len() as u32 != for_len || seq != tab.listing.arrange_seq {
                return Follow::Nothing;
            }
            let keep = cursor_idx(tab);
            tab.listing.order = order;
            restore_cursor(tab, keep);
            if tab.final_order {
                tab.final_order = false;
                complete(tab);
            }
        }
        ListingUpdate::DoneUnsorted => {
            tab.final_order = true;
            tab.touch();
            return Follow::Arrange;
        }
        ListingUpdate::Patch(p) => {
            if !apply_patch(tab, p) {
                return Follow::Nothing;
            }
        }
        ListingUpdate::Done => {
            complete(tab);
            return if tab.load_seq == tab.listing.arrange_seq {
                Follow::Nothing
            } else {
                Follow::Arrange
            };
        }
        ListingUpdate::Failed(e) => {
            tab.listing.status = ListingStatus::Failed(e);
            tab.touch();
            if tab.restore_up > 0 && tab.loc.parent().is_some() {
                return Follow::Up;
            }
            tab.pending_focus = None;
        }
    }
    tab.touch();
    Follow::Nothing
}

/// Листинг загружен и упорядочен.
fn complete(tab: &mut Tab) {
    tab.listing.status = ListingStatus::Complete;
    tab.pending_focus = None;
    tab.restore_up = 0;
    tab.touch();
}

/// После смены порядка: `pending_focus`, иначе та же запись, иначе кламп.
fn restore_cursor(tab: &mut Tab, keep: Option<u32>) {
    let pending = tab.pending_focus.clone();
    let row = match pending {
        Some(name) => row_of_name(tab, &name).inspect(|_| tab.pending_focus = None),
        None => None,
    }
    .or_else(|| keep.and_then(|idx| row_where(tab, |i, _| i == idx)));
    if let Some(r) = row {
        tab.cursor.row = r;
    }
    tab.clamp_cursor();
}

/// S0 v2 №2. `false` — патч отброшен (длина или `seq` не совпали).
fn apply_patch(tab: &mut Tab, p: Patch) -> bool {
    let l = &mut tab.listing;
    if l.entries.len() as u32 != p.base_len || p.seq != l.arrange_seq {
        return false;
    }
    let mut keep = match tab.current() {
        Some(Row::Entry { idx, .. }) => Some(idx),
        _ => None,
    };
    let l = &mut tab.listing;
    let sel = &mut tab.selection;
    let entries = Arc::make_mut(&mut l.entries);
    for (i, e) in p.update {
        let Some(old) = entries.get_mut(i as usize) else {
            continue;
        };
        add_stats(&mut l.stats, old, -1);
        add_stats(&mut l.stats, &e, 1);
        if sel.contains(i) {
            sel.set(i, false, old);
            sel.set(i, true, &e);
        }
        *old = e;
    }
    for i in p.remove {
        let last = entries.len() as u32 - 1;
        if i > last {
            continue;
        }
        sel.set(i, false, &entries[i as usize]);
        let moved_selected = i != last && sel.contains(last);
        if moved_selected {
            sel.set(last, false, &entries[last as usize]);
        }
        let removed = entries.swap_remove(i as usize);
        add_stats(&mut l.stats, &removed, -1);
        if moved_selected {
            sel.set(i, true, &entries[i as usize]);
        }
        keep = match keep {
            Some(k) if k == i => None,
            Some(k) if k == last => Some(i),
            k => k,
        };
    }
    for e in p.append {
        add_stats(&mut l.stats, &e, 1);
        entries.push(e);
    }
    l.order = p.order;
    // `pending_focus` ещё не найден, значит он может быть только среди добавленных.
    restore_cursor(tab, keep);
    true
}
