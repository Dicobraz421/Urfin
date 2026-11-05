//! Запись `.cast`: разбор на эталоне записи (`urfin-term`), вывод без VT, индекс и повтор
//! на сгенерированных файлах.
#![allow(clippy::unwrap_used, reason = "тесты")]

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use urfin_core::CancelToken;
use urfin_term::vt::grid::Defaults;

use super::key::{MARK, synth};
use super::scan::emulator;
use super::*;
use crate::source::{ByteSource, OwnedSource};

const GOLDEN: &str = include_str!("../../../term/src/rec_golden.cast");

const D: Defaults = Defaults {
    fg: [0, 0, 0],
    bg: [255, 255, 255],
    cursor: [0, 0, 0],
};

fn src(b: &[u8]) -> Arc<dyn ByteSource> {
    Arc::new(OwnedSource::new(b.to_vec()))
}

fn run(b: &[u8]) -> (Header, Arc<dyn ByteSource>, Arc<CastIndex>, String) {
    let h = header(b).unwrap();
    let s = src(b);
    let mut text = Vec::new();
    let ix = scan(
        &s,
        &h,
        &mut |c| {
            text.extend_from_slice(c);
            Ok(())
        },
        &CancelToken::new(),
        &AtomicU64::new(0),
    )
    .unwrap()
    .unwrap();
    (h, s, Arc::new(ix), String::from_utf8(text).unwrap())
}

/// Строка события.
fn out(s: &mut String, t: f64, data: &str) {
    let q = serde_json::to_string(data).unwrap();
    writeln!(s, "[{t:.6},\"o\",{q}]").unwrap();
}

/// Сеанс: цветные строки с номером раз в `step` с, каждая пятая — `needle`.
fn session(lines: usize, step: f64, pad: usize) -> Vec<u8> {
    let mut s =
        String::from("{\"version\":2,\"width\":80,\"height\":24,\"timestamp\":1792764151}\n");
    let fill = "x".repeat(pad);
    for i in 0..lines {
        #[allow(clippy::cast_precision_loss, reason = "номера строк теста")]
        let t = i as f64 * step;
        let word = if i % 5 == 0 { "needle" } else { "plain" };
        out(
            &mut s,
            t,
            &format!("\x1b[1;3{}m{i:06}\x1b[0m {word} {fill}\r\n", i % 7 + 1),
        );
    }
    s.into_bytes()
}

#[test]
fn golden_header_and_events() {
    let h = header(GOLDEN.as_bytes()).unwrap();
    assert_eq!((h.width, h.height), (80, 24));
    assert_eq!(h.timestamp, Some(1_792_764_151));
    assert_eq!(h.title, "srv1");
    let mut l = Lines::new(src(GOLDEN.as_bytes()), h.body);
    let mut ms = Vec::new();
    let mut size = None;
    while let Some(line) = l.next_line().unwrap() {
        let e = event(line).unwrap();
        ms.push(e.ms);
        if let Ev::Resize(w, h) = e.ev {
            size = Some((w, h));
        }
    }
    assert_eq!(ms, [100, 500, 1000, 1250, 2000]);
    assert_eq!(size, Some((100, 30)));
}

#[test]
fn golden_scan_and_replay() {
    let (h, s, ix, text) = run(GOLDEN.as_bytes());
    assert_eq!(ix.duration_ms, 2000);
    assert_eq!(ix.bad, 0);
    assert!(text.starts_with("admin@srv1:~$ Password: Привет \"мир\""));
    assert!(!text.contains('\x1b'));
    let mut r = Replay::new(s, &h, ix);
    r.seek(600).unwrap();
    let snap = r.snapshot(&D);
    assert_eq!((snap.cols, snap.rows), (80, 24));
    // Resize — событием на 1,0 с.
    r.seek(1500).unwrap();
    let snap = r.snapshot(&D);
    assert_eq!((snap.cols, snap.rows), (100, 30));
}

#[test]
fn not_a_cast() {
    assert!(header(b"{\"version\":1,\"width\":80,\"height\":24}\n").is_none());
    assert!(header(b"{\"version\":2,\"width\":0,\"height\":24}\n").is_none());
    assert!(header(b"plain text\n").is_none());
    assert!(header(b"{\"version\":2,\"width\":80").is_none());
}

#[test]
fn foreign_header_and_broken_lines() {
    let b = b"{\"version\":2,\"width\":120,\"height\":40,\"timestamp\":1.5e9}\n\
              [0.5,\"o\",\"a\"]\n[bad\n[1.0,\"i\",\"secret\"]\n\n[2.0,\"o\",\"b\"]\n[3.0,\"o\",\"cu";
    let (h, _, ix, text) = run(b);
    assert_eq!(h.title, "");
    assert_eq!(h.timestamp, Some(1_500_000_000));
    assert_eq!(text, "ab", "ввод `i` и оборванная строка — мимо текста");
    assert_eq!(ix.bad, 2);
    assert_eq!(ix.duration_ms, 2000);
}

#[test]
fn strip_across_events() {
    let mut st = Strip::default();
    let mut o = Vec::new();
    for part in [
        "a\x1b[3", "1mb\x1b[0m", "\x1b]0;title\x07c", "\x1b]8;;u\x1b\\d", "\x1b(Be", "\r\n", "f\rg",
        "\tу\x08\x08ж\x01",
    ] {
        st.feed(part, &mut o);
    }
    assert_eq!(String::from_utf8(o).unwrap(), "abcde\nf\ng\tж");
}

#[test]
fn long_line_crosses_read_block() {
    let (_, _, ix, text) = run(&session(3, 1.0, 200 << 10));
    assert_eq!(ix.duration_ms, 2000);
    assert_eq!(text.lines().count(), 3);
    assert_eq!(ix.bad, 0);
}

#[test]
fn keys_every_5s_and_every_chunk() {
    // 200 строк по секунде.
    let (_, _, ix, _) = run(&session(200, 1.0, 10));
    assert_eq!(ix.keys.len(), 1 + 199 / 5);
    assert!(ix.keys.windows(2).all(|k| k[1].ms - k[0].ms == 5000));
    // Весь объём в одну секунду: ключи — по объёму.
    let (_, _, ix, _) = run(&session(4000, 0.000_1, 1000));
    assert!(ix.keys.len() >= 15, "{}", ix.keys.len());
    assert!(
        ix.keys
            .windows(2)
            .all(|k| k[1].off - k[0].off <= KEY_EVERY_BYTES + 2048)
    );
}

#[test]
fn seek_equals_sequential_replay() {
    let b = session(3000, 0.1, 40);
    let (h, s, ix, _) = run(&b);
    let mut r = Replay::new(s.clone(), &h, ix.clone());
    for ms in [120_000u64, 7_300, 299_900, 50_000, 0, 150_050] {
        let mut seq = Replay::new(s.clone(), &h, ix.clone());
        seq.advance(ms).unwrap();
        r.seek(ms).unwrap();
        assert_eq!(r.ms(), ms);
        assert_eq!(r.snapshot(&D).cells, seq.snapshot(&D).cells, "t={ms}");
        assert_eq!(r.snapshot(&D).cursor, seq.snapshot(&D).cursor, "t={ms}");
    }
}

#[test]
fn alt_screen_key_keeps_primary() {
    let mut s =
        String::from("{\"version\":2,\"width\":40,\"height\":6,\"timestamp\":1792764151}\n");
    out(&mut s, 0.0, "primary\r\n");
    out(&mut s, 6.0, "second\r\n");
    out(&mut s, 7.0, "\x1b[?1049h\x1b[Halt one");
    out(&mut s, 13.0, "\x1b[2;1Halt two");
    out(&mut s, 20.0, "\x1b[?1049lback\r\n");
    let (h, f, ix, _) = run(s.as_bytes());
    let key = ix.keys.iter().find(|k| k.base.is_some()).unwrap();
    assert_eq!(key.ms, 13_000);
    let mut r = Replay::new(f, &h, ix);
    let row = |r: &Replay, n: usize| -> String {
        r.snapshot(&D).cells[n]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    r.seek(14_000).unwrap();
    r.seek(13_500).unwrap();
    assert_eq!(row(&r, 1), "alt two");
    r.seek(20_000).unwrap();
    assert_eq!(row(&r, 0), "primary", "основной экран — из его ключа");
    assert_eq!(row(&r, 2), "back");
}

#[test]
fn synth_restores_screen() {
    let t = emulator(30, 5);
    t.inject("\x1b[1;31mкрасный\x1b[0m обычный\r\n\x1b[44;4mфон\x1b[0m 漢字\r\n\x1b[?25l".as_bytes());
    let a = t.snapshot(&MARK);
    let vt = synth(&a);
    let u = emulator(30, 5);
    u.inject(&vt);
    let b = u.snapshot(&MARK);
    assert_eq!(a.cells, b.cells);
    assert_eq!(a.cursor, b.cursor);
    assert_eq!(a.cursor_kind, b.cursor_kind);
}

#[test]
fn text_offset_gives_time_when_on_screen() {
    let (_, _, ix, text) = run(&session(100, 1.0, 10));
    let hits: Vec<u64> = text.match_indices("needle").map(|(i, _)| i as u64).collect();
    assert_eq!(hits.len(), 20);
    // i-е совпадение — в строке 5·i, строка выведена в 5·i секунд.
    for (i, &off) in hits.iter().enumerate() {
        assert_eq!(ix.time_of(off), i as u64 * 5000);
    }
    assert_eq!(ix.text_at(0), text.find('\n').unwrap() as u64 + 1);
    assert_eq!(ix.text_at(99_000), text.len() as u64);
    assert!((ix.frac(49_500) - 0.5).abs() < 1e-6);
}

#[test]
fn time_map_is_sparse() {
    // Тысячи событий за 100 мс — одна запись карты.
    let (_, _, ix, text) = run(&session(2000, 0.000_01, 5));
    let last = ix.time_of(text.len() as u64 - 1);
    assert_eq!(last, ix.duration_ms);
    assert_eq!(ix.time_of(0), last);
}

#[test]
fn cancelled_scan_stops() {
    let b = session(1000, 0.1, 10);
    let h = header(&b).unwrap();
    let c = CancelToken::new();
    c.cancel();
    let r = scan(&src(&b), &h, &mut |_| Ok(()), &c, &AtomicU64::new(0)).unwrap();
    assert!(r.is_none());
}
