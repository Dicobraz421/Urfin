//! Criterion: движок редактора (S7-36, бюджеты ADR-0023) — правка и номера строк при
//! 1 млн кусков, поиск по документу с правками, писатель 1 ГБ с одной правкой, «Заменить
//! все» 100 тыс. совпадений в 50 МБ. Данные — во временном каталоге.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use criterion::{BatchSize, Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use urfin_core::CancelToken;
use urfin_view::ByteSource;
use urfin_view::config::EolCfg;
use urfin_view::detect::Eol;
use urfin_view::edit::lines::index_now;
use urfin_view::edit::load::LoadOpts;
use urfin_view::edit::open::{Opened, open_path};
use urfin_view::edit::piece::{AddBuf, TextDoc, TextHay};
use urfin_view::edit::replace::{Repl, TempAt, apply, replace_all};
use urfin_view::edit::save::{Body, ReadOnly, SaveReq, SnapBody, Unmappable, save_full};
use urfin_view::enc::Enc;
use urfin_view::index::{LineIndex, Sep};
use urfin_view::search::scan::{self, CHUNK, Scan};
use urfin_view::search::{Pattern, compile};
use urfin_view::source::{FileSource, OwnedSource};

const LINE: &[u8] = b"0123456789 abcdefghij klmnopqrst uvwxyz ABCDEFGHIJ KLMNOPQRST\n";

fn rng(mut seed: u64) -> impl FnMut() -> u64 {
    move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    }
}

fn doc_of(text: Vec<u8>) -> TextDoc {
    let sep = Sep::Byte(b'\n');
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(text));
    let ix = index_now(&*src, sep).unwrap();
    TextDoc::new(src, 0, sep, AddBuf::new(1), ix)
}

/// Документ 64 мебибайта, порезанный правками на `pieces` кусков.
fn pieces(n: u64) -> TextDoc {
    let mut doc = doc_of(LINE.iter().copied().cycle().take(64 << 20).collect());
    let mut rnd = rng(1);
    while doc.tree().sums().pieces < n {
        let at = rnd() % (doc.len() + 1);
        doc.insert(at, b"ab").unwrap();
    }
    doc
}

fn edit(c: &mut Criterion) {
    let mut doc = pieces(1_000_000);
    let mut rnd = rng(7);
    let mut g = c.benchmark_group("edit_1m");
    g.bench_function("insert", |b| {
        b.iter(|| {
            let at = rnd() % (doc.len() + 1);
            black_box(doc.insert(at, b"xy").unwrap());
        });
    });
    g.bench_function("remove", |b| {
        b.iter(|| {
            let at = rnd() % (doc.len() - 16);
            black_box(doc.remove(at..at + 1 + rnd() % 16));
        });
    });
    let total = doc.tree().sums().lf.max(1);
    g.bench_function("line_of", |b| {
        b.iter(|| black_box(doc.line_of(rnd() % doc.len())));
    });
    g.bench_function("offset_of_line", |b| {
        b.iter(|| black_box(doc.offset_of_line(rnd() % total)));
    });
    g.finish();
}

fn write_log(path: &Path, len: u64) {
    let block: Vec<u8> = LINE.iter().copied().cycle().take(1 << 20).collect();
    let mut f = std::io::BufWriter::new(File::create(path).unwrap());
    let mut done = 0;
    while done < len {
        f.write_all(&block).unwrap();
        done += block.len() as u64;
    }
    f.flush().unwrap();
}

/// Поиск по файлу 1 ГБ с тысячей правок: кэш ОС тёплый.
fn hay(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hay.log");
    let len = 1u64 << 30;
    write_log(&path, len);
    drop(std::fs::read(&path).unwrap());
    let sep = Sep::Byte(b'\n');
    let src: Arc<dyn ByteSource> = Arc::new(FileSource::new(
        File::open(&path).unwrap(),
        path.clone(),
        false,
    ));
    let mut doc = TextDoc::new(src, 0, sep, AddBuf::new(2), LineIndex::default());
    let mut rnd = rng(3);
    for _ in 0..1000 {
        let at = rnd() % doc.len();
        doc.insert(at, b"edit").unwrap();
    }
    let snap = doc.snap();
    let enc = Enc::utf8();
    let cases = [
        (
            "literal",
            Pattern::Text {
                s: "needle 4242".into(),
                case: true,
                word: false,
            },
        ),
        (
            "regex",
            Pattern::Regex {
                s: r"needle \d+".into(),
                case: true,
            },
        ),
    ];
    let mut g = c.benchmark_group("text_hay_1g");
    g.sample_size(10).sampling_mode(SamplingMode::Flat);
    g.throughput(Throughput::Bytes(snap.len()));
    for (name, pat) in cases {
        let m = compile(&pat, enc).unwrap();
        g.bench_function(name, |b| {
            b.iter(|| {
                let cancel = CancelToken::new();
                let scanned = AtomicU64::new(0);
                let o = Scan {
                    chunk: CHUNK,
                    cancel: &cancel,
                    scanned: &scanned,
                };
                let mut hay = TextHay::new(&snap);
                black_box(scan::forward(&mut hay, &m, 0, snap.len(), &o).unwrap())
            });
        });
    }
    g.finish();
}

/// Писатель: 1 ГБ с одной правкой, потоком поверх файла.
fn writer(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save.log");
    write_log(&path, 1 << 30);
    drop(std::fs::read(&path).unwrap());
    let o = LoadOpts {
        enc: None,
        tld: None,
        eol: EolCfg::File,
        name: "save.log".into(),
        new: None,
    };
    let req = |d: &Opened| SaveReq {
        text: Body::Snap(SnapBody {
            snap: d.text.snap(),
            from: Enc {
                bom_len: 0,
                ..d.enc
            },
            eol: d.eol,
            space: true,
            reopen: false,
        }),
        enc: d.enc.enc,
        bom: d.bom,
        path: path.clone(),
        check: Some(d.stamp),
        unmappable: Unmappable::Ask,
        readonly: ReadOnly::Ask,
        fault: None,
    };
    let mut g = c.benchmark_group("writer");
    g.sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .measurement_time(Duration::from_secs(30));
    g.throughput(Throughput::Bytes(1 << 30));
    g.bench_function("save_1g_one_edit", |b| {
        b.iter_batched(
            || {
                let mut d = open_path(&path, 1, &o, &CancelToken::new()).unwrap();
                let mid = d.text.len() / 2;
                d.text.insert(mid, b"EDIT").unwrap();
                req(&d)
            },
            |r| black_box(save_full(&r, &|| Ok(()), &mut |_| {}).unwrap()),
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

/// «Заменить все»: 100 тыс. совпадений в 50 МБ, поиск и новые куски.
fn replace(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let temp = TempAt {
        dir: dir.path().to_path_buf(),
        name: "rw".into(),
    };
    let mut line = vec![b'a'; 499];
    line[200..206].copy_from_slice(b"needle");
    line.push(b'\n');
    let enc = Enc::utf8();
    let sep = Sep::of(&enc, Eol::Lf);
    let src: Arc<dyn ByteSource> = Arc::new(OwnedSource::new(line.repeat(100_000)));
    let ix = index_now(&*src, sep).unwrap();
    let mut doc = TextDoc::new(src, 0, sep, AddBuf::new(3), ix);
    let pat = Pattern::Text {
        s: "needle".into(),
        case: true,
        word: false,
    };
    let m = compile(&pat, enc).unwrap();
    let repl = Repl::new(&pat, "pin", enc).unwrap();
    let mut g = c.benchmark_group("replace_all");
    g.sample_size(10).sampling_mode(SamplingMode::Flat);
    g.bench_function("100k_in_50mb", |b| {
        b.iter(|| {
            let cancel = CancelToken::new();
            let scanned = AtomicU64::new(0);
            let o = Scan {
                chunk: CHUNK,
                cancel: &cancel,
                scanned: &scanned,
            };
            let before = doc.tree().clone();
            let res = replace_all(&doc.snap(), &m, &repl, &o, 0, &temp).unwrap();
            let (tree, _) = apply(&mut doc, res, 0).unwrap();
            black_box(&tree);
            doc.set_tree(before);
        });
    });
    g.finish();
}

criterion_group!(benches, edit, hay, writer, replace);
criterion_main!(benches);
