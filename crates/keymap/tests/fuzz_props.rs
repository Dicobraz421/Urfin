//! Запасной путь фаззинга (S9-15c) без nightly: те же проверки, что у целей
//! `fuzz/fuzz_targets/*` (общий `common.rs`), на входах proptest — произвольных и
//! собранных из кусков keymap TOML, сочетаний и `when`.

#[path = "../fuzz/fuzz_targets/common.rs"]
mod common;

use proptest::prelude::*;

/// Куски, из которых собирается «почти правильный» вход.
const PIECES: &[&str] = &[
    "format = 1\n",
    "extends = \"far\"\n",
    "extends = \"dn\"\n",
    "extends = \"base\"\n",
    "extends = \"user\"\n",
    "name = \"user\"\n",
    "[context.panel]\n",
    "[context.\"panel && !qs\"]\n",
    "[context.\"focus == 'cmdline' || viewer\"]\n",
    "[context.\"(\"]\n",
    "[os.windows.context.panel]\n",
    "[os.linux.context.global]\n",
    "[os.macos.context.panel]\n",
    "\"Ctrl+K Ctrl+H\" = \"panel.hidden.toggle\"\n",
    "\"Ctrl+K\" = \"ops.copy\"\n",
    "\"F5\" = [\"ops.copy\", \"sel.all\"]\n",
    "\"Alt+F1\" = false\n",
    "\"Shift+F5\" = { cmd = \"ops.move to=/tmp\", _repeat = true }\n",
    "\"Ctrl+1\" = \"tab.select n=1\"\n",
    "\"Ctrl+S\" = \"panel.sort by=name desc=true\"\n",
    "\"AltTap\" = \"keymap.qs.start\"\n",
    "\"LCtrl+RAlt+NumEnter\" = \"cmdline.insert text='x y'\"\n",
    "\"Cmd+Q Ctrl+Q Alt+Q Shift+Q\" = \"ops.copy\"\n",
    "\"Ctrl+\" = \"ops.copy\"\n",
    "\"+\" = \"nav.up\"\n",
    "\"Ctrl++\" = \"nav.down\"\n",
    "\"ф\" = \"nav.up\"\n",
    "\"Ctrl+GrayStar\" = \"no.such\"\n",
    "\"F13\" = 5\n",
    "\"Esc\" = \"panel.go to=\"\n",
    "[context]\n",
    "context = 1\n",
    "[[context.panel]]\n",
    "\"x\" = \"",
    "\n",
];

const CHORDS: &[&str] = &[
    "Ctrl+K Ctrl+H",
    "Ctrl+K",
    "ctrl+shift+alt+cmd+f12",
    "LCtrl+RAlt+NumEnter",
    "Ctrl++",
    "+",
    "Ctrl+",
    "AltTap",
    "  F5   F6 ",
    "Ctrl+ф",
    "GraySlash GrayStar GrayMinus GrayPlus",
];

const WHENS: &[&str] = &[
    "panel",
    "!panel && qs",
    "focus == 'cmdline' || viewer",
    "(panel)",
    "((",
    "backend == 'arc'",
    "",
    "!!!",
    "a ==",
];

fn joined(pieces: &'static [&'static str], max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(pieces), 0..max).prop_map(|v| v.concat())
}

fn os_byte() -> impl Strategy<Value = u8> {
    any::<u8>()
}

fn with_os(b: u8, s: &str) -> Vec<u8> {
    let mut v = vec![b];
    v.extend_from_slice(s.as_bytes());
    v
}

fn noise() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        "[\\[\\]\"=+ a-zA-Z0-9.!&|()'\n#_{}:,-]{0,64}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn fuzz_keymap_toml(b in os_byte(), text in prop_oneof![joined(PIECES, 12), noise()]) {
        let data = with_os(b, &text);
        let (os, text) = common::split_os(&data);
        common::keymap_toml(os, &text);
    }

    #[test]
    fn fuzz_chord_when(
        c in prop_oneof![prop::sample::select(CHORDS).prop_map(str::to_owned), noise()],
        w in prop_oneof![prop::sample::select(WHENS).prop_map(str::to_owned), noise()],
    ) {
        common::chord_when(&format!("{c}\0{w}"));
    }

    #[test]
    fn fuzz_user_edit(
        b in os_byte(),
        text in prop_oneof![joined(PIECES, 8), noise()],
        c in prop_oneof![prop::sample::select(CHORDS).prop_map(str::to_owned), noise()],
        ctx in prop_oneof![prop::sample::select(WHENS).prop_map(str::to_owned), noise()],
        cmd in prop_oneof![Just("ops.copy".to_owned()), Just("panel.sort by=name".to_owned()), noise()],
    ) {
        let data = with_os(b, &format!("{text}\0{c}\0{ctx}\0{cmd}"));
        let (os, s) = common::split_os(&data);
        common::user_edit(os, &s);
    }
}
