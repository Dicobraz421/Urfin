//! Criterion: поиск по 64 МБ в памяти (S7-09) — литерал, без регистра, regex, 1251, UTF-16, hex.

use std::hint::black_box;
use std::sync::atomic::AtomicU64;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use urfin_core::CancelToken;
use urfin_view::Enc;
use urfin_view::search::scan::{self, Scan, SliceHay};
use urfin_view::search::{Pattern, compile, hex};

fn text(len: usize) -> Vec<u8> {
    let line = b"2026-10-30 12:00:00 INFO request served in 12 ms path=/api/v1/items\n";
    let mut v: Vec<u8> = line.iter().copied().cycle().take(len).collect();
    let tail = b"needle 4242\n";
    let n = v.len();
    v[n - tail.len()..].copy_from_slice(tail);
    v
}

fn bench(c: &mut Criterion) {
    let ascii = text(64 << 20);
    let ru = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь.\n";
    let ru = ru.repeat((32 << 20) / ru.len()) + "ОШИБКА 42\n";
    let (cp1251, ..) = encoding_rs::WINDOWS_1251.encode(&ru);
    let utf16: Vec<u8> = ru.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let lit = |s: &str, case| Pattern::Text {
        s: s.into(),
        case,
        word: false,
    };
    let re = |s: &str| Pattern::Regex {
        s: s.into(),
        case: true,
    };
    let cases: Vec<(&str, Pattern, Enc, &[u8])> = vec![
        ("literal", lit("needle 4242", true), Enc::utf8(), &ascii),
        ("nocase", lit("NEEDLE 4242", false), Enc::utf8(), &ascii),
        ("regex", re(r"needle \d+"), Enc::utf8(), &ascii),
        (
            "cp1251_regex",
            re(r"(?i)ошибка \d+"),
            Enc::new(encoding_rs::WINDOWS_1251),
            &cp1251,
        ),
        (
            "utf16",
            lit("ОШИБКА", true),
            Enc::new(encoding_rs::UTF_16LE),
            &utf16,
        ),
        (
            "hex",
            Pattern::Hex(hex::parse("6E 65 65 64 6C 65").unwrap()),
            Enc::utf8(),
            &ascii,
        ),
    ];
    let mut g = c.benchmark_group("search");
    g.sample_size(10);
    for (name, pat, enc, hay) in cases {
        let m = compile(&pat, enc).unwrap();
        g.throughput(Throughput::Bytes(hay.len() as u64));
        g.bench_function(name, |b| {
            b.iter(|| {
                let cancel = CancelToken::new();
                let scanned = AtomicU64::new(0);
                let o = Scan {
                    chunk: scan::CHUNK,
                    cancel: &cancel,
                    scanned: &scanned,
                };
                black_box(scan::find_next(&mut SliceHay(hay), &m, 0, false, false, &o).unwrap())
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
