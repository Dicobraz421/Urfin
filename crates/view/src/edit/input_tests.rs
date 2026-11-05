//! Приёмка S7-29 (правка в handler) через core: движения на CRLF/CR/UTF-16, слова на
//! кириллице, вверх/вниз с переносом; ввод вне кодировки → вопрос, перевод в UTF-8 и его
//! отмена; ввод, отмена и ●; буфер обмена; `editor.eol` одной группой.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use encoding_rs::{UTF_16LE, WINDOWS_1251};
use urfin_core::{Boot, Command, Core, Location, Services, Waker};

use super::*;
use crate::edit::EditorDoc;
use crate::{Doc, EditorCfg, InboxMsg, ViewState, ViewerCfg, register_with};

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

pub(crate) fn core_with(dir: &Path, cfg: EditorCfg) -> Core {
    let root = Location::local(dir);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, ViewerCfg::default(), cfg);
    core.pump(Duration::from_millis(5));
    core
}

pub(crate) fn pump_until(core: &mut Core, mut f: impl FnMut(&Core) -> bool) {
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

pub(crate) fn view(core: &Core) -> &ViewState {
    core.state().ext.get::<ViewState>().unwrap()
}

pub(crate) fn ed(core: &Core) -> &EditorDoc {
    let v = view(core);
    match v.doc(v.active().unwrap()) {
        Some(Doc::Editor(d)) => d,
        _ => panic!("не редактор"),
    }
}

pub(crate) fn eng(core: &Core) -> &Engine {
    ed(core).engine.as_deref().expect("документ на движке")
}

pub(crate) fn send(core: &mut Core, msg: InboxMsg) {
    view(core).inbox.sender().send(msg);
    core.pump(Duration::from_millis(5));
}

/// Файл → редактор → `Viewport` (документ на движке, окно `cols`×10).
pub(crate) fn open_engine(core: &mut Core, path: &Path, cols: u32) {
    core.dispatch(Command::new("editor.open").arg("path", Location::local(path)));
    core.pump(Duration::from_millis(1));
    let doc = view(core).active().unwrap();
    send(
        core,
        InboxMsg::Viewport {
            doc,
            cols,
            rows: 10,
        },
    );
    pump_until(core, |c| ed(c).engine.is_some() && ed(c).is_ready());
}

pub(crate) fn editor(dir: &Path, name: &str, bytes: &[u8], cols: u32, cfg: EditorCfg) -> Core {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    let mut core = core_with(dir, cfg);
    open_engine(&mut core, &path, cols);
    core
}

pub(crate) fn mv(core: &mut Core, to: &str) -> u64 {
    core.dispatch(Command::new("editor.move").arg("to", to));
    core.pump(Duration::from_millis(1));
    eng(core).sel.caret
}

pub(crate) fn cmd(core: &mut Core, id: &'static str) {
    core.dispatch(Command::new(id));
    core.pump(Duration::from_millis(1));
}

/// Байты документа из кэшей (тест — UI-поток: источники отсюда не читаются).
pub(crate) fn text(core: &Core) -> Vec<u8> {
    crate::edit::piece::tests::read_fwd(&eng(core).text.view())
}

pub(crate) fn input(core: &mut Core, s: &str) {
    let doc = ed(core).id;
    send(
        core,
        InboxMsg::Input {
            doc,
            text: s.into(),
        },
    );
}

/// CRLF — один шаг стрелки и Backspace; Home/End — края строки.
#[test]
fn moves_over_crlf() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "a.txt",
        b"ab\r\ncd\r\n",
        80,
        EditorCfg::default(),
    );
    assert_eq!(eng(&c).sel.caret, 0);
    assert_eq!(mv(&mut c, "right"), 1);
    assert_eq!(mv(&mut c, "right"), 2);
    assert_eq!(mv(&mut c, "right"), 4, "CRLF — один шаг");
    assert_eq!(mv(&mut c, "left"), 2);
    assert_eq!(mv(&mut c, "down"), 6, "та же колонка");
    assert_eq!(mv(&mut c, "home"), 4);
    assert_eq!(mv(&mut c, "end"), 6);
    assert_eq!(mv(&mut c, "down"), 8, "пустая строка в конце");
    assert_eq!(mv(&mut c, "up"), 6, "колонка 2 запомнена");
    assert_eq!(mv(&mut c, "doc_end"), 8);
    assert_eq!(mv(&mut c, "doc_start"), 0);
    mv(&mut c, "end");
    mv(&mut c, "right");
    c.dispatch(Command::new("editor.delete").arg("to", "left"));
    c.pump(Duration::from_millis(1));
    assert_eq!(text(&c), b"abcd\r\n");
    assert_eq!(eng(&c).sel.caret, 2);
}

/// Концы строк CR (`Eol::Cr`): строки по CR.
#[test]
fn moves_over_cr() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(dir.path(), "a.txt", b"abc\rde\rf", 80, EditorCfg::default());
    assert_eq!(ed(&c).eol, crate::Eol::Cr);
    assert_eq!(mv(&mut c, "down"), 4);
    assert_eq!(mv(&mut c, "end"), 6);
    assert_eq!(mv(&mut c, "right"), 7);
    assert_eq!(mv(&mut c, "up"), 4, "колонка 0 строки выше");
}

/// UTF-16LE с BOM: смещения документа — после BOM, шаг — единица кода, CRLF — 4 байта.
#[test]
fn moves_over_utf16() {
    let dir = tempfile::tempdir().unwrap();
    let b = super::super::open::tests::bytes_of("аб\r\nвг𝄞", UTF_16LE, true);
    let mut c = editor(dir.path(), "u.txt", &b, 80, EditorCfg::default());
    assert!(ed(&c).enc.is_utf16() && ed(&c).bom);
    assert_eq!(mv(&mut c, "right"), 2);
    assert_eq!(mv(&mut c, "right"), 4);
    assert_eq!(mv(&mut c, "right"), 8, "CRLF");
    assert_eq!(mv(&mut c, "end"), 16, "суррогатная пара — 4 байта");
    assert_eq!(mv(&mut c, "left"), 12);
    input(&mut c, "ё");
    let want = super::super::open::tests::bytes_of("аб\r\nвгё𝄞", UTF_16LE, false);
    assert_eq!(text(&c), want);
}

/// Слова — UAX#29 по кириллице: Ctrl+→ к началу следующего, Ctrl+← к началу своего.
#[test]
fn words_in_cyrillic() {
    let dir = tempfile::tempdir().unwrap();
    let s = "Привет, мир дружба\nдалее";
    let mut c = editor(dir.path(), "w.txt", s.as_bytes(), 80, EditorCfg::default());
    let at = |p: &str| s.find(p).unwrap() as u64;
    assert_eq!(mv(&mut c, "word_right"), at(","));
    assert_eq!(mv(&mut c, "word_right"), at("мир"));
    assert_eq!(mv(&mut c, "word_right"), at("дружба"));
    assert_eq!(mv(&mut c, "word_right"), at("\n"), "конец строки");
    assert_eq!(mv(&mut c, "word_right"), at("далее"), "начало следующей");
    assert_eq!(mv(&mut c, "word_left"), at("\n"));
    assert_eq!(mv(&mut c, "word_left"), at("дружба"));
    c.dispatch(Command::new("editor.delete").arg("to", "word_left"));
    c.pump(Duration::from_millis(1));
    assert_eq!(text(&c), "Привет, дружба\nдалее".as_bytes());
}

/// Вверх/вниз — по визуальным рядам переноса с памятью колонки.
#[test]
fn up_down_follow_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EditorCfg {
        wrap: true,
        ..EditorCfg::default()
    };
    // 25 символов без пробелов: ряды по 10 колонок.
    let mut c = editor(
        dir.path(),
        "r.txt",
        b"abcdefghijklmnopqrstuvwxy\nxy\nlong line here\n",
        10,
        cfg,
    );
    assert!(eng(&c).wrap);
    mv(&mut c, "right");
    mv(&mut c, "right");
    mv(&mut c, "right");
    assert_eq!(mv(&mut c, "down"), 13, "второй ряд той же строки");
    assert_eq!(mv(&mut c, "down"), 23, "третий ряд");
    assert_eq!(mv(&mut c, "down"), 28, "короткая строка — её конец");
    assert_eq!(mv(&mut c, "down"), 32, "колонка 3 запомнена");
    assert_eq!(mv(&mut c, "up"), 28);
    assert_eq!(mv(&mut c, "up"), 23);
    assert_eq!(mv(&mut c, "home"), 20, "начало ряда");
}

/// Символ вне 1251 → вопрос; «Перевести в UTF-8» — задача и одна группа, отмена
/// возвращает кодировку и текст; «Вставить «?»» — замена.
#[test]
fn no_char_converts_to_utf8_with_undo() {
    let dir = tempfile::tempdir().unwrap();
    let body = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь.\r\n";
    let src = body.repeat(4);
    let ru = super::super::open::tests::bytes_of(&src, WINDOWS_1251, false);
    let mut c = editor(dir.path(), "cp.txt", &ru, 80, EditorCfg::default());
    assert_eq!(eng(&c).enc.enc, WINDOWS_1251);
    mv(&mut c, "end");
    let at = eng(&c).sel.caret as usize;
    input(&mut c, "😀");
    assert_eq!(ed(&c).ask, Some(super::super::EdAsk::NoChar { ch: '😀' }));
    assert_eq!(text(&c), ru, "до ответа документ не тронут");

    c.dispatch(Command::new("editor.answer").arg("choice", "utf8"));
    pump_until(&mut c, |c| {
        !eng(c).busy && ed(c).enc.is_utf8() && ed(c).ask.is_none()
    });
    let mut want = src.clone();
    let cut = src.char_indices().nth(at).map_or(src.len(), |(i, _)| i);
    want.insert(cut, '😀');
    assert_eq!(text(&c), want.as_bytes());
    assert!(ed(&c).dirty);

    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), src.as_bytes(), "отмена ввода");
    cmd(&mut c, "editor.undo");
    assert_eq!(ed(&c).enc.enc, WINDOWS_1251, "отмена перевода — кодировка");
    assert_eq!(text(&c), ru, "и текст");
    assert!(!ed(&c).dirty, "сохранённое состояние");

    input(&mut c, "😀");
    assert!(matches!(
        ed(&c).ask,
        Some(super::super::EdAsk::NoChar { .. })
    ));
    c.dispatch(Command::new("editor.answer").arg("choice", "question"));
    c.pump(Duration::from_millis(1));
    let mut want = ru.clone();
    want.insert(at, b'?');
    assert_eq!(text(&c), want);
}

/// Набор, отмена до сохранённого состояния снимает ●; Ins — замена.
#[test]
fn typing_undo_and_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(dir.path(), "t.txt", b"abc\n", 80, EditorCfg::default());
    input(&mut c, "x");
    input(&mut c, "y");
    assert_eq!(text(&c), b"xyabc\n");
    assert!(ed(&c).dirty);
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"abc\n", "набор подряд — одна группа");
    assert!(!ed(&c).dirty);
    cmd(&mut c, "editor.redo");
    assert_eq!(text(&c), b"xyabc\n");
    cmd(&mut c, "editor.overwrite");
    input(&mut c, "Z");
    assert_eq!(text(&c), b"xyZbc\n");
    mv(&mut c, "end");
    input(&mut c, "!");
    assert_eq!(text(&c), b"xyZbc!\n", "в конце строки — вставка");
    cmd(&mut c, "editor.overwrite");
    cmd(&mut c, "editor.newline");
    assert_eq!(text(&c), b"xyZbc!\n\n");
    cmd(&mut c, "editor.delete_line");
    assert_eq!(text(&c), b"xyZbc!\n");
}

/// Автоотступ Enter, Tab пробелами до остановки, сдвиг строк выделения.
#[test]
fn newline_indent_and_shift() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(dir.path(), "i.txt", b"  ab\ncd\n", 80, EditorCfg::default());
    mv(&mut c, "end");
    cmd(&mut c, "editor.newline");
    assert_eq!(text(&c), b"  ab\n  \ncd\n");
    cmd(&mut c, "editor.tab");
    assert_eq!(text(&c), b"  ab\n    \ncd\n", "до колонки 4");
    cmd(&mut c, "editor.select_all");
    cmd(&mut c, "editor.block.shift_left");
    assert_eq!(text(&c), b"ab\n\ncd\n");
    cmd(&mut c, "editor.block.shift_right");
    assert_eq!(text(&c), b"    ab\n    \n    cd\n");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"ab\n\ncd\n", "сдвиг — одна группа");
}

/// Копирование, вырезание (одна группа) и вставка (EOL → `eol_new`).
#[test]
fn clipboard_copy_cut_paste() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "c.txt",
        b"one\r\ntwo\r\n",
        80,
        EditorCfg::default(),
    );
    c.dispatch(
        Command::new("editor.move")
            .arg("to", "end")
            .arg("select", "stream"),
    );
    c.pump(Duration::from_millis(1));
    assert_eq!(eng(&c).sel.stream(), Some(0..3));
    cmd(&mut c, "editor.cut");
    assert_eq!(text(&c), b"\r\ntwo\r\n");
    pump_until(&mut c, |_| {
        crate::sys::clip_read().unwrap() == ("one".into(), false)
    });
    crate::sys::clip_write("a\nb", false).unwrap();
    mv(&mut c, "doc_end");
    cmd(&mut c, "editor.paste");
    pump_until(&mut c, |c| text(c).len() > 7);
    assert_eq!(text(&c), b"\r\ntwo\r\na\r\nb");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"\r\ntwo\r\n", "вставка — одна группа");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"one\r\ntwo\r\n", "вырезание — одна группа");
}

/// `editor.eol` на движке — весь документ задачей, одна группа; отмена возвращает и
/// концы строк.
#[test]
fn eol_rewrites_document() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "e.txt",
        b"a\r\nb\r\nc",
        80,
        EditorCfg::default(),
    );
    mv(&mut c, "down");
    c.dispatch(Command::new("editor.eol").arg("to", "lf"));
    pump_until(&mut c, |c| !eng(c).busy && text(c) == b"a\nb\nc");
    assert_eq!(ed(&c).eol, crate::Eol::Lf);
    assert_eq!(eng(&c).sel.caret, 2, "курсор — на том же месте текста");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"a\r\nb\r\nc");
    assert_eq!(ed(&c).eol, crate::Eol::CrLf);
}

/// Мышь: клик, двойной (слово), тройной (строка), drag (`extend`).
#[test]
fn caret_units() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "m.txt",
        b"hello world\nnext\n",
        80,
        EditorCfg::default(),
    );
    let doc = ed(&c).id;
    let caret = |off, extend, unit| InboxMsg::Caret {
        doc,
        off,
        extend,
        unit,
        block: None,
    };
    send(&mut c, caret(7, false, Unit::Word));
    assert_eq!(eng(&c).sel.stream(), Some(6..11));
    send(&mut c, caret(2, false, Unit::Line));
    assert_eq!(eng(&c).sel.stream(), Some(0..12));
    send(&mut c, caret(1, false, Unit::Char));
    send(&mut c, caret(4, true, Unit::Char));
    assert_eq!(eng(&c).sel.stream(), Some(1..4));
}

/// Текст снимка — в отдельном потоке: UI-поток источники не читает.
fn snap_text(core: &Core) -> Vec<u8> {
    let snap = eng(core).text.snap();
    std::thread::spawn(move || {
        let mut b = vec![0u8; usize::try_from(snap.len()).unwrap()];
        snap.read_at(0, &mut b).unwrap();
        b
    })
    .join()
    .unwrap()
}

/// Большая вставка: фоном, на место выделения, концы строк документа, одна отмена.
#[test]
fn big_paste_goes_through_a_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(
        dir.path(),
        "a.txt",
        b"one\r\ntwo\r\n",
        80,
        EditorCfg::default(),
    );
    let (doc, big) = (ed(&c).id, "line\n".repeat(crate::edit::paste::BIG / 4));
    send(
        &mut c,
        InboxMsg::Caret {
            doc,
            off: 3,
            extend: false,
            unit: Unit::Char,
            block: None,
        },
    );
    input(&mut c, &big);
    pump_until(&mut c, |c| !ed(c).pasting);
    assert!(matches!(eng(&c).text.tree().iter().count(), 3..=8));
    let want: Vec<u8> = [
        &b"one"[..],
        big.replace('\n', "\r\n").as_bytes(),
        b"\r\ntwo\r\n",
    ]
    .concat();
    assert_eq!(snap_text(&c), want);
    assert!(ed(&c).dirty);
    // Куски вставки знают переводы строк: номер строки точный сразу.
    assert_eq!(
        eng(&c).text.line_of(eng(&c).len()).get(),
        u64::try_from(crate::edit::paste::BIG / 4).unwrap() + 2
    );
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"one\r\ntwo\r\n");
}
