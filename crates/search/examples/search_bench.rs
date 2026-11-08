//! Бенч поиска S8 §2.7 (S8-13): сценарии F1–F5 на сгенерированном корпусе, JSON-метрики.
//!
//! ```text
//! cargo run -p urfin-search --example search_bench --release -- gen <каталог> [--bulk 14]
//! cargo run -p urfin-search --example search_bench --release -- run <каталог> [--runs 5]
//!     [--threads <n>] [--rg <rg.exe>] [--fd <fd.exe>] [--json <файл>]
//! ```
//!
//! Корпус (`gen`, фиксированные сиды, данные — только свои):
//! - `names` — `urfin_testdata::gen_tree` 100k записей, 1000 каталогов (F1);
//! - `src` — 10k текстовых файлов ≈ 1 ГБ: строки «исходника», 10% — комментарии по-русски;
//!   1% файлов несёт `XyzzyNeedle`, ещё 1% — «Искомая строка» в UTF-8, UTF-16LE с BOM, cp1251
//!   и cp866 по очереди (F3–F5);
//! - `bulk/gNN` — `gen_tree` по 100k (F2: корень — весь корпус, ≈ 1,5M записей при `--bulk 14`).
//!
//! Сценарии (`run`, тёплый кэш: первый прогон — прогрев, в медиану не входит):
//! - F1 — имя `*.rs` в `names`: `first_ms` (первые строки в сторе), `all_ms`;
//! - F2 — имя `*.rs` во всём корпусе;
//! - F3 — литерал, кодировка `auto` (UTF-8, UTF-16 по BOM), с учётом регистра и без;
//! - F4 — те же литералы в режиме «все кодировки», `vs_f3` — во сколько раз дольше F3;
//! - F5 — отмена через 100 мс после старта (текст и обход имён): мс от отмены до возврата,
//!   живых потоков обхода после возврата.
//!
//! `--threads` — `[search] threads` (0 — авто, `volume::walk_threads`). `--rg`, `--fd` —
//! базлайны теми же запросами (`rg --files -uuu -g`, `fd -uu -g`, `rg -uuu -l`), вывод — в
//! `NUL`. F6 (кадр экрана при 1M строк) — `urfin.exe --bench-search`.
//! Поиск идёт прямо через `search_tree`, без job и `Core`: это время обхода и чтения.
//!
//! Каждый сценарий (и каждый запрос F3/F4) — в свежем дочернем процессе (`part`, #345):
//! Windows через 10–20 с понижает приоритет (`QoS`) фоновому процессу без окна, и поиск в нём
//! идёт в ~1,5 раза медленнее, а `rg` — новый процесс на каждый прогон. Окно Урфина на
//! переднем плане такого понижения не получает.

use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use encoding_rs::{Encoding, IBM866, WINDOWS_1251};
use urfin_core::search::SearchModel;
use urfin_core::{Location, Mask};
use urfin_search::{
    EncodingMode, SearchCfg, SearchQuery, TextMode, TextQuery, TreeSearch, search_tree,
};
use urfin_testdata::{Layout, Rng, TreeSpec, gen_tree};
use urfin_vfs::VfsRegistry;

const ASCII: &str = "XyzzyNeedle";
const CYR: &str = "Искомая строка";
const TEXT_FILES: usize = 10_000;
const TEXT_DIRS: usize = 100;

// ---------- корпус ----------

const IDENT: [&str; 32] = [
    "let", "mut", "self", "value", "result", "match", "Some", "None", "Ok", "Err", "fn", "pub",
    "struct", "impl", "return", "index", "buffer", "len", "iter", "map", "collect", "config",
    "state", "entry", "path", "name", "count", "items", "into", "from", "clone", "unwrap",
];
const PUNCT: [&str; 8] = [" = ", "(", ")", ".", ", ", "::", " + ", "; "];
const RU: [&str; 12] = [
    "проверка",
    "значение",
    "поле",
    "строка",
    "файл",
    "каталог",
    "если",
    "пусто",
    "ошибка",
    "размер",
    "запись",
    "конец",
];

fn pick<'a>(rng: &mut Rng, list: &[&'a str]) -> &'a str {
    list[usize::try_from(rng.below(list.len() as u64)).unwrap_or(0)]
}

/// Строка «исходника»: код или комментарий по-русски, 30–100 символов.
fn line(rng: &mut Rng, out: &mut String) {
    let indent = rng.below(4) * 4;
    for _ in 0..indent {
        out.push(' ');
    }
    let target = out.len() + 30 + usize::try_from(rng.below(70)).unwrap_or(0);
    if rng.below(10) == 0 {
        out.push_str("// ");
        while out.len() < target {
            out.push_str(pick(rng, &RU));
            out.push(' ');
        }
    } else {
        while out.len() < target {
            out.push_str(pick(rng, &IDENT));
            out.push_str(pick(rng, &PUNCT));
        }
    }
    out.push('\n');
}

/// Текст файла `i`: 40–200 КБ, иглы — в каждом сотом.
fn text_file(i: usize) -> String {
    let mut rng = Rng::new(0x5EA2_C400 + i as u64);
    let size = 40_000 + usize::try_from(rng.below(160_000)).unwrap_or(0);
    let mut s = String::with_capacity(size + 128);
    let mut planted = 0;
    while s.len() < size {
        line(&mut rng, &mut s);
        if planted < 3 && rng.below(400) == 0 {
            match i % 100 {
                7 => s.push_str("    let x = XyzzyNeedle::new(); // xyzzyneedle\n"),
                13 => s.push_str("    // Искомая строка и искомая СТРОКА\n"),
                _ => {}
            }
            planted += 1;
        }
    }
    if planted == 0 {
        match i % 100 {
            7 => s.push_str("XyzzyNeedle\n"),
            13 => s.push_str("// Искомая строка\n"),
            _ => {}
        }
    }
    s
}

fn single_byte(enc: &'static Encoding, s: &str) -> Vec<u8> {
    enc.encode(s).0.into_owned()
}

/// Байты файла `i`: файлы с кириллической иглой — по очереди в четырёх кодировках.
fn text_bytes(i: usize) -> Vec<u8> {
    let s = text_file(i);
    if i % 100 != 13 {
        return s.into_bytes();
    }
    match (i / 100) % 4 {
        1 => {
            let mut b = vec![0xFF, 0xFE];
            b.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
            b
        }
        2 => single_byte(WINDOWS_1251, &s),
        3 => single_byte(IBM866, &s),
        _ => s.into_bytes(),
    }
}

fn gen_text(root: &Path) -> std::io::Result<u64> {
    let bytes = std::sync::atomic::AtomicU64::new(0);
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|t| {
        let mut jobs = Vec::new();
        for _ in 0..threads {
            jobs.push(t.spawn(|| -> std::io::Result<()> {
                loop {
                    let d = next.fetch_add(1, Ordering::Relaxed);
                    if d >= TEXT_DIRS {
                        return Ok(());
                    }
                    let dir = root.join(format!("mod{d:03}"));
                    std::fs::create_dir_all(&dir)?;
                    let per = TEXT_FILES / TEXT_DIRS;
                    for i in d * per..(d + 1) * per {
                        let b = text_bytes(i);
                        bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
                        std::fs::write(dir.join(format!("file{i:05}.rs")), b)?;
                    }
                }
            }));
        }
        jobs.into_iter()
            .try_for_each(|j| j.join().expect("поток генератора"))
    })?;
    Ok(bytes.into_inner())
}

fn gen_corpus(dir: &Path, bulk: usize) -> std::io::Result<()> {
    let t = Instant::now();
    let names = gen_tree(
        &dir.join("names"),
        &TreeSpec {
            n: 100_000,
            seed: 1,
            layout: Layout::Nav { dirs: 1000 },
        },
    )?;
    eprintln!("names: {names:?} за {:?}", t.elapsed());
    let t = Instant::now();
    let text = gen_text(&dir.join("src"))?;
    eprintln!(
        "src: {TEXT_FILES} файлов, {} МБ за {:?}",
        text >> 20,
        t.elapsed()
    );
    let t = Instant::now();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let jobs: Vec<_> = (0..bulk.min(7))
            .map(|_| {
                s.spawn(|| -> std::io::Result<()> {
                    loop {
                        let g = next.fetch_add(1, Ordering::Relaxed);
                        if g >= bulk {
                            return Ok(());
                        }
                        let spec = TreeSpec {
                            n: 100_000,
                            seed: 100 + g as u64,
                            layout: Layout::Nav { dirs: 1000 },
                        };
                        gen_tree(&dir.join("bulk").join(format!("g{g:02}")), &spec)?;
                    }
                })
            })
            .collect();
        jobs.into_iter()
            .try_for_each(|j| j.join().expect("поток генератора"))
    })?;
    eprintln!("bulk: {bulk} × 100k за {:?}", t.elapsed());
    Ok(())
}

// ---------- замеры ----------

/// `--threads`: потоков обхода, 0 — авто.
static THREADS: AtomicU8 = AtomicU8::new(0);

fn cfg() -> SearchCfg {
    SearchCfg {
        threads: THREADS.load(Ordering::Relaxed),
        ..SearchCfg::default()
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

/// Один прогон поиска.
struct Run {
    first: f64,
    all: f64,
    rows: usize,
    files: usize,
}

fn search_once(q: &SearchQuery, reg: &VfsRegistry) -> Run {
    let cfg = cfg();
    let store = SearchModel::default().open(q.roots.clone());
    let denied = Mutex::new(Vec::new());
    let first = OnceLock::new();
    let t0 = Instant::now();
    let poke = || {
        let _ = first.set(t0.elapsed());
    };
    let run = TreeSearch {
        query: q,
        cfg: &cfg,
        reg,
        store: &store,
        denied: &denied,
        halted: &|| false,
        poke: &poke,
    };
    search_tree(&run, || {}).expect("search_tree");
    let all = t0.elapsed();
    Run {
        first: ms(first.get().copied().unwrap_or(all)),
        all: ms(all),
        rows: store.rows_len(),
        files: store.files_len(),
    }
}

/// Медианы `runs` прогонов после прогрева.
fn search(q: &SearchQuery, reg: &VfsRegistry, runs: usize) -> Run {
    let mut last = search_once(q, reg);
    let (mut first, mut all) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        last = search_once(q, reg);
        first.push(last.first);
        all.push(last.all);
    }
    Run {
        first: median(first),
        all: median(all),
        ..last
    }
}

/// Внешняя программа: медиана wall-времени, вывод — в `NUL`.
fn external(exe: &Path, args: &[&str], runs: usize) -> f64 {
    let once = || {
        let t = Instant::now();
        let st = Command::new(exe)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("запуск базлайна");
        assert!(
            st.code().is_some_and(|c| c <= 1),
            "{} {args:?}: {st}",
            exe.display()
        );
        ms(t.elapsed())
    };
    once();
    median((0..runs).map(|_| once()).collect())
}

fn name_query(root: &Path, mask: &str) -> SearchQuery {
    SearchQuery {
        roots: vec![Location::local(root)],
        name: Some(Mask::parse(mask).expect("маска")),
        ..SearchQuery::default()
    }
}

fn text_query(root: &Path, pattern: &str, case: bool, encoding: EncodingMode) -> SearchQuery {
    SearchQuery {
        roots: vec![Location::local(root)],
        text: Some(TextQuery {
            pattern: pattern.to_owned(),
            mode: TextMode::Literal,
            case,
            encoding,
            ..TextQuery::default()
        }),
        ..SearchQuery::default()
    }
}

fn opt(v: Option<f64>) -> String {
    v.map_or_else(|| "null".to_owned(), |v| format!("{v:.1}"))
}

struct Bench {
    reg: VfsRegistry,
    runs: usize,
    rg: Option<PathBuf>,
    fd: Option<PathBuf>,
}

impl Bench {
    /// F1, F2: имя `*.rs`.
    fn names(&self, root: &Path) -> String {
        let r = search(&name_query(root, "*.rs"), &self.reg, self.runs);
        let dir = root.to_str().expect("путь");
        let rg = self
            .rg
            .as_ref()
            .map(|rg| external(rg, &["--files", "-uuu", "-g", "*.rs", dir], self.runs));
        let fd = self
            .fd
            .as_ref()
            .map(|fd| external(fd, &["-uu", "-g", "*.rs", dir], self.runs));
        format!(
            "{{\"first_ms\":{:.1},\"all_ms\":{:.1},\"hits\":{},\"rg_files_ms\":{},\
             \"vs_rg\":{},\"fd_ms\":{},\"vs_fd\":{}}}",
            r.first,
            r.all,
            r.rows,
            opt(rg),
            rg.map_or_else(|| "null".to_owned(), |b| format!("{:.2}", r.all / b)),
            opt(fd),
            fd.map_or_else(|| "null".to_owned(), |b| format!("{:.2}", r.all / b)),
        )
    }

    /// Один текстовый сценарий: (мс, JSON).
    fn text(&self, root: &Path, pattern: &str, case: bool, enc: EncodingMode) -> (f64, String) {
        let r = search(&text_query(root, pattern, case, enc), &self.reg, self.runs);
        let rg = (enc == EncodingMode::Auto)
            .then_some(self.rg.as_ref())
            .flatten()
            .map(|rg| {
                let flag = if case { "-s" } else { "-i" };
                let dir = root.to_str().expect("путь");
                external(rg, &["-uuu", "-l", "-F", flag, pattern, dir], self.runs)
            });
        let json = format!(
            "\"ms\":{:.1},\"first_ms\":{:.1},\"hits\":{},\"files\":{},\"rg_l_ms\":{},\"vs_rg\":{}",
            r.all,
            r.first,
            r.rows,
            r.files,
            opt(rg),
            rg.map_or_else(|| "null".to_owned(), |b| format!("{:.2}", r.all / b)),
        );
        (r.all, json)
    }

    /// F3 и F4 одного литерала: строки `"ключ":{…}` для F3 и F4.
    fn texts(&self, root: &Path, key: &str) -> (String, String) {
        let (label, suffix) = key.split_once('_').expect("ключ F3");
        let pattern = if label == "ascii" { ASCII } else { CYR };
        let case = suffix == "case";
        let (base, j3) = self.text(root, pattern, case, EncodingMode::Auto);
        let (all, j4) = self.text(root, pattern, case, EncodingMode::All);
        eprintln!("F3/F4 {key}: {base:.0} / {all:.0} мс");
        (
            format!("\"{key}\":{{{j3}}}"),
            format!("\"{key}\":{{{j4},\"vs_f3\":{:.2}}}", all / base),
        )
    }
}

/// Запросы F3/F4.
const TEXT_KEYS: [&str; 4] = ["ascii_case", "ascii_nocase", "cyr_case", "cyr_nocase"];

// ---------- F5: отмена ----------

/// Живой поток обхода: метка в TLS снимается при выходе потока.
struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Отмена через `after`: (мс от отмены до возврата, живых потоков обхода через 1 с).
fn cancel_once(q: &SearchQuery, reg: &VfsRegistry, after: Duration) -> (f64, usize) {
    let cfg = cfg();
    let store = SearchModel::default().open(q.roots.clone());
    let denied = Mutex::new(Vec::new());
    let flag = AtomicBool::new(false);
    let live = Arc::new(AtomicUsize::new(0));
    let main: ThreadId = std::thread::current().id();
    let halted = || {
        if std::thread::current().id() != main {
            LIVE.with(|l| {
                l.borrow_mut().get_or_insert_with(|| {
                    live.fetch_add(1, Ordering::SeqCst);
                    Live(Arc::clone(&live))
                });
            });
        }
        flag.load(Ordering::SeqCst)
    };
    let run = TreeSearch {
        query: q,
        cfg: &cfg,
        reg,
        store: &store,
        denied: &denied,
        halted: &halted,
        poke: &|| {},
    };
    let stopped = Mutex::new(None);
    std::thread::scope(|t| {
        t.spawn(|| {
            std::thread::sleep(after);
            *stopped.lock().expect("mutex") = Some(Instant::now());
            flag.store(true, Ordering::SeqCst);
        });
        search_tree(&run, || {}).expect("search_tree");
    });
    let done = Instant::now();
    let at = stopped.lock().expect("mutex").unwrap_or(done);
    // TLS снимается при выходе потока, чуть позже возврата.
    while live.load(Ordering::SeqCst) != 0 && done.elapsed() < Duration::from_secs(1) {
        std::thread::yield_now();
    }
    (
        ms(done.saturating_duration_since(at)),
        live.load(Ordering::SeqCst),
    )
}

fn cancel(q: &SearchQuery, reg: &VfsRegistry, runs: usize) -> String {
    let mut stops = Vec::new();
    let mut threads = 0;
    for _ in 0..runs.max(3) {
        let (stop, alive) = cancel_once(q, reg, Duration::from_millis(100));
        stops.push(stop);
        threads = threads.max(alive);
    }
    let max = stops.iter().copied().fold(0.0, f64::max);
    format!(
        "{{\"stop_p50_ms\":{:.1},\"stop_max_ms\":{max:.1},\"threads_after\":{threads}}}",
        median(stops)
    )
}

// ---------- main ----------

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    // `part <имя> <каталог>`: один сценарий в этом процессе (его зовёт `run`).
    let name = if cmd == "part" {
        args.next().expect("part <имя>")
    } else {
        String::new()
    };
    let (mut dir, mut runs, mut bulk, mut json) = (None, 5usize, 14usize, None);
    let (mut rg, mut fd) = (None, None);
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| panic!("{a} <значение>"));
        match a.as_str() {
            "--runs" => runs = value().parse().expect("--runs <n>"),
            "--bulk" => bulk = value().parse().expect("--bulk <n>"),
            "--json" => json = Some(PathBuf::from(value())),
            "--rg" => rg = Some(PathBuf::from(value())),
            "--fd" => fd = Some(PathBuf::from(value())),
            "--threads" => {
                THREADS.store(value().parse().expect("--threads <n>"), Ordering::Relaxed);
            }
            _ => dir = Some(PathBuf::from(&a)),
        }
    }
    let dir = std::path::absolute(dir.expect("каталог корпуса")).expect("путь");
    match cmd.as_str() {
        "gen" => gen_corpus(&dir, bulk).expect("корпус"),
        "part" => {
            let b = Bench {
                reg: VfsRegistry::with_local(),
                runs,
                rg,
                fd,
            };
            for line in part(&name, &dir, &b) {
                println!("{line}");
            }
            println!("{}", peak_mb());
        }
        "run" => {
            let out = run(&dir, &PartArgs { runs, rg, fd });
            println!("{out}");
            if let Some(p) = json {
                std::fs::File::create(&p)
                    .and_then(|mut f| writeln!(f, "{out}"))
                    .expect("json");
            }
        }
        _ => panic!("gen <каталог> | run <каталог> | part <имя> <каталог>"),
    }
}

/// Что `run` передаёт дочерним `part`.
struct PartArgs {
    runs: usize,
    rg: Option<PathBuf>,
    fd: Option<PathBuf>,
}

/// Пик private-памяти процесса, МБ (`null` — не узнать).
fn peak_mb() -> String {
    urfin_vfs::diag::peak_memory()
        .and_then(|(_, p)| p)
        .map_or_else(
            || "null".to_owned(),
            |v| {
                format!(
                    "{:.1}",
                    f64::from(u32::try_from(v >> 10).unwrap_or(u32::MAX)) / 1024.0
                )
            },
        )
}

/// Сценарий в этом процессе: строки JSON.
fn part(name: &str, dir: &Path, b: &Bench) -> Vec<String> {
    let src = dir.join("src");
    match name {
        "f1" => vec![b.names(&dir.join("names"))],
        "f2" => vec![b.names(dir)],
        "f5" => {
            let slow = text_query(&src, CYR, false, EncodingMode::All);
            let text = cancel(&slow, &b.reg, b.runs);
            let walk = cancel(&name_query(dir, "*.rs"), &b.reg, b.runs);
            vec![format!("{{\"text\":{text},\"walk\":{walk}}}")]
        }
        key => {
            let (f3, f4) = b.texts(&src, key);
            vec![f3, f4]
        }
    }
}

/// Сценарий в свежем дочернем процессе: строки JSON и пик памяти (последней строкой).
fn spawn_part(name: &str, dir: &Path, a: &PartArgs) -> (Vec<String>, Option<f64>) {
    let exe = std::env::current_exe().expect("путь бенча");
    let mut cmd = Command::new(exe);
    cmd.arg("part").arg(name).arg(dir);
    cmd.args(["--runs", &a.runs.to_string()]);
    cmd.args(["--threads", &THREADS.load(Ordering::Relaxed).to_string()]);
    if let Some(rg) = &a.rg {
        cmd.arg("--rg").arg(rg);
    }
    if let Some(fd) = &a.fd {
        cmd.arg("--fd").arg(fd);
    }
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .expect("дочерний бенч");
    assert!(out.status.success(), "part {name}: {}", out.status);
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let peak = lines.pop().and_then(|p| p.parse().ok());
    (lines, peak)
}

fn run(dir: &Path, a: &PartArgs) -> String {
    let mut peak: Option<f64> = None;
    let mut one = |name: &str| {
        let (lines, p) = spawn_part(name, dir, a);
        peak = match (peak, p) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        lines
    };
    let f1 = one("f1").concat();
    eprintln!("F1: {f1}");
    let f2 = one("f2").concat();
    eprintln!("F2: {f2}");
    let (mut f3, mut f4) = (Vec::new(), Vec::new());
    for key in TEXT_KEYS {
        let mut lines = one(key).into_iter();
        f3.extend(lines.next());
        f4.extend(lines.next());
    }
    let f5 = one("f5").concat();
    eprintln!("F5: {f5}");
    let peak = peak.map_or_else(|| "null".to_owned(), |v| format!("{v:.1}"));
    let threads = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    format!(
        "{{\"corpus\":{:?},\"runs\":{},\"cpus\":{threads},\"F1\":{f1},\"F2\":{f2},\
         \"F3\":{{{}}},\"F4\":{{{}}},\"F5\":{f5},\"peak_private_mb\":{peak}}}",
        dir.display().to_string(),
        a.runs,
        f3.join(","),
        f4.join(","),
    )
}
