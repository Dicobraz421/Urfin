//! `Highlighter::classify` (S2-05): < 50 нс на элемент — встроенные группы и одно
//! пользовательское правило с маской.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use urfin_core::{Attrs, Entry, EntryKind};
use urfin_ui::config::ColorRule;
use urfin_ui::highlight::Highlighter;

fn entries() -> Vec<Entry> {
    (0..1024)
        .map(|i| {
            let ext = [
                "txt", "RS", "png", "zip", "exe", "pdf", "qwerty", "", "tar.gz", "bak",
            ][i % 10];
            let name = if ext.is_empty() {
                format!("file{i}")
            } else {
                format!("file{i}.{ext}")
            };
            let attrs = if i % 17 == 0 { Attrs::HIDDEN } else { 0 };
            Entry {
                ctime: i64::MIN,
                atime: i64::MIN,
                name: name.into(),
                size: 0,
                mtime: 0,
                attrs: Attrs(attrs),
                kind: if i % 13 == 0 {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
            }
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let list = entries();
    let builtin = Highlighter::default();
    let (user, _) = Highlighter::build(&[ColorRule {
        mask: Some("*.rs".into()),
        fg: "@accent.text".into(),
        ..ColorRule::default()
    }]);
    let mut g = c.benchmark_group("highlight");
    g.throughput(Throughput::Elements(list.len() as u64));
    for (name, hl) in [
        ("classify_builtin", &builtin),
        ("classify_user_rule", &user),
    ] {
        g.bench_function(name, |b| {
            b.iter(|| {
                for e in &list {
                    black_box(hl.classify(black_box(e)));
                }
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
