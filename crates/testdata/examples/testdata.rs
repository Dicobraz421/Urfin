//! Датасеты Urfin (H-01, S7-00a): `cargo run -p urfin-testdata --example testdata --release -- <команда>`.
//!
//! ```text
//! clean                                  удалить все (%LOCALAPPDATA%\Pets\testdata\urfin)
//! path                                   показать корень
//! v1|v2|v3|v4 [--seed N] [--size 10g]    файл датасета (готовый — как есть); печатает путь
//! all [--seed N]                         V1–V4 размеров S7 §9 (≈ 11 ГБ на диске)
//! v5                                     каталог корпуса кодировок (фикстура в репо)
//! v6 [--out файл] [--rate 10m] [--seconds N] [--limit 1g] [--seed N]
//!                                        растущий лог: --rate байт/с до Ctrl+C, N секунд или лимита
//! ```
//! V1 — лог 10 ГБ (≈ 120 млн строк), V2 — JSON в одну строку 50 МБ, V3 — разреженный
//! бинарник 4,5 ГБ (< 1 МБ на диске), V4 — UTF-16LE 1 ГБ. Замер:
//! `urfin.exe --bench-editor <путь>` / `--bench-viewer <путь>` (docs/perf/s7.md).

use std::time::{Duration, Instant};

use urfin_testdata::{FileSet, FileSpec, dataset_file, datasets_root, parse_size};

const APP: &str = "urfin";

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    let (mut seed, mut size, mut out, mut rate, mut seconds, mut limit) =
        (1u64, None, None, 10u64 << 20, None, None);
    while let Some(a) = args.next() {
        let mut val = || {
            args.next()
                .unwrap_or_else(|| usage(&format!("{a}: no value")))
        };
        let bytes = |v: String| parse_size(&v).unwrap_or_else(|| usage(&format!("size {v}")));
        match a.as_str() {
            "--seed" => seed = val().parse().unwrap_or_else(|_| usage("--seed N")),
            "--size" => size = Some(bytes(val())),
            "--out" => out = Some(std::path::PathBuf::from(val())),
            "--rate" => rate = bytes(val()),
            "--limit" => limit = Some(bytes(val())),
            "--seconds" => {
                seconds = Some(Duration::from_secs(
                    val().parse().unwrap_or_else(|_| usage("--seconds N")),
                ));
            }
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
        other => match FileSet::parse(other) {
            Some(set) => make(set, size),
            None => usage("command"),
        },
    }
}

fn usage(what: &str) -> ! {
    eprintln!(
        "{what}\nusage: testdata clean | path | v1|v2|v3|v4 [--seed N] [--size 10g] | all | v5 | v6 [--out F] [--rate 10m] [--seconds N] [--limit 1g]"
    );
    std::process::exit(2);
}
