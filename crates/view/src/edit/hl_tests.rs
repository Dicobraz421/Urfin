//! Приёмка S7-35 и S7-41 через core: подсветка открытого документа (окно, ADR-0024),
//! цвета после правки — только верные (до якоря или сдвиг инертной правкой), новые —
//! окном; `editor.highlight`, свёртка (`editor.fold`), раскладка мимо свёрнутого, курсор
//! в свёрнутом — разворачивает; большой JSON — окно в середине объекта.

use std::fmt::Write as _;
use std::time::Duration;

use urfin_core::Command;

use crate::EditorCfg;
use crate::InboxMsg;
use crate::edit::fold;
use crate::edit::input::tests::{ed, editor, eng, input, mv, pump_until, send};
use crate::syntax::Tag;

const TOML: &str = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n\
[workspace.package]\nversion = \"0.4.2\"\n";

fn tag_at(core: &urfin_core::Core, off: usize) -> Tag {
    ed(core)
        .hl_spans()
        .and_then(|l| {
            l.iter()
                .find(|s| s.start as usize <= off && off < s.end as usize)
                .map(|s| s.tag)
        })
        .unwrap_or(Tag::Plain)
}

fn settled(core: &mut urfin_core::Core) {
    pump_until(core, |c| {
        ed(c).idle() && ed(c).win.is_some() && !ed(c).hl_stats().0 && ed(c).hl_spans().is_some()
    });
}

#[test]
fn colors_follow_edits_and_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = editor(
        dir.path(),
        "Cargo.toml",
        TOML.as_bytes(),
        80,
        EditorCfg::default(),
    );
    settled(&mut core);
    let v = TOML.find("0.4.2").unwrap();
    assert_eq!(tag_at(&core, v), Tag::String);
    assert_eq!(tag_at(&core, TOML.find("resolver").unwrap()), Tag::Plain);
    // Набор перед строкой: цвет сдвигается сразу, затем — новый разбор.
    let doc = ed(&core).id;
    send(
        &mut core,
        InboxMsg::Caret {
            doc,
            off: 0,
            extend: false,
            unit: crate::edit::input::Unit::Char,
            block: None,
        },
    );
    input(&mut core, "# x\n");
    // Правка не в строке и не в комментарии: от начала её строки — без цвета до окна.
    assert_eq!(tag_at(&core, v + 4), Tag::Plain);
    settled(&mut core);
    assert_eq!(tag_at(&core, v + 4), Tag::String);
    assert_eq!(tag_at(&core, 1), Tag::Comment);
    assert!(ed(&core).hl_stats().2.is_some());
    // Набор внутри строки (инертная зона): цвета сдвигаются сразу.
    let at = v + 4 + 2;
    send(
        &mut core,
        InboxMsg::Caret {
            doc,
            off: at as u64,
            extend: false,
            unit: crate::edit::input::Unit::Char,
            block: None,
        },
    );
    input(&mut core, "zz");
    assert_eq!(tag_at(&core, at), Tag::String);
    assert_eq!(tag_at(&core, at + 4), Tag::String);
    assert_eq!(tag_at(&core, 1), Tag::Comment);
    settled(&mut core);
    assert_eq!(tag_at(&core, at + 1), Tag::String);
    // `editor.highlight` выключает и включает.
    core.dispatch(Command::new("editor.highlight"));
    core.pump(Duration::from_millis(1));
    assert!(ed(&core).hl_spans().is_none());
    core.dispatch(Command::new("editor.highlight"));
    settled(&mut core);
    assert_eq!(tag_at(&core, v + 4), Tag::String);
}

/// Пакета грамматики нет (DLL): документ работает, цветов нет, задач больше нет — и правка
/// их не будит (ADR-0024 §8).
#[test]
fn pack_language_without_package() {
    if crate::syntax::available("rust") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut core = editor(
        dir.path(),
        "main.rs",
        b"fn main() {\n    let a = 1;\n}\n",
        80,
        EditorCfg::default(),
    );
    pump_until(&mut core, |c| ed(c).idle() && ed(c).win.is_some());
    assert!(ed(&core).hl_spans().is_none());
    input(&mut core, "x");
    pump_until(&mut core, |c| ed(c).idle());
    assert!(ed(&core).hl_spans().is_none());
    assert_eq!(ed(&core).hl_folds().len(), 0);
}

#[test]
fn fold_hides_rows_and_unfolds_on_caret() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = editor(
        dir.path(),
        "Cargo.toml",
        TOML.as_bytes(),
        80,
        EditorCfg::default(),
    );
    settled(&mut core);
    assert!(ed(&core).hl_folds().iter().any(|f| f.head == 0));
    core.dispatch(Command::new("editor.fold").arg("at", 0i64));
    core.pump(Duration::from_millis(1));
    let e = eng(&core);
    assert!(e.text.folds.at_head(0).is_some());
    let rows = fold::rows_down(&e.text.view(), (0, 0), 3, &e.spec(), &e.text.folds).unwrap();
    let blank = TOML.find("\n\n").unwrap() as u64 + 1;
    let pkg = TOML.find("[workspace.package]").unwrap() as u64;
    assert_eq!(
        rows.iter().map(|r| r.start).collect::<Vec<_>>(),
        vec![0, blank, pkg]
    );
    // Вниз со строки заголовка — мимо скрытого.
    assert_eq!(mv(&mut core, "down"), blank);
    assert_eq!(mv(&mut core, "up"), 0);
    // Строка курсора — заголовок: команда без `at` разворачивает.
    core.dispatch(Command::new("editor.fold"));
    core.pump(Duration::from_millis(1));
    assert!(eng(&core).text.folds.is_empty());
    // Свернуть и поставить курсор внутрь — разворачивает.
    core.dispatch(Command::new("editor.fold"));
    core.pump(Duration::from_millis(1));
    assert!(!eng(&core).text.folds.is_empty());
    let doc = ed(&core).id;
    send(
        &mut core,
        InboxMsg::Caret {
            doc,
            off: TOML.find("resolver").unwrap() as u64,
            extend: false,
            unit: crate::edit::input::Unit::Char,
            block: None,
        },
    );
    assert!(eng(&core).text.folds.is_empty());
}

/// Markdown (решение 2026-11-03): места свёртки у списка и пункта, у цитаты; свёрнутый
/// пункт разворачивает правка внутри (отмена набора в скрытом), курсор в цитате — тоже.
#[test]
fn md_list_and_quote_fold_unfold_on_edit() {
    const MD: &str = "- one
  - two
    deep
  - three
- four

> q1
> q2
";
    let dir = tempfile::tempdir().unwrap();
    let mut core = editor(dir.path(), "a.md", MD.as_bytes(), 80, EditorCfg::default());
    settled(&mut core);
    let at = |s: &str| MD.find(s).unwrap() as u64;
    let heads: Vec<u64> = ed(&core).hl_folds().iter().map(|f| f.head).collect();
    assert_eq!(heads, vec![0, at("  - two"), at("> q1")]);
    // Набор в «deep», курсор — на заголовок «two», свернуть.
    let doc = ed(&core).id;
    let caret = |core: &mut urfin_core::Core, off: u64| {
        send(
            core,
            InboxMsg::Caret {
                doc,
                off,
                extend: false,
                unit: crate::edit::input::Unit::Char,
                block: None,
            },
        );
    };
    caret(&mut core, at("deep"));
    input(&mut core, "z");
    settled(&mut core);
    let two = at("  - two");
    caret(&mut core, two);
    core.dispatch(Command::new("editor.fold").arg("at", i64::try_from(two).unwrap()));
    core.pump(Duration::from_millis(1));
    assert!(eng(&core).text.folds.at_head(two).is_some());
    // Отмена правки в скрытом — разворачивает.
    core.dispatch(Command::new("editor.undo"));
    core.pump(Duration::from_millis(1));
    assert!(eng(&core).text.folds.is_empty());
    // Цитата: свернуть, курсор во вторую строку — разворачивает.
    settled(&mut core);
    let q = at("> q1");
    core.dispatch(Command::new("editor.fold").arg("at", i64::try_from(q).unwrap()));
    core.pump(Duration::from_millis(1));
    assert!(eng(&core).text.folds.at_head(q).is_some());
    caret(&mut core, at("> q2") + 2);
    assert!(eng(&core).text.folds.is_empty());
}

/// Большой JSON (несколько точек): окно в середине многоэкранного объекта — ключи без
/// цвета, значения по ролям; правка далеко выше (открыта кавычка) — окно верно сразу после
/// пересчёта.
#[test]
fn big_json_window_in_the_middle() {
    let mut text = String::from("{\"items\": [\n");
    for i in 0..20_000 {
        let _ = writeln!(text, "  {{\"id\": {i}, \"name\": \"n{i}\", \"ok\": true}},");
    }
    text.push_str("  {\"id\": -1}\n]}\n");
    assert!(text.len() > 3 * crate::syntax::marks::STEP as usize);
    let dir = tempfile::tempdir().unwrap();
    let mut core = editor(
        dir.path(),
        "big.json",
        text.as_bytes(),
        80,
        EditorCfg::default(),
    );
    settled(&mut core);
    // Перейти в середину.
    let mid = text.find("\"id\": 12345").unwrap();
    let doc = ed(&core).id;
    send(
        &mut core,
        InboxMsg::ScrollTo {
            doc,
            off: mid as u64,
        },
    );
    pump_until(&mut core, |c| {
        !ed(c).hl_stats().0
            && ed(c)
                .hl_spans()
                .is_some_and(|l| l.iter().any(|s| s.start as usize > mid))
    });
    assert_eq!(tag_at(&core, mid), Tag::Plain, "ключ");
    assert_eq!(tag_at(&core, mid + 7), Tag::Number, "число");
    let name = mid + text[mid..].find("\"n12345\"").unwrap();
    assert_eq!(tag_at(&core, name), Tag::String);
    let t = mid + text[mid..].find("true").unwrap();
    assert_eq!(tag_at(&core, t), Tag::Number);
}
