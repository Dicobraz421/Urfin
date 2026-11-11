//! Датасеты Urfin (H-01, S7-00a, S9-02): `cargo run -p urfin-testdata --example testdata --release -- <команда>`.
//!
//! ```text
//! clean                                  удалить все (%LOCALAPPDATA%\Pets\testdata\urfin)
//! path                                   показать корень
//! v1|v2|v3|v4 [--seed N] [--size 10g]    файл датасета (готовый — как есть); печатает путь
//! all [--seed N]                         V1–V4 размеров S7 §9 (≈ 11 ГБ на диске)
//! v5                                     каталог корпуса кодировок (фикстура в репо)
//! v6 [--out файл] [--rate 10m] [--seconds N] [--limit 1g] [--seed N]
//!                                        растущий лог: --rate байт/с до Ctrl+C, N секунд или лимита
//! b1…b6|l1…l4|m1|d1 --root <каталог> [--seed N] [--scale X] [--verify] [--force]
//!                                        набор S9 §1 в <каталог>\<ID> с манифестом
//! all --root <каталог> [...]             все наборы B1–D1 (без --root — V1–V4)
//! u1 --root <каталог> [--force]          U1 в <каталог>\U1, печатает skipped
//! ```
//! V1 — лог 10 ГБ (≈ 120 млн строк), V2 — JSON в одну строку 50 МБ, V3 — разреженный
//! бинарник 4,5 ГБ (< 1 МБ на диске), V4 — UTF-16LE 1 ГБ. Замер:
//! `urfin.exe --bench-editor <путь>` / `--bench-viewer <путь>` (docs/perf/s7.md).
//!
//! Наборы S9 (S9-02): `--root` обязателен и должен лежать на целевом томе; совпадающий
//! манифест `.urfin-dataset.json` — no-op, `--force` пересоздаёт, `--verify` перепроверяет
//! дерево по манифесту, `--scale` умножает число файлов или объём (B6 и M1 — 0,25 по
//! умолчанию). Место проверяется до записи.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use urfin_testdata::{
    Built, FileSet, FileSpec, Set, SetSpec, dataset_file, datasets_root, parse_size,
};

const APP: &str = "urfin";

#[derive(Debug, Default)]
struct SetArgs {
    root: Option<PathBuf>,
    scale: Option<f64>,
    verify: bool,
    force: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let (mut seed, mut size, mut out, mut rate, mut seconds, mut limit) =
        (1u64, None, None, 10u64 << 20, None, None);
    let mut sa = SetArgs::default();
    while let Some(a) = args.next() {
        let mut val = || {
            args.next()
                .unwrap_or_else(|| usage(&format!("{a}: no value")))
        };
        let bytes = |v: String| parse_size(&v).unwrap_or_else(|| usage(&format!("size {v}")));
        match a.as_str() {
            "--seed" => seed = val().parse().unwrap_or_else(|_| usage("--seed N")),
            "--size" => size = Some(bytes(val())),
            "--out" => out = Some(PathBuf::from(val())),
            "--rate" => rate = bytes(val()),
            "--limit" => limit = Some(bytes(val())),
            "--seconds" => {
                seconds = Some(Duration::from_secs(
                    val().parse().unwrap_or_else(|_| usage("--seconds N")),
                ));
            }
            "--root" => sa.root = Some(PathBuf::from(val())),
            "--scale" => sa.scale = Some(val().parse().unwrap_or_else(|_| usage("--scale X"))),
            "--verify" => sa.verify = true,
            "--force" => sa.force = true,
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let make = |set: FileSet, size: Option<u64>| {
        let spec = FileSpec {
            size: size.unwrap_or(set.default_size()),
            seed,
            ..FileSpec::new(set)
        };
        let t = Instant::now();
        let p = dataset_file(APP, &spec).expect("dataset");
        println!("{} ({:.1} s)", p.display(), t.elapsed().as_secs_f64());
    };
    match cmd.as_str() {
        "clean" => {
            let bytes = urfin_testdata::clean(APP).expect("clean");
            println!(
                "{}: removed {} MB",
                datasets_root(APP).display(),
                bytes / 1_000_000
            );
        }
        "path" => println!("{}", datasets_root(APP).display()),
        "all" if sa.root.is_some() => {
            for set in Set::ALL {
                run_set(set, seed, &sa);
            }
        }
        "all" => FileSet::ALL.into_iter().for_each(|s| make(s, None)),
        "v5" => println!("{}", urfin_testdata::v5_dir().display()),
        "v6" => {
            let out = out.unwrap_or_else(|| datasets_root(APP).join(format!("v6-s{seed}.log")));
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir).expect("dir");
            }
            println!("{} <- {} KiB/s", out.display(), rate >> 10);
            let n = urfin_testdata::grow_log(&out, seed, rate, limit, seconds).expect("v6");
            println!("{n} bytes");
        }
        "u1" => run_u1(&sa),
        other => match (FileSet::parse(other), Set::parse(other)) {
            (Some(set), _) => make(set, size),
            (_, Some(set)) => run_set(set, seed, &sa),
            _ => usage("command"),
        },
    }
}

fn root(sa: &SetArgs) -> &Path {
    sa.root
        .as_deref()
        .unwrap_or_else(|| usage("--root <каталог> на целевом томе"))
}

/// Набор S9: собрать (или убедиться, что готов) и при `--verify` перепроверить.
fn run_set(set: Set, seed: u64, sa: &SetArgs) {
    let root = root(sa);
    let spec = SetSpec {
        seed,
        scale: sa.scale.unwrap_or(set.default_scale()),
        ..SetSpec::new(set)
    };
    let t = Instant::now();
    let (dir, built) = urfin_testdata::build(root, &spec, sa.force).unwrap_or_else(|e| {
        eprintln!("{}: {e}", set.tag());
        std::process::exit(1);
    });
    match built {
        Built::Ready => println!(
            "{} — готов ({:.2} s)",
            dir.display(),
            t.elapsed().as_secs_f64()
        ),
        Built::Created(r) => println!(
            "{} — создан за {:.1} s: файлов {}, каталогов {}, {} МиБ, ссылок {} (не создано {})",
            dir.display(),
            t.elapsed().as_secs_f64(),
            r.files,
            r.dirs,
            r.bytes >> 20,
            r.links,
            r.links_skipped
        ),
    }
    if sa.verify {
        let t = Instant::now();
        let problems = urfin_testdata::verify(root, &spec).expect("verify");
        for p in &problems {
            println!("  {p}");
        }
        let verdict = if problems.is_empty() { "ok" } else { "FAIL" };
        println!(
            "{}: verify {verdict} ({:.1} s)",
            set.tag(),
            t.elapsed().as_secs_f64()
        );
        if !problems.is_empty() {
            std::process::exit(1);
        }
    }
}

/// U1 в `<root>\U1`: существующий — только с `--force`.
fn run_u1(sa: &SetArgs) {
    let dir = root(sa).join("U1");
    if sa.force && dir.exists() {
        let v = urfin_testdata::verbatim(&dir).expect("path");
        std::fs::remove_dir_all(v).expect("remove U1");
    }
    let r = urfin_testdata::gen_u1(&dir).unwrap_or_else(|e| {
        eprintln!("U1: {e}");
        std::process::exit(1);
    });
    println!(
        "{} ({:?}): создано {}",
        r.root.display(),
        r.fs,
        r.created.len()
    );
    for (id, why) in &r.skipped {
        println!("  skipped {id}: {why}");
    }
}

fn usage(what: &str) -> ! {
    eprintln!(
        "{what}\nusage: testdata clean | path | v1|v2|v3|v4 [--seed N] [--size 10g] | all | v5 | v6 [--out F] [--rate 10m] [--seconds N] [--limit 1g]\n       testdata b1…b6|l1…l4|m1|d1|all --root DIR [--seed N] [--scale X] [--verify] [--force] | u1 --root DIR [--force]"
    );
    std::process::exit(2);
}
