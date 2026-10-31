//! Режимы панели «Дерево» и «Инфо» (S2 §2.4, §2.5): данные.
//!
//! Панель в режиме показывает соседнюю: её каталог. Handler `ui` в `on_pump` сверяет
//! состояние с соседней панелью и ставит чтения на полосу Interactive; ответ — `Msg` с
//! ключом/поколением, устаревший не применяется. UI-поток I/O не делает.
//!
//! «Быстрый просмотр» (`qview`) — вьювер в области панели (S7 §4, S7-13: фича `Viewer`,
//! `gpui_shell/view/quick.rs`); временный предпросмотр S2-18 снят. Дерево ленивое:
//! подкаталоги узла читаются при раскрытии; движение курсора с паузой 150 мс ведёт
//! соседнюю панель (`panel.go`), Enter — сразу и с фокусом.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use compact_str::CompactString;
use urfin_core::{CancelToken, Command, Cx, Lane, Location, Msg, PanelMode, Runtime, Side};
use urfin_vfs::volume::{Media, VolCaps};
use urfin_vfs::{Space, VfsRegistry};

use crate::handler::ui_state;
use crate::side_arg;

/// Пауза, после которой курсор дерева ведёт соседнюю панель.
pub const TREE_FOLLOW: Duration = Duration::from_millis(150);

// ---- Инфо ----

/// Том для «Инфо».
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeView {
    pub root: PathBuf,
    pub label: String,
    pub fs_name: String,
    pub media: Media,
    pub caps: VolCaps,
    pub space: Option<Space>,
}

/// «Инфо» на панели: каталог соседней и его том (`None` — читается).
#[derive(Clone, Debug, PartialEq)]
pub struct InfoState {
    pub loc: Location,
    pub volume: Option<Result<VolumeView, CompactString>>,
}

// ---- Дерево ----

#[derive(Clone, Debug, PartialEq)]
pub struct TreeNode {
    pub loc: Location,
    pub name: CompactString,
    pub depth: u16,
    pub expanded: bool,
    /// Подкаталоги прочитаны.
    pub loaded: bool,
    /// Прочитаны и их нет.
    pub empty: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreeState {
    /// Узлы в порядке показа (дети — сразу за родителем); свёрнутые поддеревья
    /// остаются в списке, но не показываются.
    pub nodes: Vec<TreeNode>,
    /// Узел под курсором — по пути: переживает вставку детей.
    pub cursor: Location,
    /// Каталог соседней панели, по которому дерево сверено.
    pub synced: Location,
    pub generation: u64,
}

impl TreeState {
    /// Индексы видимых узлов (дети свёрнутых скрыты).
    pub fn visible(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut hide_below: Option<u16> = None;
        for (i, n) in self.nodes.iter().enumerate() {
            if let Some(d) = hide_below {
                if n.depth > d {
                    continue;
                }
                hide_below = None;
            }
            out.push(i);
            if !n.expanded {
                hide_below = Some(n.depth);
            }
        }
        out
    }

    pub fn index_of(&self, loc: &Location) -> Option<usize> {
        self.nodes.iter().position(|n| &n.loc == loc)
    }

    /// Конец поддерева узла `i` (исключительно).
    fn subtree_end(&self, i: usize) -> usize {
        let d = self.nodes[i].depth;
        self.nodes[i + 1..]
            .iter()
            .position(|n| n.depth <= d)
            .map_or(self.nodes.len(), |k| i + 1 + k)
    }
}

/// Корень пути: `C:\` или `/`.
pub fn root_of(loc: &Location) -> Location {
    let root = loc
        .path
        .ancestors()
        .last()
        .map_or_else(|| loc.path.clone(), std::path::Path::to_path_buf);
    Location {
        path: root,
        ..loc.clone()
    }
}

fn node(loc: Location, depth: u16) -> TreeNode {
    let name = loc.path.file_name().map_or_else(
        || loc.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    TreeNode {
        loc,
        name: name.into(),
        depth,
        expanded: false,
        loaded: false,
        empty: false,
    }
}

/// Дерево с раскрытой цепочкой от корня до `loc`.
pub fn tree_for(loc: &Location, generation: u64) -> TreeState {
    let root = root_of(loc);
    let mut chain: Vec<Location> = loc
        .path
        .ancestors()
        .map(|p| Location {
            path: p.to_path_buf(),
            ..loc.clone()
        })
        .collect();
    chain.reverse();
    if chain.first() != Some(&root) {
        chain.insert(0, root);
    }
    let nodes = chain
        .iter()
        .enumerate()
        .map(|(d, l)| TreeNode {
            expanded: true,
            ..node(l.clone(), u16::try_from(d).unwrap_or(u16::MAX))
        })
        .collect();
    TreeState {
        nodes,
        cursor: loc.clone(),
        synced: loc.clone(),
        generation,
    }
}

/// Подкаталоги узла `parent` прочитаны: вставить, сохранив уже раскрытые поддеревья.
pub fn merge_children(t: &mut TreeState, parent: &Location, names: &[CompactString]) {
    let Some(i) = t.index_of(parent) else {
        return;
    };
    let depth = t.nodes[i].depth + 1;
    let end = t.subtree_end(i);
    // Уже известные дети (цепочка к текущему каталогу) — вместе с поддеревьями.
    let mut blocks: Vec<(CompactString, Vec<TreeNode>)> = Vec::new();
    let mut k = i + 1;
    while k < end {
        let e = t.subtree_end(k);
        blocks.push((t.nodes[k].name.clone(), t.nodes[k..e].to_vec()));
        k = e;
    }
    let mut children = Vec::new();
    for name in names {
        if let Some(pos) = blocks.iter().position(|(n, _)| n == name) {
            children.extend(blocks.swap_remove(pos).1);
        } else {
            let loc = Location {
                path: t.nodes[i].loc.path.join(name.as_str()),
                ..t.nodes[i].loc.clone()
            };
            children.push(node(loc, depth));
        }
    }
    // Каталоги, которых нет в ответе (удалены или скрыты), но на пути курсора, — в конец.
    for (_, b) in blocks {
        children.extend(b);
    }
    t.nodes.splice(i + 1..end, children);
    let n = &mut t.nodes[i];
    n.loaded = true;
    n.empty = names.is_empty();
}

// ---- Синхронизация (on_pump) ----

/// Задачи режимов панелей: отменяются, когда цель меняется.
#[derive(Debug, Default)]
pub struct ViewTasks {
    follow: Option<CancelToken>,
}

/// Сверить «Инфо» и дерево с соседними панелями; запустить чтения.
pub fn sync(cx: &mut Cx<'_>, vfs: &Arc<VfsRegistry>, _tasks: &mut ViewTasks) {
    for side in [Side::Left, Side::Right] {
        let mode = cx.state.panel(side).tab().mode;
        match mode {
            PanelMode::Info => sync_info(cx, side),
            PanelMode::Tree => sync_tree(cx, vfs, side),
            PanelMode::QView | PanelMode::Files => {}
        }
        clear_unused(cx, side, mode);
    }
}

/// Режим выключен — данные не держим.
fn clear_unused(cx: &mut Cx<'_>, side: Side, mode: PanelMode) {
    let i = side.idx();
    let st = ui_state(cx);
    let changed = (mode != PanelMode::Info && st.info[i].take().is_some())
        | (mode != PanelMode::Tree && st.tree[i].take().is_some());
    if changed {
        cx.state.touch();
    }
}

fn sync_info(cx: &mut Cx<'_>, side: Side) {
    let loc = cx.state.panel(side.other()).tab().loc.clone();
    let i = side.idx();
    if ui_state(cx).info[i].as_ref().is_some_and(|s| s.loc == loc) {
        return;
    }
    ui_state(cx).info[i] = Some(InfoState {
        loc: loc.clone(),
        volume: None,
    });
    cx.state.touch();
    if !loc.is_local() {
        ui_state(cx).info[i] = Some(InfoState {
            loc,
            volume: Some(Err("—".into())),
        });
        return;
    }
    cx.rt
        .spawn(Lane::Interactive, "ui.volume", cx.rt.token(), move |t| {
            let volume = urfin_vfs::volume::of(&loc.path)
                .map(|v| VolumeView {
                    space: urfin_vfs::volume::space(&loc.path).ok(),
                    root: v.root,
                    label: v.label,
                    fs_name: v.fs_name,
                    media: v.media,
                    caps: v.caps,
                })
                .map_err(|e| CompactString::from(e.to_string()));
            let _ = t
                .sink
                .send(Box::new(VolumeRead { side, loc, volume }), &t.cancel);
        });
}

struct VolumeRead {
    side: Side,
    loc: Location,
    volume: Result<VolumeView, CompactString>,
}

impl Msg for VolumeRead {
    fn name(&self) -> &'static str {
        "ui.volume"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        match ui_state(cx).info[self.side.idx()].as_mut() {
            Some(s) if s.loc == self.loc => s.volume = Some(self.volume),
            _ => cx.keep_rev(),
        }
    }
}

// ---- Дерево ----

fn sync_tree(cx: &mut Cx<'_>, vfs: &Arc<VfsRegistry>, side: Side) {
    let i = side.idx();
    let loc = cx.state.panel(side.other()).tab().loc.clone();
    let st = ui_state(cx);
    let fresh = match &st.tree[i] {
        Some(t) if t.synced == loc => return,
        // Другой том — дерево заново.
        Some(t) => root_of(&t.synced) != root_of(&loc),
        None => true,
    };
    let generation = st.bump();
    if fresh {
        st.tree[i] = Some(tree_for(&loc, generation));
    } else if let Some(t) = st.tree[i].as_mut() {
        t.synced = loc.clone();
        t.cursor = loc.clone();
        t.generation = generation;
        ensure_path(t, &loc);
    }
    cx.state.touch();
    load_pending(cx, vfs, side);
}

/// Раскрыть узлы на пути к `loc`; недостающие — добавить к родителю (подгрузятся).
fn ensure_path(t: &mut TreeState, loc: &Location) {
    let mut chain: Vec<Location> = loc
        .path
        .ancestors()
        .map(|p| Location {
            path: p.to_path_buf(),
            ..loc.clone()
        })
        .collect();
    chain.reverse();
    for w in chain.windows(2) {
        let (parent, child) = (&w[0], &w[1]);
        let Some(pi) = t.index_of(parent) else {
            continue;
        };
        t.nodes[pi].expanded = true;
        if t.index_of(child).is_none() {
            let at = t.subtree_end(pi);
            t.nodes
                .insert(at, node(child.clone(), t.nodes[pi].depth + 1));
        }
    }
    if let Some(i) = t.index_of(loc) {
        t.nodes[i].expanded = true;
    }
}

/// Прочитать подкаталоги раскрытых, ещё не прочитанных узлов.
fn load_pending(cx: &mut Cx<'_>, vfs: &Arc<VfsRegistry>, side: Side) {
    let Some(t) = ui_state(cx).tree[side.idx()].clone() else {
        return;
    };
    for n in t.nodes.iter().filter(|n| n.expanded && !n.loaded) {
        crate::fs_tasks::spawn_tree_children(
            cx.rt,
            Arc::clone(vfs),
            side,
            n.loc.clone(),
            t.generation,
            cx.rt.token(),
        );
    }
}

/// Подкаталоги узла прочитаны (`fs_tasks`): вставить, если дерево того же поколения.
pub(crate) fn tree_children(
    cx: &mut Cx<'_>,
    side: Side,
    parent: &Location,
    generation: u64,
    names: Result<Vec<CompactString>, CompactString>,
) {
    let Some(t) = ui_state(cx).tree[side.idx()].as_mut() else {
        cx.keep_rev();
        return;
    };
    if t.generation != generation {
        cx.keep_rev();
        return;
    }
    // Ошибка чтения (нет прав) — узел без детей.
    merge_children(t, parent, &names.unwrap_or_default());
}

/// Команды дерева активной панели (`ui.tree.*`, контекст keymap `panel && tree`).
pub fn tree_command(
    cx: &mut Cx<'_>,
    vfs: &Arc<VfsRegistry>,
    tasks: &mut ViewTasks,
    id: &str,
    n: Option<i64>,
) {
    let side = cx.state.active;
    let page = 20usize;
    let Some(t) = ui_state(cx).tree[side.idx()].as_mut() else {
        cx.keep_rev();
        return;
    };
    let vis = t.visible();
    let Some(cur) = t
        .index_of(&t.cursor)
        .and_then(|i| vis.iter().position(|v| *v == i))
    else {
        cx.keep_rev();
        return;
    };
    let last = vis.len().saturating_sub(1);
    let mut load = None;
    let to = match id {
        "ui.tree.up" => cur.saturating_sub(1),
        "ui.tree.down" => (cur + 1).min(last),
        "ui.tree.page_up" => cur.saturating_sub(page),
        "ui.tree.page_down" => (cur + page).min(last),
        "ui.tree.home" => 0,
        "ui.tree.end" => last,
        "ui.tree.select" => usize::try_from(n.unwrap_or(0)).unwrap_or(0).min(last),
        "ui.tree.expand" => {
            let i = vis[cur];
            if t.nodes[i].expanded {
                // Уже раскрыт — к первому ребёнку.
                if vis
                    .get(cur + 1)
                    .is_some_and(|&k| t.nodes[k].depth > t.nodes[i].depth)
                {
                    cur + 1
                } else {
                    cur
                }
            } else {
                t.nodes[i].expanded = true;
                if !t.nodes[i].loaded {
                    load = Some((t.nodes[i].loc.clone(), t.generation));
                }
                cur
            }
        }
        "ui.tree.collapse" => {
            let i = vis[cur];
            if t.nodes[i].expanded {
                t.nodes[i].expanded = false;
                cur
            } else {
                // Свёрнут — к родителю.
                let d = t.nodes[i].depth;
                vis[..cur]
                    .iter()
                    .rposition(|&k| t.nodes[k].depth < d)
                    .unwrap_or(cur)
            }
        }
        // `ui.tree.enter` и прочие — курсор на месте.
        _ => cur,
    };
    let target = t.nodes[vis.get(to).copied().unwrap_or(vis[cur])]
        .loc
        .clone();
    t.cursor = target.clone();
    if let Some((loc, generation)) = load {
        crate::fs_tasks::spawn_tree_children(
            cx.rt,
            Arc::clone(vfs),
            side,
            loc,
            generation,
            cx.rt.token(),
        );
    }
    if let Some(f) = tasks.follow.take() {
        f.cancel();
    }
    let other = side_arg(side.other());
    if id == "ui.tree.enter" {
        cx.dispatch(
            Command::new("panel.go")
                .arg("side", other)
                .arg("path", target),
        );
        cx.dispatch(Command::new("panel.focus").arg("side", other));
        return;
    }
    if to != cur {
        tasks.follow = Some(follow_later(cx.rt, side, target));
    }
}

/// Через [`TREE_FOLLOW`] — `panel.go` соседней панели, если курсор дерева всё там же.
fn follow_later(rt: &Runtime, side: Side, loc: Location) -> CancelToken {
    let cancel = rt.token();
    rt.spawn(
        Lane::Interactive,
        "ui.tree.follow",
        cancel.clone(),
        move |t| {
            std::thread::sleep(TREE_FOLLOW);
            if t.cancel.is_cancelled() {
                return;
            }
            let _ = t.sink.send(Box::new(TreeFollow { side, loc }), &t.cancel);
        },
    );
    cancel
}

struct TreeFollow {
    side: Side,
    loc: Location,
}

impl Msg for TreeFollow {
    fn name(&self) -> &'static str {
        "ui.tree.follow"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let still = ui_state(cx).tree[self.side.idx()]
            .as_ref()
            .is_some_and(|t| t.cursor == self.loc);
        if still {
            cx.dispatch(
                Command::new("panel.go")
                    .arg("side", side_arg(self.side.other()))
                    .arg("path", self.loc),
            );
        } else {
            cx.keep_rev();
        }
    }
}

#[cfg(test)]
#[path = "views_tests.rs"]
mod tests;
