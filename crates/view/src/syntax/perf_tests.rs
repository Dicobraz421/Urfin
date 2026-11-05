//! Замер подсветки окном (ADR-0024 §7, `docs/perf/s7.md`): проход по файлу, окно с
//! любой точки, задача после правки (точки + пересчёт + окно), память точек.
//!
//! `cargo nextest run -p urfin-view --release --run-ignored only --no-capture -E 'test(hl_window_perf)'`
//! (`HL_PERF_GB=1` — ещё JSON 1 ГБ во временном файле).

#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "замер: короткие имена и доли"
)]

use std::io::Write as _;
use std::time::Instant;

use super::lex::testkit::Mem;
use super::lex::{Code, Feed, Lang, Src, Text};
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

/// YAML — список объектов (как `kubectl get -o yaml`) — `n` байт.
fn yaml_list(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 200);
    v.extend_from_slice(b"apiVersion: v1\nitems:\n");
    let mut i = 0u64;
    while v.len() < n {
        let t = format!(
            "- kind: Pod\n  metadata:\n    name: pod-{i}\n    labels:\n      app: \"web\"\n  spec:\n    replicas: {}\n    note: |\n      line one\n      line two\n",
            i % 9
        );
        v.extend_from_slice(t.as_bytes());
        i += 1;
    }
    v
}

/// Markdown — разделы с абзацами, списками и кодом — `n` байт.
fn md_doc(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 200);
    let mut i = 0u64;
    while v.len() < n {
        let t = format!(
            "## Section {i}\n\nText with `code` and [link](http://x/{i}) and *em*.\nSecond line.\n\n- item one\n- item `two`\n\n```rust\nfn f{i}() {{}}\n```\n\n"
        );
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
    let mem_mb = (m.v.len() * std::mem::size_of::<super::marks::Mark>()) as f64 / 1_048_576.0;
    // Окно с разных мест (прыжок): видимое ≈ 8 КБ ± запас языка.
    let (margin, _) = super::win::margins(lang);
    let mut win = Vec::new();
    for k in 1..=20u64 {
        let a = len / 21 * k;
        let t = Instant::now();
        let w = window(
            &src,
            Enc::utf8(),
            lang,
            &m,
            a.saturating_sub(margin),
            (a + 8192 + margin).min(len),
            &|| false,
        )
        .expect("window")
        .expect("marks");
        assert_ne!(w.spans.len(), 0);
        win.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    // Задача после правки: копия точек, правка внутри строки (сходится), пересчёт до окна,
    // окно у правки. Текст — тот же (правка нулевой длины меняет только точки), чтобы
    // мерить работу задачи, а не копию файла.
    let mut job = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        let mut mm = m.clone();
        mm.edit(
            &TextEdit {
                at: edit_at,
                old: 0,
                new: 0,
            },
            lang,
        );
        while !(mm.complete() || mm.frontier() >= edit_at.saturating_sub(margin)) {
            advance(&mut mm, &src, lang, edit_at.saturating_sub(margin), &|| {
                false
            })
            .expect("resync");
        }
        let _w = window(
            &src,
            Enc::utf8(),
            lang,
            &mm,
            edit_at.saturating_sub(margin),
            (edit_at + 8192 + margin).min(len),
            &|| false,
        )
        .expect("window");
        job.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!(
        "hl_window_perf {name}: size_mb={:.0} pass_ms={pass_ms:.0} ({:.0} МБ/с) marks={} mem_mb={mem_mb:.2} window_p50={:.2} window_p99={:.2} job_p50={:.2} job_p99={:.2}",
        len as f64 / 1_048_576.0,
        len as f64 / 1_048_576.0 / (pass_ms / 1000.0),
        m.v.len(),
        pct(&mut win, 0.5),
        pct(&mut win, 0.99),
        pct(&mut job, 0.5),
        pct(&mut job, 0.99),
    );
}

/// Заготовка файла кода: начало, блок (`#` — номер), конец. Обёртки — как в жизни: всё внутри
/// одного уровня (класс Java, `namespace`, IIFE), куски идут не с верхнего уровня.
fn code_parts(code: Code) -> (&'static str, &'static str, &'static str) {
    match code {
        Code::Rust => (
            "pub mod big {\n",
            "    /// Функция #.\n    pub fn f#(a: u32, name: &str) -> Result<u32, String> {\n        let x = a + 1;\n        if x > 2 {\n            println!(\"{name} {x}\");\n            return Err(format!(\"bad {}\", x));\n        }\n        match x {\n            0 => Ok(1),\n            n => Ok(n * 2),\n        }\n    }\n\n",
            "}\n",
        ),
        Code::Go => (
            "package big\n\n",
            "// F# — функция.\nfunc F#(a int, name string) (int, error) {\n\tx := a + 1\n\tif x > 2 {\n\t\tfmt.Println(name, x)\n\t\treturn 0, errors.New(\"bad\")\n\t}\n\tswitch x {\n\tcase 0:\n\t\treturn 1, nil\n\tdefault:\n\t\treturn x * 2, nil\n\t}\n}\n\n",
            "",
        ),
        Code::C => (
            "#include <stdio.h>\n\n",
            "/* Функция #. */\nstatic int f#(int a, const char *name)\n{\n    int x = a + 1;\n    if (x > 2) {\n        printf(\"%s %d\\n\", name, x);\n        return -1;\n    }\n    switch (x) {\n    case 0:\n        return 1;\n    default:\n        return x * 2;\n    }\n}\n\n",
            "",
        ),
        Code::Cpp => (
            "#include <string>\n\nnamespace big {\n\n",
            "class C# : public Base {\npublic:\n    explicit C#(int a) : a_(a) {}\n    int run(const std::string &name) const override {\n        auto x = a_ + 1;\n        if (x > 2) {\n            std::cout << name << x << std::endl;\n            return -1;\n        }\n        return x * 2;\n    }\n\nprivate:\n    int a_ = 0;\n};\n\n",
            "}  // namespace big\n",
        ),
        Code::Java => (
            "package big;\n\npublic class Big {\n",
            "    /** Метод #. */\n    public int f#(int a, String name) throws Exception {\n        int x = a + 1;\n        if (x > 2) {\n            System.out.println(name + x);\n            throw new Exception(\"bad\");\n        }\n        switch (x) {\n            case 0:\n                return 1;\n            default:\n                return x * 2;\n        }\n    }\n\n",
            "}\n",
        ),
        Code::CSharp => (
            "using System;\n\nnamespace Big\n{\n    public class C\n    {\n",
            "        /// <summary>Метод #.</summary>\n        public int F#(int a, string name)\n        {\n            var x = a + 1;\n            if (x > 2)\n            {\n                Console.WriteLine($\"{name} {x}\");\n                throw new Exception(\"bad\");\n            }\n            return x switch\n            {\n                0 => 1,\n                _ => x * 2,\n            };\n        }\n\n",
            "    }\n}\n",
        ),
        Code::Js => (
            "(function () {\n  'use strict';\n\n",
            "  /** Функция #. */\n  function f#(a, name) {\n    const x = a + 1;\n    if (x > 2) {\n      console.log(`${name} ${x}`);\n      throw new Error('bad');\n    }\n    return [1, 2, 3].map((v) => v * x / 2);\n  }\n\n",
            "})();\n",
        ),
        Code::Ts | Code::Tsx => (
            "export namespace Big {\n",
            "  /** Функция #. */\n  export function f#(a: number, name: string): number {\n    const x: number = a + 1;\n    if (x > 2) {\n      console.log(`${name} ${x}`);\n      throw new Error('bad');\n    }\n    return [1, 2, 3].map((v) => v * x / 2).length;\n  }\n\n",
            "}\n",
        ),
        Code::Css => (
            "",
            ".c# {\n  color: red;\n  margin: 0 1px;\n}\n\n@media (min-width: #px) {\n  .d# {\n    background: url(a.png);\n  }\n}\n\n",
            "",
        ),
        Code::Lua => (
            "local M = {}\n\n",
            "local function f#(a, name)\n  local x = a + 1\n  if x > 2 then\n    print(name, x)\n    return nil, \"bad\"\n  end\n  return x * 2\nend\n\n",
            "return M\n",
        ),
        Code::Cmake => (
            "cmake_minimum_required(VERSION 3.20)\n\n",
            "if(ENABLE_#)\n  add_library(lib# STATIC a#.c b#.c)\n  target_compile_definitions(lib# PRIVATE X=#)\nendif()\n\n",
            "",
        ),
        Code::Make => (
            "CC = cc\n\n",
            "target#: a#.o b#.o\n\t$(CC) -o $@ $^\n\t@echo done\n\n",
            "",
        ),
        Code::Diff => (
            "",
            "diff --git a/f# b/f#\nindex 1111111..2222222 100644\n--- a/f#\n+++ b/f#\n@@ -1,3 +1,3 @@\n context\n-old #\n+new #\n context\n",
            "",
        ),
        Code::Python => (
            "import sys\n\n\nclass Big:\n",
            "    def f#(self, a, name):\n        \"\"\"Метод #.\"\"\"\n        x = a + 1\n        if x > 2:\n            print(f\"{name} {x}\")\n            raise ValueError(\"bad\")\n        return [v * x for v in (1, 2, 3)]\n\n",
            "",
        ),
    }
}

/// Текст кода `n` байт.
fn code_text(code: Code, n: usize) -> Vec<u8> {
    let (head, block, tail) = code_parts(code);
    let mut v = Vec::with_capacity(n + 4096);
    v.extend_from_slice(head.as_bytes());
    let mut i = 0u64;
    while v.len() < n {
        v.extend_from_slice(block.replace('#', &i.to_string()).as_bytes());
        i += 1;
    }
    v.extend_from_slice(tail.as_bytes());
    v
}

/// Пакетные языки: 16 `МиБ` и 1 `ГБ` (`HL_PERF_GB`). Нужны пакеты (`HL_PACK`).
///
/// `HL_PACK=<папка DLL> HL_PERF_GB=1 cargo nextest run -p urfin-view --run-ignored only --no-capture -E 'test(hl_code_perf)'`
#[test]
#[ignore = "замер: нужны пакеты грамматик"]
fn hl_code_perf() {
    let langs = [
        Code::Rust,
        Code::C,
        Code::Cpp,
        Code::CSharp,
        Code::Java,
        Code::Go,
        Code::Js,
        Code::Ts,
        Code::Python,
        Code::Css,
        Code::Lua,
        Code::Diff,
        Code::Cmake,
        Code::Make,
    ];
    for code in langs {
        let lang = Lang::Code(code);
        if super::grammar(lang.name()).is_none() {
            eprintln!("нет пакета {}", lang.name());
            continue;
        }
        let t = code_text(code, 16 << 20);
        let at = t.len() as u64 / 2;
        bench(&format!("{}-16mb", lang.name()), lang, &Mem(t), at);
        if std::env::var("HL_PERF_GB").is_ok() {
            let (head, block, tail) = code_parts(code);
            let dir = tempfile::tempdir().expect("tmp");
            let p = dir.path().join("big.txt");
            {
                let mut f = std::io::BufWriter::new(std::fs::File::create(&p).expect("create"));
                f.write_all(head.as_bytes()).unwrap();
                let (mut n, mut i) = (0u64, 0u64);
                while n < 1 << 30 {
                    let b = block.replace('#', &i.to_string());
                    f.write_all(b.as_bytes()).unwrap();
                    n += b.len() as u64;
                    i += 1;
                }
                f.write_all(tail.as_bytes()).unwrap();
            }
            let f = std::fs::File::open(&p).expect("open");
            let len = f.metadata().expect("meta").len();
            bench(
                &format!("{}-1gb", lang.name()),
                lang,
                &FileText(f, len),
                len / 2,
            );
        }
    }
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
    let y = yaml_list(16 << 20);
    let at = y.len() as u64 / 2;
    bench("yaml-16mb", Lang::Yaml, &Mem(y), at);
    let d = md_doc(16 << 20);
    let at = d.len() as u64 / 2;
    bench("md-16mb", Lang::Md, &Mem(d), at);
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
