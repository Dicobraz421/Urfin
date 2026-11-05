//! Запись `.cast`: разбор на эталоне записи (`urfin-term`), вывод без VT, индекс и повтор
//! на сгенерированных файлах.
#![allow(clippy::unwrap_used, reason = "тесты")]

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use proptest::prelude::*;
use urfin_core::CancelToken;
use urfin_term::vt::grid::Defaults;

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
        "a\x1b[3",
        "1mb\x1b[0m",
        "\x1b]0;title\x07c",
        "\x1b]8;;u\x1b\\d",
        "\x1b(Be",
        "\r\n",
        "f\rg",
        "\tу\x08\x08ж\x01",
    ] {
        st.feed(part, &mut o);
    }
    assert_eq!(String::from_utf8(o).unwrap(), "abcde\nf\ngж");
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
    // Весь объём в одну секунду: ключи — по объёму (в файле байт чуть больше: JSON).
    let (_, _, ix, _) = run(&session(4000, 0.000_1, 1000));
    assert!(ix.keys.len() >= 15, "{}", ix.keys.len());
    assert!(
        ix.keys
            .windows(2)
            .all(|k| k[1].off - k[0].off <= KEY_EVERY_BYTES * 11 / 10)
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
    let key = ix
        .keys
        .iter()
        .find(|k| {
            k.image
                .as_ref()
                .is_some_and(urfin_term::vt::image::Image::alt)
        })
        .unwrap();
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
    assert_eq!(row(&r, 0), "primary");
    assert_eq!(
        row(&r, 1),
        "second",
        "вывод между ключом и входом не теряется"
    );
    assert_eq!(row(&r, 2), "back");
}

/// Закон перемотки: к любому моменту, из любого положения — экран проигрывания с нуля.
#[allow(clippy::many_single_char_names, reason = "как в `run`")]
fn seek_law(b: &[u8]) {
    let (h, s, ix, _) = run(b);
    let mut times = vec![0, ix.duration_ms];
    let mut l = Lines::new(s.clone(), h.body);
    while let Some(line) = l.next_line().unwrap() {
        if let Some(e) = event(line) {
            times.extend([e.ms, e.ms.saturating_sub(1)]);
        }
    }
    times.sort_unstable();
    times.dedup();
    let mut seq = Replay::new(s.clone(), &h, ix.clone());
    let truth: Vec<_> = times
        .iter()
        .map(|&t| {
            seq.advance(t).unwrap();
            seq.snapshot(&D)
        })
        .collect();
    let n = times.len();
    // Вперёд, назад и вразброс; новый плеер — с первой же перемотки.
    let order = (0..n)
        .chain((0..n).rev())
        .chain((0..n).map(|i| i * 7 % n))
        .chain((0..n).map(|i| i * 11 % n))
        .chain([0]);
    let mut r = Replay::new(s.clone(), &h, ix.clone());
    for i in order {
        r.seek(times[i]).unwrap();
        assert_eq!(r.snapshot(&D), truth[i], "t={}", times[i]);
    }
    for i in [0, n / 2, n - 1] {
        let mut r = Replay::new(s.clone(), &h, ix.clone());
        r.seek(times[i]).unwrap();
        assert_eq!(r.snapshot(&D), truth[i], "новый плеер, t={}", times[i]);
    }
}

#[test]
fn golden_seek_law() {
    // Эталон короче шага ключей: время растянуто, чтобы ключи были внутри него.
    let mut lines = GOLDEN.lines();
    let mut out = format!("{}\n", lines.next().unwrap());
    for l in lines {
        let mut e: serde_json::Value = serde_json::from_str(l).unwrap();
        e[0] = (e[0].as_f64().unwrap() * 20.0).into();
        writeln!(out, "{e}").unwrap();
    }
    let (_, _, ix, _) = run(out.as_bytes());
    assert!(ix.keys.len() > 2, "{}", ix.keys.len());
    seek_law(out.as_bytes());
}

#[test]
fn key_waits_for_sequence_end() {
    let mut s = String::from("{\"version\":2,\"width\":20,\"height\":3}\n");
    out(&mut s, 0.0, "a");
    out(&mut s, 6.0, "\x1b[3");
    out(&mut s, 7.0, "1mb");
    out(&mut s, 8.0, "c");
    let (_, _, ix, _) = run(s.as_bytes());
    assert_eq!(ix.keys.len(), 2);
    assert_eq!(ix.keys[1].ms, 7_000);
    seek_law(s.as_bytes());
}

/// Цветной фон пера: со сменой размера не сочетается (`urfin_term::vt::image`, следы смены
/// размера вне кадра).
const BG_PENS: [&str; 2] = ["\x1b[2;4;42m", "\x1b[38;5;200;48;2;1;2;3m"];

/// Кусок потока: всё, что меняет состояние эмулятора. Ширины окна и колонка HTS — мимо
/// стопа в последней колонке. Без DCH: `alacritty` в debug падает на курсоре в колонке 0
/// над хвостом широкого символа.
fn piece(bg: bool) -> impl Strategy<Value = String> {
    let mut fixed = vec![
        "\r\n",
        "\r",
        "\n",
        "\t",
        "\x08",
        "\x0e",
        "\x0f",
        "\x18",
        // Экраны и режимы.
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[?6h",
        "\x1b[?6l",
        "\x1b[?7l",
        "\x1b[?7h",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[20h",
        "\x1b[20l",
        "\x1b[?25l",
        "\x1b[?25h",
        "\x1b[?1h",
        "\x1b=",
        "\x1b>",
        "\x1b[?1000h",
        "\x1b[?2004h",
        "\x1b[?2026h",
        "\x1b[?2026l",
        "\x1bc",
        // Область, курсор, сохранение.
        "\x1b[r",
        "\x1b7",
        "\x1b8",
        "\x1b[s",
        "\x1b[u",
        "\x1bD",
        "\x1bE",
        "\x1bM",
        "\x1b[6 q",
        "\x1b[4 q",
        "\x1b[0 q",
        "\x1b[?12h",
        // Правка экрана.
        "\x1b[2J",
        "\x1b[J",
        "\x1b[1J",
        "\x1b[K",
        "\x1b[1K",
        "\x1b[2L",
        "\x1b[2M",
        "\x1b[3@",
        "\x1b[2X",
        "\x1b[2S",
        "\x1b[2T",
        "\x1b#8",
        "\x1b[3b",
        // Перо.
        "\x1b[0m",
        "\x1b[1;31m",
        "\x1b[7m",
        "\x1b[8m",
        "\x1b[9;3m",
        "\x1b[38;5;200m",
        "\x1b[4:3;58;5;9m",
        // Таблицы символов, табуляции, палитра, ссылки.
        "\x1b(0",
        "\x1b(B",
        "\x1b)0",
        "\x1b)B",
        "\x1b[5G\x1bH",
        "\x1b[3g",
        "\x1b[0g",
        "\x1b[2I",
        "\x1b[2Z",
        "\x1b]4;1;rgb:10/20/30\x07",
        "\x1b]104\x07",
        "\x1b]10;#102030\x07",
        "\x1b]12;#405060\x1b\\",
        "\x1b]8;;http://x\x1b\\",
        "\x1b]8;;\x07",
        "\x1bPqabc\x1b\\",
        "\x1b]0;title\x07",
        // Широкие и составные символы, оборванные последовательности.
        "Привет",
        "漢字漢字",
        "e\u{301}",
        "\x1b[",
        "\x1b]2;оборвано",
    ];
    if bg {
        fixed.extend(BG_PENS);
    }
    prop_oneof![
        3 => prop::sample::select(fixed).prop_map(str::to_owned),
        3 => "[a-z0-9 ]{1,50}",
        2 => (1u16..40, 1u16..40).prop_map(|(t, b)| format!("\x1b[{t};{b}r")),
        2 => (1u16..30, 1u16..90).prop_map(|(l, c)| format!("\x1b[{l};{c}H")),
    ]
}

/// Шаг записи: кусок вывода (конец уходит в следующее событие) или новый размер.
#[derive(Clone, Debug)]
enum Step {
    Out(String, usize),
    Resize(u16, u16),
}

fn out_step(bg: bool) -> impl Strategy<Value = Step> {
    (prop::collection::vec(piece(bg), 1..6), 0usize..200)
        .prop_map(|(p, cut)| Step::Out(p.concat(), cut))
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    let size = (
        prop::sample::select(vec![10u16, 20, 34, 80]),
        prop::sample::select(vec![1u16, 3, 6, 24]),
    );
    let with_resize = prop_oneof![
        12 => out_step(false),
        1 => size.prop_map(|(w, h)| Step::Resize(w, h)),
    ];
    prop_oneof![
        prop::collection::vec(with_resize, 1..48),
        prop::collection::vec(out_step(true), 1..48),
    ]
}

/// Файл записи: событие раз в 1,5 с — ключи ложатся между событиями.
fn recording(steps: &[Step]) -> Vec<u8> {
    let mut s = String::from("{\"version\":2,\"width\":20,\"height\":6}\n");
    let mut t = 0.0;
    let mut tail = String::new();
    for st in steps {
        t += 1.5;
        match st {
            Step::Out(text, cut) => {
                let all = std::mem::take(&mut tail) + text;
                let mut at = (*cut).min(all.len());
                while !all.is_char_boundary(at) {
                    at -= 1;
                }
                out(&mut s, t, &all[..at]);
                tail = all[at..].to_owned();
            }
            Step::Resize(w, h) => {
                // Перо чистое: цветной фон строк в резерве сетки — вне кадра.
                out(&mut s, t, &(std::mem::take(&mut tail) + "[0m"));
                t += 0.001;
                writeln!(s, "[{t:.6},\"r\",\"{w}x{h}\"]").unwrap();
            }
        }
    }
    out(&mut s, t + 1.5, &tail);
    s.into_bytes()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Приёмка хвоста F03-13: перемотка к любому моменту = проигрывание с нуля до него.
    #[test]
    fn seek_equals_play_from_zero(steps in steps()) {
        seek_law(&recording(&steps));
    }
}

#[test]
fn text_offset_gives_time_when_on_screen() {
    let (_, _, ix, text) = run(&session(100, 1.0, 10));
    let hits: Vec<u64> = text
        .match_indices("needle")
        .map(|(i, _)| i as u64)
        .collect();
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
