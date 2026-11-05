use std::sync::Arc;

use urfin_core::Focus;

use super::*;
use crate::compile::tests::{flags, reg, state};
use crate::keymap::Layer;
use crate::{Keymap, Lookup, Stroke, WarnKind, key_risk};

const OS: Os = Os::Windows;

fn chord(s: &str) -> Chord {
    Chord::parse(s, OS).unwrap()
}

fn bind(ctx: &str, c: &str, cmd: &str) -> Edit {
    Edit::Bind {
        context: ctx.into(),
        chord: chord(c),
        target: Target::command(cmd),
    }
}

/// Пресет теста и пользовательский файл с текстом `user`.
fn load(user: &str) -> Arc<Keymap> {
    let base = "format = 1\nname = \"p\"\n[context.panel]\n\"F5\" = \"a.one\"\n\"F6\" = \"a.two\"\n[context.viewer]\n\"F7\" = \"a.one\"\n";
    let u = format!("name = \"mine\"\n{}", user.replace("name = \"user\"\n", ""));
    Keymap::from_str_layers(
        &[(base, Layer::Preset), (&u, Layer::User)],
        &reg(),
        &flags(),
        OS,
    )
    .0
}

fn of(km: &Keymap, c: &str) -> Vec<String> {
    crate::catalog::live(km.bindings())
        .iter()
        .filter(|b| b.chord == chord(c))
        .map(|b| b.cmds[0].id.as_str().to_owned())
        .collect()
}

#[test]
fn comments_and_order_survive() {
    let text = "# моя шапка\nformat = 1\n\n# панели\n[context.panel]\n\"Ctrl+B\" = \"a.one\" # важная\n# вторая\n\"Ctrl+D\" = \"a.two\"\n\n[context.viewer]\n\"Ctrl+E\" = \"a.one\"\n";
    let out = apply(text, &[bind("panel", "Ctrl+Q", "a.three")], OS).unwrap();
    assert!(out.contains("# моя шапка"));
    assert!(out.contains("# панели"));
    assert!(out.contains("# важная"));
    assert!(out.contains("# вторая"));
    let b = out.find("Ctrl+B").unwrap();
    let d = out.find("Ctrl+D").unwrap();
    let q = out.find("Ctrl+Q").unwrap();
    let v = out.find("[context.viewer]").unwrap();
    assert!(b < d && d < q && q < v, "{out}");
}

#[test]
fn replacing_value_keeps_line_comment() {
    let text = "[context.panel]\n\"ctrl+b\" = \"a.one\" # важная\n";
    let out = apply(text, &[bind("panel", "Ctrl+B", "a.two")], OS).unwrap();
    assert_eq!(out, "[context.panel]\n\"ctrl+b\" = \"a.two\" # важная\n");
}

#[test]
fn empty_file_gets_template_and_reads_back() {
    let out = apply("", &[bind("panel", "Ctrl+Q", "a.three")], OS).unwrap();
    assert!(out.starts_with("# Свои клавиши Urfin"));
    let km = load(&out);
    assert_eq!(of(&km, "Ctrl+Q"), ["a.three"]);
}

#[test]
fn complex_context_is_quoted_and_found_again() {
    let ctx = "panel && cmdline_empty";
    let out = apply("", &[bind(ctx, "Ctrl+Q", "a.three")], OS).unwrap();
    assert!(
        out.contains("[context.\"panel && cmdline_empty\"]"),
        "{out}"
    );
    let spaced = "panel&&cmdline_empty";
    let out = apply(&out, &[bind(spaced, "Ctrl+W", "a.three")], OS).unwrap();
    assert_eq!(
        out.matches(
            "
[context."
        )
        .count(),
        1,
        "{out}"
    );
}

#[test]
fn args_and_repeat_round_trip() {
    let target = Target {
        cmds: std::iter::once("panel.sort key=mtime".into()).collect(),
        repeat: Some(true),
    };
    let out = apply(
        "",
        &[Edit::Bind {
            context: "panel".into(),
            chord: chord("Ctrl+Q"),
            target: target.clone(),
        }],
        OS,
    )
    .unwrap();
    let km = load(&out);
    let b = km
        .bindings()
        .iter()
        .find(|b| b.chord == chord("Ctrl+Q"))
        .unwrap();
    assert_eq!(Target::of(b), target);
}

#[test]
fn broken_file_is_not_rewritten() {
    let e = apply("[context.panel\n", &[bind("panel", "F1", "a.three")], OS);
    assert!(matches!(e, Err(EditError::Parse(_))));
}

#[test]
fn odd_context_is_refused() {
    let e = apply("context = 1\n", &[bind("panel", "F1", "a.three")], OS);
    assert!(matches!(e, Err(EditError::Layout)));
}

#[test]
fn unbind_own_line_removes_it_preset_gets_false() {
    let user = apply("", &[bind("panel", "Ctrl+Q", "a.three")], OS).unwrap();
    let km = load(&user);
    let out = apply(&user, &plan_unbind(&km, "panel", &chord("Ctrl+Q")), OS).unwrap();
    assert!(!out.contains("Ctrl+Q"), "{out}");
    let out = apply(&user, &plan_unbind(&km, "panel", &chord("F5")), OS).unwrap();
    assert!(out.contains("F5 = false"), "{out}");
    assert_eq!(of(&load(&out), "F5"), Vec::<String>::new());
}

#[test]
fn reset_row_returns_preset() {
    // F5 сняли, «a.one» дали Ctrl+Q; F6 заняли другой командой.
    let user = apply(
        "",
        &[
            Edit::Off {
                context: "panel".into(),
                chord: chord("F5"),
            },
            bind("panel", "Ctrl+Q", "a.one"),
            bind("panel", "F6", "a.three"),
        ],
        OS,
    )
    .unwrap();
    let km = load(&user);
    let at = crate::catalog::place("panel");
    let out = apply(&user, &plan_reset(&km, "a.one", at), OS).unwrap();
    assert!(!out.contains("F5") && !out.contains("Ctrl+Q"), "{out}");
    assert!(out.contains("F6"), "чужая правка остаётся: {out}");
    let out = apply(&out, &plan_reset(&km, "a.two", at), OS).unwrap();
    let list = |km: &Keymap| {
        let mut v: Vec<String> = crate::catalog::live(km.bindings())
            .iter()
            .map(|b| format!("{} {}", b.chord, b.cmds[0]))
            .collect();
        v.sort();
        v
    };
    assert_eq!(list(&load(&out)), list(&load("")));
}

#[test]
fn clear_keeps_header_comment() {
    let user = apply(
        "# моё\nformat = 1\n",
        &[bind("panel", "Ctrl+Q", "a.three")],
        OS,
    )
    .unwrap();
    let out = apply(&user, &[Edit::Clear], OS).unwrap();
    assert!(out.contains("# моё"));
    assert!(!out.contains("Ctrl+Q"));
    assert_eq!(of(&load(&out), "F5"), ["a.one"]);
}

#[test]
fn replace_moves_key_within_row() {
    let km = load("");
    let plan = BindPlan {
        context: "panel",
        chord: chord("Ctrl+Q"),
        target: Target::command("a.one"),
        old: Some(("panel", chord("F5"))),
        steal: false,
    };
    let km2 = load(&apply("", &plan_bind(&km, &plan), OS).unwrap());
    assert_eq!(of(&km2, "F5"), Vec::<String>::new());
    assert_eq!(of(&km2, "Ctrl+Q"), ["a.one"]);
}

#[test]
fn steal_takes_key_from_overlapping_context_only() {
    let user = "[context.\"panel || cmdline\"]\n\"F9\" = \"a.two\"\n[context.viewer]\n\"F9\" = \"a.three\"\n";
    let km = load(user);
    let plan = BindPlan {
        context: "panel",
        chord: chord("F9"),
        target: Target::command("a.one"),
        old: None,
        steal: true,
    };
    let out = apply(user, &plan_bind(&km, &plan), OS).unwrap();
    let got = of(&load(&out), "F9");
    assert_eq!(got, ["a.three", "a.one"], "{out}");
}

/// Решение 2026-11-05: `Ctrl+Alt+буква` и `AltGr` не запрещены. Записывается, читается
/// обратно, срабатывает; пояснение есть, баннера нет.
#[test]
fn altgr_risk_binds_reads_back_and_fires() {
    let c = chord("Ctrl+Alt+K");
    assert_eq!(key_risk(c.0[0], OS), Some(WarnKind::AltGrRisk));
    let km = load("");
    let plan = BindPlan {
        context: "panel",
        chord: c,
        target: Target::command("a.three"),
        old: None,
        steal: false,
    };
    let out = apply("", &plan_bind(&km, &plan), OS).unwrap();
    assert!(out.contains("\"Ctrl+Alt+K\" = \"a.three\""), "{out}");
    let km = load(&out);
    let w: Vec<_> = km
        .warnings()
        .iter()
        .filter(|w| matches!(w.kind, WarnKind::AltGrRisk))
        .collect();
    assert_eq!(w.len(), 1);
    assert!(w[0].advisory(), "пояснение, не баннер");
    let st = state(Focus::Panel);
    let s = Stroke::parse("Ctrl+Alt+K", OS).unwrap();
    let Lookup::Hit(cmds, _) = km.lookup(None, s, &st) else {
        panic!("не сработало");
    };
    assert_eq!(cmds[0].id.as_str(), "a.three");
}
