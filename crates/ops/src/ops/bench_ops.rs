//! Бенч-харнесс S5-25 (S5 §7): сценарии на датасетах B1, B2, B4, B5 против
//! Проводника (`IFileOperation`), robocopy и `rd /s /q`. Замер, не приёмка:
//!
//! ```text
//! cargo nextest run --release -p urfin-ops bench_ops --run-ignored only --no-capture
//! ```
//!
//! Переменные: `URFIN_BENCH_OPS` — сценарии через запятую (по умолчанию все
//! доступные на одном томе NTFS: `3,5,7,9,12,13,14`; #6 и #9 считаются по прогонам
//! #3), `URFIN_BENCH_RUNS` — прогонов (5), `URFIN_BENCH_B2_N` — файлов B2 (100k),
//! `URFIN_BENCH_B1_GIB` — размер B1 (2), `URFIN_BENCH_B5_GIB` — данных B5 (1, файл
//! в 10 раз больше), `URFIN_BENCH_OUT` — дописать таблицу в файл.
//!
//! Датасеты — `urfin-testdata` (`%LOCALAPPDATA%\Pets\testdata\urfin`, сиды
//! фиксированы), рабочий каталог — рядом (тот же том), удаляется в конце. Кэш ОС
//! горячий: сброс (`RAMMap -Et`) требует прав администратора. Прогоны чередуют
//! инструменты, чтобы фон (Defender, турбобуст) делился поровну.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::Command as Proc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use super::*;
use crate::sys::shell_ops::{self, ShellOp};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn env_num(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(d)
}

/// Буфер псевдослучайных байт: из него режутся файлы датасетов.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut r = urfin_testdata::Rng::new(seed);
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        v.extend_from_slice(&r.next_u64().to_le_bytes());
    }
    v.truncate(len);
    v
}

/// B2: `n` файлов `1–64 KiB`, по 20 в каталоге, каталоги — глубина 6 (как
/// `node_modules`).
fn b2(n: usize) -> PathBuf {
    let sig = format!("b2 v1 n={n} seed=25");
    urfin_testdata::dataset_with("urfin", &format!("ops-b2-{n}"), &sig, |root| {
        let buf = noise(128 << 10, 25);
        let mut r = urfin_testdata::Rng::new(25);
        for i in 0..n {
            let mut d = i / 20;
            let mut dir = root.to_path_buf();
            for _ in 0..6 {
                dir.push(format!("node_modules/pkg-{}", d % 5));
                d /= 5;
            }
            if i % 20 == 0 {
                std::fs::create_dir_all(&dir)?;
            }
            let len = usize::try_from(1024 + r.below(63 * 1024)).unwrap_or(1024);
            let off = usize::try_from(r.below(64 << 10)).unwrap_or(0);
            std::fs::write(dir.join(format!("f{i}.js")), &buf[off..off + len])?;
        }
        Ok(())
    })
    .unwrap()
}

/// B4: 10k файлов, пути 300–1000 символов (цепочки каталогов по 100 символов).
fn b4() -> PathBuf {
    urfin_testdata::dataset_with("urfin", "ops-b4", "b4 v1", |root| {
        for g in 0..100usize {
            let mut dir = root.join(format!("g{g:03}"));
            for k in 0..(3 + g % 7) {
                dir.push(format!("{k}{}", "long_directory_name_".repeat(5)));
            }
            std::fs::create_dir_all(&dir)?;
            for i in 0..100 {
                std::fs::write(
                    dir.join(format!("{}{i:03}.txt", "file_".repeat(8))),
                    [b'x'; 1024],
                )?;
            }
        }
        Ok(())
    })
    .unwrap()
}

/// Один файл `bytes` в каталоге датасета; `sparse` — разреженный, данные — каждая
/// десятая полоса по `64 MiB`.
fn big(name: &str, bytes: u64, sparse: bool) -> PathBuf {
    let sig = format!("{name} v1 bytes={bytes} sparse={sparse}");
    urfin_testdata::dataset_with("urfin", name, &sig, |root| {
        let p = root.join("big.bin");
        let mut f = std::fs::File::create(&p)?;
        let buf = noise(64 << 20, 1);
        if sparse {
            drop(f);
            crate::sys::fs::make_sparse(&p)?;
            f = std::fs::OpenOptions::new().write(true).open(&p)?;
            f.set_len(bytes)?;
            let mut at = 0;
            while at < bytes {
                use std::io::{Seek, SeekFrom};
                f.seek(SeekFrom::Start(at))?;
                f.write_all(&buf)?;
                at += 10 * buf.len() as u64;
            }
        } else {
            let mut left = bytes;
            while left > 0 {
                let k = usize::try_from(left.min(buf.len() as u64)).unwrap_or(0);
                f.write_all(&buf[..k])?;
                left -= k as u64;
            }
        }
        f.sync_all()
    })
    .unwrap()
}

fn rmrf(p: &Path) {
    if p.exists() {
        remove_dir_all::remove_dir_all(p).unwrap();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tool {
    Urfin,
    UrfinVerify,
    Explorer,
    Robocopy,
    RobocopyJ,
    Rd,
    CmdMove,
}

impl Tool {
    fn label(self) -> &'static str {
        match self {
            Tool::Urfin => "Urfin",
            Tool::UrfinVerify => "Urfin, verify xxh3",
            Tool::Explorer => "Explorer (IFileOperation)",
            Tool::Robocopy => "robocopy /E /MT:16",
            Tool::RobocopyJ => "robocopy /E /J",
            Tool::Rd => "rd /s /q",
            Tool::CmdMove => "move",
        }
    }
}

fn robocopy(src: &Path, dst: &Path, unbuffered: bool) {
    let mut c = Proc::new("robocopy");
    c.arg(src)
        .arg(dst)
        .args(["/E", "/R:0", "/W:0", "/NFL", "/NDL", "/NP", "/NJH", "/NJS"]);
    c.arg(if unbuffered { "/J" } else { "/MT:16" });
    let code = c.output().unwrap().status.code().unwrap_or(16);
    // 0–7 — успех (что скопировано, что пропущено); 8+ — ошибки.
    assert!(code < 8, "robocopy exit {code}");
}

fn cmd(args: &[&std::ffi::OsStr]) {
    let st = Proc::new("cmd").arg("/C").args(args).status().unwrap();
    assert!(st.success(), "cmd {args:?}: {st}");
}

/// Копия или перенос `src` (каталог) в каталог `into` (появится `into/<имя src>`)
/// движком Urfin.
fn urfin(h: &mut H, mode: Mode, src: &Path, into: &Path, opts: OpOptions) -> JobSummary {
    let name = src.file_name().unwrap().to_string_lossy().into_owned();
    let id = h.start(
        super::super::WORKERS,
        transfer(mode, src.parent().unwrap(), &[&name], into, None, opts),
    );
    while h.state(id).is_active() || h.eng.live_threads() > 0 {
        assert!(!has_prompt(h), "unexpected prompt");
        h.pump();
    }
    match h.state(id) {
        JobState::Done(s) => s,
        st => panic!("urfin {mode:?}: {st:?}"),
    }
}

/// Время до первой записи в `dir` (TTFB: назначение начало заполняться).
struct Ttfb {
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<Option<Duration>>,
}

impl Ttfb {
    fn watch(dir: PathBuf, t0: Instant) -> Ttfb {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let s = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut got = None;
            while !s.load(Ordering::Relaxed) {
                if std::fs::read_dir(&dir).is_ok_and(|mut r| r.next().is_some()) {
                    got = Some(t0.elapsed());
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = tx.send(got);
        });
        Ttfb { stop, rx }
    }

    fn get(self) -> Option<Duration> {
        self.stop.store(true, Ordering::Relaxed);
        self.rx.recv().ok().flatten()
    }
}

/// Ряд таблицы: время прогонов одного инструмента в сценарии.
#[derive(Default)]
struct Row {
    times: Vec<f64>,
    ttfb: Vec<f64>,
    errors: u64,
}

fn stats(v: &[f64]) -> (f64, f64, f64) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let med = s[s.len() / 2];
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "индекс перцентиля"
    )]
    let p95 = s[((s.len() as f64 * 0.95).ceil() as usize).clamp(1, s.len()) - 1];
    #[allow(clippy::cast_precision_loss, reason = "статистика")]
    let n = s.len() as f64;
    let mean = s.iter().sum::<f64>() / n;
    let sd = (s.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt();
    (med, p95, sd / mean * 100.0)
}

/// Таблица сценария в markdown: медиана, p95, разброс (CV), TTFB, ошибки.
fn table(title: &str, rows: &[(Tool, Row)], out: &mut String) {
    let _ = writeln!(out, "\n### {title}\n");
    let _ = writeln!(
        out,
        "| Инструмент | медиана, с | p95, с | CV | TTFB, мс | ошибок |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|");
    for (t, r) in rows {
        if r.times.is_empty() {
            continue;
        }
        let (med, p95, cv) = stats(&r.times);
        let ttfb = if r.ttfb.is_empty() {
            "—".to_owned()
        } else {
            format!("{:.0}", stats(&r.ttfb).0 * 1000.0)
        };
        let _ = writeln!(
            out,
            "| {} | {med:.2} | {p95:.2} | {cv:.1} % | {ttfb} | {} |",
            t.label(),
            r.errors
        );
    }
    eprint!("{out}");
}

fn free_gib(p: &Path) -> u64 {
    crate::sys::fs::free_space(p).map_or(0, |(free, _)| free / GIB)
}

/// Хватит ли места на `need` байт с запасом `5 GiB`; иначе сценарий пропускается.
fn room(work: &Path, need: u64, what: &str, out: &mut String) -> bool {
    let free = free_gib(work);
    if free < need / GIB + 5 {
        let _ = writeln!(
            out,
            "\n{what}: пропущен, свободно {free} ГиБ, нужно {} ГиБ + 5",
            need / GIB
        );
        return false;
    }
    true
}

/// #3 копия B2 (и #6 TTFB по тем же прогонам), #9 удаление копий: каждая копия
/// удаляется следующим по кругу инструментом удаления.
fn copy_delete_b2(h: &mut H, work: &Path, runs: usize, del: bool, out: &mut String) {
    let n = usize::try_from(env_num("URFIN_BENCH_B2_N", 100_000)).unwrap_or(100_000);
    let src = b2(n);
    let files = count_files(&src);
    let copy_tools = [Tool::Urfin, Tool::Explorer, Tool::Robocopy];
    let del_tools = [Tool::Urfin, Tool::Explorer, Tool::Rd];
    let mut copies: Vec<(Tool, Row)> = copy_tools.map(|t| (t, Row::default())).into();
    let mut dels: Vec<(Tool, Row)> = del_tools.map(|t| (t, Row::default())).into();
    let name = src.file_name().unwrap().to_owned();
    for r in 0..runs {
        for (k, (tool, row)) in copies.iter_mut().enumerate() {
            let into = work.join("b2-dst");
            rmrf(&into);
            std::fs::create_dir_all(&into).unwrap();
            let dst = into.join(&name);
            let t0 = Instant::now();
            let ttfb = Ttfb::watch(dst.clone(), t0);
            match tool {
                Tool::Urfin => {
                    urfin(h, Mode::Copy, &src, &into, OpOptions::default());
                }
                Tool::Explorer => {
                    shell_ops::run(ShellOp::Copy, &src, Some(&into)).unwrap();
                }
                _ => robocopy(&src, &dst, false),
            }
            row.times.push(t0.elapsed().as_secs_f64());
            if let Some(t) = ttfb.get() {
                row.ttfb.push(t.as_secs_f64());
            }
            row.errors += files.saturating_sub(count_files(&dst));
            if del {
                let (dt, drow) = &mut dels[(r + k) % del_tools.len()];
                let t0 = Instant::now();
                match dt {
                    Tool::Urfin => {
                        let errs = crate::ops::delete::delete_permanent(
                            &into,
                            &[CompactString::from(name.to_string_lossy().as_ref())],
                            &|| false,
                            &|_| {},
                        );
                        drow.errors += errs.len() as u64;
                    }
                    Tool::Explorer => {
                        shell_ops::run(ShellOp::Delete, &dst, None).unwrap();
                    }
                    _ => cmd(&["rd".as_ref(), "/s".as_ref(), "/q".as_ref(), dst.as_os_str()]),
                }
                drow.times.push(t0.elapsed().as_secs_f64());
                drow.errors += u64::from(dst.exists());
            }
            rmrf(&into);
        }
    }
    let sz = urfin_bytes(&src) / MIB;
    table(
        &format!("#3 копия B2 ({files} файлов, {sz} МиБ), тот же том NVMe; #6 TTFB"),
        &copies,
        out,
    );
    if del {
        table(
            &format!("#9 удаление B2 насовсем ({files} файлов)"),
            &dels,
            out,
        );
    }
}

fn urfin_bytes(root: &Path) -> u64 {
    let mut n = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => n += m.len(),
                Err(_) => {}
            }
        }
    }
    n
}

/// #7 перенос B2 в пределах тома: туда инструментом, обратно — `rename`.
fn move_b2(h: &mut H, work: &Path, runs: usize, out: &mut String) {
    let n = usize::try_from(env_num("URFIN_BENCH_B2_N", 100_000)).unwrap_or(100_000);
    let src = b2(n);
    let name = src.file_name().unwrap().to_owned();
    let into = work.join("b2-moved");
    std::fs::create_dir_all(&into).unwrap();
    let moved = into.join(&name);
    let mut rows: Vec<(Tool, Row)> = [Tool::Urfin, Tool::Explorer, Tool::CmdMove]
        .map(|t| (t, Row::default()))
        .into();
    for _ in 0..runs {
        for (tool, row) in &mut rows {
            let t0 = Instant::now();
            match tool {
                Tool::Urfin => {
                    urfin(h, Mode::Move, &src, &into, OpOptions::default());
                }
                Tool::Explorer => {
                    shell_ops::run(ShellOp::Move, &src, Some(&into)).unwrap();
                }
                _ => cmd(&["move".as_ref(), src.as_os_str(), moved.as_os_str()]),
            }
            row.times.push(t0.elapsed().as_secs_f64());
            row.errors += u64::from(src.exists() || !moved.is_dir());
            std::fs::rename(&moved, &src).unwrap();
        }
    }
    rmrf(&into);
    table("#7 перенос B2, тот же том", &rows, out);
}

/// #5 копия B1 (один большой файл, тот же том) и #12 с проверкой xxh3.
fn big_b1(h: &mut H, work: &Path, runs: usize, verify: bool, out: &mut String) {
    let gib = env_num("URFIN_BENCH_B1_GIB", 2);
    if !room(work, 2 * gib * GIB, "#5/#12 B1", out) {
        return;
    }
    let src = big(&format!("ops-b1-{gib}g"), gib * GIB, false);
    let mut tools = vec![Tool::Urfin, Tool::Explorer, Tool::RobocopyJ];
    if verify {
        tools.insert(1, Tool::UrfinVerify);
    }
    let mut rows: Vec<(Tool, Row)> = tools.into_iter().map(|t| (t, Row::default())).collect();
    for _ in 0..runs {
        for (tool, row) in &mut rows {
            let into = work.join("b1-dst");
            rmrf(&into);
            std::fs::create_dir_all(&into).unwrap();
            let dst = into.join(src.file_name().unwrap());
            let t0 = Instant::now();
            match tool {
                Tool::Urfin => {
                    urfin(h, Mode::Copy, &src, &into, OpOptions::default());
                }
                Tool::UrfinVerify => {
                    urfin(h, Mode::Copy, &src, &into, hash_opts());
                }
                Tool::Explorer => {
                    shell_ops::run(ShellOp::Copy, &src, Some(&into)).unwrap();
                }
                _ => robocopy(&src, &dst, true),
            }
            row.times.push(t0.elapsed().as_secs_f64());
            row.errors += u64::from(!dst.join("big.bin").is_file());
            rmrf(&into);
        }
    }
    table(
        &format!("#5/#12 копия B1 ({gib} ГиБ), тот же том NVMe"),
        &rows,
        out,
    );
}

/// #13 длинные пути: копия B4, ошибки — недостающие файлы.
fn long_b4(h: &mut H, work: &Path, out: &mut String) {
    let src = b4();
    let files = count_files(&src);
    let mut rows: Vec<(Tool, Row)> = [Tool::Urfin, Tool::Explorer, Tool::Robocopy]
        .map(|t| (t, Row::default()))
        .into();
    for (tool, row) in &mut rows {
        let into = work.join("b4-dst");
        rmrf(&into);
        std::fs::create_dir_all(&into).unwrap();
        let dst = into.join(src.file_name().unwrap());
        let t0 = Instant::now();
        match tool {
            Tool::Urfin => {
                let s = urfin(h, Mode::Copy, &src, &into, OpOptions::default());
                row.errors += s.errors;
            }
            Tool::Explorer => {
                // Shell не открывает пути длиннее MAX_PATH: ошибка — не провал бенча.
                let _ = shell_ops::run(ShellOp::Copy, &src, Some(&into));
            }
            _ => robocopy(&src, &dst, false),
        }
        row.times.push(t0.elapsed().as_secs_f64());
        row.errors = row.errors.max(files.saturating_sub(count_files(&dst)));
        rmrf(&into);
    }
    table(
        &format!("#13 длинные пути B4 ({files} файлов, 300–1000 символов), 1 прогон"),
        &rows,
        out,
    );
}

/// #14 разреженный B5: место копии на диске.
fn sparse_b5(h: &mut H, work: &Path, runs: usize, out: &mut String) {
    let gib = env_num("URFIN_BENCH_B5_GIB", 1);
    // Копия без поддержки sparse разворачивается в полный размер.
    if !room(work, 12 * gib * GIB, "#14 B5", out) {
        return;
    }
    let src = big(&format!("ops-b5-{gib}g"), 10 * gib * GIB, true);
    let mut rows: Vec<(Tool, Row)> = [Tool::Urfin, Tool::Explorer]
        .map(|t| (t, Row::default()))
        .into();
    let mut disk = String::new();
    for r in 0..runs {
        for (tool, row) in &mut rows {
            let into = work.join("b5-dst");
            rmrf(&into);
            std::fs::create_dir_all(&into).unwrap();
            let t0 = Instant::now();
            match tool {
                Tool::Urfin => {
                    urfin(h, Mode::Copy, &src, &into, OpOptions::default());
                }
                _ => {
                    shell_ops::run(ShellOp::Copy, &src, Some(&into)).unwrap();
                }
            }
            row.times.push(t0.elapsed().as_secs_f64());
            let f = into.join(src.file_name().unwrap()).join("big.bin");
            let on_disk = crate::sys::fs::disk_size(&f).unwrap_or(0);
            if r == 0 {
                let _ = write!(disk, "{}: {} МиБ; ", tool.label(), on_disk / MIB);
            }
            rmrf(&into);
        }
    }
    let src_disk = crate::sys::fs::disk_size(&src.join("big.bin")).unwrap_or(0) / MIB;
    table(
        &format!(
            "#14 разреженный B5 ({} ГиБ, данных {src_disk} МиБ)",
            10 * gib
        ),
        &rows,
        out,
    );
    let _ = writeln!(out, "\nМесто копии на диске: {disk}");
    eprintln!("Место копии на диске: {disk}");
}

/// S5-25: сценарии §7, доступные на стенде (один том SSD, NTFS).
#[test]
#[ignore = "замер на стенде, долго и гигабайты записи"]
fn bench_ops() {
    let list = std::env::var("URFIN_BENCH_OPS").unwrap_or_else(|_| "3,5,7,9,12,13,14".into());
    let on: Vec<u32> = list
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let runs = usize::try_from(env_num("URFIN_BENCH_RUNS", 5))
        .unwrap_or(5)
        .max(1);
    let work = urfin_testdata::datasets_root("urfin").join("ops-work");
    rmrf(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut h = H::new(64);
    h.deadline = Duration::from_secs(3600);
    let mut out = format!(
        "\n## Прогон {} (прогонов: {runs}, кэш горячий)\n",
        crate::sys::fs::local_now_text()
    );
    if on.contains(&3) || on.contains(&9) {
        copy_delete_b2(&mut h, &work, runs, on.contains(&9), &mut out);
    }
    if on.contains(&7) {
        move_b2(&mut h, &work, runs, &mut out);
    }
    if on.contains(&5) || on.contains(&12) {
        big_b1(&mut h, &work, runs, on.contains(&12), &mut out);
    }
    if on.contains(&13) {
        long_b4(&mut h, &work, &mut out);
    }
    if on.contains(&14) {
        sparse_b5(&mut h, &work, runs, &mut out);
    }
    rmrf(&work);
    if let Ok(p) = std::env::var("URFIN_BENCH_OUT") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        f.write_all(out.as_bytes()).unwrap();
    }
}
