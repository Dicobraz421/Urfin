//! Поиск: данные экрана и панели результатов (S8 §2.4, решение 2026-11-06 «s8-answers»).
//!
//! Чистые данные, без grep и I/O. Обход `urfin-search` дописывает попадания из своих
//! потоков, UI и `SearchVfs` читают окно строк. `urfin-ui` от `urfin-search` не зависит.
//! Экран показывает строку на совпадение, панель `search:` — строку на файл: это два ряда
//! одного [`SearchStore`].

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockWriteGuard};

use compact_str::CompactString;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::{Entry, Location};

/// Строк в одном куске ряда, не больше.
const CHUNK: usize = 2048;
/// Запросов в истории (S8-06).
const HISTORY: usize = 32;

/// Номер поиска: адрес панели `search:/<id>`, ключ стора в [`SearchModel`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SearchId(pub u32);

/// Попадание: строка экрана.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Запись файла; `name` — путь от корня поиска.
    pub e: Entry,
    /// Индекс в [`SearchStore::roots`].
    pub root: u16,
    /// Номер строки с единицы; 0 — поиск без текста.
    pub line: u32,
    /// Совпадение в `preview`: байты, от и до.
    pub hl: (u16, u16),
    /// Фрагмент строки, не больше 120 символов.
    pub preview: Option<CompactString>,
    /// Состояние в `cmp:`; у поиска 0.
    pub status: u8,
}

impl Hit {
    fn same_file(&self, other: &Hit) -> bool {
        self.root == other.root && self.e.name == other.e.name
    }
}

/// Счётчики хода поиска, читаются без блокировок.
#[derive(Debug, Default)]
pub struct SearchInfo {
    /// Каталогов просмотрено.
    pub dirs: AtomicU64,
    /// Файлов просмотрено.
    pub files: AtomicU64,
    /// Попаданий: растёт вместе со строками стора.
    pub found: AtomicU64,
    /// Каталогов без доступа.
    pub denied: AtomicU64,
    /// Поиск закончен, новых строк не будет.
    pub done: AtomicBool,
    /// Время поиска, мс; итог — в [`SearchStore::finish`].
    pub elapsed_ms: AtomicU64,
}

/// Куски одного ряда: каждый неизменяем, новые дописываются в конец.
#[derive(Debug)]
struct Chunks<T> {
    parts: Vec<Arc<[T]>>,
    /// Номер первой строки куска, по возрастанию.
    starts: Vec<u32>,
    len: u32,
}

impl<T> Default for Chunks<T> {
    fn default() -> Self {
        Chunks {
            parts: Vec::new(),
            starts: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Chunks<T> {
    fn extend(&mut self, parts: Vec<Arc<[T]>>) {
        for part in parts {
            self.starts.push(self.len);
            self.len = self.len.saturating_add(index(part.len()));
            self.parts.push(part);
        }
    }

    /// Копирует только указатели на куски: блокировка держится коротко.
    fn window(&self, range: Range<usize>) -> Window<T> {
        let end = range.end.min(self.len as usize);
        let start = range.start.min(end);
        if start == end {
            return Window::default();
        }
        let first = self.starts.partition_point(|&s| s as usize <= start) - 1;
        let last = self.starts.partition_point(|&s| (s as usize) < end);
        Window {
            parts: self.parts[first..last].to_vec(),
            skip: start - self.starts[first] as usize,
            len: end - start,
        }
    }
}

/// Режет на куски. Строки режутся до блокировки: под ней только запись указателей.
fn cut<T>(mut items: Vec<T>) -> Vec<Arc<[T]>> {
    let mut parts = Vec::with_capacity(items.len().div_ceil(CHUNK));
    while items.len() > CHUNK {
        let rest = items.split_off(CHUNK);
        parts.push(Arc::from(items));
        items = rest;
    }
    if !items.is_empty() {
        parts.push(Arc::from(items));
    }
    parts
}

fn index(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Окно ряда: держит свои куски и не держит блокировку стора, писатель его не ждёт.
#[derive(Debug)]
pub struct Window<T> {
    parts: Vec<Arc<[T]>>,
    /// Сколько пропустить в первом куске.
    skip: usize,
    len: usize,
}

impl<T> Default for Window<T> {
    fn default() -> Self {
        Window {
            parts: Vec::new(),
            skip: 0,
            len: 0,
        }
    }
}

impl<T> Window<T> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let parts = self.parts.iter().enumerate();
        parts
            .flat_map(|(i, p)| p[if i == 0 { self.skip } else { 0 }..].iter())
            .take(self.len)
    }
}

/// Результаты одного поиска: живут в `Arc`, только дописываются.
///
/// Порядок блокировок писателя: `rows`, затем `files`. Читатель берёт по одной, поэтому
/// индекс из `files` всегда указывает на уже видимую строку.
#[derive(Debug)]
pub struct SearchStore {
    pub id: SearchId,
    /// Корни поиска, на них ссылается [`Hit::root`].
    pub roots: Arc<[Location]>,
    /// Экран: строка на совпадение.
    rows: RwLock<Chunks<Hit>>,
    /// Панель: индекс первой строки файла в `rows`.
    files: RwLock<Chunks<u32>>,
    pub info: SearchInfo,
    /// Общий с [`SearchModel`].
    rev: Arc<AtomicU64>,
}

impl SearchStore {
    fn new(id: SearchId, roots: Vec<Location>, rev: Arc<AtomicU64>) -> SearchStore {
        SearchStore {
            id,
            roots: roots.into(),
            rows: RwLock::default(),
            files: RwLock::default(),
            info: SearchInfo::default(),
            rev,
        }
    }

    /// Дописывает попадания. Можно звать из нескольких потоков. Все попадания одного файла
    /// идут одним вызовом и подряд: строка панели заводится по смене файла.
    pub fn append(&self, hits: Vec<Hit>) {
        if hits.is_empty() {
            return;
        }
        let n = hits.len() as u64;
        let firsts: Vec<u32> = (0..hits.len())
            .filter(|&i| i == 0 || !hits[i - 1].same_file(&hits[i]))
            .map(index)
            .collect();
        let parts = cut(hits);
        {
            let mut rows = write(&self.rows);
            let base = rows.len;
            let firsts = firsts.into_iter().map(|i| base.saturating_add(i)).collect();
            rows.extend(parts);
            write(&self.files).extend(cut(firsts));
        }
        self.info.found.fetch_add(n, Ordering::AcqRel);
        self.rev.fetch_add(1, Ordering::AcqRel);
    }

    /// Строк на экране.
    pub fn rows_len(&self) -> usize {
        read(&self.rows, |r| r.len as usize)
    }

    /// Файлов с попаданиями: строк панели.
    pub fn files_len(&self) -> usize {
        read(&self.files, |f| f.len as usize)
    }

    /// Окно строк экрана; конец за рядом обрезается.
    pub fn rows(&self, range: Range<usize>) -> Window<Hit> {
        read(&self.rows, |r| r.window(range))
    }

    /// Окно панели: индексы первых строк файлов.
    pub fn files(&self, range: Range<usize>) -> Window<u32> {
        read(&self.files, |f| f.window(range))
    }

    /// Поиск закончен: строк больше не будет.
    pub fn finish(&self, elapsed_ms: u64) {
        self.info.elapsed_ms.store(elapsed_ms, Ordering::Release);
        self.info.done.store(true, Ordering::Release);
        self.rev.fetch_add(1, Ordering::AcqRel);
    }

    pub fn is_done(&self) -> bool {
        self.info.done.load(Ordering::Acquire)
    }
}

fn read<T, R>(lock: &RwLock<Chunks<T>>, f: impl FnOnce(&Chunks<T>) -> R) -> R {
    f(&lock.read().unwrap_or_else(PoisonError::into_inner))
}

fn write<T>(lock: &RwLock<Chunks<T>>) -> RwLockWriteGuard<'_, Chunks<T>> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// Запрос из истории: поля формы экрана (S8-06, S8-07).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PastQuery {
    pub name: CompactString,
    pub text: CompactString,
    /// Поле «В», как введено.
    pub place: CompactString,
    pub case: bool,
    pub regex: bool,
    pub recursive: bool,
}

/// Модель поиска в `AppState.ext`: мутации — в Handler и Msg, строки стора дописывает обход.
#[derive(Debug, Default)]
pub struct SearchModel {
    stores: FxHashMap<SearchId, Arc<SearchStore>>,
    next: u32,
    /// Стор на экране.
    current: Option<SearchId>,
    /// Курсор списка экрана.
    cursor: u32,
    /// Свежие сверху.
    history: VecDeque<PastQuery>,
    /// Ключ перерисовки UI, общий со сторами.
    rev: Arc<AtomicU64>,
}

impl SearchModel {
    /// Новый стор.
    pub fn open(&mut self, roots: Vec<Location>) -> Arc<SearchStore> {
        self.next += 1;
        let id = SearchId(self.next);
        let store = Arc::new(SearchStore::new(id, roots, Arc::clone(&self.rev)));
        self.stores.insert(id, Arc::clone(&store));
        self.bump();
        store
    }

    pub fn get(&self, id: SearchId) -> Option<&Arc<SearchStore>> {
        self.stores.get(&id)
    }

    /// Убирает стор из модели; job и вкладки дочитают его по своему `Arc`.
    pub fn remove(&mut self, id: SearchId) -> Option<Arc<SearchStore>> {
        if self.current == Some(id) {
            self.show(None);
        }
        let store = self.stores.remove(&id);
        self.bump();
        store
    }

    pub fn current(&self) -> Option<&Arc<SearchStore>> {
        self.get(self.current?)
    }

    /// Стор на экране; курсор — на первую строку.
    pub fn show(&mut self, id: Option<SearchId>) {
        self.current = id;
        self.cursor = 0;
        self.bump();
    }

    pub fn cursor(&self) -> u32 {
        self.cursor
    }

    pub fn set_cursor(&mut self, row: u32) {
        self.cursor = row;
        self.bump();
    }

    pub fn history(&self) -> &VecDeque<PastQuery> {
        &self.history
    }

    /// Запрос — наверх истории: повтор поднимается, старые вытесняются.
    pub fn remember(&mut self, q: PastQuery) {
        self.history.retain(|p| *p != q);
        self.history.push_front(q);
        self.history.truncate(HISTORY);
        self.bump();
    }

    /// Растёт на каждое изменение модели и сторов.
    pub fn rev(&self) -> u64 {
        self.rev.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.rev.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::{Attrs, EntryKind};

    fn hit(name: &str, line: u32) -> Hit {
        Hit {
            e: Entry {
                name: name.into(),
                size: 1,
                mtime: 0,
                ctime: 0,
                atime: 0,
                attrs: Attrs::default(),
                kind: EntryKind::File,
            },
            root: 0,
            line,
            hl: (0, 1),
            preview: None,
            status: 0,
        }
    }

    fn store() -> Arc<SearchStore> {
        SearchModel::default().open(vec![Location::local("C:\\")])
    }

    /// Файл из трёх строк: одним вызовом.
    fn file(name: &str) -> Vec<Hit> {
        (1..=3).map(|line| hit(name, line)).collect()
    }

    #[test]
    fn hit_keeps_the_budget() {
        assert!(size_of::<Hit>() <= 104, "{}", size_of::<Hit>());
    }

    #[test]
    fn append_from_threads_keeps_files_whole() {
        const THREADS: usize = 8;
        const FILES: usize = 300;
        let s = store();
        std::thread::scope(|t| {
            for k in 0..THREADS {
                let s = &s;
                t.spawn(move || {
                    for f in 0..FILES {
                        s.append(file(&format!("t{k}\\f{f}")));
                    }
                });
            }
            // Читатель видит только целый ряд: индекс панели — всегда готовая первая строка.
            let s = &s;
            t.spawn(move || {
                while s.files_len() < THREADS * FILES {
                    let n = s.files_len();
                    for &i in s.files(n.saturating_sub(16)..n).iter() {
                        let row = s.rows(i as usize..i as usize + 1);
                        assert_eq!(row.iter().next().map(|h| h.line), Some(1));
                    }
                }
            });
        });
        assert_eq!(s.rows_len(), THREADS * FILES * 3);
        assert_eq!(s.files_len(), THREADS * FILES);
        assert_eq!(
            s.info.found.load(Ordering::Acquire),
            (THREADS * FILES * 3) as u64
        );
        let mut seen = rustc_hash::FxHashSet::default();
        for &i in s.files(0..usize::MAX).iter() {
            let rows = s.rows(i as usize..i as usize + 3);
            let lines: Vec<_> = rows.iter().map(|h| (h.e.name.clone(), h.line)).collect();
            let name = &lines[0].0;
            assert_eq!(
                lines,
                [1, 2, 3].map(|l| (name.clone(), l)),
                "строки файла идут подряд"
            );
            assert!(seen.insert(name.clone()), "файл не повторяется");
        }
    }

    #[test]
    fn open_window_does_not_stop_the_writer() {
        let s = store();
        s.append(file("a"));
        let rows = s.rows(0..3);
        let files = s.files(0..1);
        let (tx, rx) = mpsc::channel();
        let writer = Arc::clone(&s);
        std::thread::spawn(move || {
            for i in 0..1000 {
                writer.append(file(&format!("{i}")));
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("писатель не дождался окна");
        assert_eq!(s.rows_len(), 3003);
        assert_eq!(rows.len(), 3, "окно — снимок");
        assert_eq!(rows.iter().count(), 3);
        assert_eq!(files.iter().copied().collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn window_crosses_chunks_and_clips() {
        let s = store();
        s.append((0..5000).map(|i| hit(&format!("{i}"), 0)).collect());
        s.append(vec![hit("край", 0)]);
        assert_eq!(s.rows_len(), 5001);
        assert_eq!(s.files_len(), 5001);

        let names = |r: Range<usize>| -> Vec<String> {
            s.rows(r).iter().map(|h| h.e.name.to_string()).collect()
        };
        assert_eq!(names(2046..2050), ["2046", "2047", "2048", "2049"]);
        assert_eq!(names(4999..7000), ["4999", "край"]);
        assert_eq!(s.rows(0..5001).iter().count(), 5001);
        assert!(s.rows(5001..5002).is_empty());
        assert!(s.rows(10..10).is_empty());
        assert_eq!(s.files(5000..5001).iter().copied().next(), Some(5000));
    }

    #[test]
    fn panel_row_is_the_first_line_of_a_file() {
        let s = store();
        let mut other_root = hit("a", 7);
        other_root.root = 1;
        s.append(vec![hit("a", 1), hit("a", 5), other_root, hit("b", 2)]);
        assert_eq!(s.files(0..9).iter().copied().collect::<Vec<_>>(), [0, 2, 3]);
    }

    #[test]
    fn finish_is_seen_and_empty_append_changes_nothing() {
        let mut m = SearchModel::default();
        let s = m.open(Vec::new());
        let rev = m.rev();
        s.append(Vec::new());
        assert_eq!(m.rev(), rev);
        assert!(!s.is_done());
        s.finish(12);
        assert!(s.is_done());
        assert_eq!(s.info.elapsed_ms.load(Ordering::Acquire), 12);
        assert!(m.rev() > rev);
    }

    #[test]
    fn model_tracks_stores_screen_and_rev() {
        let mut m = SearchModel::default();
        let a = m.open(Vec::new());
        let b = m.open(Vec::new());
        assert_ne!(a.id, b.id);
        m.show(Some(b.id));
        m.set_cursor(5);
        assert_eq!(m.current().map(|s| s.id), Some(b.id));

        let rev = m.rev();
        b.append(vec![hit("a", 1)]);
        assert!(m.rev() > rev, "строка из потока будит UI");

        m.show(Some(a.id));
        assert_eq!(m.cursor(), 0);
        m.remove(a.id);
        assert!(m.current().is_none());
        assert!(m.get(a.id).is_none());
        assert!(m.get(b.id).is_some());
    }

    #[test]
    fn history_lifts_repeats_and_keeps_32() {
        let mut m = SearchModel::default();
        let q = |n: usize| PastQuery {
            name: format!("{n}").into(),
            ..PastQuery::default()
        };
        for n in 0..40 {
            m.remember(q(n));
        }
        assert_eq!(m.history().len(), 32);
        assert_eq!(m.history()[0], q(39));
        m.remember(q(20));
        assert_eq!(m.history().len(), 32);
        assert_eq!(m.history()[0], q(20));
        assert_eq!(m.history().iter().filter(|p| **p == q(20)).count(), 1);
    }
}
