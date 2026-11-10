use proptest::prelude::*;
use urfin_core::{AppState, Focus, Location, OverlayKind, Panel, PromptId, Side, Tab, TabId};

use super::*;

fn state(focus: Focus) -> AppState {
    let loc = Location::local(if cfg!(windows) {
        r"C:\work\a"
    } else {
        "/work/a"
    });
    let mut st = AppState::new(
        Panel::new(Tab::new(TabId(1), loc.clone())),
        Panel::new(Tab::new(TabId(2), loc)),
    );
    st.focus = focus;
    st
}

const FOCUSES: [Focus; 12] = [
    Focus::Panel,
    Focus::Cmdline,
    Focus::Prompt(PromptId(1)),
    Focus::Viewer,
    Focus::Editor,
    Focus::Terminal,
    Focus::Overlay(OverlayKind::Menu),
    Focus::Overlay(OverlayKind::QuickSearch),
    Focus::Overlay(OverlayKind::InlineEdit),
    Focus::Overlay(OverlayKind::Draft),
    Focus::Overlay(OverlayKind::Palette),
    Focus::Overlay(OverlayKind::Find),
];

fn w(s: &str) -> When {
    When::parse(s, &FlagRegistry::with_builtin()).unwrap()
}

/// Таблица истинности атомов фокуса для всех `Focus`.
#[test]
fn focus_truth_table() {
    let cases: [(&str, &[usize]); 11] = [
        ("panel", &[0]),
        ("cmdline", &[1]),
        ("prompt", &[2]),
        ("viewer", &[3]),
        ("editor", &[4]),
        ("terminal", &[5]),
        ("overlay", &[6, 7, 8, 9, 10, 11]),
        ("overlay:menu", &[6]),
        ("overlay:quick_search", &[7]),
        ("overlay:find", &[11]),
        // ADR-0015: всё, кроме терминала и вопроса.
        ("global", &[0, 1, 3, 4, 6, 7, 8, 9, 10, 11]),
    ];
    for (expr, truthy) in cases {
        let when = w(expr);
        for (i, f) in FOCUSES.iter().enumerate() {
            assert_eq!(
                when.eval(&state(*f)),
                truthy.contains(&i),
                "{expr} in {f:?}"
            );
        }
    }
}

/// S8 §2.5: `in_search` — панель на `search:`, `cmp:`; `search_screen` — экран поиска.
#[test]
fn search_flags() {
    let (panel, screen) = (w("panel && in_search"), w("overlay && search_screen"));
    assert!(!panel.eval(&state(Focus::Panel)));
    for (backend, want) in [("search", true), ("cmp", true), ("du", false)] {
        let mut loc = Location::local(if cfg!(windows) { r"C:\w" } else { "/w" });
        loc.backend = backend.into();
        let st = AppState::new(
            Panel::new(Tab::new(TabId(1), loc.clone())),
            Panel::new(Tab::new(TabId(2), loc)),
        );
        assert_eq!(panel.eval(&st), want, "{backend}");
    }
    assert!(screen.eval(&state(Focus::Overlay(OverlayKind::Search))));
    assert!(!screen.eval(&state(Focus::Overlay(OverlayKind::Find))));
    assert!(!screen.eval(&state(Focus::Panel)));
}

/// S8 §3.1: `in_archive` — у адреса активной вкладки есть `outer`.
#[test]
fn archive_flag() {
    let f = w("panel && in_archive");
    assert!(!f.eval(&state(Focus::Panel)));
    let file = Location::local(if cfg!(windows) {
        r"C:\w\a.zip"
    } else {
        "/w/a.zip"
    });
    let inner = Location {
        backend: "zip".into(),
        authority: None,
        path: "/".into(),
        outer: Some(std::sync::Arc::new(file)),
    };
    let st = AppState::new(
        Panel::new(Tab::new(TabId(1), inner)),
        Panel::new(Tab::new(
            TabId(2),
            Location::local(if cfg!(windows) { r"C:\w" } else { "/w" }),
        )),
    );
    assert!(f.eval(&st));
}

#[test]
fn global_is_false_in_terminal_and_prompt() {
    let g = w("global");
    assert!(!g.eval(&state(Focus::Terminal)));
    assert!(!g.eval(&state(Focus::Prompt(PromptId(7)))));
    assert!(g.eval(&state(Focus::Overlay(OverlayKind::Menu))));
    assert_eq!(
        g.matches(&state(Focus::Panel)),
        Some(0),
        "global — специфичность 0"
    );
}

#[test]
fn operators_and_specificity() {
    let st = state(Focus::Panel);
    assert_eq!(w("panel").matches(&st), Some(1));
    assert_eq!(w("panel && !has_selection").matches(&st), Some(2));
    assert_eq!(w("panel && has_selection").matches(&st), None);
    assert_eq!(w("cmdline || panel").matches(&st), Some(1));
    let e = w("panel || (cmdline && os_windows)");
    assert_eq!(e.specificity(), 2);
    assert_eq!(e.matches(&st), Some(1));
    assert_eq!(e.matches(&state(Focus::Cmdline)).is_some(), cfg!(windows));
    assert_eq!(w("  ( ( panel ) )  ").matches(&st), Some(1));
    assert!(w("!terminal").eval(&st));
    assert_eq!(w("os_windows").eval(&st), cfg!(windows));
    // `*` — любой контекст.
    let any = When::parse(" * ", &FlagRegistry::with_builtin()).unwrap();
    assert!(any.is_any() && any.eval(&state(Focus::Terminal)));
}

#[test]
fn builtin_flags() {
    let mut st = state(Focus::Panel);
    let qs = w("qs");
    assert!(!qs.eval(&st));
    st.focus = Focus::Overlay(OverlayKind::QuickSearch);
    assert!(qs.eval(&st));
    // Строка 0 у каталога с родителем — `..`.
    let st = state(Focus::Panel);
    assert!(w("on_parent_row").eval(&st));
    assert!(!w("has_selection").eval(&st));
}

/// Неизвестный флаг — `UnknownFlag` с позицией; прочие ошибки — тоже с позицией.
#[test]
fn errors() {
    let r = FlagRegistry::with_builtin();
    let e = |s: &str| When::parse(s, &r).unwrap_err();
    let u = e("panel && nope");
    assert_eq!(u.kind, WhenErrorKind::UnknownFlag("nope".into()));
    assert_eq!(u.pos, 9);
    assert_eq!(
        e("overlay:bogus").kind,
        WhenErrorKind::UnknownOverlay("bogus".into())
    );
    assert_eq!(e("").kind, WhenErrorKind::Empty);
    assert_eq!(e("(panel").kind, WhenErrorKind::Expected("')'"));
    assert_eq!(e("panel *").kind, WhenErrorKind::StarNotAlone);
    assert_eq!(
        e("panel &&").kind,
        WhenErrorKind::Expected("context or flag")
    );
    let deep = (0..20).map(|_| "panel").collect::<Vec<_>>().join(" || (");
    assert_eq!(
        e(&format!("{deep}{}", ")".repeat(19))).kind,
        WhenErrorKind::TooDeep
    );
    // Флаг, зарегистрированный крейтом, — известен.
    let mut r2 = FlagRegistry::with_builtin();
    r2.register("cmdline_empty", |_| true);
    assert!(When::parse("cmdline && cmdline_empty", &r2).is_ok());
}

/// Выражение для сверки с наивным интерпретатором.
#[derive(Clone, Debug)]
enum Expr {
    Atom(&'static str),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

const ATOMS: [&str; 9] = [
    "panel",
    "cmdline",
    "terminal",
    "overlay",
    "overlay:menu",
    "global",
    "prompt",
    "f_left",
    "f_vis",
];

fn render(e: &Expr) -> String {
    match e {
        Expr::Atom(a) => (*a).to_owned(),
        Expr::Not(x) => format!("!({})", render(x)),
        Expr::And(a, b) => format!("({} && {})", render(a), render(b)),
        Expr::Or(a, b) => format!("({} || {})", render(a), render(b)),
    }
}

fn naive(e: &Expr, st: &AppState) -> bool {
    match e {
        Expr::Atom(a) => match *a {
            "panel" => st.focus == Focus::Panel,
            "cmdline" => st.focus == Focus::Cmdline,
            "terminal" => st.focus == Focus::Terminal,
            "prompt" => matches!(st.focus, Focus::Prompt(_)),
            "overlay" => matches!(st.focus, Focus::Overlay(_)),
            "overlay:menu" => st.focus == Focus::Overlay(OverlayKind::Menu),
            "global" => !matches!(st.focus, Focus::Terminal | Focus::Prompt(_)),
            "f_left" => st.active == Side::Left,
            "f_vis" => st.panel(Side::Right).visible,
            _ => unreachable!(),
        },
        Expr::Not(x) => !naive(x, st),
        Expr::And(a, b) => naive(a, st) && naive(b, st),
        Expr::Or(a, b) => naive(a, st) || naive(b, st),
    }
}

fn expr() -> impl Strategy<Value = Expr> {
    let leaf = prop::sample::select(&ATOMS[..]).prop_map(Expr::Atom);
    leaf.prop_recursive(3, 12, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|x| Expr::Not(Box::new(x))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| Expr::Or(Box::new(a), Box::new(b))),
        ]
    })
}

proptest! {
    #[test]
    fn matches_naive_interpreter(e in expr()) {
        let mut flags = FlagRegistry::with_builtin();
        flags.register("f_left", |st| st.active == Side::Left);
        flags.register("f_vis", |st| st.panel(Side::Right).visible);
        let when = When::parse(&render(&e), &flags).unwrap();
        for f in FOCUSES {
            for (active, visible) in [(Side::Left, true), (Side::Right, false), (Side::Right, true)] {
                let mut st = state(f);
                st.active = active;
                st.panel_mut(Side::Right).visible = visible;
                prop_assert_eq!(when.eval(&st), naive(&e, &st), "{}", render(&e));
            }
        }
    }
}
