use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ev::{Event, ImeEvent, Key, Modifiers};
use urfin_core::{AppState, ArgSpec, CommandMeta, CommandRegistry, Focus, OverlayKind, ValueKind};

use super::vk;
use crate::compile::tests::state;
use crate::{
    Chord, FlagRegistry, KeyInput, KeyNote, Keymap, KeymapCfg, Layer, MatchMode, Mods, Os,
};

const TEXT: &[ArgSpec] = &[ArgSpec {
    name: "text",
    kind: ValueKind::Str,
    required: true,
}];
/// `cmdline.paste`: без `text` — из буфера обмена (S6).
const TEXT_OPT: &[ArgSpec] = &[ArgSpec {
    name: "text",
    kind: ValueKind::Str,
    required: false,
}];

const fn meta(id: &'static str, args: &'static [ArgSpec], repeatable: bool) -> CommandMeta {
    CommandMeta {
        id,
        title: id,
        category: "test",
        args,
        repeatable,
    }
}

static METAS: &[CommandMeta] = &[
    meta("panel.cursor.down", &[], true),
    meta("panel.switch", &[], false),
    meta("ops.copy", &[], false),
    meta("sel.all", &[], false),
    meta("sel.toggle", &[], false),
    meta("a.chord", &[], false),
    meta("a.gray", &[], false),
    meta("a.center", &[], false),
    meta("a.right", &[], false),
    meta("a.digit", &[], false),
    meta("ui.cancel", &[], false),
    meta("cmdline.paste", TEXT_OPT, false),
    meta("cmdline.insert", TEXT, false),
    meta("keymap.qs.start", TEXT, false),
    meta("keymap.qs.input", TEXT, false),
    meta("term.input", TEXT, false),
    meta("term.key", &[], true),
];

const PRESET: &str = r#"format = 1
name = "far"
[context.panel]
"Down" = "panel.cursor.down"
"Tab" = "panel.switch"
"F5" = "ops.copy"
"Ctrl+A" = "sel.all"
"Space" = "sel.toggle"
"Ctrl+K Ctrl+C" = "a.chord"
"GrayMinus" = "a.gray"
"Num5" = "a.center"
"RCtrl+1" = "a.right"
"Shift+1" = "a.digit"
"Ctrl+V" = "cmdline.paste"
"Shift+Insert" = "cmdline.paste"
[context.overlay]
"Esc" = "ui.cancel"
"#;

fn input_with(cfg: &KeymapCfg) -> KeyInput {
    let mut reg = CommandRegistry::new();
    reg.register(METAS).unwrap();
    let (km, w) = Keymap::from_str_layers(
        &[(PRESET, Layer::Preset)],
        &reg,
        &FlagRegistry::with_builtin(),
        Os::Windows,
    );
    assert_eq!(w, Vec::new());
    KeyInput::new(km, cfg)
}

fn input() -> KeyInput {
    input_with(&KeymapCfg::default())
}

fn key(key: Key, m: Modifiers, pressed: bool) -> Event {
    Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: m,
    }
}

fn press(k: Key, m: Modifiers) -> Event {
    key(k, m, true)
}

struct Frame {
    events: Vec<Event>,
    cmds: Vec<String>,
    wake_at: Option<Instant>,
}

fn run_at(ki: &mut KeyInput, st: &AppState, now: Instant, events: Vec<Event>) -> Frame {
    let mut raw = crate::ev::RawInput { events };
    let mut out = Vec::new();
    let o = ki.process(&mut raw, st, now, &mut out);
    assert!(out.iter().all(|c| c.origin == urfin_core::Origin::Key));
    Frame {
        events: raw.events,
        cmds: out.iter().map(ToString::to_string).collect(),
        wake_at: o.wake_at,
    }
}

fn run(ki: &mut KeyInput, st: &AppState, events: Vec<Event>) -> Frame {
    run_at(ki, st, Instant::now(), events)
}

/// RU-раскладка: у «ф» нет латинской логической клавиши, egui даёт `key` = физическая A.
#[test]
fn ctrl_letter_on_russian_layout() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let f = run(&mut ki, &st, vec![press(Key::A, Modifiers::CTRL)]);
    assert_eq!(f.cmds, ["sel.all"]);
    assert_eq!(f.events, []);
}

/// `match = "physical"`: сочетание по позиции, даже если логическая клавиша другая.
#[test]
fn physical_match_mode() {
    let cfg = KeymapCfg {
        r#match: MatchMode::Physical,
        ..KeymapCfg::default()
    };
    let mut ki = input_with(&cfg);
    let st = state(Focus::Panel);
    // Dvorak: на позиции A — та же A, на позиции Q — «'».
    let ev = Event::Key {
        key: Key::Quote,
        physical_key: Some(Key::A),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::CTRL,
    };
    assert_eq!(run(&mut ki, &st, vec![ev]).cmds, ["sel.all"]);
}

/// Far: `Alt+ф` → `Key A` + `Text "ф"` → один `keymap.qs.start`, символ не печатается.
#[test]
fn alt_letter_starts_quick_search() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let f = run(
        &mut ki,
        &st,
        vec![
            Event::ModifiersChanged(Modifiers::ALT),
            press(Key::A, Modifiers::ALT),
            Event::Text("ф".into()),
        ],
    );
    assert_eq!(f.cmds, ["keymap.qs.start text=ф"]);
    assert!(!f.events.iter().any(|e| matches!(e, Event::Text(_))));
}

/// Режим `alt_tap` (DN): Alt+буква не начинает поиск — символ уходит в cmdline.
#[test]
fn alt_letter_without_far_mode() {
    let cfg = KeymapCfg {
        preset: "dn".into(),
        ..KeymapCfg::default()
    };
    let mut ki = input_with(&cfg);
    let st = state(Focus::Panel);
    let f = run(
        &mut ki,
        &st,
        vec![press(Key::A, Modifiers::ALT), Event::Text("ф".into())],
    );
    assert_eq!(f.cmds, ["cmdline.insert text=ф"]);
}

/// Внутри быстрого поиска печать → `keymap.qs.input`.
#[test]
fn quick_search_input() {
    let mut ki = input();
    let st = state(Focus::Overlay(OverlayKind::QuickSearch));
    let f = run(&mut ki, &st, vec![Event::Text("x".into())]);
    assert_eq!(f.cmds, ["keymap.qs.input text=x"]);
}

/// Удержание F5 (без автоповтора) → одна команда; удержание Down → по команде на повтор.
#[test]
fn repeat_policy() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let none = Modifiers::NONE;
    let f = run(
        &mut ki,
        &st,
        vec![
            press(Key::F5, none),
            press(Key::F5, none),
            press(Key::F5, none),
        ],
    );
    assert_eq!(f.cmds, ["ops.copy"]);
    let f = run(
        &mut ki,
        &st,
        vec![press(Key::F5, none), key(Key::F5, none, false)],
    );
    assert_eq!(f.cmds, Vec::<String>::new(), "still held");
    assert_eq!(f.events, [], "release of a consumed key is swallowed");
    assert_eq!(
        run(&mut ki, &st, vec![press(Key::F5, none)]).cmds,
        ["ops.copy"]
    );
    let f = run(
        &mut ki,
        &st,
        vec![
            press(Key::ArrowDown, none),
            press(Key::ArrowDown, none),
            press(Key::ArrowDown, none),
        ],
    );
    assert_eq!(f.cmds, ["panel.cursor.down"; 3]);
}

/// Tab в панели — команда, egui его не видит (фокус egui не двигается).
#[test]
fn tab_does_not_reach_egui() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let f = run(
        &mut ki,
        &st,
        vec![
            press(Key::Tab, Modifiers::NONE),
            key(Key::Tab, Modifiers::NONE, false),
        ],
    );
    assert_eq!(f.cmds, ["panel.switch"]);
    assert_eq!(f.events, []);
}

/// Пробел — команда, его `Text` не печатается; прочие символы в панели → cmdline.
#[test]
fn text_after_hit_is_suppressed() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let f = run(
        &mut ki,
        &st,
        vec![
            press(Key::Space, Modifiers::NONE),
            Event::Text(" ".into()),
            press(Key::B, Modifiers::NONE),
            Event::Text("b".into()),
        ],
    );
    assert_eq!(f.cmds, ["sel.toggle", "cmdline.insert text=b"]);
    assert_eq!(f.events, [press(Key::B, Modifiers::NONE)]);
}

/// В cmdline печать и непривязанные клавиши — виджету.
#[test]
fn cmdline_keeps_text() {
    let mut ki = input();
    let st = state(Focus::Cmdline);
    let evs = vec![press(Key::B, Modifiers::NONE), Event::Text("b".into())];
    let f = run(&mut ki, &st, evs);
    // Строку ввода правит Core (S6-15): печать — `cmdline.insert`, как из панели.
    assert_eq!(f.cmds, ["cmdline.insert text=b"]);
}

/// Терминал (ADR-0015): привязок `panel` и `global` нет — непечатные клавиши и
/// Ctrl+буква уходят в PTY (`term.key`), печать — текстом (`term.input`), без дублей.
#[test]
fn terminal_gets_raw_keys() {
    let mut ki = input();
    let st = state(Focus::Terminal);
    let f = run(
        &mut ki,
        &st,
        vec![
            press(Key::ArrowUp, Modifiers::NONE),
            press(Key::F5, Modifiers::NONE),
            press(Key::C, Modifiers::CTRL),
            press(Key::A, Modifiers::NONE),
            Event::Text("ф".into()),
        ],
    );
    assert_eq!(
        f.cmds,
        [
            "term.key key=up",
            "term.key key=f5",
            "term.key key=c mods=ctrl",
            "term.input text=ф",
        ]
    );
}

/// Во время IME-композиции ничего не матчится; после Commit — снова.
#[test]
fn ime_preedit_passthrough() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let preedit = Event::Ime(ImeEvent::Preedit {
        text: "ㅎ".into(),
        active_range_chars: None,
    });
    let f = run(&mut ki, &st, vec![preedit, press(Key::F5, Modifiers::NONE)]);
    assert_eq!(f.cmds, Vec::<String>::new());
    assert_eq!(f.events.len(), 2);
    let f = run(
        &mut ki,
        &st,
        vec![Event::Ime(ImeEvent::Commit("하".into()))],
    );
    assert_eq!(f.events.len(), 1);
    let f = run(
        &mut ki,
        &st,
        vec![
            key(Key::F5, Modifiers::NONE, false),
            press(Key::F5, Modifiers::NONE),
        ],
    );
    assert_eq!(f.cmds, ["ops.copy"]);
}

/// Аккорд: начало поглощается, дедлайн будит UI; по таймауту сбрасывается.
#[test]
fn chord_and_timeout() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let ctrl = Modifiers::CTRL;
    let t0 = Instant::now();
    let f = run_at(
        &mut ki,
        &st,
        t0,
        vec![press(Key::K, ctrl), key(Key::K, ctrl, false)],
    );
    assert_eq!(f.events, []);
    assert_eq!(f.wake_at, Some(t0 + Duration::from_millis(1500)));
    assert_eq!(ki.pending().unwrap().to_string(), "Ctrl+K");
    let f = run_at(
        &mut ki,
        &st,
        t0 + Duration::from_millis(100),
        vec![press(Key::C, ctrl)],
    );
    assert_eq!(f.cmds, ["a.chord"]);
    assert!(ki.pending().is_none());

    run_at(
        &mut ki,
        &st,
        t0,
        vec![press(Key::K, ctrl), key(Key::K, ctrl, false)],
    );
    let late = t0 + Duration::from_millis(1600);
    let f = run_at(&mut ki, &st, late, vec![]);
    assert_eq!(f.wake_at, None);
    assert!(ki.pending().is_none());
    // Ctrl+C после таймаута — не аккорд (и не привязан: уходит в egui).
    let f = run_at(&mut ki, &st, late, vec![press(Key::C, ctrl)]);
    assert_eq!(f.cmds, Vec::<String>::new());
    assert_eq!(f.events.len(), 1);
}

/// Непривязанное продолжение аккорда сбрасывает его и поглощается.
#[test]
fn chord_miss_resets() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let ctrl = Modifiers::CTRL;
    run(&mut ki, &st, vec![press(Key::K, ctrl)]);
    // Повтор начала аккорда ничего не меняет.
    let f = run(&mut ki, &st, vec![press(Key::K, ctrl)]);
    assert_eq!(f.events, []);
    assert!(ki.pending().is_some());
    let f = run(&mut ki, &st, vec![press(Key::Z, ctrl)]);
    assert_eq!(f.cmds, Vec::<String>::new());
    assert_eq!(f.events, []);
    assert!(ki.pending().is_none());
}

/// Потеря фокуса окна (Alt+Tab) сбрасывает аккорд и нажатые клавиши.
#[test]
fn focus_loss_resets() {
    let mut ki = input();
    let st = state(Focus::Panel);
    run(
        &mut ki,
        &st,
        vec![
            press(Key::K, Modifiers::CTRL),
            press(Key::F5, Modifiers::NONE),
        ],
    );
    run(&mut ki, &st, vec![Event::WindowFocused(false)]);
    assert!(ki.pending().is_none());
    // F5 после возврата — новое нажатие, не повтор.
    let f = run(&mut ki, &st, vec![press(Key::F5, Modifiers::NONE)]);
    assert_eq!(f.cmds, ["ops.copy"]);
}

/// `Paste` без `Key`: сочетание `Ctrl+V`, текст — в аргумент `text`.
#[test]
fn paste_becomes_stroke_with_text() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let f = run(&mut ki, &st, vec![Event::Paste("a b".into())]);
    assert_eq!(f.cmds, [r#"cmdline.paste text="a b""#]);
    assert_eq!(f.events, []);
    // Copy не привязан — остаётся виджетам.
    let f = run(&mut ki, &st, vec![Event::Copy]);
    assert_eq!(f.events, [Event::Copy]);
}

fn note(vk: u16, mods: Mods) -> KeyNote {
    KeyNote {
        vk,
        extended: false,
        mods,
        down: true,
    }
}

/// Записки Windows: `Shift+Ins` с текстом и при пустом буфере (событий нет вовсе).
#[test]
fn notes_restore_shift_insert() {
    let mut ki = input();
    let st = state(Focus::Panel);
    ki.notes().push(note(vk::INSERT, Mods::SHIFT));
    let f = run(&mut ki, &st, vec![Event::Paste("x".into())]);
    assert_eq!(f.cmds, ["cmdline.paste text=x"]);
    ki.notes().push(note(vk::INSERT, Mods::SHIFT));
    let f = run(&mut ki, &st, vec![]);
    assert_eq!(f.cmds, ["cmdline.paste"]);
}

/// Серый минус по записке; `Num5` без `NumLock` — синтетический; без записки — обычный `-`.
#[test]
fn notes_numpad() {
    let mut ki = input();
    let st = state(Focus::Panel);
    ki.notes().push(note(vk::SUBTRACT, Mods::empty()));
    let f = run(
        &mut ki,
        &st,
        vec![press(Key::Minus, Modifiers::NONE), Event::Text("-".into())],
    );
    assert_eq!(f.cmds, ["a.gray"]);
    assert_eq!(f.events, []);
    let f = run(
        &mut ki,
        &st,
        vec![
            key(Key::Minus, Modifiers::NONE, false),
            press(Key::Minus, Modifiers::NONE),
            Event::Text("-".into()),
        ],
    );
    assert_eq!(f.cmds, ["cmdline.insert text=-"]);
    ki.notes().push(note(vk::CLEAR, Mods::empty()));
    assert_eq!(run(&mut ki, &st, vec![]).cmds, ["a.center"]);
}

/// Сброшенная записка (нажатие разобрал не конвейер) сочетания не даёт.
#[test]
fn cleared_notes_are_dropped() {
    let mut ki = input();
    let st = state(Focus::Panel);
    ki.notes().push(note(vk::CLEAR, Mods::empty()));
    ki.notes().clear();
    assert_eq!(run(&mut ki, &st, vec![]).cmds, [] as [String; 0]);
}

/// Сторона модификатора — из записки хука: `RCtrl+1`.
#[test]
fn modifier_side_from_notes() {
    let mut ki = input();
    let st = state(Focus::Panel);
    ki.notes().push(KeyNote {
        vk: vk::CONTROL,
        extended: true,
        mods: Mods::CTRL | Mods::RCTRL,
        down: true,
    });
    let f = run(
        &mut ki,
        &st,
        vec![
            Event::ModifiersChanged(Modifiers::CTRL),
            press(Key::Num1, Modifiers::CTRL),
        ],
    );
    assert_eq!(f.cmds, ["a.right"]);
    // Ctrl отпущен — сторона забыта.
    run(&mut ki, &st, vec![Event::ModifiersChanged(Modifiers::NONE)]);
    let f = run(&mut ki, &st, vec![press(Key::Num2, Modifiers::CTRL)]);
    assert_eq!(f.cmds, Vec::<String>::new());
}

/// Shift+цифра: символ (`!`) у egui — отдельная клавиша, сочетание — по цифре.
#[test]
fn shift_digit_by_position() {
    let mut ki = input();
    let st = state(Focus::Panel);
    let ev = Event::Key {
        key: Key::Exclamationmark,
        physical_key: Some(Key::Num1),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::SHIFT,
    };
    let f = run(&mut ki, &st, vec![ev, Event::Text("!".into())]);
    assert_eq!(f.cmds, ["a.digit"]);
    assert_eq!(f.events, []);
}

/// Оверлей: Esc → `ui.cancel`; прочее — виджетам оверлея.
#[test]
fn overlay_escape() {
    let mut ki = input();
    let st = state(Focus::Overlay(OverlayKind::Menu));
    let f = run(
        &mut ki,
        &st,
        vec![
            press(Key::Escape, Modifiers::NONE),
            press(Key::ArrowDown, Modifiers::NONE),
        ],
    );
    assert_eq!(f.cmds, ["ui.cancel"]);
    assert_eq!(f.events, [press(Key::ArrowDown, Modifiers::NONE)]);
}

/// Новая раскладка сбрасывает начатый аккорд.
#[test]
fn set_keymap_resets_chord() {
    let mut ki = input();
    let st = state(Focus::Panel);
    run(&mut ki, &st, vec![press(Key::K, Modifiers::CTRL)]);
    let other = input();
    ki.set_keymap(Arc::clone(&other.km));
    assert!(ki.pending().is_none());
}

// ---- S3-06: быстрый поиск, одиночный Alt, начатый аккорд ----

const CHORD: &[ArgSpec] = &[ArgSpec {
    name: "chord",
    kind: ValueKind::Str,
    required: false,
}];

static QS_METAS: &[CommandMeta] = &[
    meta("keymap.qs.cancel", &[], false),
    meta("keymap.qs.next", &[], true),
    meta("keymap.pending", CHORD, false),
];

const QS_PRESET: &str = r#"format = 1
name = "qs"
extends = "far"
[context."overlay && qs"]
"Down" = "keymap.qs.next"
"#;

/// Как `input_with`, плюс команды handler `keymap` (S3-06).
fn input_qs(cfg: &KeymapCfg) -> KeyInput {
    let mut reg = CommandRegistry::new();
    reg.register(METAS).unwrap();
    reg.register(QS_METAS).unwrap();
    let (km, w) = Keymap::from_str_layers(
        &[(PRESET, Layer::Preset), (QS_PRESET, Layer::User)],
        &reg,
        &FlagRegistry::with_builtin(),
        Os::Windows,
    );
    assert_eq!(w, Vec::new());
    KeyInput::new(km, cfg)
}

fn dn() -> KeymapCfg {
    KeymapCfg {
        preset: "dn".into(),
        ..KeymapCfg::default()
    }
}

fn mods(m: Modifiers) -> Event {
    Event::ModifiersChanged(m)
}

/// DN: одиночный Alt начинает поиск.
#[test]
fn alt_tap_starts_quick_search() {
    let mut ki = input_qs(&dn());
    let st = state(Focus::Panel);
    let t0 = Instant::now();
    run_at(&mut ki, &st, t0, vec![mods(Modifiers::ALT)]);
    let f = run_at(
        &mut ki,
        &st,
        t0 + Duration::from_millis(100),
        vec![mods(Modifiers::NONE)],
    );
    assert_eq!(f.cmds, ["keymap.qs.start"]);
}

/// Alt → Shift → отпустить (смена раскладки), долгое удержание, клавиша между,
/// потеря фокуса (Alt+Tab) — не одиночный Alt.
#[test]
fn alt_tap_rejections() {
    let st = state(Focus::Panel);
    let t0 = Instant::now();
    let soon = t0 + Duration::from_millis(100);
    let alt_shift = Modifiers {
        alt: true,
        shift: true,
        ..Modifiers::NONE
    };
    let cases: [(&str, Vec<Event>, Instant); 4] = [
        (
            "alt+shift",
            vec![mods(alt_shift), mods(Modifiers::ALT), mods(Modifiers::NONE)],
            soon,
        ),
        (
            "too long",
            vec![mods(Modifiers::NONE)],
            t0 + Duration::from_millis(900),
        ),
        (
            "key between",
            vec![press(Key::F5, Modifiers::ALT), mods(Modifiers::NONE)],
            soon,
        ),
        (
            "focus lost",
            vec![Event::WindowFocused(false), mods(Modifiers::NONE)],
            soon,
        ),
    ];
    for (name, rest, at) in cases {
        let mut ki = input_qs(&dn());
        run_at(&mut ki, &st, t0, vec![mods(Modifiers::ALT)]);
        let f = run_at(&mut ki, &st, at, rest);
        assert!(
            !f.cmds.iter().any(|c| c.starts_with("keymap.qs.start")),
            "{name}: {:?}",
            f.cmds
        );
    }
}

/// Far: одиночный Alt поиск не начинает (там — Alt+буква).
#[test]
fn alt_tap_off_in_far() {
    let mut ki = input_qs(&KeymapCfg::default());
    let st = state(Focus::Panel);
    let t0 = Instant::now();
    run_at(&mut ki, &st, t0, vec![mods(Modifiers::ALT)]);
    let f = run_at(&mut ki, &st, t0, vec![mods(Modifiers::NONE)]);
    assert_eq!(f.cmds, Vec::<String>::new());
}

/// В поиске: своя привязка — команда поиска; печать — в маску; прочая клавиша
/// закрывает поиск и выполняется как на панели.
#[test]
fn quick_search_keys() {
    let mut ki = input_qs(&KeymapCfg::default());
    let st = state(Focus::Overlay(OverlayKind::QuickSearch));
    let none = Modifiers::NONE;
    let f = run(&mut ki, &st, vec![press(Key::ArrowDown, none)]);
    assert_eq!(f.cmds, ["keymap.qs.next"]);
    let f = run(
        &mut ki,
        &st,
        vec![press(Key::B, none), Event::Text("b".into())],
    );
    assert_eq!(f.cmds, ["keymap.qs.input text=b"]);
    // Пробел печатается в маску, а не помечает файл.
    let f = run(
        &mut ki,
        &st,
        vec![press(Key::Space, none), Event::Text(" ".into())],
    );
    assert_eq!(f.cmds, [r#"keymap.qs.input text=" ""#]);
    let f = run(&mut ki, &st, vec![press(Key::F5, none)]);
    assert_eq!(f.cmds, ["keymap.qs.cancel", "ops.copy"]);
    assert_eq!(f.events, []);
    // Непривязанная и на панели — только закрыть.
    let f = run(&mut ki, &st, vec![press(Key::F12, none)]);
    assert_eq!(f.cmds, ["keymap.qs.cancel"]);
}

/// Вставка в поиске — в маску; клик — закрыть.
#[test]
fn quick_search_paste_and_click() {
    let mut ki = input_qs(&KeymapCfg::default());
    let st = state(Focus::Overlay(OverlayKind::QuickSearch));
    let f = run(&mut ki, &st, vec![Event::Paste("abc".into())]);
    assert_eq!(f.cmds, ["keymap.qs.input text=abc"]);
    let click = Event::PointerButton {
        pos: (1.0, 1.0),
        button: crate::ev::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    };
    let f = run(&mut ki, &st, vec![click.clone()]);
    assert_eq!(f.cmds, ["keymap.qs.cancel"]);
    assert_eq!(f.events, [click]);
}

/// Начатый аккорд — `keymap.pending chord=…`, сброс — без `chord`.
#[test]
fn pending_chord_is_reported() {
    let mut ki = input_qs(&KeymapCfg::default());
    let st = state(Focus::Panel);
    let ctrl = Modifiers::CTRL;
    let f = run(&mut ki, &st, vec![press(Key::K, ctrl)]);
    assert_eq!(f.cmds, ["keymap.pending chord=Ctrl+K"]);
    let f = run(&mut ki, &st, vec![press(Key::K, ctrl)]);
    assert_eq!(f.cmds, Vec::<String>::new(), "repeat: no change");
    let f = run(&mut ki, &st, vec![press(Key::C, ctrl)]);
    assert_eq!(f.cmds, ["a.chord", "keymap.pending"]);
}

// ---- S3-07: записки хука Windows ----

static WIN_METAS: &[CommandMeta] = &[
    meta("w.enter", &[], false),
    meta("w.invert", &[], false),
    meta("w.permanent", &[], false),
    meta("w.names", &[], false),
    meta("w.slash", &[], false),
];

const WIN_PRESET: &str = r#"format = 1
name = "win"
extends = "far"
[context.panel]
"Enter" = "w.enter"
"GrayStar" = "w.invert"
"Shift+Delete" = "w.permanent"
"Ctrl+Insert" = "w.names"
"Ctrl+GraySlash" = "w.slash"
"#;

fn input_win() -> KeyInput {
    let mut reg = CommandRegistry::new();
    reg.register(METAS).unwrap();
    reg.register(WIN_METAS).unwrap();
    let (km, w) = Keymap::from_str_layers(
        &[(PRESET, Layer::Preset), (WIN_PRESET, Layer::User)],
        &reg,
        &FlagRegistry::with_builtin(),
        Os::Windows,
    );
    assert_eq!(w, Vec::new());
    KeyInput::new(km, &KeymapCfg::default())
}

fn ext_note(code: u16, mods: Mods) -> KeyNote {
    KeyNote {
        vk: code,
        extended: true,
        mods,
        down: true,
    }
}

/// `NumEnter` без своей привязки работает как `Enter`.
#[test]
fn num_enter_falls_back_to_enter() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.notes().push(ext_note(vk::RETURN, Mods::empty()));
    let f = run(&mut ki, &st, vec![press(Key::Enter, Modifiers::NONE)]);
    assert_eq!(f.cmds, ["w.enter"]);
}

/// Серая `*` — команда, `*` основного ряда (Shift+8) — печать в cmdline.
#[test]
fn gray_star_versus_main_row() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.notes().push(note(vk::MULTIPLY, Mods::empty()));
    let f = run(&mut ki, &st, vec![Event::Text("*".into())]);
    assert_eq!(f.cmds, ["w.invert"]);
    assert_eq!(f.events, []);
    let f = run(
        &mut ki,
        &st,
        vec![press(Key::Num8, Modifiers::SHIFT), Event::Text("*".into())],
    );
    assert_eq!(f.cmds, ["cmdline.insert text=*"]);
}

/// `Shift+Del` и `Ctrl+Ins` приходят от egui как `Cut`/`Copy` — сочетание по записке.
#[test]
fn shift_del_and_ctrl_ins() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.notes().push(ext_note(vk::DELETE, Mods::SHIFT));
    let f = run(&mut ki, &st, vec![Event::Cut]);
    assert_eq!(f.cmds, ["w.permanent"]);
    assert_eq!(f.events, []);
    ki.notes()
        .push(ext_note(vk::INSERT, Mods::CTRL | Mods::LCTRL));
    let f = run(&mut ki, &st, vec![Event::Copy]);
    assert_eq!(f.cmds, ["w.names"]);
    // Без записки (не Windows) Copy — это Ctrl+C: не привязано — виджетам.
    let f = run(&mut ki, &st, vec![Event::Copy]);
    assert_eq!(f.events, [Event::Copy]);
}

/// Серый `/` с Ctrl: по записке — `Ctrl+GraySlash`, иначе — обычный `/`.
#[test]
fn gray_slash() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.notes().push(note(vk::DIVIDE, Mods::CTRL));
    let f = run(&mut ki, &st, vec![press(Key::Slash, Modifiers::CTRL)]);
    assert_eq!(f.cmds, ["w.slash"]);
    let f = run(&mut ki, &st, vec![press(Key::Slash, Modifiers::CTRL)]);
    assert_eq!(
        f.cmds,
        Vec::<String>::new(),
        "repeat of the same held key is ignored"
    );
}

/// IME в быстром поиске (S2-16): итог композиции — в маску, preedit — мимо.
#[test]
fn ime_commit_goes_to_quick_search() {
    let mut ki = input_qs(&KeymapCfg::default());
    let st = state(Focus::Overlay(OverlayKind::QuickSearch));
    let preedit = Event::Ime(ImeEvent::Preedit {
        text: "に".into(),
        active_range_chars: None,
    });
    let f = run(&mut ki, &st, vec![preedit]);
    assert_eq!(f.cmds, Vec::<String>::new());
    let f = run(
        &mut ki,
        &st,
        vec![Event::Ime(ImeEvent::Commit("日本".into()))],
    );
    assert_eq!(f.cmds, ["keymap.qs.input text=日本"]);
    assert_eq!(f.events, []);
}

/// Захват сочетания: нажатие не становится командой, запоминается как в поиске.
#[test]
fn capture_swallows_press_and_keeps_stroke() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.set_capture(true);
    let f = run(&mut ki, &st, vec![press(Key::F5, Modifiers::NONE)]);
    assert_eq!(f.cmds, Vec::<String>::new());
    assert_eq!(f.events, []);
    assert_eq!(ki.take_captured().unwrap().to_string(), "F5");
    assert_eq!(ki.take_captured(), None);
}

/// Клавиша цифрового блока — через записку, как в обычном поиске; сторона модификатора
/// в файл не попадает.
#[test]
fn capture_uses_notes_and_drops_sides() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.set_capture(true);
    ki.notes().push(note(vk::DIVIDE, Mods::CTRL));
    run(&mut ki, &st, vec![press(Key::Slash, Modifiers::CTRL)]);
    assert_eq!(ki.take_captured().unwrap().to_string(), "Ctrl+GraySlash");
    ki.notes().push(KeyNote {
        vk: vk::RCONTROL,
        extended: true,
        mods: Mods::empty(),
        down: true,
    });
    run(&mut ki, &st, vec![press(Key::A, Modifiers::CTRL)]);
    assert_eq!(ki.take_captured().unwrap().to_string(), "Ctrl+A");
}

/// Текст и одиночный Alt при захвате не срабатывают; клавиша, что ещё зажата после
/// захвата, автоповтором команду не вызовет.
#[test]
fn capture_blocks_text_tap_and_held_repeat() {
    let cfg = KeymapCfg {
        quick_search: crate::QsMode::AltTap,
        ..KeymapCfg::default()
    };
    let mut ki = input_qs(&cfg);
    let st = state(Focus::Panel);
    ki.set_capture(true);
    let f = run(
        &mut ki,
        &st,
        vec![
            Event::ModifiersChanged(Modifiers::ALT),
            Event::ModifiersChanged(Modifiers::NONE),
            press(Key::F5, Modifiers::NONE),
            Event::Text("x".into()),
        ],
    );
    assert_eq!(f.cmds, Vec::<String>::new());
    assert_eq!(f.events.len(), 2, "модификаторы идут дальше, текст — нет");
    ki.set_capture(false);
    let f = run(&mut ki, &st, vec![press(Key::F5, Modifiers::NONE)]);
    assert_eq!(f.cmds, Vec::<String>::new(), "та же нажатая клавиша");
    let f = run(
        &mut ki,
        &st,
        vec![Event::Key {
            key: Key::F5,
            physical_key: Some(Key::F5),
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert_eq!(f.cmds, Vec::<String>::new());
    let f = run(&mut ki, &st, vec![press(Key::F5, Modifiers::NONE)]);
    assert_eq!(f.cmds, ["ops.copy"], "после отпускания — как обычно");
}

/// D-13: два нажатия в окне ожидания — одна последовательность; готова, когда окно
/// истекло (дедлайн — в `wake_at`), до этого видна как начатая.
#[test]
fn capture_two_presses_make_one_sequence() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.set_capture(true);
    let ctrl = Modifiers::CTRL;
    let t0 = Instant::now();
    let f = run_at(
        &mut ki,
        &st,
        t0,
        vec![press(Key::K, ctrl), key(Key::K, ctrl, false)],
    );
    assert_eq!(f.cmds, Vec::<String>::new());
    assert_eq!(f.wake_at, Some(t0 + Duration::from_millis(1500)));
    assert_eq!(ki.take_captured_chord(), None, "ждём второе нажатие");
    assert_eq!(ki.capture_progress().unwrap().to_string(), "Ctrl+K");
    let t1 = t0 + Duration::from_millis(1000);
    let f = run_at(&mut ki, &st, t1, vec![press(Key::H, ctrl)]);
    assert_eq!(f.cmds, Vec::<String>::new(), "аккорд не выполняется");
    assert_eq!(f.wake_at, Some(t1 + Duration::from_millis(1500)));
    assert_eq!(ki.take_captured_chord(), None);
    let f = run_at(&mut ki, &st, t1 + Duration::from_millis(1500), vec![]);
    assert_eq!(f.wake_at, None);
    let c = ki.take_captured_chord().unwrap();
    assert_eq!(c.display(Os::Windows), "Ctrl+K Ctrl+H");
    assert_eq!(c, Chord::parse("Ctrl+K Ctrl+H", Os::Windows).unwrap());
    assert_eq!(ki.take_captured_chord(), None);
    assert_eq!(ki.capture_progress(), None);
}

/// D-13: одиночное нажатие — как раньше: `take_captured` сразу; последовательность из
/// одного нажатия — по истечении окна; после окна нажатие начинает новую.
#[test]
fn capture_single_press_as_before() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.set_capture(true);
    let t0 = Instant::now();
    run_at(&mut ki, &st, t0, vec![press(Key::F5, Modifiers::NONE)]);
    assert_eq!(ki.take_captured().unwrap().to_string(), "F5");
    assert_eq!(ki.take_captured_chord(), None);
    let t1 = t0 + Duration::from_millis(1600);
    run_at(
        &mut ki,
        &st,
        t1,
        vec![
            key(Key::F5, Modifiers::NONE, false),
            press(Key::F6, Modifiers::NONE),
        ],
    );
    assert_eq!(ki.take_captured_chord().unwrap().display(Os::Windows), "F5");
    assert_eq!(ki.take_captured().unwrap().to_string(), "F6");
    assert_eq!(ki.capture_progress().unwrap().display(Os::Windows), "F6");
}

/// Не больше трёх нажатий: третье заканчивает последовательность сразу; автоповтор
/// зажатой клавиши её не продолжает; выключение захвата сбрасывает начатое.
#[test]
fn capture_sequence_limit_repeat_and_reset() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.set_capture(true);
    let ctrl = Modifiers::CTRL;
    let t0 = Instant::now();
    run_at(
        &mut ki,
        &st,
        t0,
        vec![
            press(Key::K, ctrl),
            press(Key::K, ctrl),
            key(Key::K, ctrl, false),
            press(Key::B, ctrl),
            key(Key::B, ctrl, false),
            press(Key::C, ctrl),
        ],
    );
    assert_eq!(
        ki.take_captured_chord().unwrap().display(Os::Windows),
        "Ctrl+K Ctrl+B Ctrl+C"
    );
    assert_eq!(ki.capture_progress(), None);
    run_at(
        &mut ki,
        &st,
        t0,
        vec![key(Key::C, ctrl, false), press(Key::D, ctrl)],
    );
    assert!(ki.capture_progress().is_some());
    ki.set_capture(false);
    ki.set_capture(true);
    assert_eq!(ki.capture_progress(), None);
    assert_eq!(ki.take_captured_chord(), None);
}

/// Буквы — на любой раскладке: на русской egui даёт латинскую клавишу, а текст
/// («р») захват поглощает; в последовательность идёт `Ctrl+K H`.
#[test]
fn capture_sequence_on_russian_layout() {
    let mut ki = input_win();
    let st = state(Focus::Panel);
    ki.set_capture(true);
    let t0 = Instant::now();
    let f = run_at(
        &mut ki,
        &st,
        t0,
        vec![
            press(Key::K, Modifiers::CTRL),
            key(Key::K, Modifiers::CTRL, false),
            Event::ModifiersChanged(Modifiers::NONE),
            press(Key::H, Modifiers::NONE),
            Event::Text("р".into()),
        ],
    );
    assert_eq!(f.cmds, Vec::<String>::new());
    assert!(!f.events.iter().any(|e| matches!(e, Event::Text(_))));
    run_at(&mut ki, &st, t0 + Duration::from_secs(2), vec![]);
    assert_eq!(
        ki.take_captured_chord().unwrap().display(Os::Windows),
        "Ctrl+K H"
    );
}
