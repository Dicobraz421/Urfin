//! S7-15, S7-34: открытие, кодировки, EOL, счётчик изменений, переход, команды редактора
//! через core (документ — на движке).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use encoding_rs::{Encoding, IBM866, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251};
use urfin_core::{Boot, Command, Core, Focus, Location, Services, UiEvent, Waker};

use super::EdState;
use super::input::Unit;
use crate::detect::Eol;
use crate::history::HistKind;
use crate::{Doc, EditorCfg, EditorDoc, InboxMsg, ViewState, ViewerCfg, register_with};

const RU: &str = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь.";

/// Текст в кодировке (UTF-16 — вручную, `encoding_rs` его не кодирует).
fn bytes_of(text: &str, e: &'static Encoding, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if e == UTF_16LE || e == UTF_16BE {
        if bom {
            out.extend_from_slice(if e == UTF_16BE {
                &[0xFE, 0xFF]
            } else {
                &[0xFF, 0xFE]
            });
        }
        for u in text.encode_utf16() {
            out.extend_from_slice(&if e == UTF_16BE {
                u.to_be_bytes()
            } else {
                u.to_le_bytes()
            });
        }
        return out;
    }
    if bom {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    let (b, _, bad) = e.encode(text);
    assert!(!bad, "{}", e.name());
    out.extend_from_slice(&b);
    out
}

/// Текст с переводами строк вида `eol`.
fn lines(body: &str, n: usize, eol: &str) -> String {
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(body);
        s.push_str(eol);
    }
    s
}

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

fn core(dir: &Path) -> Core {
    let root = Location::local(dir);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, ViewerCfg::default(), EditorCfg::default());
    core.pump(Duration::from_millis(5));
    core
}

fn pump_until(core: &mut Core, mut f: impl FnMut(&Core) -> bool) {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        if f(core) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn view(core: &Core) -> &ViewState {
    core.state().ext.get::<ViewState>().unwrap()
}

fn ed(core: &Core) -> &EditorDoc {
    let v = view(core);
    match v.doc(v.active().unwrap()) {
        Some(Doc::Editor(d)) => d,
        _ => panic!("не редактор"),
    }
}

/// Файл → редактор на движке, открытие закончено.
fn open(core: &mut Core, path: &Path) {
    core.dispatch(Command::new("editor.open").arg("path", Location::local(path)));
    core.pump(Duration::from_millis(1));
    pump_until(core, |c| !matches!(ed(c).state, EdState::Loading { .. }));
}

fn send(core: &mut Core, msg: InboxMsg) {
    view(core).inbox.sender().send(msg);
    core.pump(Duration::from_millis(5));
}

/// Весь текст документа `doc` — `text` (выделить всё мышью и набрать).
pub(super) fn set_text(core: &mut Core, doc: crate::DocId, text: &str) {
    for (off, extend) in [(0, false), (u64::MAX, true)] {
        send(
            core,
            InboxMsg::Caret {
                doc,
                off,
                extend,
                unit: Unit::Char,
            },
        );
    }
    send(
        core,
        InboxMsg::Input {
            doc,
            text: text.into(),
        },
    );
}

fn ext(core: &mut Core, id: &str) -> Vec<urfin_core::Args> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id: i, args } if i.as_str() == id => Some(args),
            _ => None,
        })
        .collect()
}

/// Открытие → Ready на движке; правка → ● и счётчик шапки (фоном, по кускам); закрытие с
/// правками — вопрос; закрытие → запись правки в истории; повторное открытие — курсор из
/// неё; `line`/`col` команды — с 1.
#[test]
fn open_edit_count_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf.toml");
    let text = "a = 1\r\nb = 2\r\nc = 3\r\n";
    std::fs::write(&path, text).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    assert_eq!(core.state().focus, Focus::Editor);
    let d = ed(&core);
    let doc = d.id;
    assert_eq!(d.state, EdState::Ready);
    assert!(d.engine.is_some());
    assert!(d.enc.is_utf8() && d.enc.auto && !d.bom && !d.lossy && !d.readonly);
    assert_eq!(
        (d.eol, d.eol_new, d.lang),
        (Eol::CrLf, Eol::CrLf, Some("toml"))
    );
    assert_eq!(d.stamp.size, text.len() as u64);
    assert!(d.stamp.file_id.is_some());

    set_text(&mut core, doc, "a = 1\r\nB = 2\r\nc = 3\r\nd = 4\r\n");
    assert!(ed(&core).dirty);
    pump_until(&mut core, |c| !ed(c).changes.is_empty());
    let ch = &ed(&core).changes;
    assert!(ch.added + ch.modified > 0, "{ch:?}");

    // Курсор — на строку 4, колонку 3.
    core.dispatch(Command::new("editor.goto").arg("to", "4:3"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ed(&core).cursor, (3, 2));

    // С правками закрытие ждёт подтверждения (S7-17); Esc при вопросе — вопрос убирается.
    core.dispatch(Command::new("editor.close"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ed(&core).ask, Some(super::EdAsk::Close));
    core.dispatch(Command::new("editor.close"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ed(&core).ask, None);
    core.dispatch(Command::new("editor.close"));
    core.dispatch(Command::new("editor.answer").arg("choice", "discard"));
    core.pump(Duration::from_millis(5));
    assert!(view(&core).is_empty());
    assert_eq!(core.state().focus, Focus::Panel);
    let loc = Location::local(&path);
    let m = view(&core).history.get_kind(&loc, HistKind::Edit).unwrap();
    assert_eq!((m.top, m.hscroll), (3, 2));
    assert!(
        view(&core).history.get(&loc).is_none(),
        "не запись просмотра"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text,
        "не сохранено"
    );

    // Повторно — курсор из истории (строки 4 нет: файл не сохранён — конец).
    open(&mut core, &path);
    assert_eq!(ed(&core).cursor.0, 3);
    // Тот же файл — тот же экран; `line`/`col` — с 1.
    core.dispatch(Command::new("editor.open").arg("path", Location::local(&path)));
    core.pump(Duration::from_millis(5));
    assert_eq!(view(&core).screens.len(), 1);
    let other = dir.path().join("b.txt");
    std::fs::write(&other, "x\nyyyyyy\n").unwrap();
    core.dispatch(
        Command::new("editor.open")
            .arg("path", Location::local(&other))
            .arg("line", 2)
            .arg("col", 5),
    );
    pump_until(&mut core, |c| ed(c).is_ready() && ed(c).cursor == (1, 4));
    assert_eq!(view(&core).screens.len(), 2);
}

/// F8 без правок перечитывает файл в новой кодировке; с правками — только кодировка
/// сохранения и тост. `editor.eol` — EOL новых строк и перевод документа одной правкой.
#[test]
fn codepage_reinterprets_or_sets_save_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ru.txt");
    std::fs::write(&path, bytes_of(&lines(RU, 30, "\r\n"), WINDOWS_1251, false)).unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    let doc = ed(&core).id;
    assert_eq!(ed(&core).enc.enc, WINDOWS_1251);
    assert!(ed(&core).enc.auto);

    core.dispatch(Command::new("editor.codepage").arg("cp", "ibm866"));
    pump_until(&mut core, |c| ed(c).is_ready() && ed(c).enc.enc == IBM866);
    assert!(!ed(&core).enc.auto);
    // F8 по кольцу: auto → utf-8 → windows-1251 → ibm866 → auto.
    core.dispatch(Command::new("editor.codepage"));
    pump_until(&mut core, |c| ed(c).is_ready() && ed(c).enc.auto);
    assert_eq!(ed(&core).enc.enc, WINDOWS_1251);

    set_text(&mut core, doc, "правка\r\n");
    assert!(ed(&core).dirty);
    core.take_ui_events();
    core.dispatch(Command::new("editor.codepage").arg("cp", "utf-8"));
    core.pump(Duration::from_millis(5));
    let d = ed(&core);
    assert_eq!(d.enc.enc, UTF_8);
    assert!(d.is_ready() && d.dirty, "без перечитывания");
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 1);

    core.dispatch(Command::new("editor.eol").arg("to", "lf"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ed(&core).eol_new, Eol::Lf);
    pump_until(&mut core, |c| ed(c).eol == Eol::Lf);
    core.dispatch(Command::new("editor.codepage_menu"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ext(&mut core, "editor.codepage_menu").len(), 1);
}

/// Лимита размера нет (17 МБ — на движке); файла нет — тост и документ закрыт; атрибут
/// «только чтение» — `readonly`.
#[test]
fn big_missing_and_readonly() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.log");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(17 << 20).unwrap();
    drop(f);
    let mut core = core(dir.path());
    open(&mut core, &big);
    assert_eq!(ed(&core).state, EdState::Ready);
    assert!(ed(&core).engine.is_some());

    core.take_ui_events();
    core.dispatch(
        Command::new("editor.open").arg("path", Location::local(dir.path().join("nope.txt"))),
    );
    pump_until(&mut core, |c| view(c).screens.len() == 1);
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 1);

    let ro = dir.path().join("ro.txt");
    std::fs::write(&ro, "ro").unwrap();
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&ro, perm.clone()).unwrap();
    open(&mut core, &ro);
    assert!(ed(&core).readonly);
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "снять атрибут, чтобы tempdir удалился"
    )]
    perm.set_readonly(false);
    std::fs::set_permissions(&ro, perm).unwrap();
}

#[path = "save_tests.rs"]
mod save_tests;

/// S7-18, S7-34: F7 / Ctrl+F7 / Alt+F8 без образца — фокус строке и `Ext` фиче UI;
/// `editor.goto to=…` проверяется здесь, ставит курсор движком и возвращает фокус
/// редактору; Esc строки — `editor.search_cancel`.
#[test]
fn find_and_goto_route() {
    use urfin_core::OverlayKind;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "one\ntwo two\n").unwrap();
    let mut core = core(dir.path());
    open(&mut core, &path);
    let doc = i64::from(ed(&core).id.0);
    core.take_ui_events();
    let find = Focus::Overlay(OverlayKind::Find);
    for id in ["editor.search", "editor.replace", "editor.goto"] {
        core.dispatch(Command::new(id));
        core.pump(Duration::from_millis(1));
        assert_eq!(core.state().focus, find, "{id}");
        assert_eq!(ext(&mut core, id)[0].int("doc"), Some(doc), "{id}");
    }
    core.dispatch(Command::new("editor.goto").arg("to", "abc"));
    core.pump(Duration::from_millis(1));
    assert_eq!(core.state().focus, find, "не разобрано");
    core.dispatch(Command::new("editor.goto").arg("to", " 2:3 "));
    core.pump(Duration::from_millis(1));
    assert_eq!(ed(&core).cursor, (1, 2));
    assert_eq!(core.state().focus, Focus::Editor);
    core.dispatch(Command::new("editor.goto").arg("to", "-1"));
    core.pump(Duration::from_millis(1));
    assert_eq!(ed(&core).cursor, (0, 0));
    core.dispatch(Command::new("editor.search"));
    core.dispatch(Command::new("editor.search_cancel"));
    core.pump(Duration::from_millis(1));
    assert_eq!(core.state().focus, Focus::Editor);
    assert_eq!(ext(&mut core, "editor.search_cancel").len(), 1);
    core.dispatch(Command::new("editor.search_next"));
    core.pump(Duration::from_millis(1));
    assert_eq!(ext(&mut core, "editor.search_next").len(), 1);
}
