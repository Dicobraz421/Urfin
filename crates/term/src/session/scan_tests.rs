//! Пре-сканер OSC: разбор, доверие, пределы и независимость от разбиения на чанки.

use proptest::prelude::*;

use super::*;

/// Выход сканера: соседние байты слиты, события — на своих местах.
#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Bytes(Vec<u8>),
    Ev(OscEvent),
}

fn run(nonce: Option<&str>, chunks: &[&[u8]]) -> Vec<Tok> {
    let mut s = Scanner::new(nonce.map(str::to_owned));
    let mut out: Vec<Tok> = Vec::new();
    for c in chunks {
        s.feed(c, &mut |p| match p {
            Piece::Bytes(b) => {
                if let Some(Tok::Bytes(last)) = out.last_mut() {
                    last.extend_from_slice(b);
                } else {
                    out.push(Tok::Bytes(b.to_vec()));
                }
            }
            Piece::Event(e) => out.push(Tok::Ev(e)),
        });
    }
    out
}

fn b(s: &str) -> Tok {
    Tok::Bytes(s.as_bytes().to_vec())
}

fn cwd(path: &str, source: CwdSource) -> Tok {
    Tok::Ev(OscEvent::Cwd {
        path: path.into(),
        source,
    })
}

#[test]
fn our_osc_is_cut_foreign_passes() {
    let s = "a\x1b]0;title\x07b\x1b]633;A\x07$ \x1b]633;B\x07ls\x1b]633;C\x1b\\out\x1b]133;D;2\x07\x1b[1mz";
    assert_eq!(
        run(None, &[s.as_bytes()]),
        [
            b("a\x1b]0;title\x07b"),
            Tok::Ev(OscEvent::PromptStart),
            b("$ "),
            Tok::Ev(OscEvent::CommandStart),
            b("ls"),
            Tok::Ev(OscEvent::Executed),
            b("out"),
            Tok::Ev(OscEvent::Finished(Some(2))),
            b("\x1b[1mz"),
        ]
    );
}

#[test]
fn osc9_other_than_9_and_iterm_images_pass() {
    let s = "\x1b]9;4;1;50\x07\x1b]1337;File=x:AAAA\x07\x1b]99;x\x07\x1b]13;y\x07";
    assert_eq!(run(None, &[s.as_bytes()]), [b(s)]);
}

#[test]
fn nonce_guards_fncwd_and_command_line() {
    let n = Some("n0nce");
    let s = "\x1b]633;P;FnCwd=n0nce;C:\\x\\x3by\x07\x1b]633;P;FnCwd=bad;C:\\evil\x07\x1b]633;E;ls -la\\x3b pwd;n0nce\x07\x1b]633;E;rm;bad\x07\x1b]633;P;IsWindows=True\x07";
    assert_eq!(
        run(n, &[s.as_bytes()]),
        [
            cwd("C:\\x;y", CwdSource::FnCwd),
            Tok::Ev(OscEvent::CommandLine("ls -la; pwd".into())),
            Tok::Ev(OscEvent::Prop("IsWindows".into(), "True".into())),
        ]
    );
    // Сеанс без интеграции: FnCwd и E не принимаются вовсе.
    let s = "\x1b]633;P;FnCwd=n0nce;C:\\x\x07\x1b]633;E;ls;n0nce\x07";
    assert_eq!(run(None, &[s.as_bytes()]), []);
}

#[test]
fn foreign_cwd_sources() {
    let s = "\x1b]7;file://host/C:/My%20Dir\x07\x1b]7;file:///home/u/%D0%B4\x1b\\\x1b]9;9;\"D:\\w\"\x07\x1b]1337;CurrentDir=/tmp\x07";
    assert_eq!(
        run(None, &[s.as_bytes()]),
        [
            cwd("C:/My Dir", CwdSource::Osc7),
            cwd("/home/u/д", CwdSource::Osc7),
            cwd("D:\\w", CwdSource::Osc9_9),
            cwd("/tmp", CwdSource::Iterm),
        ]
    );
}

#[test]
fn overflow_cancel_and_esc_inside() {
    let long = format!("\x1b]633;E;{};n\x07after", "x".repeat(MAX_OSC + 10));
    assert_eq!(run(Some("n"), &[long.as_bytes()]), [b("after")]);
    let s = "\x1b]633;A\x18x\x1b]633;B\x1b[2J";
    assert_eq!(
        run(None, &[s.as_bytes()]),
        [b("x"), Tok::Ev(OscEvent::CommandStart), b("\x1b[2J")]
    );
}

#[test]
fn split_inside_prefix_and_body() {
    let s = b"x\x1b]63";
    let t = b"3;A\x07y\x1b";
    let u = b"]0;t\x07";
    assert_eq!(
        run(None, &[s, t, u]),
        [b("x"), Tok::Ev(OscEvent::PromptStart), b("y\x1b]0;t\x07")]
    );
}

/// Фрагменты потока для свойства: текст, CSI, наши и чужие OSC, мусор.
fn piece() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        "[a-zа-я $\r\n]{0,12}".prop_map(String::into_bytes),
        Just(b"\x1b[31m".to_vec()),
        Just(b"\x1b]0;title\x07".to_vec()),
        Just(b"\x1b]633;A\x07".to_vec()),
        Just(b"\x1b]633;B\x1b\\".to_vec()),
        Just(b"\x1b]633;D;0\x07".to_vec()),
        Just(b"\x1b]633;P;FnCwd=k;C:\\w\x07".to_vec()),
        Just(b"\x1b]633;E;dir;k\x07".to_vec()),
        Just(b"\x1b]7;file:///tmp\x07".to_vec()),
        Just(b"\x1b]9;9;C:\\\x07".to_vec()),
        Just(b"\x1b]9;4;1\x07".to_vec()),
        Just(b"\x1b".to_vec()),
        Just(b"\x1b]".to_vec()),
        Just(b"\x07\x18".to_vec()),
        proptest::collection::vec(any::<u8>(), 0..8),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Приёмка S6-04: любое разбиение на чанки даёт те же байты и события.
    #[test]
    fn any_split_same_result(
        parts in proptest::collection::vec(piece(), 0..24),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..12),
    ) {
        let stream: Vec<u8> = parts.concat();
        let whole = run(Some("k"), &[&stream]);
        let mut at: Vec<usize> = cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
        at.sort_unstable();
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut prev = 0;
        for a in at {
            chunks.push(&stream[prev..a]);
            prev = a;
        }
        chunks.push(&stream[prev..]);
        prop_assert_eq!(run(Some("k"), &chunks), whole);
    }
}

/// S6-20: `CSI ?9001h/l` от `ConPTY` — и в списке режимов, и разрезанный между чанками.
#[test]
fn win32_input_mode_watch() {
    let mut w = ModeWatch::default();
    assert_eq!(w.feed(b"hello\x1b[?25h"), None);
    assert_eq!(w.feed(b"\x1b[?9001h\x1b[?1004h"), Some(true));
    assert_eq!(w.feed(b"x\x1b[?90"), None);
    assert_eq!(w.feed(b"01l"), Some(false));
    assert_eq!(w.feed(b"\x1b[?1;9001;2004h"), Some(true));
    // Не приватный режим и не 9001 — мимо.
    assert_eq!(w.feed(b"\x1b[9001h\x1b[?19001h"), None);
}
