//! Бюджеты core (S9 §2): C.1 `dispatch(panel.cursor.down)` < 1 мкс (настоящий
//! handler core), C.2 `pump` 10k пустых `Msg` < 2 мс (S1-07), C.3 применение
//! 100k записей чанками по 2048 (49 чанков) со статистикой < 5 мс, C.4 инверсия
//! выделения на 100k < 1 мс (S1-14).

use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use urfin_core::{
    ArrangeReq, Attrs, Boot, CmdError, Command, CommandMeta, Core, Cx, Entry, EntryKind, Gen,
    Handler, Listing, ListingService, ListingStatus, ListingUpdate, LoadReq, Location, Msg,
    Runtime, RuntimeConfig, Selection, Services, Waker, listing_msg, msg,
};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

const N_MSGS: u32 = 10_000;
/// Записей в каталоге C.3/C.4 и размер чанка `Batcher` S4.
const N_ENTRIES: u32 = 100_000;
const CHUNK: usize = 2048;

static BENCH: [CommandMeta; 2] = [
    CommandMeta {
        id: "bench.fill",
        title: "Fill",
        category: "bench",
        args: &[],
        repeatable: false,
    },
    CommandMeta {
        id: "bench.feed",
        title: "Feed",
        category: "bench",
        args: &[],
        repeatable: false,
    },
];

/// `bench.fill` кладёт `N_MSGS` пустых `Msg`; `bench.feed` — заготовленные
/// сообщения листинга из `feed`.
struct Bench {
    feed: Arc<Mutex<Vec<Box<dyn Msg>>>>,
}

impl Handler for Bench {
    fn namespace(&self) -> &'static str {
        "bench"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &BENCH
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let sink = cx.rt.sink();
        let full = |_| CmdError::Rejected("full".into());
        if cmd.id.as_str() == "bench.feed" {
            for m in self.feed.lock().expect("feed").drain(..) {
                sink.try_send(m).map_err(full)?;
            }
        } else {
            for _ in 0..N_MSGS {
                sink.try_send(msg("empty", |_| {})).map_err(full)?;
            }
        }
        cx.keep_rev();
        Ok(())
    }
}

/// Листинг, который ничего не присылает: таб остаётся в `Loading`, записи
/// подаёт бенч.
struct Silent;

impl ListingService for Silent {
    fn load(&self, _req: LoadReq, _rt: &Runtime) {}
    fn arrange(&self, _req: ArrangeReq, _rt: &Runtime) {}
}

fn core(feed: &Arc<Mutex<Vec<Box<dyn Msg>>>>) -> Core {
    let mut boot = Boot::new(Location::local("/"), Location::local("/"));
    boot.config.runtime = RuntimeConfig {
        msg_capacity: N_MSGS * 2,
        ..RuntimeConfig::default()
    };
    let services = Services {
        listing: Arc::new(Silent),
        open_resolvers: Vec::new(),
    };
    let mut c = Core::new(boot, services, Arc::new(NoWake));
    c.register(Box::new(Bench {
        feed: Arc::clone(feed),
    }));
    c.pump(Duration::from_secs(1));
    c
}

fn entries(n: u32) -> Vec<Entry> {
    (0..n)
        .map(|i| Entry {
            ctime: i64::MIN,
            atime: i64::MIN,
            name: format!("file-{i:06}.txt").into(),
            size: u64::from(i),
            mtime: 0,
            attrs: Attrs::default(),
            kind: EntryKind::File,
        })
        .collect()
}

/// 49 чанков по 2048 + `Done` для активного таба в его текущем поколении.
fn listing_msgs(core: &Core, all: &[Entry]) -> Vec<Box<dyn Msg>> {
    let s = core.state();
    let t = s.panel(s.active).tab();
    let (tab, generation) = (t.id, t.listing.generation);
    let mut out: Vec<Box<dyn Msg>> = Vec::new();
    let mut base = 0u32;
    for part in all.chunks(CHUNK) {
        let len = u32::try_from(part.len()).expect("chunk fits u32");
        out.push(listing_msg(
            tab,
            generation,
            ListingUpdate::Chunk {
                entries: part.to_vec(),
                visible: (base..base + len).collect(),
            },
        ));
        base += len;
    }
    out.push(listing_msg(tab, generation, ListingUpdate::Done));
    out
}

fn bench(c: &mut Criterion) {
    let feed = Arc::new(Mutex::new(Vec::new()));
    let mut core = core(&feed);
    let down = Command::new("panel.cursor.down");
    c.bench_function("C.1 dispatch panel.cursor.down", |b| {
        b.iter(|| core.dispatch(black_box(down.clone())));
    });

    let fill = Command::new("bench.fill");
    c.bench_function("C.2 pump 10k empty msgs", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                core.dispatch(fill.clone());
                let t0 = Instant::now();
                let p = core.pump(Duration::from_secs(10));
                total += t0.elapsed();
                assert!(!p.more);
            }
            total
        });
    });

    let all = entries(N_ENTRIES);
    let chunks = all.chunks(CHUNK).count();
    assert_eq!(chunks, 49, "S1-14: 100k = 49 чанков по 2048");
    let feed_cmd = Command::new("bench.feed");
    let mut dir = 0u32;
    c.bench_function("C.3 apply 100k entries in 49 chunks + stats", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Новое поколение: таб снова пуст и в `Loading`.
                dir += 1;
                core.dispatch(
                    Command::new("panel.go").arg("path", Location::local(format!("/d{dir}"))),
                );
                core.pump(Duration::from_secs(1));
                *feed.lock().expect("feed") = listing_msgs(&core, &all);
                core.dispatch(feed_cmd.clone());
                let t0 = Instant::now();
                let pumped = core.pump(Duration::from_secs(10));
                total += t0.elapsed();
                assert!(!pumped.more);
                let s = core.state();
                let l = &s.panel(s.active).tab().listing;
                assert!(matches!(l.status, ListingStatus::Complete));
                assert_eq!(l.stats.files, N_ENTRIES);
            }
            total
        });
    });

    // C.4 на том же каталоге 100k: через команду (как у пользователя) и сам алгоритм.
    let invert = Command::new("sel.invert");
    c.bench_function("C.4 dispatch sel.invert 100k", |b| {
        b.iter(|| core.dispatch(black_box(invert.clone())));
    });
    let listing = Listing::with_entries(all, (0..N_ENTRIES).collect(), Gen::default());
    let mut sel = Selection::default();
    c.bench_function("C.4 Selection::invert 100k", |b| {
        b.iter(|| sel.invert(black_box(&listing), false));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
