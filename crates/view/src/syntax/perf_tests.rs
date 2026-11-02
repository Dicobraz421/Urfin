//! Замер подсветки окном (ADR-0024 §7, `docs/perf/s7.md`): проход по файлу, окно с
//! любой точки, задача после правки (точки + пересчёт + окно), память точек.
//!
//! `cargo nextest run -p urfin-view --release --run-ignored only --no-capture -E 'test(hl_window_perf)'`
//! (`HL_PERF_GB=1` — ещё JSON 1 ГБ во временном файле).

use std::io::Write as _;
use std::time::Instant;

use super::lex::testkit::Mem;
use super::lex::{Feed, Lang, Src, Text};
use super::marks::{Marks, advance};
use super::win::window;
use crate::edit::piece::TextEdit;
use crate::enc::Enc;

/// Файл как текст прохода (чтение с позиции).
struct FileText(std::fs::File, u64);

impl Text for FileText {
    fn len(&self) -> u64 {
        self.1
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::windows::fs::FileExt as _;
        self.0.seek_read(buf, off)
    }
}

/// JSON-массив записей в одну строку (как S7-35) — `n` байт.
fn json_line(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 200);
    v.extend_from_slice(b"[");
    let mut i = 0u64;
    while v.len() < n {
        let rec = format!(
            "{{\"id\":{i},\"name\":\"item {i}\",\"tags\":[\"a\",\"b\"],\"ok\":true,\"price\":{}.25,\"note\":null}},",
            i % 1000
        );
        v.extend_from_slice(rec.as_bytes());
        i += 1;
    }
    v.extend_from_slice(b"{}]");
    v
}

/// TOML — таблицы (как S7-35) — `n` байт.
fn toml_tables(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 200);
    let mut i = 0u64;
    while v.len() < n {
        let t = format!("[t{i}]\nname = \"x{i}\"\nn = {i}\nok = true # c\n");
        v.extend_from_slice(t.as_bytes());
        i += 1;
    }
    v
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i.min(v.len() - 1)]
}

fn bench(name: &str, lang: Lang, text: &dyn Text, edit_at: u64) {
    let src = Src {
        text,
        feed: Feed::Bytes,
        start: 0,
    };
    let len = text.len();
    let t = Instant::now();
    let mut m = Marks::new(0, len);
    advance(&mut m, &src, lang, u64::MAX, &|| false).expect("pass");
    let pass_ms = t.elapsed().as_secs_f64() * 1000.0;
    let mem_mb = (m.v.len() * std::mem::size_of::<super::marks::Mark>()) as f64 / 1048576.0;
    // Окно с разных мест (прыжок): видимое ≈ 8 КБ ± 64 КиБ.
    let mut win = Vec::new();
    for k in 1..=20u64 {
        let a = len / 21 * k;
        let t = Instant::now();
        let w = window(
            &src,
            Enc::utf8(),
            lang,
            &m,
            a.saturating_sub(65536),
            (a + 8192 + 65536).min(len),
            &|| false,
        )
        .expect("window")
        .expect("marks");
        assert!(!w.spans.is_empty());
        win.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    // Задача после правки: копия точек, правка внутри строки (сходится), пересчёт до окна,
    // окно у правки. Текст — тот же (правка нулевой длины меняет только точки), чтобы
    // мерить работу задачи, а не копию файла.
    let mut job = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        let mut mm = m.clone();
        mm.edit(&TextEdit {
            at: edit_at,
            old: 0,
            new: 0,
        });
        while !(mm.complete() || mm.frontier() >= edit_at.saturating_sub(65536)) {
            advance(&mut mm, &src, lang, edit_at.saturating_sub(65536), &|| {
                false
            })
            .expect("resync");
        }
        let _w = window(
            &src,
            Enc::utf8(),
            lang,
            &mm,
            edit_at.saturating_sub(65536),
            (edit_at + 73728).min(len),
            &|| false,
        )
        .expect("window");
        job.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!(
        "hl_window_perf {name}: size_mb={:.0} pass_ms={pass_ms:.0} ({:.0} МБ/с) marks={} mem_mb={mem_mb:.2} window_p50={:.2} window_p99={:.2} job_p50={:.2} job_p99={:.2}",
        len as f64 / 1048576.0,
        len as f64 / 1048576.0 / (pass_ms / 1000.0),
        m.v.len(),
        pct(&mut win, 0.5),
        pct(&mut win, 0.99),
        pct(&mut job, 0.5),
        pct(&mut job, 0.99),
    );
}

#[test]
#[ignore = "замер: cargo nextest run -p urfin-view --release --run-ignored only --no-capture -E 'test(hl_window_perf)'"]
fn hl_window_perf() {
    let j = json_line(16 << 20);
    let mid = j.len() as u64 / 2;
    let at = mid + j[mid as usize..].iter().position(|&b| b == b'm').unwrap() as u64;
    bench("json-16mb-line", Lang::Json, &Mem(j), at);
    let t = toml_tables(16 << 20);
    let mid = t.len() as u64 / 2;
    let at = mid + t[mid as usize..].iter().position(|&b| b == b'x').unwrap() as u64;
    bench("toml-16mb", Lang::Toml, &Mem(t), at);
    if std::env::var("HL_PERF_GB").is_ok() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("big.json");
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&p).expect("create"));
            let block = json_line(64 << 20);
            let body = &block[1..block.len() - 3];
            f.write_all(b"[").unwrap();
            for _ in 0..16 {
                f.write_all(body).unwrap();
            }
            f.write_all(b"{}]").unwrap();
        }
        let f = std::fs::File::open(&p).expect("open");
        let len = f.metadata().expect("meta").len();
        let at = len / 2 + 5;
        bench("json-1gb-line", Lang::Json, &FileText(f, len), at);
    }
}
