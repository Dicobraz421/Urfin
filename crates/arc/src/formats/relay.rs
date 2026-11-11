//! Конвейер выборки (S8 §7, #719): распаковщик идёт впереди писателей на бюджет байт, а
//! файлы одного прохода разбирают несколько потоков выборки — ops пишет их параллельно.
//!
//! - Распаковщик ([`Relay`]) отдаёт файл за файлом: заявку ([`Grant`]: индекс во входном
//!   списке, размер, канал данных) — в общую очередь, затем данные порциями ([`Chunk`]).
//!   Порция держит свою долю бюджета, пока её не прочли или не бросили: распаковщик не
//!   ждёт создания каждого файла, но и не уходит вперёд больше, чем на бюджет.
//! - Потоки выборки ([`streams`]) берут заявки из очереди по порядку архива: свободный
//!   поток — следующий файл. Читают их по очереди (`arc.test`) — первый поток отдаст всё.
//! - Проход стартует в первом `next` любого потока; ушли все потоки (или читатель одного
//!   файла, [`pair`]) — бюджет закрыт, распаковщик кончает на ближайшей порции.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use urfin_core::{CancelToken, Location};
use urfin_vfs::{ExtractStream, Extracted, VfsError};

use super::{CHUNK, Pump, read_up_to};

/// Бюджет распакованного впрок на всю выборку (делится между проходами).
pub(crate) const BUDGET: usize = 64 << 20;

/// Бюджет чтения одного файла (`open_read`): как прежний канал на 4 порции.
pub(crate) const ONE: usize = 4 * CHUNK;

/// Файлов на писателя: мелкую выборку параллелить незачем.
const FILES_PER_WRITER: usize = 16;

/// Заявок в очереди: пустые файлы бюджета не занимают.
const QUEUE: usize = 1024;

/// Как часто ожидающий следующий файл проверяет отмену.
const WAIT: Duration = Duration::from_millis(20);

/// Писателей на проход: доля `parallel` на `passes` проходов, не больше одного на
/// [`FILES_PER_WRITER`] файлов прохода.
pub(crate) fn writers(parallel: u8, passes: usize, files: usize) -> usize {
    let cap = files.div_ceil(FILES_PER_WRITER).max(1);
    (usize::from(parallel.max(1)) / passes.max(1)).clamp(1, cap)
}

/// Бюджет байт между распаковщиком и читателями.
struct Budget {
    st: Mutex<BudgetSt>,
    cv: Condvar,
    cap: usize,
}

#[derive(Default)]
struct BudgetSt {
    used: usize,
    closed: bool,
}

impl Budget {
    fn new(cap: usize) -> Arc<Budget> {
        Arc::new(Budget {
            st: Mutex::default(),
            cv: Condvar::new(),
            cap: cap.max(CHUNK),
        })
    }

    fn lock(&self) -> MutexGuard<'_, BudgetSt> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Место под `n` байт; `false` — читатели ушли. Порция больше бюджета проходит, когда
    /// он пуст.
    fn take(&self, n: usize) -> bool {
        let mut g = self.lock();
        loop {
            if g.closed {
                return false;
            }
            if g.used == 0 || g.used + n <= self.cap {
                g.used += n;
                return true;
            }
            g = self.cv.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn give(&self, n: usize) {
        let mut g = self.lock();
        g.used = g.used.saturating_sub(n);
        drop(g);
        self.cv.notify_all();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

/// Порция данных файла: доля бюджета возвращается при сбросе.
pub(crate) struct Chunk {
    data: Vec<u8>,
    budget: Arc<Budget>,
}

impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        self.budget.give(self.data.len());
    }
}

/// Файл прохода: индекс во входном списке, размер, данные.
pub(crate) type Grant = (u32, u64, Receiver<io::Result<Chunk>>);

/// Закрывает бюджет при сбросе: последний читатель ушёл.
pub(crate) struct Closer(Arc<Budget>);

impl Drop for Closer {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Чем кончился файл.
pub(crate) enum Fed {
    /// Отдан (или его читатель ушёл раньше — остаток распаковщик пропускает сам).
    Ok,
    /// Читатели прохода ушли — стоп.
    Gone,
    /// Чтение упало: ошибка — у читателя файла, копия — проходу.
    Failed(io::Error),
}

/// Сторона распаковщика.
pub(crate) struct Relay {
    tx: Sender<io::Result<Grant>>,
    budget: Arc<Budget>,
}

impl Relay {
    /// Читатели ушли: дальше распаковывать незачем.
    pub(crate) fn closed(&self) -> bool {
        self.budget.is_closed()
    }

    /// Ошибка прохода — следующему читателю.
    pub(crate) fn fail(&self, e: io::Error) {
        let _ = self.tx.send(Err(e));
    }

    /// Файл `input` (`len` байт) из `data` до конца; `map` — ошибка чтения для читателя
    /// файла.
    pub(crate) fn feed(
        &self,
        input: u32,
        len: u64,
        data: &mut dyn Read,
        map: impl Fn(io::Error) -> io::Error,
    ) -> Fed {
        let (ftx, frx) = crossbeam_channel::unbounded();
        if self.tx.send(Ok((input, len, frx))).is_err() {
            return Fed::Gone;
        }
        loop {
            let mut buf = vec![0u8; CHUNK];
            match read_up_to(data, &mut buf) {
                Ok(0) => return Fed::Ok,
                Ok(n) => {
                    if n < buf.len() {
                        buf.truncate(n);
                        // Хвост файла: бюджет считает байты, а не ёмкость.
                        buf.shrink_to_fit();
                    }
                    if !self.budget.take(n) {
                        return Fed::Gone;
                    }
                    let chunk = Chunk {
                        data: buf,
                        budget: Arc::clone(&self.budget),
                    };
                    if ftx.send(Ok(chunk)).is_err() {
                        return Fed::Ok;
                    }
                }
                Err(e) => {
                    let e = map(e);
                    let copy = io::Error::new(e.kind(), e.to_string());
                    let _ = ftx.send(Err(e));
                    return Fed::Failed(copy);
                }
            }
        }
    }
}

/// Распаковщик и очередь его файлов на бюджет `cap`; [`Closer`] — у читателя.
pub(crate) fn pair(cap: usize) -> (Relay, Receiver<io::Result<Grant>>, Closer) {
    let (tx, rx) = crossbeam_channel::bounded(QUEUE);
    let budget = Budget::new(cap);
    let closer = Closer(Arc::clone(&budget));
    (Relay { tx, budget }, rx, closer)
}

/// Запуск прохода: распаковщик в своём потоке.
pub(crate) type Start = Box<dyn FnOnce(Relay) -> Result<(), VfsError> + Send>;

/// Общее для потоков одного прохода.
struct Shared {
    start: Mutex<Option<(Start, Relay)>>,
    _closer: Closer,
}

/// `n` потоков выборки одного прохода на бюджет `cap`; `empty` (файлы без данных) — у
/// первого. Без I/O: проход стартует в первом `next`.
pub(crate) fn streams(
    outer: &Location,
    n: usize,
    cap: usize,
    empty: VecDeque<u32>,
    start: Start,
) -> Vec<Box<dyn ExtractStream>> {
    let (relay, rx, closer) = pair(cap);
    let shared = Arc::new(Shared {
        start: Mutex::new(Some((start, relay))),
        _closer: closer,
    });
    let mut empty = Some(empty);
    (0..n.max(1))
        .map(|_| {
            Box::new(RelayStream {
                outer: outer.clone(),
                empty: empty.take().unwrap_or_default(),
                shared: Arc::clone(&shared),
                rx: rx.clone(),
                cur: None,
            }) as Box<dyn ExtractStream>
        })
        .collect()
}

/// Поток выборки: файлы без данных, затем заявки прохода по очереди.
struct RelayStream {
    outer: Location,
    empty: VecDeque<u32>,
    shared: Arc<Shared>,
    rx: Receiver<io::Result<Grant>>,
    cur: Option<Pump<Chunk>>,
}

impl ExtractStream for RelayStream {
    fn next(&mut self, cancel: &CancelToken) -> Result<Option<Extracted<'_>>, VfsError> {
        // Прошлый файл закрываем: его непрочитанные порции возвращают бюджет.
        self.cur = None;
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        if let Some(input) = self.empty.pop_front() {
            return Ok(Some((input, Box::new(io::empty()))));
        }
        let start = self
            .shared
            .start
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((start, relay)) = start {
            start(relay)?;
        }
        loop {
            match self.rx.recv_timeout(WAIT) {
                Ok(Ok((input, len, data))) => {
                    let p = self.cur.insert(Pump::new(data, len));
                    return Ok(Some((input, Box::new(p))));
                }
                Ok(Err(e)) => return Err(VfsError::from_io(self.outer.path.clone(), e)),
                Err(RecvTimeoutError::Disconnected) => return Ok(None),
                Err(RecvTimeoutError::Timeout) if cancel.is_cancelled() => {
                    return Err(VfsError::Cancelled);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use urfin_core::{CancelToken, Location};

    use super::*;

    fn body(i: u32) -> Vec<u8> {
        (0..(i as usize % 5) * 100_000)
            .map(|k| (k as u32 ^ i) as u8)
            .collect()
    }

    /// Проход из `files` файлов в своём потоке; `peak` — наибольший занятый бюджет.
    fn start(files: u32, peak: Arc<AtomicUsize>) -> Start {
        Box::new(move |relay: Relay| {
            std::thread::spawn(move || {
                for i in 0..files {
                    let b = body(i);
                    if let Fed::Gone = relay.feed(i, b.len() as u64, &mut b.as_slice(), |e| e) {
                        return;
                    }
                    peak.fetch_max(relay.budget.lock().used, Ordering::Relaxed);
                }
            });
            Ok(())
        })
    }

    #[test]
    fn writers_share_parallel_and_skip_small_selections() {
        assert_eq!(writers(4, 1, 3187), 4);
        assert_eq!(writers(4, 2, 3187), 2);
        assert_eq!(writers(4, 4, 3187), 1);
        assert_eq!(writers(4, 1, 2), 1, "мелкая выборка — один писатель");
        assert_eq!(writers(0, 0, 0), 1);
    }

    /// Один проход — четыре потока выборки пишут параллельно: все четыре держат по файлу
    /// одновременно, каждый файл — ровно в одном потоке, бюджет не превышен.
    #[test]
    fn one_pass_feeds_parallel_writers_within_budget() {
        let peak = Arc::new(AtomicUsize::new(0));
        let cap = 8 * CHUNK;
        let streams = streams(
            &Location::local("x.tar.zst"),
            4,
            cap,
            VecDeque::from([1000]),
            start(200, Arc::clone(&peak)),
        );
        assert_eq!(streams.len(), 4);
        let barrier = Arc::new(Barrier::new(4));
        let seen: Vec<Vec<u32>> = std::thread::scope(|sc| {
            let hs: Vec<_> = streams
                .into_iter()
                .map(|mut s| {
                    let barrier = Arc::clone(&barrier);
                    sc.spawn(move || {
                        let cancel = CancelToken::new();
                        let mut got = Vec::new();
                        let mut first = true;
                        while let Some((i, mut r)) = s.next(&cancel).unwrap() {
                            if i == 1000 {
                                got.push(i);
                                continue;
                            }
                            if first {
                                // Все четыре держат свой первый файл одновременно.
                                first = false;
                                barrier.wait();
                            }
                            let mut data = Vec::new();
                            r.read_to_end(&mut data).unwrap();
                            assert_eq!(data, body(i), "{i}");
                            got.push(i);
                        }
                        got
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut all: Vec<u32> = seen.iter().flatten().copied().collect();
        all.sort_unstable();
        let want: Vec<u32> = (0..200).chain([1000]).collect();
        assert_eq!(all, want, "каждый файл — ровно в одном потоке");
        assert!(seen.iter().all(|s| !s.is_empty()), "писали все четыре");
        assert!(peak.load(Ordering::Relaxed) <= cap, "бюджет");
    }

    /// Читатели по очереди (`arc.test`): первый поток отдаёт все файлы, остальные — ничего.
    /// Брошенный недочитанным файл и ушедшие потоки освобождают бюджет: проход кончается.
    #[test]
    fn sequential_reader_drop_and_close() {
        let peak = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();
        let mut ss = streams(
            &Location::local("x.7z"),
            3,
            2 * CHUNK,
            VecDeque::new(),
            start(50, Arc::clone(&peak)),
        );
        let mut n = 0;
        while let Some((i, mut r)) = ss[0].next(&cancel).unwrap() {
            if i % 2 == 0 {
                let mut d = Vec::new();
                r.read_to_end(&mut d).unwrap();
                assert_eq!(d, body(i));
            }
            n += 1;
        }
        assert_eq!(n, 50);
        assert!(ss[1].next(&cancel).unwrap().is_none());

        // Ушли посреди прохода: распаковщик на бюджете видит закрытие.
        let (relay, rx, closer) = pair(CHUNK);
        let h = std::thread::spawn(move || {
            let big = vec![7u8; 10 * CHUNK];
            matches!(
                relay.feed(0, big.len() as u64, &mut big.as_slice(), |e| e),
                Fed::Gone
            )
        });
        let (_, _, data) = rx.recv().unwrap().unwrap();
        let mut p = Pump::new(data, 0);
        let mut b = [0u8; 10];
        p.read_exact(&mut b).unwrap();
        drop(closer);
        assert!(h.join().unwrap(), "читатель ушёл — проход стоп");
    }
}
