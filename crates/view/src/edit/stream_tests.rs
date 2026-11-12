//! Приёмка S7-31 (сохранение потоком): без правок и с правками = собранный буфер (все
//! кодировки V5, BOM, lossy, перекодирование); непредставимые — вопрос, цель не тронута;
//! обрыв после временного файла; жёсткие ссылки и атрибуты; через core — документ
//! переходит на новый файл, отмена после сохранения, правка во время записи, большой
//! файл под запретом записи; `ReplaceFileW` при своём handle (Windows).

#![allow(clippy::many_single_char_names, reason = "тесты: короткие имена")]
use std::path::Path;
use std::time::Duration;

use encoding_rs::{
    Encoding, IBM866, KOI8_R, KOI8_U, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251,
    WINDOWS_1252,
};
use urfin_core::Command;

use super::super::{Body, Fault, How, Outcome, ReadOnly, SaveReq, Unmappable, save_full};
use super::SnapBody;
use crate::EditorCfg;
use crate::edit::EdState;
use crate::edit::input::tests::{
    cmd, core_with, ed, editor, eng, input, open_engine, pump_until, text,
};
use crate::edit::open::tests::{FR, JA, RU, bytes_of, lines, open_bytes};
use crate::edit::open::{Guard, Opened};
use crate::edit::piece::Src;
use crate::enc::Enc;

/// Запрос записи снимка документа `o` по пути `path` в его кодировке.
fn req(o: &Opened, path: &Path) -> SaveReq {
    SaveReq {
        text: Body::Snap(SnapBody {
            snap: o.text.snap(),
            from: Enc {
                bom_len: 0,
                ..o.enc
            },
            eol: o.eol,
            space: true,
            reopen: true,
        }),
        enc: o.enc.enc,
        bom: o.bom,
        path: path.to_path_buf(),
        check: Some(o.stamp),
        unmappable: Unmappable::Ask,
        readonly: ReadOnly::Ask,
        fault: None,
    }
}

fn run(r: &SaveReq) -> super::Saved {
    save_full(r, &|| Ok(()), &mut |_| {}).unwrap()
}

fn saved(s: &super::Saved) -> How {
    match &s.outcome {
        Outcome::Saved { how, .. } => *how,
        other => panic!("не сохранено: {other:?}"),
    }
}

/// Документ на кусках в памяти: байты снимка (тест — не UI-поток).
fn snap_bytes(o: &Opened) -> Vec<u8> {
    let s = o.text.snap();
    let mut b = vec![0u8; s.len() as usize];
    assert_eq!(s.read_at(0, &mut b).unwrap(), b.len());
    b
}

const CASES: [(&str, &Encoding, bool); 13] = [
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

/// Приёмка: без правок — те же байты; с правками (вставка, удаление по краям кусков) —
/// BOM и собранные куски; файл открыт заново — в памяти, индекс строк точный.
#[test]
fn save_equals_assembled_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    let mut checked = 0;
    for (body, e, bom) in CASES {
        for eol in ["\n", "\r\n", "\r", "mixed"] {
            let src = lines(body, 30, eol) + body;
            let b = bytes_of(&src, e, bom);
            let o = open_bytes(dir.path(), "a.txt", &b, Some(e));
            let s = run(&req(&o, &path));
            assert_ne!(saved(&s), How::InPlace);
            assert!(std::fs::read(&path).unwrap() == b, "{} {eol:?}", e.name());
            let r = s.reopened.expect("открыт заново");
            assert_eq!(r.guard, Guard::Memory);
            assert_eq!(r.bom, u64::from(o.enc.bom_len));
            assert!(r.index.done);
            assert_eq!(r.index.lines, o.text.index().lines, "{} {eol:?}", e.name());
            // Правки: вставка в середину, удаление начала, дописывание в конец.
            let mut o = open_bytes(dir.path(), "a.txt", &b, Some(e));
            let ins = bytes_of(
                "·вставка·",
                if e == UTF_8 || o.enc.is_utf16() {
                    e
                } else {
                    UTF_8
                },
                false,
            );
            let unit = u64::from(o.enc.unit());
            let mid = (o.text.len() / 2) / unit * unit;
            if e == UTF_8 || o.enc.is_utf16() {
                o.text.insert(mid, &ins).unwrap();
            }
            let cut = 4 * unit;
            o.text.remove(0..cut);
            let end = o.text.len();
            o.text.insert(end, &bytes_of("end", e, false)).unwrap();
            let mut want = b[..usize::from(o.enc.bom_len)].to_vec();
            want.extend_from_slice(&snap_bytes(&o));
            run(&req(&o, &path));
            assert!(
                std::fs::read(&path).unwrap() == want,
                "{} {eol:?} с правками",
                e.name()
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 52);
}

/// Lossy: недекодируемые байты и нечётный хвост UTF-16 сохраняются как были.
#[test]
fn lossy_saved_as_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("l.txt");
    let b = b"ok \xFF\xFE bad\r\nnext \xC3 line\r\n".to_vec();
    let o = open_bytes(dir.path(), "l.txt", &b, Some(UTF_8));
    assert!(o.lossy);
    run(&req(&o, &path));
    assert_eq!(std::fs::read(&path).unwrap(), b);
    let mut b = bytes_of("абв\r\n", UTF_16LE, true);
    b.push(0x41);
    let o = open_bytes(dir.path(), "l.txt", &b, Some(UTF_16LE));
    run(&req(&o, &path));
    assert_eq!(std::fs::read(&path).unwrap(), b);
}

/// Другая кодировка (F8 с правками): перекодирование потоком + BOM; непредставимые —
/// вопрос, цель не тронута и временного файла нет; «Заменить на ?» — записано.
#[test]
fn recode_and_unmappable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.txt");
    let b = bytes_of("Привет\r\nмир\r\n", WINDOWS_1251, false);
    let o = open_bytes(dir.path(), "r.txt", &b, Some(WINDOWS_1251));
    let mut r = req(&o, &path);
    r.enc = UTF_8;
    r.bom = true;
    let s = run(&r);
    saved(&s);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes_of("Привет\r\nмир\r\n", UTF_8, true)
    );
    assert_eq!(s.reopened.unwrap().index.lines, 2);
    // UTF-8 с японским → 1251.
    let b = bytes_of("日本 и мир", UTF_8, false);
    let o = open_bytes(dir.path(), "r.txt", &b, Some(UTF_8));
    let mut r = req(&o, &path);
    r.enc = WINDOWS_1251;
    assert_eq!(run(&r).outcome, Outcome::Unmappable(2));
    assert_eq!(std::fs::read(&path).unwrap(), b, "цель не тронута");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "без временного"
    );
    r.unmappable = Unmappable::Replace;
    r.check = None;
    saved(&run(&r));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes_of("?? и мир", WINDOWS_1251, false)
    );
}

/// Обрыв после временного файла: исходник цел, временный остаётся.
#[test]
fn fault_after_temp_keeps_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    let mut o = open_bytes(dir.path(), "f.txt", b"one\ntwo\n", None);
    o.text.insert(0, b"new ").unwrap();
    let mut r = req(&o, &path);
    r.fault = Some(Fault::AfterTemp);
    assert!(save_full(&r, &|| Ok(()), &mut |_| {}).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");
    let temp = super::temp_path(&path);
    assert_eq!(std::fs::read(&temp).unwrap(), b"new one\ntwo\n");
}

/// Жёсткие ссылки — на месте (через `%TEMP%`): обе ссылки видят новый текст; атрибуты
/// (скрытый) остаются; «только чтение» — вопрос, затем «Оставить».
#[test]
fn hardlink_and_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h.txt");
    let link = dir.path().join("h2.txt");
    let mut o = open_bytes(dir.path(), "h.txt", b"old\n", None);
    std::fs::hard_link(&path, &link).unwrap();
    o.text.insert(0, b"new ").unwrap();
    let mut r = req(&o, &path);
    r.check = None;
    assert_eq!(saved(&run(&r)), How::InPlace);
    assert_eq!(std::fs::read(&link).unwrap(), b"new old\n");
    std::fs::remove_file(&link).unwrap();
    let o = open_bytes(dir.path(), "h.txt", b"x\n", None);
    crate::sys::set_attrs(&path, crate::sys::ATTR_HIDDEN | crate::sys::ATTR_READONLY).unwrap();
    let mut r = req(&o, &path);
    r.check = None;
    assert_eq!(run(&r).outcome, Outcome::ReadOnly);
    r.readonly = ReadOnly::Keep;
    assert_ne!(saved(&run(&r)), How::InPlace);
    let a = crate::sys::attrs(&path).unwrap();
    assert_ne!(a & crate::sys::ATTR_HIDDEN, 0, "скрытый остался");
    assert_ne!(a & crate::sys::ATTR_READONLY, 0, "только чтение остался");
    crate::sys::set_attrs(&path, 0).unwrap();
}

/// Первые `n` байт документа по снимку (вне UI-потока: источник большого файла с
/// потока теста не читается).
fn head(c: &urfin_core::Core, n: usize) -> Vec<u8> {
    let snap = eng(c).text.snap();
    std::thread::spawn(move || {
        let mut b = vec![0u8; n];
        let k = snap.read_at(0, &mut b).unwrap();
        b.truncate(k);
        b
    })
    .join()
    .unwrap()
}

/// Через core: правка → `editor.save` → файл, ● снят, документ — один кусок нового
/// файла; отмена после сохранения возвращает прежний текст (куски прежней версии) и ●;
/// повтор — снова сохранённое состояние.
#[test]
fn engine_save_rebases_and_undo_survives() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.txt");
    let mut c = editor(
        dir.path(),
        "s.txt",
        b"one\r\ntwo\r\n",
        80,
        EditorCfg::default(),
    );
    input(&mut c, "X");
    assert!(ed(&c).dirty);
    cmd(&mut c, "editor.save");
    pump_until(&mut c, |c| ed(c).is_ready() && !ed(c).dirty);
    assert_eq!(std::fs::read(&path).unwrap(), b"Xone\r\ntwo\r\n");
    let e = eng(&c);
    let pieces: Vec<_> = e.text.tree().iter().map(|(_, p)| *p).collect();
    assert_eq!(pieces.len(), 1, "один кусок нового файла");
    assert_eq!(pieces[0].src, Src::Original);
    assert_eq!(text(&c), b"Xone\r\ntwo\r\n");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), b"one\r\ntwo\r\n", "отмена после сохранения");
    assert!(ed(&c).dirty);
    cmd(&mut c, "editor.redo");
    assert_eq!(text(&c), b"Xone\r\ntwo\r\n");
    assert!(!ed(&c).dirty, "повтор — сохранённое состояние");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "ни временного, ни резервной копии: {names:?}"
    );
}

/// Правка во время записи (снимок): файл — без неё, ● остаётся, отмена правки — ●
/// снят.
#[test]
fn edit_while_saving() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.txt");
    let body = "строка текста для записи\n".repeat(400_000);
    let mut c = editor(
        dir.path(),
        "w.txt",
        body.as_bytes(),
        80,
        EditorCfg::default(),
    );
    input(&mut c, "A");
    cmd(&mut c, "editor.save");
    assert!(
        matches!(ed(&c).state, EdState::Saving(_)),
        "{:?}",
        ed(&c).state
    );
    input(&mut c, "B");
    pump_until(&mut c, |c| ed(c).is_ready());
    let file = std::fs::read(&path).unwrap();
    assert!(file.starts_with("A".as_bytes()) && !file.starts_with(b"AB"));
    assert_eq!(file.len(), body.len() + 1);
    assert!(ed(&c).dirty, "правка после снимка");
    assert_eq!(head(&c, 2), b"AB");
    cmd(&mut c, "editor.undo");
    assert!(!ed(&c).dirty, "отмена — сохранённое состояние");
}

/// Файл больше 16 МБ: после сохранения документ снова под запретом записи (новый файл),
/// старая версия читается через свой handle (отмена), резервных копий нет.
#[test]
fn big_file_save_keeps_lock_and_old_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    let line = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
    let n = (17 << 20) / line.len() + 1;
    let body: Vec<u8> = line.repeat(n);
    std::fs::write(&path, &body).unwrap();
    let mut c = core_with(dir.path(), EditorCfg::default());
    open_engine(&mut c, &path, 80);
    assert_eq!(eng(&c).guard, Guard::Locked);
    input(&mut c, "Z");
    cmd(&mut c, "editor.save");
    pump_until(&mut c, |c| ed(c).is_ready() && !ed(c).dirty);
    let file = std::fs::read(&path).unwrap();
    assert_eq!(file.len(), body.len() + 1);
    assert!(file.starts_with(b"Z0123"));
    let e = eng(&c);
    assert_eq!(e.guard, Guard::Locked);
    assert_eq!(e.text.tree().iter().count(), 1);
    #[cfg(windows)]
    {
        let w = std::fs::OpenOptions::new().write(true).open(&path);
        assert!(w.is_err(), "новый файл — под запретом записи");
    }
    cmd(&mut c, "editor.undo");
    assert_eq!(
        head(&c, 8),
        b"01234567",
        "старая версия — через свой handle"
    );
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    // Закрытие документа освобождает handle старой версии.
    cmd(&mut c, "editor.close");
    c.dispatch(Command::new("editor.answer").arg("choice", "discard"));
    c.pump(Duration::from_millis(5));
}

/// S7-44: файл больше 16 МБ с жёсткой ссылкой (запись на месте) под нашим запретом
/// записи сохраняется: исходник переходит на свою копию, обе ссылки видят новый текст,
/// документ снова под запретом, отмена читает прежнюю версию из копии.
#[test]
fn big_hardlinked_save_under_own_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    let second = dir.path().join("big2.log");
    let line = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
    let n = (17 << 20) / line.len() + 1;
    let body: Vec<u8> = line.repeat(n);
    std::fs::write(&path, &body).unwrap();
    std::fs::hard_link(&path, &second).unwrap();
    let mut c = core_with(dir.path(), EditorCfg::default());
    open_engine(&mut c, &path, 80);
    assert_eq!(eng(&c).guard, Guard::Locked);
    input(&mut c, "Z");
    cmd(&mut c, "editor.save");
    pump_until(&mut c, |c| ed(c).is_ready() && !ed(c).dirty);
    let file = std::fs::read(&path).unwrap();
    assert_eq!(file.len(), body.len() + 1);
    assert!(file.starts_with(b"Z0123"));
    assert_eq!(
        std::fs::read(&second).unwrap(),
        file,
        "вторая ссылка — тот же файл"
    );
    assert_eq!(eng(&c).guard, Guard::Locked);
    #[cfg(windows)]
    {
        let w = std::fs::OpenOptions::new().write(true).open(&path);
        assert!(w.is_err(), "файл снова под запретом записи");
    }
    cmd(&mut c, "editor.undo");
    assert_eq!(head(&c, 8), b"01234567", "отмена после сохранения");
    cmd(&mut c, "editor.close");
    c.dispatch(Command::new("editor.answer").arg("choice", "discard"));
    c.pump(Duration::from_millis(5));
}

/// Windows (проверка S7-31): `ReplaceFileW` при открытом нами handle с запретом записи
/// другим проходит, прежний handle читает старые байты; запасной путь — с резервной
/// копией, которая удаляется сразу (handle держит байты). `MoveFileExW` при нашем
/// handle — отказ.
#[cfg(windows)]
#[test]
fn replace_under_own_lock() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t.txt");
    let temp = dir.path().join(".t.txt.urfin~");
    std::fs::write(&target, b"old").unwrap();
    std::fs::write(&temp, b"new").unwrap();
    let mut held = crate::sys::open_locked(&target).unwrap();
    crate::sys::replace_file(&target, &temp).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    let mut b = Vec::new();
    held.read_to_end(&mut b).unwrap();
    assert_eq!(b, b"old");
    drop(held);
    // Запасной путь.
    std::fs::write(&temp, b"newer").unwrap();
    let held = crate::sys::open_locked(&target).unwrap();
    let backup = dir.path().join(".t.txt.urfin-old~");
    crate::sys::replace_file_backup(&target, &temp, &backup).unwrap();
    std::fs::remove_file(&backup).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"newer");
    let mut b = [0u8; 3];
    crate::sys::read_at(&held, &mut b, 0).unwrap();
    assert_eq!(&b, b"new");
    // MoveFileExW при нашем handle — отказ.
    std::fs::write(&temp, b"x").unwrap();
    let held2 = crate::sys::open_locked(&target).unwrap();
    assert!(crate::sys::move_replace(&temp, &target).is_err());
    drop((held, held2));
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 2, "цель и временный: {left:?}");
}

/// Свободное место тома известно (Windows) — вопрос «Нет места» возможен.
#[test]
fn free_space_is_known() {
    let dir = tempfile::tempdir().unwrap();
    assert!(crate::sys::free_space(dir.path()).unwrap() > 0);
}
