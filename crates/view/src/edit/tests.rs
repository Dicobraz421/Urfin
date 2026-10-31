//! S7-15: загрузка, кодировки, EOL, полоса изменений, команды редактора через core.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use encoding_rs::{
    Encoding, IBM866, KOI8_R, KOI8_U, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251,
    WINDOWS_1252,
};
use urfin_core::{Boot, CancelToken, Command, Core, Focus, Location, Services, UiEvent, Waker};

use super::load::{LoadError, decode, eol_new, eol_of};
use super::{Change, EdState, LoadOpts, encode};
use crate::config::EolCfg;
use crate::detect::Eol;
use crate::history::HistKind;
use crate::inbox::After;
use crate::{Doc, EditorCfg, EditorDoc, InboxMsg, ViewState, ViewerCfg, register_with};

const RU: &str = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь.";
const FR: &str = "Le marché du samedi s'installe très tôt sur la place de l'église.";
const JA: &str = "今日は朝から雨が降っていたので、私は図書館へ行って本を読みました。";

fn opts(enc: Option<&'static Encoding>) -> LoadOpts {
    LoadOpts {
        enc,
        limit: 64 << 20,
        tld: Some(b"ru"),
        eol: EolCfg::File,
        name: "a.txt".into(),
        new: None,
    }
}

fn run(bytes: &[u8], enc: Option<&'static Encoding>) -> super::Decoded {
    decode(
        &mut Cursor::new(bytes),
        Some(bytes.len() as u64),
        &opts(enc),
        &CancelToken::new(),
        &mut |_| {},
    )
    .unwrap()
}

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

/// Текст с переводами строк вида `eol` (`mixed` — все три по очереди).
fn lines(body: &str, n: usize, eol: &str) -> String {
    let mixed = ["\r\n", "\n", "\r"];
    let mut s = String::new();
    for i in 0..n {
        s.push_str(body);
        s.push_str(if eol == "mixed" { mixed[i % 3] } else { eol });
    }
    s
}

/// Приёмка: загрузка → кодирование без правок = те же байты (кодировки V5, BOM, все
/// виды EOL); и с кодировкой вручную, и по детекту (если он не потерял байты).
#[test]
fn load_then_encode_gives_same_bytes() {
    let cases: [(&str, &'static Encoding, bool); 13] = [
        (RU, WINDOWS_1251, false),
        (RU, IBM866, false),
        (RU, KOI8_R, false),
        (
            "Їжак ішов стежкою через ґанок до саду, де під яблунею росла трава.",
            KOI8_U,
            false,
        ),
        (FR, WINDOWS_1252, false),
        (JA, SHIFT_JIS, false),
        (RU, UTF_8, false),
        (RU, UTF_8, true),
        (RU, UTF_16LE, true),
        (RU, UTF_16BE, true),
        (FR, UTF_16LE, false),
        (FR, UTF_16BE, false),
        ("plain ascii", UTF_8, false),
    ];
    let mut checked = 0;
    for (body, e, bom) in cases {
        for eol in ["\n", "\r\n", "\r", "mixed"] {
            let text = lines(body, 40, eol) + body;
            let b = bytes_of(&text, e, bom);
            let d = run(&b, Some(e));
            assert!(!d.lossy, "{} {eol:?}", e.name());
            assert_eq!(d.bom, bom, "{} {eol:?}", e.name());
            assert_eq!(d.text.to_string(), text, "{} {eol:?}", e.name());
            let out = encode(&d.text, d.enc.enc, d.bom);
            assert_eq!(out.unmappable, 0);
            assert!(out.bytes == b, "{} {eol:?}: байты другие", e.name());
            let auto = run(&b, None);
            if !auto.lossy {
                let out = encode(&auto.text, auto.enc.enc, auto.bom);
                assert!(out.bytes == b, "{} {eol:?} по детекту", e.name());
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 52);
}

#[test]
fn eol_detected_over_whole_text() {
    let d = run(lines("x", 10, "\r\n").as_bytes(), None);
    assert_eq!((d.eol, d.eol_new), (Eol::CrLf, Eol::CrLf));
    let d = run(lines("x", 10, "\n").as_bytes(), None);
    assert_eq!((d.eol, d.eol_new), (Eol::Lf, Eol::Lf));
    let d = run((lines("x", 10, "\n") + "y\r\n").as_bytes(), None);
    assert_eq!((d.eol, d.eol_new), (Eol::Mixed, Eol::Lf), "доминирует LF");
    let d = run(b"one line", None);
    assert_eq!(
        (d.eol, d.eol_new),
        (Eol::CrLf, Eol::CrLf),
        "без переводов — CRLF"
    );
    let c = super::diff::EolCount {
        lf: 3,
        crlf: 3,
        cr: 0,
    };
    assert_eq!(eol_of(c), Eol::Mixed);
    assert_eq!(eol_new(EolCfg::File, c), Eol::CrLf, "поровну — CRLF");
    assert_eq!(eol_new(EolCfg::Lf, c), Eol::Lf);
}

#[test]
fn lossy_and_limits() {
    let d = run(b"ok \xFF\xFE bad", Some(UTF_8));
    assert!(d.lossy);
    let big = vec![b'a'; 3 << 20];
    let mut o = opts(None);
    o.limit = 1 << 20;
    let err = decode(
        &mut Cursor::new(&big),
        Some(big.len() as u64),
        &o,
        &CancelToken::new(),
        &mut |_| {},
    );
    assert!(matches!(err, Err(LoadError::TooLarge(n)) if n == 3 << 20));
    // Поток без размера — по прочитанному.
    let err = decode(
        &mut Cursor::new(&big),
        None,
        &o,
        &CancelToken::new(),
        &mut |_| {},
    );
    assert!(matches!(err, Err(LoadError::TooLarge(_))));
    let mut seen = Vec::new();
    let d = decode(
        &mut Cursor::new(&big),
        None,
        &opts(None),
        &CancelToken::new(),
        &mut |n| seen.push(n),
    )
    .unwrap();
    assert_eq!(d.bytes, 3 << 20);
    assert_eq!(seen.last(), Some(&(3 << 20)), "прогресс по кускам");
    let ru = bytes_of(RU, WINDOWS_1251, false);
    let out = encode(
        &run(&ru, Some(WINDOWS_1251)).text,
        encoding_rs::WINDOWS_1252,
        false,
    );
    assert!(out.unmappable > 0, "кириллица в 1252 непредставима");
}

// ---------------------------------------------------------------- через core

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

fn open(core: &mut Core, path: &Path) {
    core.dispatch(Command::new("editor.open").arg("path", Location::local(path)));
    core.pump(Duration::from_millis(1));
    pump_until(core, |c| !matches!(ed(c).state, EdState::Loading { .. }));
}

fn send(core: &mut Core, msg: InboxMsg) {
    view(core).inbox.sender().send(msg);
    core.pump(Duration::from_millis(5));
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

/// Открытие → Ready и текст; UI забирает → `Adopted`; снимок → полоса изменений; закрытие
/// → запись правки в истории; повторное открытие — курсор из неё.
#[test]
fn open_adopt_diff_close() {
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
    assert_eq!(d.loaded.as_ref().unwrap().to_string(), text);
    assert!(d.enc.is_utf8() && d.enc.auto && !d.bom && !d.lossy && !d.readonly);
    assert_eq!(
        (d.eol, d.eol_new, d.lang),
        (Eol::CrLf, Eol::CrLf, Some("toml"))
    );
    assert_eq!(d.stamp.size, text.len() as u64);
    assert!(d.stamp.file_id.is_some());

    send(&mut core, InboxMsg::Adopted(doc));
    assert!(ed(&core).loaded.is_none());

    send(
        &mut core,
        InboxMsg::Edited {
            doc,
            dirty: true,
            cursor: (3, 2),
        },
    );
    let now = ropey::Rope::from_str("a = 1\r\nB = 2\r\nc = 3\r\nd = 4\r\n");
    send(
        &mut core,
        InboxMsg::Snapshot {
            doc,
            text: now,
            then: After::Diff,
        },
    );
    pump_until(&mut core, |c| !ed(c).changes.is_empty());
    let ch = &ed(&core).changes;
    assert_eq!((ch.added, ch.modified), (1, 1));
    assert_eq!(&*ch.lines, &[(1, Change::Modified), (3, Change::Added)]);

    // С правками закрытие ждёт подтверждения (S7-17).
    core.dispatch(Command::new("editor.close"));
    core.pump(Duration::from_millis(5));
    assert!(view(&core).doc(doc).is_some());
    assert_eq!(ed(&core).ask, Some(super::EdAsk::Close));
    // Esc при вопросе — вопрос убирается.
    core.dispatch(Command::new("editor.close"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ed(&core).ask, None);
    send(
        &mut core,
        InboxMsg::Edited {
            doc,
            dirty: false,
            cursor: (3, 2),
        },
    );
    core.dispatch(Command::new("editor.close"));
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

    open(&mut core, &path);
    assert_eq!(ed(&core).start, Some((3, 2)));
    // Тот же файл — тот же экран; `line`/`col` — с 1.
    core.dispatch(Command::new("editor.open").arg("path", Location::local(&path)));
    core.pump(Duration::from_millis(5));
    assert_eq!(view(&core).screens.len(), 1);
    let other = dir.path().join("b.txt");
    std::fs::write(&other, "x\ny\n").unwrap();
    core.dispatch(
        Command::new("editor.open")
            .arg("path", Location::local(&other))
            .arg("line", 2)
            .arg("col", 5),
    );
    pump_until(&mut core, |c| ed(c).is_ready());
    assert_eq!(ed(&core).start, Some((1, 4)));
    assert_eq!(view(&core).screens.len(), 2);
}

/// F8 без правок перечитывает файл в новой кодировке; с правками — только кодировка
/// сохранения и тост. `editor.eol` — EOL новых строк и Ext для UI.
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
    send(&mut core, InboxMsg::Adopted(doc));

    core.dispatch(Command::new("editor.codepage").arg("cp", "ibm866"));
    pump_until(&mut core, |c| ed(c).is_ready() && ed(c).loaded.is_some());
    let d = ed(&core);
    assert_eq!(d.enc.enc, IBM866);
    assert!(!d.enc.auto);
    assert!(!d.loaded.as_ref().unwrap().to_string().contains("туман"));
    // F8 по кольцу: auto → utf-8 → windows-1251 → ibm866 → auto.
    core.dispatch(Command::new("editor.codepage"));
    pump_until(&mut core, |c| ed(c).is_ready() && ed(c).enc.auto);
    assert!(
        ed(&core)
            .loaded
            .as_ref()
            .unwrap()
            .to_string()
            .contains("туман")
    );
    send(&mut core, InboxMsg::Adopted(doc));

    send(
        &mut core,
        InboxMsg::Edited {
            doc,
            dirty: true,
            cursor: (0, 0),
        },
    );
    core.take_ui_events();
    core.dispatch(Command::new("editor.codepage").arg("cp", "utf-8"));
    core.pump(Duration::from_millis(5));
    let d = ed(&core);
    assert_eq!(d.enc.enc, UTF_8);
    assert!(d.loaded.is_none() && d.is_ready(), "без перечитывания");
    let toasts = core
        .take_ui_events()
        .into_iter()
        .filter(|e| matches!(e, UiEvent::Toast { .. }))
        .count();
    assert_eq!(toasts, 1);

    core.dispatch(Command::new("editor.eol").arg("to", "lf"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ed(&core).eol_new, Eol::Lf);
    let ev = ext(&mut core, "editor.eol");
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].str("to"), Some("lf"));
    core.dispatch(Command::new("editor.codepage_menu"));
    core.pump(Duration::from_millis(5));
    assert_eq!(ext(&mut core, "editor.codepage_menu").len(), 1);
}

/// Больше `editor.max_size_mb` — `TooLarge` без текста; файла нет — тост и документ
/// закрыт; атрибут «только чтение» — `readonly`.
#[test]
fn too_large_missing_and_readonly() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.log");
    std::fs::write(&big, vec![b'x'; (1 << 20) + 1]).unwrap();
    let root = Location::local(dir.path());
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    let cfg = EditorCfg {
        max_size_mb: 1,
        ..EditorCfg::default()
    };
    register_with(&mut core, ViewerCfg::default(), cfg);
    core.pump(Duration::from_millis(5));
    open(&mut core, &big);
    assert_eq!(
        ed(&core).state,
        EdState::TooLarge {
            size: (1 << 20) + 1
        }
    );
    assert!(ed(&core).loaded.is_none());

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
