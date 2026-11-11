//! `ArchiveIndex` — оглавление архива в памяти (S8 §3.2): все имена одним буфером
//! ([`NameArena`]), узлы `Vec<Node>` (0 — корень), дети каталога — CSR-срез `children`,
//! отсортированный по байтам имени.
//!
//! Строится потоком ([`IndexBuilder::push`]) по мере чтения оглавления: пути
//! нормализуются внутрь корня ([`crate::path`]), недостающие каталоги создаются
//! (неявные), при дублях имён побеждает последняя запись (как в 7-Zip).
//!
//! Каталоги — как в обычной панели: без размера и «Сжато» (`<DIR>`, макет
//! `screen-archive`); суммы архива — в сводке. Символьные ссылки хранят цель
//! ([`ArchiveIndex::link_target`]); ссылка на каталог архива — `LinkDir`, листинг идёт
//! в цель.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::path::Path;

use urfin_core::arc::ArcSummary;
use urfin_core::{Attrs, Entry, EntryKind, Location};

use crate::format::OpenCtx;

/// Номер узла в индексе.
pub type NodeId = u32;

/// Корень архива.
pub const ROOT: NodeId = 0;

/// Ключ архива в кэше: адрес файла, размер и время изменения. Файл изменился — новый
/// ключ, старый индекс уходит из кэша.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OuterKey {
    pub loc: Location,
    pub size: u64,
    /// Наносекунды Unix.
    pub mtime: i64,
}

/// Где данные записи внутри архива — смысл у формата.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum EntryLoc {
    /// Данных нет: каталог (неявный или из оглавления).
    #[default]
    None,
    /// Номер записи: zip — индекс central directory, rar — порядковый номер, мок.
    Index(u32),
    /// Смещение данных: plain tar.
    Offset(u64),
    /// 7z: папка (solid-блок) и номер файла в ней.
    Folder { folder: u32, index: u32 },
}

/// Узел индекса.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct Node {
    /// Номер имени в [`NameArena`].
    pub name: u32,
    pub parent: NodeId,
    /// Дети: `children[first_child..first_child + n_children]`.
    pub first_child: u32,
    pub n_children: u32,
    pub kind: EntryKind,
    pub attrs: Attrs,
    pub size: u64,
    /// Сжатый размер.
    pub packed: u64,
    /// Наносекунды Unix; `i64::MIN` — неизвестно.
    pub mtime: i64,
    pub loc: EntryLoc,
}

/// Все имена одним буфером: имя `i` — `buf[starts[i]..starts[i + 1]]`.
#[derive(Debug, Default)]
pub struct NameArena {
    buf: String,
    starts: Vec<u32>,
}

impl NameArena {
    fn with_capacity(names: usize, bytes: usize) -> NameArena {
        let mut starts = Vec::with_capacity(names + 1);
        starts.push(0);
        NameArena {
            buf: String::with_capacity(bytes),
            starts,
        }
    }

    fn push(&mut self, name: &str) -> u32 {
        if self.starts.is_empty() {
            self.starts.push(0);
        }
        let id = self.starts.len() as u32 - 1;
        self.buf.push_str(name);
        self.starts.push(self.buf.len() as u32);
        id
    }

    /// Имя номер `id`.
    pub fn get(&self, id: u32) -> &str {
        let i = id as usize;
        &self.buf[self.starts[i] as usize..self.starts[i + 1] as usize]
    }

    fn bytes(&self) -> usize {
        self.buf.capacity() + self.starts.capacity() * 4
    }
}

/// Запись оглавления от формата.
#[derive(Copy, Clone, Debug)]
pub struct RawEntry<'a> {
    /// Имя как в архиве: разделители `/` и `\`, может быть абсолютным или с `..`.
    pub path: &'a str,
    pub kind: EntryKind,
    pub attrs: Attrs,
    pub size: u64,
    pub packed: u64,
    pub mtime: i64,
    pub loc: EntryLoc,
}

impl<'a> RawEntry<'a> {
    /// Файл с размерами; время неизвестно, атрибутов нет.
    pub fn file(path: &'a str, size: u64, packed: u64, loc: EntryLoc) -> RawEntry<'a> {
        RawEntry {
            path,
            kind: EntryKind::File,
            attrs: Attrs::default(),
            size,
            packed,
            mtime: i64::MIN,
            loc,
        }
    }

    /// Каталог из оглавления.
    pub fn dir(path: &'a str) -> RawEntry<'a> {
        RawEntry {
            path,
            kind: EntryKind::Dir,
            attrs: Attrs::default(),
            size: 0,
            packed: 0,
            mtime: i64::MIN,
            loc: EntryLoc::None,
        }
    }
}

/// Хэш ключа уже посчитан (xxh3 имени, смешанный с родителем): хэшер его не трогает.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7C_C1_B7_27_22_0A_95);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// (родитель, xxh3 имени) → смешанный 64-битный ключ.
#[derive(Copy, Clone, PartialEq, Eq)]
struct Key(u64);

impl std::hash::Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0);
    }
}

fn key(parent: NodeId, name: &str) -> Key {
    let h = xxhash_rust::xxh3::xxh3_64(name.as_bytes());
    Key(h ^ u64::from(parent).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

type Map<V> = HashMap<Key, V, BuildHasherDefault<KeyHasher>>;

/// Построение индекса потоком.
#[derive(Default)]
pub struct IndexBuilder {
    names: NameArena,
    nodes: Vec<Node>,
    /// Узел создан без записи оглавления (каталог из пути ребёнка).
    implicit: Vec<bool>,
    map: Map<NodeId>,
    /// Редкие совпадения ключа у разных имён одного каталога.
    clash: Map<Vec<NodeId>>,
    /// Дети корня в порядке появления: листинг корня, пока индекс строится.
    root_kids: Vec<NodeId>,
    files: u64,
    unpacked: u64,
    packed: u64,
    encrypted: bool,
    solid: bool,
    comment: Option<Box<str>>,
    cipher: Option<&'static str>,
    /// Цели символьных ссылок.
    links: HashMap<NodeId, Box<str>>,
    comps: Vec<Range<usize>>,
}

impl std::fmt::Debug for IndexBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBuilder")
            .field("nodes", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

impl IndexBuilder {
    pub fn new() -> IndexBuilder {
        IndexBuilder::with_capacity(0)
    }

    /// С запасом на `entries` записей (оглавление zip и 7z знает число заранее).
    pub fn with_capacity(entries: usize) -> IndexBuilder {
        let mut b = IndexBuilder {
            names: NameArena::with_capacity(entries + 1, entries * 16),
            nodes: Vec::with_capacity(entries + 1),
            implicit: Vec::with_capacity(entries + 1),
            map: Map::with_capacity_and_hasher(entries, BuildHasherDefault::default()),
            ..IndexBuilder::default()
        };
        let name = b.names.push("");
        b.nodes.push(Node {
            name,
            parent: ROOT,
            first_child: 0,
            n_children: 0,
            kind: EntryKind::Dir,
            attrs: Attrs::default(),
            size: 0,
            packed: 0,
            mtime: i64::MIN,
            loc: EntryLoc::None,
        });
        b.implicit.push(true);
        b
    }

    /// Узлов вместе с корнем.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn set_encrypted(&mut self, v: bool) {
        self.encrypted |= v;
    }

    pub fn set_solid(&mut self, v: bool) {
        self.solid |= v;
    }

    pub fn set_comment(&mut self, c: impl Into<Box<str>>) {
        self.comment = Some(c.into());
    }

    /// Шифр записей (`AES-256`, `ZipCrypto`) — подпись у поля пароля; первый найденный.
    pub fn set_cipher(&mut self, c: &'static str) {
        self.cipher.get_or_insert(c);
    }

    /// Цель символьной ссылки `id` (узел `LinkFile` из [`IndexBuilder::push`]).
    pub fn set_link(&mut self, id: NodeId, target: &str) {
        let link = self.nodes.get(id as usize).map(|n| n.kind) == Some(EntryKind::LinkFile);
        if link && !target.is_empty() {
            self.links.insert(id, target.into());
        }
    }

    fn find(&self, parent: NodeId, name: &str) -> Option<NodeId> {
        let k = key(parent, name);
        let same = |&id: &NodeId| {
            let n = &self.nodes[id as usize];
            n.parent == parent && self.names.get(n.name) == name
        };
        match self.map.get(&k) {
            Some(id) if same(id) => Some(*id),
            Some(_) => self.clash.get(&k)?.iter().copied().find(same),
            None => None,
        }
    }

    fn create(&mut self, parent: NodeId, name: &str, kind: EntryKind, implicit: bool) -> NodeId {
        let id = self.nodes.len() as NodeId;
        let name_id = self.names.push(name);
        self.nodes.push(Node {
            name: name_id,
            parent,
            first_child: 0,
            n_children: 0,
            kind,
            attrs: Attrs::default(),
            size: 0,
            packed: 0,
            mtime: i64::MIN,
            loc: EntryLoc::None,
        });
        self.implicit.push(implicit);
        let k = key(parent, name);
        if let Some(prev) = self.map.insert(k, id) {
            self.map.insert(k, prev);
            self.clash.entry(k).or_default().push(id);
        }
        if parent == ROOT {
            self.root_kids.push(id);
        }
        id
    }

    /// Узел перестаёт быть файлом: его размеры уходят из сумм.
    fn unfile(&mut self, id: NodeId) {
        let n = &mut self.nodes[id as usize];
        if !n.kind.is_dir_like() {
            self.files -= 1;
            self.unpacked -= n.size;
            self.packed -= n.packed;
            n.size = 0;
            n.packed = 0;
            n.loc = EntryLoc::None;
            n.kind = EntryKind::Dir;
        }
    }

    /// Запись оглавления. Возвращает её узел; `None` — имя пусто после нормализации
    /// (корень). Каталог поверх файла с тем же именем делает его каталогом; файл поверх
    /// каталога с детьми не затирает каталог.
    pub fn push(&mut self, e: RawEntry<'_>) -> Option<NodeId> {
        let mut comps = std::mem::take(&mut self.comps);
        crate::path::split(e.path, &mut comps);
        let Some((last, dirs)) = comps.split_last() else {
            self.comps = comps;
            return None;
        };
        let mut parent = ROOT;
        for r in dirs {
            let name = &e.path[r.clone()];
            parent = match self.find(parent, name) {
                Some(id) => {
                    self.unfile(id);
                    id
                }
                None => self.create(parent, name, EntryKind::Dir, true),
            };
        }
        let name = &e.path[last.clone()];
        let is_dir = e.kind.is_dir_like();
        let id = match self.find(parent, name) {
            Some(id) => {
                let was_dir = self.nodes[id as usize].kind.is_dir_like();
                if was_dir && !is_dir {
                    // Файл поверх каталога: каталог остаётся (у него могут быть дети).
                    self.comps = comps;
                    return Some(id);
                }
                self.unfile(id);
                id
            }
            None => self.create(parent, name, e.kind, false),
        };
        self.comps = comps;
        self.implicit[id as usize] = false;
        self.links.remove(&id);
        let n = &mut self.nodes[id as usize];
        n.kind = e.kind;
        n.attrs = e.attrs;
        n.mtime = e.mtime;
        if is_dir {
            n.loc = EntryLoc::None;
        } else {
            n.size = e.size;
            n.packed = e.packed;
            n.loc = e.loc;
            self.files += 1;
            self.unpacked += e.size;
            self.packed += e.packed;
        }
        if e.attrs.has(Attrs::ENCRYPTED) {
            self.encrypted = true;
        }
        Some(id)
    }

    /// Сводка того, что прочитано: `size` — размер файла архива.
    pub fn summary(&self, format: &str, size: u64, complete: bool) -> ArcSummary {
        ArcSummary {
            format: format.into(),
            size,
            unpacked: self.unpacked,
            packed: self.packed,
            files: self.files,
            encrypted: self.encrypted,
            solid: self.solid,
            complete,
        }
    }

    /// Дети корня с номера `from` (в порядке появления) — в `out`; возвращает новый
    /// номер. Для листинга корня, пока индекс строится (tar.*, rar).
    pub fn root_since(&self, from: usize, out: &mut Vec<Entry>) -> usize {
        for &id in &self.root_kids[from.min(self.root_kids.len())..] {
            out.push(entry_of(
                &self.nodes[id as usize],
                self.names.get(self.nodes[id as usize].name),
            ));
        }
        self.root_kids.len()
    }

    /// Готовый индекс: дети в CSR по имени; у неявных каталогов — время самого свежего
    /// ребёнка. Размера у каталогов нет (как у папок в панели).
    pub fn finish(mut self, format: &'static str, outer_key: OuterKey) -> ArchiveIndex {
        let n = self.nodes.len();
        // Родитель создаётся раньше ребёнка: обратный проход поднимает время.
        for i in (1..n).rev() {
            let c = self.nodes[i];
            let p = &mut self.nodes[c.parent as usize];
            if self.implicit[c.parent as usize] && c.mtime > p.mtime {
                p.mtime = c.mtime;
            }
        }
        let mut counts = vec![0u32; n];
        for node in &self.nodes[1..] {
            counts[node.parent as usize] += 1;
        }
        let mut at = 0u32;
        for (node, &c) in self.nodes.iter_mut().zip(&counts) {
            node.first_child = at;
            node.n_children = c;
            at += c;
        }
        let mut fill: Vec<u32> = self.nodes.iter().map(|x| x.first_child).collect();
        let mut children = vec![0u32; n - 1];
        for i in 1..n {
            let p = self.nodes[i].parent as usize;
            children[fill[p] as usize] = i as u32;
            fill[p] += 1;
        }
        drop(fill);
        let names = &self.names;
        let nodes = &self.nodes;
        for node in nodes.iter().filter(|x| x.n_children > 1) {
            let s = node.first_child as usize;
            children[s..s + node.n_children as usize].sort_unstable_by(|&a, &b| {
                names
                    .get(nodes[a as usize].name)
                    .cmp(names.get(nodes[b as usize].name))
            });
        }
        let mut idx = ArchiveIndex {
            format,
            outer_key,
            names: self.names,
            nodes: self.nodes,
            children,
            encrypted: self.encrypted,
            solid: self.solid,
            comment: self.comment,
            cipher: self.cipher,
            files: self.files,
            unpacked: self.unpacked,
            packed: self.packed,
            links: self.links,
            link_dirs: HashMap::new(),
            ctx: OpenCtx::default(),
        };
        // Ссылка на каталог архива — `LinkDir` (Windows создаёт её `symlink_dir`).
        let dirs: Vec<(NodeId, NodeId)> = idx
            .links
            .iter()
            .filter_map(|(&id, t)| {
                let to = idx.resolve(idx.node(id).parent, t)?;
                (idx.node(to).kind == EntryKind::Dir).then_some((id, to))
            })
            .collect();
        for (id, to) in dirs {
            idx.nodes[id as usize].kind = EntryKind::LinkDir;
            idx.link_dirs.insert(id, to);
        }
        idx
    }
}

/// Оглавление архива.
pub struct ArchiveIndex {
    /// Метка формата (`zip`, `tar.gz`).
    pub format: &'static str,
    pub outer_key: OuterKey,
    names: NameArena,
    nodes: Vec<Node>,
    children: Vec<NodeId>,
    pub encrypted: bool,
    pub solid: bool,
    pub comment: Option<Box<str>>,
    /// Шифр записей для подписи у пароля (`AES-256`, `ZipCrypto`).
    pub cipher: Option<&'static str>,
    files: u64,
    unpacked: u64,
    packed: u64,
    links: HashMap<NodeId, Box<str>>,
    /// Ссылка на каталог → каталог.
    link_dirs: HashMap<NodeId, NodeId>,
    ctx: OpenCtx,
}

impl std::fmt::Debug for ArchiveIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveIndex")
            .field("format", &self.format)
            .field("outer", &self.outer_key)
            .field("nodes", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

impl ArchiveIndex {
    /// Узлов вместе с корнем.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Пусто: только корень.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn name(&self, id: NodeId) -> &str {
        self.names.get(self.nodes[id as usize].name)
    }

    /// Дети каталога по имени (байты).
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        let n = &self.nodes[id as usize];
        let s = n.first_child as usize;
        &self.children[s..s + n.n_children as usize]
    }

    /// Ребёнок `dir` по имени: точно, иначе без учёта регистра (путь, набранный руками).
    pub fn child(&self, dir: NodeId, name: &str) -> Option<NodeId> {
        let kids = self.children(dir);
        if let Ok(i) = kids.binary_search_by(|&k| self.name(k).cmp(name)) {
            return Some(kids[i]);
        }
        let low = name.to_lowercase();
        kids.iter()
            .copied()
            .find(|&k| self.name(k).to_lowercase() == low)
    }

    /// Узел по пути внутри архива (`Location.path`: `/`, `/dir/sub`, разделители любые).
    /// Ссылка на каталог в середине пути ведёт в цель; последний узел — как есть.
    pub fn lookup(&self, path: &Path) -> Option<NodeId> {
        let s = path.to_string_lossy();
        let mut id = ROOT;
        for c in s.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".") {
            id = self.follow(id);
            if !self.node(id).kind.is_dir_like() {
                return None;
            }
            id = self.child(id, c)?;
        }
        Some(id)
    }

    /// Каталог, на который ведёт ссылка `id`; не ссылка на каталог — сам `id`.
    pub fn follow(&self, id: NodeId) -> NodeId {
        self.link_dirs.get(&id).copied().unwrap_or(id)
    }

    /// Цель символьной ссылки как в архиве (`../lib`, `src/main.rs`).
    pub fn link_target(&self, id: NodeId) -> Option<&str> {
        self.links.get(&id).map(|t| &**t)
    }

    /// Цель ссылки `target` от каталога `from` лексически, не выше корня; абсолютная и
    /// с диском — `None` (вне архива).
    fn resolve(&self, from: NodeId, target: &str) -> Option<NodeId> {
        if target.starts_with(['/', '\\']) || target.contains(':') {
            return None;
        }
        let mut id = from;
        for c in target.split(['/', '\\']) {
            match c {
                "" | "." => {}
                ".." if id == ROOT => return None,
                ".." => id = self.node(id).parent,
                name => {
                    let d = self.follow(id);
                    if !self.node(d).kind.is_dir_like() {
                        return None;
                    }
                    id = self.child(d, name)?;
                }
            }
        }
        Some(id)
    }

    /// Файлы поддерева `id` (и сам файл): `f` на каждый; итог — (байт, файлов). Ссылки на
    /// каталог не раскрываются.
    pub fn subtree(&self, id: NodeId, mut f: impl FnMut(NodeId)) -> (u64, u64) {
        let (mut size, mut files) = (0, 0);
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let n = self.node(id);
            if n.kind == EntryKind::Dir {
                stack.extend_from_slice(self.children(id));
            } else if !n.kind.is_dir_like() {
                size += n.size;
                files += 1;
                f(id);
            }
        }
        (size, files)
    }

    /// Путь узла от корня: `/dir/file`.
    pub fn path_of(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != ROOT {
            parts.push(self.name(cur));
            cur = self.node(cur).parent;
        }
        let mut s = String::new();
        for p in parts.iter().rev() {
            s.push('/');
            s.push_str(p);
        }
        if s.is_empty() {
            s.push('/');
        }
        s
    }

    /// Строка панели для узла.
    pub fn entry(&self, id: NodeId) -> Entry {
        entry_of(self.node(id), self.name(id))
    }

    /// Пароль архива и прочее для чтения записей.
    pub fn ctx(&self) -> &OpenCtx {
        &self.ctx
    }

    /// Сводка для `ArcModel`.
    pub fn summary(&self) -> ArcSummary {
        ArcSummary {
            format: self.format.into(),
            size: self.outer_key.size,
            unpacked: self.unpacked,
            packed: self.packed,
            files: self.files,
            encrypted: self.encrypted,
            solid: self.solid,
            complete: true,
        }
    }

    /// Оценка памяти индекса, байты: лимит кэша.
    pub fn approx_bytes(&self) -> u64 {
        (self.names.bytes()
            + self.nodes.capacity() * size_of::<Node>()
            + self.children.capacity() * 4
            + self.links.values().map(|t| t.len() + 32).sum::<usize>()
            + self.link_dirs.len() * 16
            + size_of::<ArchiveIndex>()) as u64
    }
}

fn entry_of(n: &Node, name: &str) -> Entry {
    Entry {
        name: name.into(),
        size: n.size,
        mtime: n.mtime,
        ctime: i64::MIN,
        atime: i64::MIN,
        attrs: n.attrs,
        kind: n.kind,
    }
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
