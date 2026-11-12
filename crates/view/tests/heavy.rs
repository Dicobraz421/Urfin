//! Тяжёлые приёмочные замеры S7-02…S7-06: `#[ignore]`, прогон один раз в release:
//! `cargo nextest run -p urfin-view --release --run-ignored only --no-capture`.
//! Цифры — в карточки `docs/tasks/S7-0*.md`.

#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    clippy::too_many_lines,
    reason = "тесты: короткие имена, доли длины, таблицы случаев, замер — один сценарий"
)]

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use urfin_core::{CancelToken, Location};
use urfin_vfs::VfsRegistry;
use urfin_view::cache::{Loader, PageCache, Prio};
use urfin_view::index::{IndexError, IndexMode, Indexer, Sep};
use urfin_view::layout::{LayoutSpec, RowText, WrapKind, line_start_before, render_row, rows_down};
use urfin_view::source::{OwnedSource, SourceKind, SourceOpts, open_source, read_full};
use urfin_view::{ByteSource, detect};

fn log(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len + 200);
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = 40 + (x % 120) as usize;
        v.extend(std::iter::repeat_n(b'a' + (x % 26) as u8, n));
        v.push(b'\n');
    }
    v.truncate(len);
    v
}

/// S7-02: sparse 4,5 ГБ открывается ≤ 20 мс (`open_source` + первые 64 КБ + хвост).
#[test]
#[ignore = "тяжёлый: 4,5 ГБ sparse, release"]
fn sparse_4_5gb_opens_fast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v3.bin");
    let f = File::create(&path).unwrap();
    drop(f);
    let sparse = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&path)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(sparse, "fsutil sparse setflag не сработал");
    let len: u64 = 4_500_000_000;
    let mut f = File::options().write(true).open(&path).unwrap();
    f.set_len(len).unwrap();
    for at in [0u64, 1 << 32, len - 4096] {
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(&[0xAB; 4096]).unwrap();
    }
    f.set_modified(SystemTime::now() - Duration::from_secs(300))
        .unwrap();
    drop(f);
    let reg = VfsRegistry::with_local();
    let loc = Location::local(&path);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let o = open_source(&reg, &loc, &SourceOpts::default(), &CancelToken::new()).unwrap();
        let mut head = vec![0u8; 64 << 10];
        assert_eq!(read_full(&*o.src, 0, &mut head).unwrap(), 64 << 10);
        let mut tail = [0u8; 16];
        read_full(&*o.src, (1 << 32) + 8, &mut tail).unwrap();
        let el = t.elapsed();
        assert_eq!(o.src.kind(), SourceKind::Mapped);
        assert_eq!(o.src.len(), len);
        assert_eq!((head[0], tail[0]), (0xAB, 0xAB));
        best = best.min(el);
    }
    println!("S7-02 sparse 4.5 GB open+64KiB: best {best:?}");
    assert!(best <= Duration::from_millis(20), "{best:?}");
}

/// S7-03: `PageCache::get` ≤ 50 нс.
#[test]
#[ignore = "замер, release"]
fn cache_get_ns() {
    let data = log(8 << 20);
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(data));
    let mut c = PageCache::with_source(8 << 20, src.clone());
    let l = Loader::new(src);
    for at in (0..8u64 << 20).step_by(64 << 10) {
        c.want(at..at + 100, Prio::Visible);
        while let Some(r) = c.take_request() {
            c.insert(l.load(&r, &CancelToken::new()));
        }
    }
    let n = 2_000_000u64;
    let t = Instant::now();
    let mut off = 0u64;
    let mut sum = 0usize;
    for _ in 0..n {
        off = (off + 40_009) % ((8 << 20) - 200);
        sum += c.get(black_box(off..off + 160)).map_or(0, |b| b.len());
    }
    let ns = t.elapsed().as_nanos() as f64 / n as f64;
    black_box(sum);
    println!("S7-03 PageCache::get: {ns:.1} ns");
    assert!(ns <= 50.0, "{ns} ns");
}

/// S7-04: детект ≤ 1 мс на 64 КБ (худший — chardetng на однобайтовой кириллице).
#[test]
#[ignore = "замер, release"]
fn detect_64k_under_1ms() {
    let ru = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь. ".repeat(1600);
    let (cp1251, ..) = encoding_rs::WINDOWS_1251.encode(&ru);
    let samples: [(&str, Vec<u8>); 3] = [
        ("cp1251", cp1251[..64 << 10].to_vec()),
        ("utf-8", ru.as_bytes()[..64 << 10].to_vec()),
        ("log", log(64 << 10)),
    ];
    for (name, s) in samples {
        let mut best = Duration::MAX;
        for _ in 0..30 {
            let t = Instant::now();
            black_box(detect(black_box(&s), false, Some(b"ru")));
            best = best.min(t.elapsed());
        }
        println!("S7-04 detect 64 KiB {name}: {best:?}");
        assert!(best <= Duration::from_millis(1), "{name}: {best:?}");
    }
}

/// S7-05: скорость индекса в памяти (warm) и отмена ≤ 50 мс в обоих режимах.
#[test]
#[ignore = "тяжёлый: 1 ГиБ в памяти, release"]
fn index_speed_and_cancel() {
    let len = 1usize << 30;
    let src = Arc::new(OwnedSource::new(log(len)));
    for mode in [IndexMode::Parallel, IndexMode::Sequential] {
        let mut ix = Indexer::new(Sep::Byte(b'\n'));
        let t = Instant::now();
        ix.run(&*src, mode, &CancelToken::new(), &mut |_| {})
            .unwrap();
        let el = t.elapsed();
        let gbs = len as f64 / el.as_secs_f64() / 1e9;
        println!(
            "S7-05 index {mode:?}: {gbs:.2} GB/s ({} lines, {} threads)",
            ix.lines,
            rayon::current_num_threads()
        );
    }
    for mode in [IndexMode::Parallel, IndexMode::Sequential] {
        let cancel = CancelToken::new();
        let (c2, src) = (cancel.clone(), src.clone());
        let h = std::thread::spawn(move || {
            let mut ix = Indexer::new(Sep::Byte(b'\n'));
            let r = ix.run(&*src, mode, &c2, &mut |_| {});
            (matches!(r, Err(IndexError::Cancelled)), Instant::now())
        });
        std::thread::sleep(Duration::from_millis(20));
        let at = Instant::now();
        cancel.cancel();
        let (cancelled, done) = h.join().unwrap();
        let lag = done.saturating_duration_since(at);
        println!("S7-05 cancel {mode:?}: {lag:?}");
        assert!(cancelled, "{mode:?}: успел дойти до конца");
        assert!(lag <= Duration::from_millis(50), "{mode:?}: {lag:?}");
    }
}

/// S7-06: V2 (строка 50 МБ) — строка экрана ≤ 50 мкс без переноса и с переносом.
#[test]
#[ignore = "замер, release"]
fn v2_screen_row_under_50us() {
    let v2: Vec<u8> = b"{\"key\":\"value\",\"n\":12345,\"list\":[1,2,3]},"
        .iter()
        .copied()
        .cycle()
        .take(50 << 20)
        .collect();
    let rows_n = 60;
    for wrap in [None, Some(WrapKind::Chars), Some(WrapKind::Words)] {
        let s = LayoutSpec {
            width_cols: 160,
            wrap,
            ..LayoutSpec::default()
        };
        let mut worst = 0f64;
        for at in [0u64, 7 << 20, 25 << 20, (50 << 20) - 5000] {
            let mut best = f64::MAX;
            for _ in 0..10 {
                let t = Instant::now();
                let top = line_start_before(&v2[..], at, &s).unwrap();
                let rows = rows_down(&v2[..], (top, 0), rows_n, &s).unwrap();
                let mut out = RowText::default();
                for r in &rows {
                    render_row(&v2[..], r, 0, &s, &mut out).unwrap();
                    black_box(&out);
                }
                best = best.min(t.elapsed().as_secs_f64() * 1e6 / rows.len() as f64);
            }
            worst = worst.max(best);
        }
        println!("S7-06 V2 screen row {wrap:?}: {worst:.2} µs");
        assert!(worst <= 50.0, "{wrap:?}: {worst} µs");
    }
}

struct NoWake;

impl urfin_core::Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

/// S7-08: 100 ГБ sparse — первый экран ≤ 50 мс: `viewer.open` через core → документ готов,
/// страница верхней строки в кэше.
#[test]
#[ignore = "тяжёлый: 100 ГБ sparse, release"]
fn sparse_100gb_first_screen() {
    use urfin_core::{Boot, Command as Cmd, Core, Services};
    use urfin_view::{EditorCfg, ViewState, ViewerCfg};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v100.log");
    drop(File::create(&path).unwrap());
    let sparse = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&path)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(sparse, "fsutil sparse setflag не сработал");
    let len: u64 = 100 << 30;
    let mut f = File::options().write(true).open(&path).unwrap();
    f.set_len(len).unwrap();
    f.write_all(&log(64 << 10)).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(300))
        .unwrap();
    drop(f);
    let root = Location::local(dir.path());
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    urfin_view::register_with(&mut core, ViewerCfg::default(), EditorCfg::default());
    core.pump(Duration::from_millis(5));
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        core.dispatch(Cmd::new("viewer.open").arg("path", Location::local(&path)));
        loop {
            core.pump(Duration::from_millis(1));
            let v = core.state().ext.get::<ViewState>().unwrap();
            let d = v.viewer(v.active_viewer().unwrap()).unwrap();
            if d.is_ready() && d.pages.get(d.top..d.top + 4096).is_ok() {
                assert_eq!((d.len, d.kind), (len, Some(SourceKind::Mapped)));
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(5));
            std::hint::spin_loop();
        }
        best = best.min(t.elapsed());
        core.dispatch(Cmd::new("viewer.close"));
        core.pump(Duration::from_millis(5));
    }
    println!("S7-08 100 GB sparse first screen: best {best:?}");
    assert!(best <= Duration::from_millis(50), "{best:?}");
}

/// S7-09: скорость поиска (warm, в памяти) и отмена ≤ 50 мс.
#[test]
#[ignore = "тяжёлый: 512 МиБ в памяти, release"]
#[allow(clippy::too_many_lines, reason = "один замер: данные, случаи, отмена")]
fn search_speed_and_cancel() {
    use std::sync::atomic::AtomicU64;
    use urfin_view::Enc;
    use urfin_view::search::scan::{self, Scan, SliceHay};
    use urfin_view::search::{Pattern, compile, hex};

    let len = 512usize << 20;
    let mut ascii = log(len);
    let tail = b"needle 4242 the END\n";
    let n = ascii.len();
    ascii[n - tail.len()..].copy_from_slice(tail);
    let ru = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь.\n";
    let ru_text = ru.repeat(len / ru.len() / 2) + "ОШИБКА 4242\n";
    let (cp1251, ..) = encoding_rs::WINDOWS_1251.encode(&ru_text);
    let utf16: Vec<u8> = ru
        .repeat((128 << 20) / ru.len() / 2)
        .encode_utf16()
        .chain("needle".encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    let cases: Vec<(&str, Pattern, Enc, &[u8])> = vec![
        (
            "literal",
            Pattern::Text {
                s: "needle 4242".into(),
                case: true,
                word: false,
            },
            Enc::utf8(),
            &ascii,
        ),
        (
            "case-insensitive",
            Pattern::Text {
                s: "NEEDLE 4242".into(),
                case: false,
                word: false,
            },
            Enc::utf8(),
            &ascii,
        ),
        (
            "regex",
            Pattern::Regex {
                s: r"needle \d+".into(),
                case: true,
            },
            Enc::utf8(),
            &ascii,
        ),
        (
            "1251-regex",
            Pattern::Regex {
                s: r"(?i)ошибка \d+".into(),
                case: true,
            },
            Enc::new(encoding_rs::WINDOWS_1251),
            &cp1251,
        ),
        (
            "utf-16",
            Pattern::Text {
                s: "needle".into(),
                case: true,
                word: false,
            },
            Enc::new(encoding_rs::UTF_16LE),
            &utf16,
        ),
        (
            "hex",
            Pattern::Hex(hex::parse("6E 65 65 64 6C 65 20 34").unwrap()),
            Enc::utf8(),
            &ascii,
        ),
    ];
    for (name, pat, enc, hay) in cases {
        let m = compile(&pat, enc).unwrap();
        let mut best = f64::MAX;
        for _ in 0..3 {
            let cancel = CancelToken::new();
            let scanned = AtomicU64::new(0);
            let o = Scan {
                chunk: scan::CHUNK,
                cancel: &cancel,
                scanned: &scanned,
            };
            let t = Instant::now();
            let hit = scan::find_next(&mut SliceHay(hay), &m, 0, false, false, &o).unwrap();
            let el = t.elapsed().as_secs_f64();
            assert!(hit.is_some(), "{name}");
            best = best.min(el);
        }
        println!(
            "S7-09 search {name}: {:.2} GB/s",
            hay.len() as f64 / best / 1e9
        );
    }
    // Отмена: regex без совпадений по 512 МиБ.
    let m = compile(
        &Pattern::Regex {
            s: r"(?i)zz\w+qq".into(),
            case: true,
        },
        Enc::utf8(),
    )
    .unwrap();
    let hay: Arc<[u8]> = ascii.into();
    let cancel = CancelToken::new();
    let (c2, h2) = (cancel.clone(), hay.clone());
    let h = std::thread::spawn(move || {
        let scanned = AtomicU64::new(0);
        let o = Scan {
            chunk: scan::CHUNK,
            cancel: &c2,
            scanned: &scanned,
        };
        let r = scan::find_next(&mut SliceHay(&h2), &m, 0, false, true, &o);
        (r.is_err(), Instant::now())
    });
    std::thread::sleep(Duration::from_millis(30));
    let at = Instant::now();
    cancel.cancel();
    let (cancelled, done) = h.join().unwrap();
    let lag = done.saturating_duration_since(at);
    println!("S7-09 search cancel: {lag:?}");
    assert!(cancelled && lag <= Duration::from_millis(50), "{lag:?}");
}

/// S7-29: документ 10 ГБ на движке готов ≤ 50 мс (тёплый): выборка, детект, один кусок
/// `Original` под запретом записи; индекс строк — фоном, не здесь.
#[test]
#[ignore = "тяжёлый: разреженный файл 10 ГБ, release"]
fn engine_opens_10gb_within_50ms() {
    use urfin_view::config::EolCfg;
    use urfin_view::edit::Guard;
    use urfin_view::edit::load::LoadOpts;
    use urfin_view::edit::open::open_path;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.log");
    std::fs::write(&path, log(1 << 20)).unwrap();
    // Разреженный: 10 ГБ без места на диске.
    let sparse = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&path)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(sparse, "fsutil sparse setflag не сработал");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(10 << 30)
        .unwrap();
    let o = LoadOpts {
        enc: None,
        tld: None,
        eol: EolCfg::File,
        name: "v1.log".into(),
        new: None,
    };
    // Прогрев кэша ОС, затем замер.
    drop(open_path(&path, 1, &o, &CancelToken::new()).unwrap());
    let t = Instant::now();
    let d = open_path(&path, 2, &o, &CancelToken::new()).unwrap();
    let el = t.elapsed();
    println!("S7-29: 10 ГБ готов за {el:?}");
    assert_eq!(d.text.len(), 10 << 30);
    assert_eq!(d.guard, Guard::Locked);
    assert!(el <= Duration::from_millis(50), "{el:?}");
}

/// Файл из строк журнала `len` байт (пишется блоками по 64 МБ, без всего текста в памяти).
fn write_log(path: &std::path::Path, len: u64) {
    let block = log(64 << 20);
    let mut f = std::io::BufWriter::new(File::create(path).unwrap());
    let mut left = len;
    while left > 0 {
        let k = left.min(block.len() as u64) as usize;
        f.write_all(&block[..k]).unwrap();
        left -= k as u64;
    }
    f.flush().unwrap();
}

/// S7-31: сохранение 1 ГБ с одной правкой потоком ≤ 3 с (`NVMe` стенда); отмена ≤ 100 мс.
#[test]
#[ignore = "тяжёлый: файл 1 ГБ, release"]
fn engine_save_1gb_one_edit() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use urfin_core::Cancelled;
    use urfin_view::config::EolCfg;
    use urfin_view::edit::Guard;
    use urfin_view::edit::load::LoadOpts;
    use urfin_view::edit::open::open_path;
    use urfin_view::edit::save::{
        Body, Outcome, ReadOnly, SaveReq, SnapBody, Unmappable, save_full,
    };
    use urfin_view::enc::Enc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g1.log");
    let len: u64 = 1 << 30;
    write_log(&path, len);
    let o = LoadOpts {
        enc: None,
        tld: None,
        eol: EolCfg::File,
        name: "g1.log".into(),
        new: None,
    };
    let req = |d: &urfin_view::edit::open::Opened| SaveReq {
        text: Body::Snap(SnapBody {
            snap: d.text.snap(),
            from: Enc {
                bom_len: 0,
                ..d.enc
            },
            eol: d.eol,
            space: true,
            reopen: true,
        }),
        enc: d.enc.enc,
        bom: d.bom,
        path: path.clone(),
        check: Some(d.stamp),
        unmappable: Unmappable::Ask,
        readonly: ReadOnly::Ask,
        fault: None,
    };
    // Прогрев кэша ОС чтением файла.
    drop(std::fs::read(&path).unwrap());
    let mut d = open_path(&path, 1, &o, &CancelToken::new()).unwrap();
    assert_eq!(d.guard, Guard::Locked);
    d.text.insert(len / 2, b"EDIT").unwrap();
    let r = req(&d);
    let t = Instant::now();
    let s = save_full(&r, &|| Ok(()), &mut |_| {}).unwrap();
    let el = t.elapsed();
    println!("S7-31: сохранение 1 ГБ с одной правкой за {el:?}");
    assert!(
        matches!(s.outcome, Outcome::Saved { .. }),
        "{:?}",
        s.outcome
    );
    let re = s.reopened.expect("открыт заново");
    assert_eq!(re.guard, Guard::Locked);
    assert_eq!(re.src.len(), len + 4);
    drop((d, re));
    // Отмена посреди записи.
    let mut d = open_path(&path, 2, &o, &CancelToken::new()).unwrap();
    d.text.insert(0, b"X").unwrap();
    let r = req(&d);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let h = std::thread::spawn(move || {
        let res = save_full(
            &r,
            &|| {
                if flag.load(Ordering::Relaxed) {
                    Err(Cancelled)
                } else {
                    Ok(())
                }
            },
            &mut |_| {},
        );
        (res.is_err(), Instant::now())
    });
    std::thread::sleep(Duration::from_millis(300));
    let at = Instant::now();
    stop.store(true, Ordering::Relaxed);
    let (cancelled, done) = h.join().unwrap();
    let lag = done.saturating_duration_since(at);
    println!("S7-31: отмена сохранения за {lag:?}");
    assert!(cancelled && lag <= Duration::from_millis(100), "{lag:?}");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        len + 4,
        "исходник цел"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "временный убран"
    );
    // Гейт бюджета — на NVMe стенда со свободным местом (S7-36, `URFIN_PERF_GATE=1`);
    // здесь — цифры рядом с копией файла той же системой.
    let copy = dir.path().join("copy.log");
    let t = Instant::now();
    std::fs::copy(&path, &copy).unwrap();
    File::options()
        .write(true)
        .open(&copy)
        .unwrap()
        .sync_all()
        .unwrap();
    let base = t.elapsed();
    std::fs::remove_file(&copy).unwrap();
    println!("S7-31: копия того же файла (copy + sync) за {base:?}");
    if std::env::var_os("URFIN_PERF_GATE").is_some() {
        assert!(el <= Duration::from_secs(3), "{el:?}");
    }
}

/// S7-30: «Заменить все» — 100 тыс. совпадений в 50 МБ ≤ 1 с (поиск + новые куски), одна
/// отмена возвращает исходник; 1 млн совпадений в 1 ГБ — результат потоком в `Rewrite`,
/// отмена задачи ≤ 100 мс.
#[test]
#[ignore = "тяжёлый: 50 МБ в памяти и файл 1 ГБ, release"]
fn engine_replace_all_speed() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use urfin_view::edit::lines::index_now;
    use urfin_view::edit::piece::{AddBuf, TextDoc};
    use urfin_view::edit::replace::{Repl, Replaced, TempAt, apply, replace_all};
    use urfin_view::enc::Enc;
    use urfin_view::search::scan::{CHUNK, Scan};
    use urfin_view::search::{Pattern, compile};
    let dir = tempfile::tempdir().unwrap();
    let temp = TempAt {
        dir: dir.path().to_path_buf(),
        name: "rw".into(),
    };
    // 50 МБ: строка 500 байт с одним «needle».
    let mut line = vec![b'a'; 499];
    line[200..206].copy_from_slice(b"needle");
    line.push(b'\n');
    let body: Vec<u8> = line.repeat(100_000);
    let enc = Enc::utf8();
    let sep = Sep::of(&enc, urfin_view::detect::Eol::Lf);
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(body.clone()));
    let ix = index_now(&*src, sep).unwrap();
    let mut doc = TextDoc::new(src, 0, sep, AddBuf::new(7), ix);
    let cancel = CancelToken::new();
    let scanned = AtomicU64::new(0);
    let o = Scan {
        chunk: CHUNK,
        cancel: &cancel,
        scanned: &scanned,
    };
    for (pat, with) in [
        (
            Pattern::Text {
                s: "needle".into(),
                case: true,
                word: false,
            },
            "pin",
        ),
        (
            Pattern::Regex {
                s: "ne(e)dle".into(),
                case: true,
            },
            "<$1>",
        ),
    ] {
        let m = compile(&pat, enc).unwrap();
        let repl = Repl::new(&pat, with, enc).unwrap();
        let before = doc.tree().clone();
        let t = Instant::now();
        let res = replace_all(&doc.snap(), &m, &repl, &o, 0, &temp).unwrap();
        let n = res.count();
        let (tree, _) = apply(&mut doc, res, 0);
        let el = t.elapsed();
        println!("S7-30: «Заменить все» {n} в 50 МБ ({pat:?}) за {el:?}");
        assert_eq!(n, 100_000);
        doc.set_tree(tree);
        // «pin» и «<e>» — по 3 байта вместо 6.
        assert_eq!(doc.len(), (body.len() - n * 3) as u64);
        doc.set_tree(before);
        let mut b = vec![0u8; body.len()];
        doc.snap().read_at(0, &mut b).unwrap();
        assert!(b == body, "одна отмена — исходник");
        if std::env::var_os("URFIN_PERF_GATE").is_some() || with == "pin" {
            assert!(el <= Duration::from_secs(1), "{el:?}");
        }
    }
    // 1 ГБ, 1 млн совпадений: строка 1 КБ с «needle».
    let mut line = vec![b'b'; 1023];
    line[500..506].copy_from_slice(b"needle");
    line.push(b'\n');
    let block = line.repeat(64 << 10);
    let path = dir.path().join("r1.log");
    {
        let mut f = std::io::BufWriter::new(File::create(&path).unwrap());
        for _ in 0..16 {
            f.write_all(&block).unwrap();
        }
        f.flush().unwrap();
    }
    let len = 16 * block.len() as u64;
    let f = File::open(&path).unwrap();
    let src: Arc<dyn ByteSource> =
        Arc::new(urfin_view::source::FileSource::new(f, path.clone(), false));
    let snap = TextDoc::new(
        src,
        0,
        sep,
        AddBuf::new(8),
        urfin_view::index::LineIndex::default(),
    )
    .snap();
    let pat = Pattern::Text {
        s: "needle".into(),
        case: true,
        word: false,
    };
    let m = compile(&pat, enc).unwrap();
    let repl = Repl::new(&pat, "pin", enc).unwrap();
    let t = Instant::now();
    let res = replace_all(&snap, &m, &repl, &o, 0, &temp).unwrap();
    let el = t.elapsed();
    let Replaced::Big { rw, count } = res else {
        panic!("больше 64 тыс. — поток в Rewrite");
    };
    println!("S7-30: «Заменить все» {count} в 1 ГБ (поток в Rewrite) за {el:?}");
    assert_eq!(count, 1 << 20);
    assert_eq!(rw.src.len(), len - 3 * (1 << 20));
    drop(rw);
    // Отмена задачи.
    let cancel2 = CancelToken::new();
    let c2 = cancel2.clone();
    let h = std::thread::spawn(move || {
        let scanned = AtomicU64::new(0);
        let o = Scan {
            chunk: CHUNK,
            cancel: &c2,
            scanned: &scanned,
        };
        let r = replace_all(&snap, &m, &repl, &o, 0, &temp);
        (r.is_err(), Instant::now(), scanned.load(Ordering::Relaxed))
    });
    std::thread::sleep(Duration::from_millis(300));
    let at = Instant::now();
    cancel2.cancel();
    let (cancelled, done, _) = h.join().unwrap();
    let lag = done.saturating_duration_since(at);
    println!("S7-30: отмена «Заменить все» за {lag:?}");
    assert!(cancelled && lag <= Duration::from_millis(100), "{lag:?}");
}

/// S7-42: блок на 1 млн строк в файле 10 ГБ — фоновой задачей: память задачи (окно
/// чтения и буфер результата) вместе с кэшем страниц документа ≤ 64 мебибайт; отмена ≤ 100 мс
/// и возвращает текст как был; итог — одна группа отмены.
#[test]
#[ignore = "тяжёлый: разреженный файл 10 ГБ, release"]
fn engine_block_1m_lines_in_10gb() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use urfin_view::config::EolCfg;
    use urfin_view::edit::Engine;
    use urfin_view::edit::bigblock::{BlockError, BlockJob, BlockRes, run};
    use urfin_view::edit::input::Area;
    use urfin_view::edit::load::LoadOpts;
    use urfin_view::edit::open::open_path;
    use urfin_view::edit::piece::DOC_CACHE;
    use urfin_view::edit::replace::TempAt;
    use urfin_view::enc::Enc;
    const ROWS: usize = 1_000_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.log");
    let real: u64 = 128 << 20;
    write_log(&path, real);
    let sparse = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&path)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(sparse, "fsutil sparse setflag не сработал");
    let len: u64 = 10 << 30;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len)
        .unwrap();
    // Начала строк — по тем же байтам, что записал `write_log`.
    let block = log(64 << 20);
    let mut starts = vec![0u64];
    for base in [0u64, block.len() as u64] {
        starts.extend(
            block
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| base + i as u64 + 1),
        );
    }
    assert!(starts.len() > ROWS + 2000, "{}", starts.len());
    let o = LoadOpts {
        enc: None,
        tld: None,
        eol: EolCfg::File,
        name: "b.log".into(),
        new: None,
    };
    let d = open_path(&path, 1, &o, &CancelToken::new()).unwrap();
    assert_eq!(d.text.len(), len);
    let bom = u64::from(d.enc.bom_len);
    let enc = Enc {
        bom_len: 0,
        ..d.enc
    };
    let mut e = Engine::new(d.text, enc, d.eol, d.guard, bom..len);
    (e.cols, e.rows) = (200, 50);
    let (top, bottom) = (starts[1000], starts[1000 + ROWS - 1]);
    let job = BlockJob::Replace {
        area: Area {
            top,
            bottom,
            c0: 2,
            c1: 10,
        },
        caret_line: top,
        bytes: Vec::new(),
        copy: None,
    };
    let temp = TempAt {
        dir: dir.path().to_path_buf(),
        name: "block".into(),
    };
    let spec = e.seg_spec();
    let head = |e: &Engine| {
        let mut b = vec![0u8; 64];
        e.text.snap().read_at(top, &mut b).unwrap();
        b
    };
    let before = head(&e);
    // Отмена посреди задачи.
    let snap = e.text.snap();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let (flag, j, t2, s2) = (stop.clone(), job.clone(), temp.clone(), spec);
    let h = std::thread::spawn(move || {
        let p = AtomicU64::new(0);
        let r = run(&snap, &j, &s2, &t2, &p, &|| flag.load(Ordering::Relaxed));
        (matches!(r, Err(BlockError::Cancelled)), Instant::now())
    });
    std::thread::sleep(Duration::from_millis(300));
    let at = Instant::now();
    stop.store(true, Ordering::Relaxed);
    let (cancelled, done) = h.join().unwrap();
    let lag = done.saturating_duration_since(at);
    println!("S7-42: отмена блочной задачи за {lag:?}");
    assert!(cancelled && lag <= Duration::from_millis(100), "{lag:?}");
    assert_eq!(e.text.edits(), 0, "отмена — документ не тронут");
    assert_eq!(head(&e), before);
    // Целиком.
    let p = AtomicU64::new(0);
    let t = Instant::now();
    let res = run(&e.text.snap(), &job, &spec, &temp, &p, &|| false).unwrap();
    let el = t.elapsed();
    let BlockRes::Done(dn) = res else {
        panic!("не сделано: {res:?}")
    };
    println!(
        "S7-42: блок 1 млн строк в 10 ГБ за {el:?}, память задачи {} МиБ",
        dn.peak >> 20
    );
    assert!(
        dn.peak + DOC_CACHE <= 64 << 20,
        "память задачи {} + кэш {DOC_CACHE}",
        dn.peak
    );
    e.apply_block(dn, Instant::now());
    assert_eq!(e.text.len(), len - 8 * ROWS as u64, "по 8 байт из строки");
    let mut want = before.clone();
    want.drain(2..10);
    assert_eq!(&head(&e)[..32], &want[..32]);
    assert!(e.undo(false).is_some());
    assert_eq!(e.text.len(), len, "одна группа отмены");
    assert_eq!(head(&e), before);
}
