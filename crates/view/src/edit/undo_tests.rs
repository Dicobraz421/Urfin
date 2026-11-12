//! Приёмка S7-28: отмена всех правок = исходник, повтор всех = последний документ,
//! отмена восстанавливает выделение; «изменён» по позиции сохранения; новая правка
//! отрезает повтор; память ≤ потолка при миллионе нажатий.

#![allow(
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "тесты: смещения в памяти, короткие имена правок"
)]

use std::time::{Duration, Instant};

use encoding_rs::WINDOWS_1251;
use proptest::prelude::*;

use super::*;
use crate::edit::piece::tests::{Kind as Enc8, doc_of, kind, pos, text};
use crate::index::Sep;

/// Выделение теста: курсор.
type Sel = u64;

/// Байты документа.
fn bytes(doc: &TextDoc) -> Vec<u8> {
    let mut b = vec![0; doc.len() as usize];
    doc.snap().read_at(0, &mut b).unwrap();
    b
}

#[derive(Clone, Debug)]
enum Act {
    /// Правка: доля позиции, удалить единиц, вставить.
    Edit(f64, usize, Vec<u8>),
    /// Пауза в мс до следующей правки.
    Wait(u64),
    /// Переход курсора.
    Move,
    /// Команда: несколько правок одной группой.
    Command(Vec<(f64, usize, Vec<u8>)>),
    /// Массовая правка деревом целиком.
    Root(f64, Vec<u8>),
}

fn act() -> impl Strategy<Value = Act> {
    let edit = || (0.0..=1.0f64, 0..6usize, text());
    prop_oneof![
        6 => edit().prop_map(|(a, n, t)| Act::Edit(a, n, t)),
        1 => (0..2000u64).prop_map(Act::Wait),
        1 => Just(Act::Move),
        1 => proptest::collection::vec(edit(), 1..4).prop_map(Act::Command),
        1 => (0.0..=1.0f64, text()).prop_map(|(a, t)| Act::Root(a, t)),
    ]
}

/// Правка документа и модели; `Splice` и вид для журнала.
fn edit(
    doc: &mut TextDoc,
    model: &mut Vec<u8>,
    k: Enc8,
    (a, n, t): &(f64, usize, Vec<u8>),
) -> (Splice, Kind, Sel) {
    let u = k.unit();
    let at = pos(*a, model.len(), u);
    let end = (at + n * u).min(model.len());
    let b = k.encode(t);
    let s = doc.replace(at as u64..end as u64, &b).unwrap();
    let kind = if b.is_empty() {
        Kind::Delete
    } else {
        Kind::Input
    };
    model.splice(at..end, b.iter().copied());
    (s, kind, (at + b.len()) as u64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// N правок → отмена всех = исходник (по пути — состояния границ групп и выделение
    /// до группы), повтор всех = последний документ.
    #[test]
    fn undo_redo_all(
        k in kind(),
        orig in proptest::collection::vec(prop_oneof![Just(b'x'), Just(b'\n')], 0..200),
        acts in proptest::collection::vec(act(), 1..80),
    ) {
        let orig = k.encode(&orig);
        let mut doc = doc_of(&orig, k.sep());
        let mut model = orig.clone();
        let mut j: Journal<Sel> = Journal::new();
        let mut now = Instant::now();
        let mut sel: Sel = 0;
        // Состояние и выделение перед каждой группой.
        let mut marks: Vec<(Vec<u8>, Sel)> = Vec::new();
        for a in &acts {
            let pos0 = j.pos();
            let before = (model.clone(), sel);
            match a {
                Act::Edit(f, n, t) => {
                    let (s, kind, after) = edit(&mut doc, &mut model, k, &(*f, *n, t.clone()));
                    j.record(s.into(), kind, &sel, &after, now);
                    sel = after;
                }
                Act::Wait(ms) => now += Duration::from_millis(*ms),
                Act::Move => {
                    j.break_group();
                    sel = pos(0.5, model.len(), k.unit()) as u64;
                }
                Act::Command(es) => {
                    j.begin_group(Kind::Command);
                    for e in es {
                        let (s, _, after) = edit(&mut doc, &mut model, k, e);
                        j.record(s.into(), Kind::Command, &sel, &after, now);
                        sel = after;
                    }
                }
                Act::Root(f, t) => {
                    let before_tree = doc.tree().clone();
                    let (_, _, after) = edit(&mut doc, &mut model, k, &(*f, 0, t.clone()));
                    let after_tree = doc.tree().clone();
                    j.begin_group(Kind::Command);
                    j.record(
                        Op::Root { before: before_tree, after: after_tree, enc: None, eol: None },
                        Kind::Command,
                        &sel,
                        &after,
                        now,
                    );
                    sel = after;
                }
            }
            if j.pos() > pos0 {
                marks.push(before);
            }
            prop_assert_eq!(j.pos(), marks.len());
        }
        let last = model.clone();
        prop_assert_eq!(j.is_modified(), !marks.is_empty());
        // Отмена по группам.
        for (m, s) in marks.iter().rev() {
            let step = j.undo(&mut doc).unwrap();
            prop_assert_eq!(&bytes(&doc), m);
            prop_assert_eq!(step.sel, *s);
            doc.tree().check();
        }
        prop_assert!(j.undo(&mut doc).is_none());
        prop_assert!(!j.can_undo());
        prop_assert_eq!(bytes(&doc), orig);
        prop_assert!(!j.is_modified());
        // Повтор всех.
        while j.redo(&mut doc).is_some() {}
        prop_assert_eq!(bytes(&doc), last);
        prop_assert!(!j.can_redo());
    }
}

fn doc() -> TextDoc {
    doc_of(b"abc\ndef\n", Sep::Byte(b'\n'))
}

/// Набирает `s` с позиции `at` по символу.
fn type_at(doc: &mut TextDoc, j: &mut Journal<Sel>, at: u64, s: &[u8], now: Instant) -> u64 {
    let mut at = at;
    for c in s {
        let sp = doc.insert(at, &[*c]).unwrap();
        j.record(sp.into(), Kind::Input, &at, &(at + 1), now);
        at += 1;
    }
    at
}

#[test]
fn grouping_rules() {
    let mut d = doc();
    let mut j: Journal<Sel> = Journal::new();
    let t = Instant::now();
    // Серия нажатий без пауз — одна группа, одна операция.
    let at = type_at(&mut d, &mut j, 3, b"hello", t);
    assert_eq!((j.len(), j.groups[0].ops.len()), (1, 1));
    // Backspace — смена вида: новая группа; серия Backspace — одна операция.
    for i in 0..3 {
        let sp = d.remove(at - 1 - i..at - i);
        j.record(sp.into(), Kind::Delete, &(at - i), &(at - 1 - i), t);
    }
    assert_eq!((j.len(), j.groups[1].ops.len()), (2, 1));
    // Пауза ≥ 1 с — новая группа.
    type_at(&mut d, &mut j, at - 3, b"x", t);
    type_at(
        &mut d,
        &mut j,
        at - 2,
        b"y",
        t + Duration::from_millis(1500),
    );
    assert_eq!(j.len(), 4);
    // Переход курсора — новая группа.
    j.break_group();
    type_at(&mut d, &mut j, 0, b"z", t + Duration::from_millis(1600));
    assert_eq!(j.len(), 5);
    // Команда — своя группа, следующая правка — новая.
    j.begin_group(Kind::Command);
    let sp = d.insert(0, b"cmd").unwrap();
    j.record(sp.into(), Kind::Command, &0, &3, t);
    let sp = d.remove(5..6);
    j.record(sp.into(), Kind::Command, &3, &3, t);
    assert_eq!((j.len(), j.groups[5].ops.len()), (6, 2));
    type_at(&mut d, &mut j, 0, b"q", t + Duration::from_millis(1700));
    assert_eq!(j.len(), 7);
    // Отмена команды — одной группой.
    j.undo(&mut d).unwrap();
    let step = j.undo(&mut d).unwrap();
    assert_eq!(step.sel, 0);
    assert_eq!(bytes(&d), b"zabchexy\ndef\n");
}

#[test]
fn saved_position_and_redo_cut() {
    let mut d = doc();
    let mut j: Journal<Sel> = Journal::new();
    let t = Instant::now();
    assert!(!j.is_modified() && !j.can_undo() && !j.can_redo());
    type_at(&mut d, &mut j, 0, b"ab", t);
    assert!(j.is_modified() && j.can_undo());
    j.mark_saved();
    assert!(!j.is_modified());
    // После сохранения — новая группа даже без паузы.
    type_at(&mut d, &mut j, 2, b"cd", t);
    assert_eq!(j.len(), 2);
    assert!(j.is_modified());
    // Отмена до сохранённого — не изменён; ещё отмена — изменён; повтор — снова нет.
    j.undo(&mut d).unwrap();
    assert!(!j.is_modified() && j.can_redo());
    j.undo(&mut d).unwrap();
    assert!(j.is_modified());
    j.redo(&mut d).unwrap();
    assert!(!j.is_modified());
    // Новая правка после отмены отрезает повтор; сохранённое — там, где было.
    j.undo(&mut d).unwrap();
    type_at(&mut d, &mut j, 0, b"Z", t + Duration::from_secs(5));
    assert!(!j.can_redo());
    assert_eq!(j.len(), 1);
    assert!(j.is_modified());
    j.undo(&mut d).unwrap();
    assert!(j.is_modified(), "saved state was cut away");
    assert_eq!(bytes(&d), b"abc\ndef\n");
}

#[test]
fn root_op_restores_encoding() {
    let mut d = doc();
    let mut j: Journal<Sel> = Journal::new();
    let before = d.tree().clone();
    d.replace(0..3, b"\xc0\xc1").unwrap();
    let after = d.tree().clone();
    let (u8e, cp) = (Enc::utf8(), Enc::new(WINDOWS_1251));
    j.begin_group(Kind::Command);
    j.record(
        Op::Root {
            before,
            after,
            enc: Some((u8e, cp)),
            eol: None,
        },
        Kind::Command,
        &0,
        &2,
        Instant::now(),
    );
    let s = j.undo(&mut d).unwrap();
    assert_eq!((s.sel, s.enc), (0, Some(u8e)));
    assert_eq!(bytes(&d), b"abc\ndef\n");
    let s = j.redo(&mut d).unwrap();
    assert_eq!((s.sel, s.enc), (2, Some(cp)));
    assert_eq!(bytes(&d), b"\xc0\xc1\ndef\n");
}

#[test]
fn drop_src_cuts_unreachable_groups() {
    let mut d = doc();
    let mut j: Journal<Sel> = Journal::new();
    let t = Instant::now();
    type_at(&mut d, &mut j, 0, b"a", t);
    j.break_group();
    let sp = d.remove(2..3);
    j.record(sp.into(), Kind::Delete, &3, &2, t);
    j.break_group();
    type_at(&mut d, &mut j, 0, b"b", t);
    j.break_group();
    type_at(&mut d, &mut j, 0, b"c", t);
    j.undo(&mut d).unwrap();
    // Группа 1 убирала куски `Original`; 2 и 3 — только `Add`.
    assert_eq!(j.drop_src(Src::Original), 2);
    assert_eq!((j.len(), j.pos()), (2, 1));
    assert!(j.can_undo() && j.can_redo());
    j.undo(&mut d).unwrap();
    assert!(!j.can_undo());
}

/// Миллион нажатий: память ≤ потолка, старые группы отброшены, оставшиеся отменяются и
/// повторяются.
fn million(presses: usize, cap: usize) {
    let mut d = doc();
    let mut j: Journal<Sel> = Journal::with_cap(cap);
    let t = Instant::now();
    let mut seed = 7u64;
    let mut at = 0u64;
    for i in 0..presses {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let now = t + Duration::from_millis(i as u64 * 50);
        if seed.is_multiple_of(16) {
            // Переход курсора: правки по всему документу, группы не сливаются.
            j.break_group();
            at = seed % (d.len() + 1);
        }
        if seed.is_multiple_of(5) && at > 0 {
            let sp = d.remove(at - 1..at);
            j.record(sp.into(), Kind::Delete, &at, &(at - 1), now);
            at -= 1;
        } else {
            let sp = d.insert(at, b"k").unwrap();
            j.record(sp.into(), Kind::Input, &at, &(at + 1), now);
            at += 1;
        }
        assert!(j.bytes() <= cap, "journal {} > {cap}", j.bytes());
    }
    assert!(j.base > 0, "old groups dropped");
    let last = bytes(&d);
    let n = j.pos();
    while j.undo(&mut d).is_some() {}
    while j.redo(&mut d).is_some() {}
    assert_eq!(j.pos(), n);
    assert_eq!(bytes(&d), last);
    println!(
        "{presses} presses: {} groups kept, {} dropped, journal {} B",
        j.len(),
        j.base,
        j.bytes()
    );
}

#[test]
fn journal_cap_small() {
    million(100_000, 256 << 10);
}

/// Миллион нажатий, потолок 16 МБ (`--release --run-ignored only`).
#[test]
#[ignore = "миллион правок, release"]
fn journal_cap_million() {
    million(1_000_000, CAP);
}

/// S7-44: временный файл `Rewrite(n)` освобождается, когда на него не ссылаются ни
/// журнал (отрезанный повтор, обрезка по потолку), ни текст: handle закрыт — файл с
/// диска ушёл (Windows: `FILE_FLAG_DELETE_ON_CLOSE`), слот — следующему временному файлу.
#[test]
#[allow(clippy::too_many_lines, reason = "сценарий: повтор, затем обрезка")]
fn rewrite_slot_released_after_trim() {
    use std::sync::{Arc, Weak};

    use crate::detect::Eol;
    use crate::edit::input::{Act, Engine, Env, Selections};
    use crate::edit::open::Guard;
    use crate::edit::piece::{Lf, Piece, SpillSource, create_temp};
    use crate::enc::Enc;
    use crate::source::ByteSource;

    let dir = tempfile::tempdir().unwrap();
    let mut k = 0;
    // Временный файл массовой правки с байтами `b`: источник, его `Weak`, путь.
    let mut temp = |b: &[u8]| {
        k += 1;
        let name = format!("rw{k}");
        let f = create_temp(dir.path(), &name, "rw").unwrap();
        crate::sys::write_all_at(&f, b, 0).unwrap();
        let src: Arc<dyn ByteSource> = Arc::new(SpillSource::sealed(f, b.len() as u64));
        let weak: Weak<dyn ByteSource> = Arc::downgrade(&src);
        (src, weak, dir.path().join(format!("{name}.rw")))
    };
    let gone = |w: &Weak<dyn ByteSource>, p: &std::path::Path| {
        let closed = w.upgrade().is_none();
        if cfg!(windows) {
            assert_eq!(closed, !p.exists(), "файл на диске — пока открыт");
        }
        closed
    };
    let body = b"one\ntwo\n";
    let mut e = Engine::new(
        doc_of(body, Sep::Byte(b'\n')),
        Enc::utf8(),
        Eol::Lf,
        Guard::Memory,
        0..body.len() as u64,
    );
    e.undo = Journal::with_cap(4 << 10);
    let t0 = Instant::now();
    let mut tick = 0u64;
    let mut env = || {
        tick += 1;
        Env {
            eol_new: Eol::Lf,
            readonly: false,
            now: t0 + Duration::from_secs(2 * tick),
        }
    };
    // «Заменить все»: новое дерево над временным файлом одной группой `Op::Root`.
    let mass = |e: &mut Engine, src: Arc<dyn ByteSource>, now: Instant| {
        let len = src.len();
        let n = e.text.push_rewrite(src);
        let before = e.text.tree().clone();
        let after = Tree::from_pieces([Piece {
            src: n,
            start: 0,
            len,
            lf: Lf::Known(2),
        }]);
        e.text.set_tree(after.clone());
        e.text.load_now(0..len);
        let sel = Selections::at(0);
        e.undo.begin_group(Kind::Command);
        e.undo.record(
            Op::Root {
                before,
                after,
                enc: None,
                eol: None,
            },
            Kind::Command,
            &sel,
            &sel,
            now,
        );
        e.sel = sel;
        n
    };
    let input = |e: &mut Engine, env: &Env| {
        e.act(
            Act::Input {
                text: "x".into(),
                replace: false,
            },
            env,
        )
        .unwrap();
        e.release_rewrites()
    };

    // Повтор отрезан новой правкой.
    let (src, weak, path) = temp(b"ONE\nTWO\n");
    let n = mass(&mut e, src, env().now);
    assert!(e.undo(false).is_some());
    assert_eq!(e.release_rewrites(), 0);
    assert!(!gone(&weak, &path), "повтор ещё ссылается");
    assert_eq!(input(&mut e, &env()), 1);
    assert!(gone(&weak, &path), "повтор отрезан — файл закрыт");
    let (src, weak2, path2) = temp(b"AAA\nBBB\n");
    assert_eq!(mass(&mut e, src, env().now), n, "слот — снова в деле");
    assert_eq!(bytes(&e.text), b"AAA\nBBB\n");

    // Обрезка журнала: пока текст ссылается — файл открыт.
    for _ in 0..200 {
        let env = env();
        input(&mut e, &env);
    }
    assert!(!e.undo.uses_rewrite(match n {
        Src::Rewrite(n) => n,
        _ => unreachable!(),
    }));
    assert!(!gone(&weak2, &path2), "текст ссылается");
    // Весь текст прочь: ссылка — только в группе удаления; обрезка — файл закрыт.
    let env1 = env();
    e.act(Act::SelectAll, &env1).unwrap();
    e.act(Act::Delete(crate::edit::input::Move::Right), &env1)
        .unwrap();
    assert!(e.text.is_empty());
    assert_eq!(e.release_rewrites(), 0);
    assert!(!gone(&weak2, &path2), "группа удаления ссылается");
    let mut freed = 0;
    for _ in 0..200 {
        let env = env();
        freed += input(&mut e, &env);
    }
    assert_eq!(freed, 1);
    assert!(
        gone(&weak2, &path2),
        "после обрезки журнала место освобождено"
    );
    assert!(e.undo.bytes() <= 4 << 10);
}
