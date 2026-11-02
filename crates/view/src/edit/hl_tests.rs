//! Приёмка S7-35 через core: подсветка открытого документа, сдвиг и новые цвета после
//! правки, `editor.highlight`, свёртка (`editor.fold`), раскладка мимо свёрнутого,
//! курсор в свёрнутом — разворачивает.

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
        ed(c).idle() && ed(c).hl.as_ref().is_some_and(|h| !h.busy())
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
    assert_eq!(tag_at(&core, v + 4), Tag::String);
    settled(&mut core);
    assert_eq!(tag_at(&core, v + 4), Tag::String);
    assert_eq!(tag_at(&core, 1), Tag::Comment);
    assert!(ed(&core).hl.as_ref().unwrap().last_ms.is_some());
    // `editor.highlight` выключает и включает.
    core.dispatch(Command::new("editor.highlight"));
    core.pump(Duration::from_millis(1));
    assert!(ed(&core).hl_spans().is_none());
    core.dispatch(Command::new("editor.highlight"));
    settled(&mut core);
    assert_eq!(tag_at(&core, v + 4), Tag::String);
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
