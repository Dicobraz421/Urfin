//! Подсветка вывода (F03-10): разметка фикстур (insta), «только неокрашенный»,
//! alt-screen, кэш.

use super::*;
use crate::session::{SessionEvent, SessionOpts, SessionSink, parse_inline};
use crate::sys::conpty::PtySize;
use crate::vt::grid::Defaults;

struct Quiet;

impl SessionSink for Quiet {
    fn poke(&self) {}
    fn event(&self, _ev: SessionEvent) {}
}

const D: Defaults = Defaults {
    fg: [0xcc, 0xcc, 0xcc],
    bg: [0x0c, 0x0c, 0x0c],
    cursor: [0xff, 0xff, 0xff],
};

const PAL: Palette = Palette {
    error: [1, 0, 0],
    warning: [2, 0, 0],
    success: [3, 0, 0],
    link: [4, 0, 0],
    dim: [5, 0, 0],
};

fn snap(bytes: &str, cols: u16, rows: u16) -> Snapshot {
    let opts = SessionOpts {
        size: PtySize { cols, rows },
        scrollback: 100,
        osc52: "off".into(),
        nonce: None,
    };
    parse_inline(bytes.as_bytes(), &opts, &Quiet).snapshot(&D)
}

/// Вывод `cargo test` с SGR, как его печатает cargo в консоль (цвета программы), и
/// pwsh-промпт, окрашенный оболочкой.
fn cargo_test() -> String {
    let g = "\x1b[1m\x1b[32m";
    let r = "\x1b[31m";
    let ok = "\x1b[32mok\x1b[0m";
    let n = "\x1b[0m";
    [
        format!("\x1b[90mPS \x1b[34mC:\\Dev\\urfin\x1b[90m> {n}cargo test -p urfin-ops"),
        format!("{g}   Compiling{n} urfin-ops v0.4.2 (C:\\Dev\\urfin\\crates\\ops)"),
        format!("{g}    Finished{n} `test` profile [unoptimized + debuginfo] target(s) in 4.12s"),
        format!(
            "{g}     Running{n} unittests src\\lib.rs (target\\debug\\deps\\urfin_ops-3f2a9c1d7b5e8a40.exe)"
        ),
        String::new(),
        "running 4 tests".to_owned(),
        format!("test copy::tests::cancel_before_start ... {ok}"),
        format!("test copy::tests::cancel_mid_file_removes_partial ... {r}FAILED{n}"),
        String::new(),
        "2026-10-23T11:02:12.418Z WARN urfin_ops::copy: partial left: C:\\Users\\a\\AppData\\Local\\Temp\\.urfin-part-7f3a".to_owned(),
        "thread 'copy::tests::cancel_mid' panicked at crates\\ops\\src\\copy.rs:212:9:".to_owned(),
        "assertion failed: !partial.exists()".to_owned(),
        format!("test result: {r}FAILED{n}. 3 passed; 1 failed; 0 ignored; finished in 0.31s"),
        format!("\x1b[1m\x1b[31merror\x1b[0m\x1b[1m: test failed, to rerun pass `-p urfin-ops --lib`{n}"),
    ]
    .join("\r\n")
}

/// Сеть, журнал, git, ссылки.
fn misc() -> String {
    [
        "Ответ от 10.20.4.30: число байт=32 время=2мс TTL=63",
        "listening on 10.20.4.11:8080 and [::1]:8080, peer fe80::1ff:fe23:4567:890a",
        "Oct 23 14:02:12 srv1 app[48211]: WARN cache.ttl not set, using 60s",
        "2026/10/23 14:02:31 fatal: unable to access 'https://git.corp.local/web/app.git/': ok",
        "From ssh://git.corp.local/web/app",
        "   3f9c2e1..a71d04b  main       -> origin/main",
        " src/http/server.go | 18 ++++---",
        "status=1/FAILURE copy::tests::run ~/x/y.txt /var/www/app, \\\\srv\\share\\x.txt",
        "java.lang.NullPointerException at Main.run; warning: errors=0 error_count=3 warned",
        "Active: active (running) since Fri; 1 test succeeded, PASSED, version 1.2.3.4.5",
    ]
    .join("\r\n")
}

/// Что нарисует оверлей: `[e:…]` вокруг покрашенных ячеек (роль — по цвету `PAL`).
fn markup_all(s: &Snapshot, h: &mut Highlighter) -> String {
    let mut after = s.clone();
    h.apply(&mut after, &PAL);
    let role = |fg: [u8; 3]| {
        [
            (PAL.error, 'e'),
            (PAL.warning, 'w'),
            (PAL.success, 's'),
            (PAL.link, 'u'),
            (PAL.dim, 'd'),
        ]
        .into_iter()
        .find(|(c, _)| *c == fg)
        .map(|(_, r)| r)
    };
    let mut out = String::new();
    for (a, b) in s.cells.iter().zip(&after.cells) {
        let mut line = String::new();
        let mut cur = None;
        for (x, y) in a.iter().zip(b) {
            let r = if x == y { None } else { role(y.fg) };
            if r != cur {
                if cur.is_some() {
                    line.push(']');
                }
                if let Some(r) = r {
                    line.push('[');
                    line.push(r);
                    line.push(':');
                }
                cur = r;
            }
            if !y.spacer {
                line.push(y.ch);
            }
        }
        if cur.is_some() {
            line.push(']');
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim_end().to_owned()
}

#[test]
fn markup_cargo_test() {
    let s = snap(&cargo_test(), 120, 16);
    let mut h = Highlighter::new(Rules::default());
    insta::assert_snapshot!(markup_all(&s, &mut h));
}

#[test]
fn markup_misc_with_hashes() {
    let s = snap(&misc(), 120, 12);
    let mut h = Highlighter::new(Rules {
        hashes: true,
        ..Rules::default()
    });
    insta::assert_snapshot!(markup_all(&s, &mut h));
}

/// Приёмка: цвета cargo и промпта не меняются — красятся только ячейки `plain`.
#[test]
fn program_colors_untouched() {
    let before = snap(&cargo_test(), 120, 16);
    let mut after = before.clone();
    let mut h = Highlighter::new(Rules::default());
    h.apply(&mut after, &PAL);
    let mut painted = 0;
    for (a, b) in before
        .cells
        .iter()
        .flatten()
        .zip(after.cells.iter().flatten())
    {
        if a.plain {
            painted += usize::from(a != b);
        } else {
            assert_eq!(a, b, "colored cell changed: {:?}", a.ch);
        }
    }
    assert!(painted > 0);
    // `FAILED` cargo — красный программы, не роль ошибки; `failed` в тексте — роль.
    let row = |r: usize| &after.cells[r];
    let col = |r: usize, word: &str| {
        let t: String = row(r).iter().map(|c| c.ch).collect();
        t.find(word).unwrap()
    };
    assert_eq!(row(7)[col(7, "FAILED")].fg, [0xc5, 0x0f, 0x1f]);
    assert_eq!(row(11)[col(11, "failed")].fg, PAL.error);
    // Промпт окрашен оболочкой: путь в нём — не ссылка.
    assert!(!row(0)[col(0, "Dev")].underline);
    // Путь в скобках — ссылка, скобка — нет.
    let c = col(1, "C:\\Dev");
    assert!(row(1)[c].underline && row(1)[c].fg == PAL.link);
    assert!(!row(1)[col(1, ")")].underline);
}

/// Приёмка: alt-screen (TUI) — без подсветки.
#[test]
fn alt_screen_untouched() {
    let before = snap("\x1b[?1049h\x1b[Herror at C:\\x\\y 10.0.0.1", 60, 5);
    assert!(before.alt);
    let mut after = before.clone();
    assert_eq!(
        Highlighter::new(Rules::default()).apply(&mut after, &PAL),
        0
    );
    assert_eq!(before, after);
}

/// Кэш: повторный кадр не гоняет regex; новая строка — один промах.
#[test]
fn cache_counts_changed_rows() {
    let mut s = snap(&misc(), 120, 12);
    let mut h = Highlighter::new(Rules::default());
    let first = h.apply(&mut s.clone(), &PAL);
    assert_eq!(first, 10);
    assert_eq!(h.apply(&mut s.clone(), &PAL), 0);
    s.cells[0][0].ch = 'X';
    assert_eq!(h.apply(&mut s, &PAL), 1);
}

#[test]
fn rules_off_and_log_rules() {
    let s = snap(&misc(), 120, 12);
    let none = Rules {
        errors: false,
        warnings: false,
        success: false,
        paths: false,
        urls: false,
        ip: false,
        dates: false,
        hashes: false,
    };
    let mut h = Highlighter::new(none);
    assert!(s.cells.iter().all(|l| h.spans(l).is_empty()));
    // Логи вьювера: байты строки, только ошибки, предупреждения и даты.
    let m = Matcher::new(Rules::LOG);
    let line = "2026-10-23 14:02:11 ERROR db 10.0.0.1 refused, WARN retry";
    let roles: Vec<_> = spans_of(&m, line)
        .iter()
        .map(|s| (&line[s.start as usize..s.end as usize], s.role))
        .collect();
    assert_eq!(
        roles,
        [
            ("2026-10-23 14:02:11", Role::Dim),
            ("ERROR", Role::Error),
            ("WARN", Role::Warning)
        ]
    );
}

#[test]
fn config_section_parses() {
    let c: crate::TermConfig =
        toml::from_str("[highlight]\nenabled = false\nrules = { hashes = true }\n").unwrap();
    assert!(!c.highlight.enabled);
    assert!(c.highlight.rules.hashes && c.highlight.rules.errors);
}
