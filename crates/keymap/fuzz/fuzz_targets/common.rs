//! Проверки целей cargo-fuzz urfin-keymap (S9-15c). Тот же файл подключает запасной
//! путь без nightly — proptest `crates/keymap/tests/fuzz_props.rs`.
#![allow(dead_code)]

use std::borrow::Cow;

use urfin_core::{
    AppState, ArgSpec, CommandMeta, CommandRegistry, Focus, Location, OverlayKind, Panel, Tab,
    TabId, ValueKind,
};
use urfin_keymap::edit::{self, BindPlan, Edit, Target};
use urfin_keymap::handler::{dump_toml, effective_lines};
use urfin_keymap::{
    Chord, ChordState, FlagRegistry, Keymap, Layer, Lookup, Mods, Os, Stroke, When, key_risk,
};

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, args: &'static [ArgSpec], repeatable: bool) -> CommandMeta {
    CommandMeta {
        id,
        title: id,
        category: "fuzz",
        args,
        repeatable,
    }
}

/// Команды с аргументами всех видов; остальные id из пресетов дают
/// `UnknownCommand` — тоже путь разбора.
static COMMANDS: &[CommandMeta] = &[
    meta("ops.copy", &[], false),
    meta("ops.move", &[arg("to", ValueKind::Str, false)], false),
    meta("sel.all", &[], false),
    meta("nav.up", &[], true),
    meta("nav.down", &[], true),
    meta("panel.hidden.toggle", &[], false),
    meta(
        "panel.sort",
        &[
            arg("by", ValueKind::Str, true),
            arg("desc", ValueKind::Bool, false),
        ],
        false,
    ),
    meta("panel.go", &[arg("to", ValueKind::Loc, true)], false),
    meta("tab.select", &[arg("n", ValueKind::Int, true)], false),
    meta("cmdline.insert", &[arg("text", ValueKind::Str, true)], true),
    meta("view.open", &[arg("files", ValueKind::List, false)], false),
    meta(
        "keymap.qs.start",
        &[arg("text", ValueKind::Str, false)],
        false,
    ),
    meta("keymap.qs.next", &[], true),
    meta(
        "keymap.pending",
        &[arg("chord", ValueKind::Str, false)],
        false,
    ),
];

pub fn registry() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    r.register(COMMANDS).expect("fuzz commands");
    r
}

pub fn flags() -> FlagRegistry {
    FlagRegistry::with_builtin()
}

pub const OSES: [Os; 3] = [Os::Windows, Os::Linux, Os::Macos];

/// Первый байт — ОС, остальное — текст (не UTF-8 — с заменой).
pub fn split_os(data: &[u8]) -> (Os, Cow<'_, str>) {
    match data.split_first() {
        Some((b, rest)) => (OSES[usize::from(*b) % 3], String::from_utf8_lossy(rest)),
        None => (Os::Windows, Cow::Borrowed("")),
    }
}

fn state(focus: Focus) -> AppState {
    let loc = Location::local(if cfg!(windows) { r"C:\w" } else { "/w" });
    let mut st = AppState::new(
        Panel::new(Tab::new(TabId(1), loc.clone())),
        Panel::new(Tab::new(TabId(2), loc)),
    );
    st.focus = focus;
    st
}

pub fn states() -> Vec<AppState> {
    [
        Focus::Panel,
        Focus::Cmdline,
        Focus::Viewer,
        Focus::Terminal,
        Focus::Overlay(OverlayKind::QuickSearch),
    ]
    .into_iter()
    .map(state)
    .collect()
}

/// Что даёт поиск сочетания по нажатиям: команды, начало аккорда или промах.
fn resolve(km: &Keymap, chord: &Chord, st: &AppState) -> String {
    let mut pending: Option<ChordState> = None;
    for s in &chord.0 {
        match km.lookup(pending.as_ref(), *s, st) {
            Lookup::Prefix => {
                let mut p = pending.take().unwrap_or_default();
                p.strokes.push(*s);
                pending = Some(p);
            }
            Lookup::Hit(cmds, rep) => {
                let cmds: Vec<String> = cmds.iter().map(ToString::to_string).collect();
                return format!("{} {rep}", cmds.join(", "));
            }
            Lookup::Miss => return "miss".into(),
        }
    }
    "prefix".into()
}

/// Раскладки ведут себя одинаково: каждое сочетание обеих — в каждом фокусе.
fn same_behavior(a: &Keymap, b: &Keymap, ctx: &str) {
    for st in states() {
        for bd in a.bindings().iter().chain(b.bindings()) {
            let (x, y) = (resolve(a, &bd.chord, &st), resolve(b, &bd.chord, &st));
            assert_eq!(x, y, "{:?} in {:?}: {ctx}", bd.chord, st.focus);
        }
    }
}

fn load(os: Os, text: &str) -> std::sync::Arc<Keymap> {
    Keymap::from_str_layers(&[(text, Layer::User)], &registry(), &flags(), os).0
}

/// Поиск по каждой привязке во всех фокусах, keybar, `keys_for`.
fn exercise(km: &Keymap) {
    for st in states() {
        for b in km.bindings() {
            let mut pending: Option<ChordState> = None;
            for s in &b.chord.0 {
                match km.lookup(pending.as_ref(), *s, &st) {
                    Lookup::Prefix => {
                        let mut p = pending.take().unwrap_or_default();
                        p.strokes.push(*s);
                        pending = Some(p);
                    }
                    Lookup::Hit(..) | Lookup::Miss => break,
                }
            }
            for c in b.cmds.iter() {
                let _ = km.keys_for(c, &st);
            }
            let _ = b.when.eval(&st);
        }
        for m in [Mods::empty(), Mods::SHIFT, Mods::CTRL, Mods::ALT] {
            let _ = km.keybar(m, &st);
        }
    }
}

/// Цель `keymap_toml`.
pub fn keymap_toml(os: Os, text: &str) {
    let km = load(os, text);
    exercise(&km);
    let dump = dump_toml(&km);
    let _ = effective_lines(&km);
    let again = load(os, &dump);
    same_behavior(&km, &again, &dump);
}

/// Цель `chord_when`: `сочетание\0when`.
pub fn chord_when(s: &str) {
    let (chord, when) = s.split_once('\0').unwrap_or((s, s));
    for os in OSES {
        if let Ok(c) = Chord::parse(chord, os) {
            let text = c.display(os);
            assert_eq!(Chord::parse(&text, os).as_ref(), Ok(&c), "text = {text:?}");
            for st in &c.0 {
                let _ = key_risk(*st, os);
                let one = st.display(os);
                assert_eq!(Stroke::parse(&one, os).as_ref(), Ok(st), "stroke = {one:?}");
            }
        }
    }
    let flags = flags();
    if let Ok(w) = When::parse(when, &flags) {
        let again = When::parse(w.source(), &flags).expect("source parses");
        let _ = w.specificity();
        let _ = w.ambiguous_with(&again);
        for st in states() {
            assert_eq!(again.eval(&st), w.eval(&st), "when = {when:?}");
            let _ = w.matches(&st);
            let _ = w.matches_as(&st, Focus::Panel);
        }
    }
}

/// Цель `user_edit`: `файл\0сочетание\0контекст\0команда`.
pub fn user_edit(os: Os, s: &str) {
    let mut it = s.splitn(4, '\0');
    let text = it.next().unwrap_or_default();
    let chord = it.next().unwrap_or("Ctrl+K Ctrl+H");
    let context = it.next().unwrap_or("panel");
    let cmd = it.next().unwrap_or("ops.copy");
    let km = load(os, text);
    let Ok(chord) = Chord::parse(chord, os) else {
        let _ = edit::apply(text, &[Edit::Clear], os);
        return;
    };
    let target = Target {
        cmds: [cmd.into()].into_iter().collect(),
        repeat: None,
    };
    let mut plans = edit::plan_bind(
        &km,
        &BindPlan {
            context,
            chord: chord.clone(),
            target: target.clone(),
            old: km.bindings().first().map(|b| (context, b.chord.clone())),
            steal: true,
        },
    );
    plans.extend(edit::plan_unbind(&km, context, &chord));
    if let Some(b) = km.bindings().first() {
        let key = Target::of(b).key();
        plans.extend(edit::plan_reset(&km, &key, &b.src.context));
    }
    plans.push(Edit::Clear);
    for e in &plans {
        let Ok(out) = edit::apply(text, std::slice::from_ref(e), os) else {
            continue;
        };
        let twice = edit::apply(&out, std::slice::from_ref(e), os).expect("result is TOML");
        // Пустой результат следующая правка начинает с шаблона `USER_TEMPLATE`.
        if !out.trim().is_empty() {
            assert_eq!(twice, out, "edit = {e:?}");
        }
        exercise(&load(os, &out));
    }
}
