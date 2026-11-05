//! Кадр эмулятора: то, что экраном не видно, — по поведению после восстановления.
#![allow(clippy::unwrap_used, reason = "тесты")]

use super::*;
use crate::session::{SessionEvent, SessionOpts, SessionSink, parse_inline};
use crate::vt::grid::{Defaults, Snapshot};

const D: Defaults = Defaults {
    fg: [1, 1, 1],
    bg: [2, 2, 2],
    cursor: [3, 3, 3],
};

struct Quiet;

impl SessionSink for Quiet {
    fn poke(&self) {}
    fn event(&self, _: SessionEvent) {}
}

fn emulator(size: PtySize) -> Arc<TermShared> {
    let opts = SessionOpts {
        size,
        scrollback: 0,
        osc52: "off".into(),
        nonce: None,
    };
    parse_inline(&[][..], &opts, &Quiet)
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { cols, rows }
}

fn restored(img: &Image) -> Arc<TermShared> {
    let u = emulator(img.start());
    img.restore(&u);
    u
}

/// Поток целиком и он же через кадр после `head`: экраны равны сразу и после `next`.
fn same_after(cols: u16, rows: u16, head: &str, next: &str) -> Snapshot {
    let t = emulator(size(cols, rows));
    let mut tr = Tracker::default();
    tr.feed(&t, head.as_bytes());
    let u = restored(&tr.capture(&t).expect("граница"));
    assert_eq!(u.snapshot(&D), t.snapshot(&D), "сразу после кадра");
    tr.feed(&t, next.as_bytes());
    u.inject(next.as_bytes());
    assert_eq!(u.snapshot(&D), t.snapshot(&D), "после {next:?}");
    t.snapshot(&D)
}

#[test]
fn scroll_region_survives() {
    same_after(20, 6, "\x1b[2;4r\x1b[4;1Ha\r\nb\r\nc", "\r\nd\r\ne");
}

#[test]
fn empty_and_bottom_regions_differ() {
    // Обрезка по экрану: на 6 строках `9;12r` — пустая область, `6;9r` — нижняя строка.
    same_after(20, 6, "ab\x1b[9;12r\x1b[6;1Hc", "\x1bM\x1bMd\n\ne");
    same_after(20, 6, "ab\x1b[6;9r\x1b[6;1Hc", "\x1bM\x1bMd\n\ne");
    same_after(20, 1, "ab\x1b[9;12rc", "\x1bMd");
}

#[test]
fn origin_wrap_insert_modes() {
    same_after(20, 6, "\x1b[2;5r\x1b[?6h", "\x1b[1;1Hx");
    same_after(5, 3, "\x1b[?7labcdefgh", "ij");
    same_after(10, 3, "abcdef\x1b[1;3H\x1b[4h", "XY");
    same_after(10, 3, "\x1b[20hab", "\ncd");
}

#[test]
fn pending_wrap_and_wrapped_lines() {
    // Курсор «ждёт переноса», признак переноса строки виден при смене размера.
    same_after(5, 3, "abcde", "f");
    let t = emulator(size(5, 4));
    let mut tr = Tracker::default();
    tr.feed(&t, b"abcdefgh");
    let u = restored(&tr.capture(&t).unwrap());
    t.resize(size(10, 4));
    u.resize(size(10, 4));
    assert_eq!(u.snapshot(&D), t.snapshot(&D));
}

#[test]
fn cursor_pen_charsets_and_saved() {
    same_after(20, 3, "\x1b[1;31;44m", "a\x1b[K");
    same_after(20, 3, "\x1b(0", "qq\x1b(B");
    same_after(20, 3, "\x0e\x1b)0", "qq\x0fqq");
    same_after(20, 3, "\x1b[2;5H\x1b[33m\x1b7\x1b[0m\x1b[1;1H", "\x1b8x");
}

#[test]
fn colors_stay_symbolic() {
    // OSC 4 перекрашивает уже выведенное; `dim` и `hidden` — не RGB снимка.
    same_after(
        20,
        3,
        "\x1b[31ma\x1b[2mb\x1b[8mc",
        "\x1b]4;1;rgb:10/20/30\x07",
    );
}

#[test]
fn tab_stops_custom() {
    same_after(30, 3, "\x1b[3g\x1b[5G\x1bH\x1b[1G", "\tx\tz");
    same_after(30, 3, "\x1b[1G\x1b[0g\x1b[17G", "\x1b[Zx\x1b[Zy\x1b[Zz");
}

#[test]
fn repeat_after_key() {
    same_after(20, 3, "ab", "\x1b[3b");
    same_after(20, 3, "ab\x1b[0m\r\n", "\x1b[3b");
}

#[test]
fn alt_screen_both_grids() {
    let last = same_after(
        20,
        4,
        "one\r\n\x1b7\x1b[?1049h\x1b[2;3r\x1b[2;2Halt\x1b7",
        "\x1b[3;1H\x1b8x\x1b[?1049l\x1b[?1049h\x1b8y\x1b[?1049lz\x1b8w",
    );
    assert!(!last.alt);
}

#[test]
fn alt_saved_cursor_between_sessions() {
    same_after(
        20,
        4,
        "\x1b[?1049h\x1b[3;4H\x1b7\x1b[?1049lprimary",
        "\x1b[?1049h\x1b8x",
    );
}

#[test]
fn alt_after_resize_replays_primary() {
    // Reflow основного экрана за альтернативным: курсор и сохранённый курсор расходятся.
    let t = emulator(size(6, 3));
    let mut tr = Tracker::default();
    tr.feed(&t, b"abcdefghij\x1b[?1049hA");
    tr.resize(&t, size(3, 3));
    tr.feed(&t, b"B");
    let img = tr.capture(&t).unwrap();
    assert!(img.alt());
    let u = restored(&img);
    assert_eq!(u.snapshot(&D), t.snapshot(&D));
    for x in [&t, &u] {
        x.inject(b"\x1b[?1049l\x1b8x");
    }
    assert_eq!(u.snapshot(&D), t.snapshot(&D));
}

#[test]
fn alt_reentered_within_one_event() {
    let t = emulator(size(20, 3));
    let mut tr = Tracker::default();
    tr.feed(&t, b"one\x1b[?1049hA");
    tr.feed(&t, b"\x1b[?1049ltwo\x1b[?1049hB");
    let u = restored(&tr.capture(&t).unwrap());
    for x in [&t, &u] {
        x.inject(b"\x1b[?1049l");
    }
    assert_eq!(u.snapshot(&D), t.snapshot(&D));
}

#[test]
fn no_key_inside_sequence_or_sync() {
    let t = emulator(size(20, 3));
    let mut tr = Tracker::default();
    for (s, ok) in [
        ("a\x1b", false),
        ("[3", false),
        ("1m", true),
        ("\x1b]0;ti", false),
        ("tle\x07\n", true),
        ("\x1b[?2026h", false),
        ("hidden", false),
        ("\x1b[?2026l", true),
        ("\x1b[\n", false),
        ("\x18", true),
    ] {
        tr.feed(&t, s.as_bytes());
        assert_eq!(tr.capture(&t).is_some(), ok, "{s:?}");
    }
}
