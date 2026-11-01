//! Приёмка S7-27 (полоса изменений): набрал и стёр — строка не изменена; вставка строки —
//! `added = 1`; удаление и правка внутри строки; счётчик по снимку = метки видимого.

#![allow(clippy::cast_possible_truncation, reason = "тесты: номера строк малы")]

use proptest::prelude::*;
use urfin_core::CancelToken;

use super::*;
use crate::edit::piece::tests::{Kind, doc_of, kind, pos, text};
use crate::index::scan;

/// Счётчик по снимку и метки всего документа; счётчик = числу меток.
fn both(doc: &TextDoc, orig_len: u64) -> (LineChanges, Vec<(u32, Change)>) {
    let c = count(&doc.snap(), 0..orig_len, &CancelToken::new()).unwrap();
    let v = visible(doc, 0..doc.len(), 0..orig_len).unwrap();
    let n = |k: Change| v.iter().filter(|(_, c)| *c == k).count() as u32;
    assert_eq!(
        (c.added, c.modified, c.deleted),
        (n(Change::Added), n(Change::Modified), n(Change::Deleted)),
        "count vs visible {v:?}"
    );
    (c, v)
}

fn doc(text: &[u8]) -> TextDoc {
    doc_of(text, Sep::Byte(b'\n'))
}

const ABC: &[u8] = b"abc\ndef\nghi\n";

#[test]
fn type_and_erase_is_unchanged() {
    let mut d = doc(ABC);
    d.insert(5, b"XY").unwrap();
    let (c, v) = both(&d, 12);
    assert_eq!((c.modified, v), (1, vec![(1, Change::Modified)]));
    d.remove(6..7);
    d.remove(5..6);
    let (c, v) = both(&d, 12);
    assert!(c.is_empty() && v.is_empty(), "{c:?} {v:?}");
}

#[test]
fn inserted_line_is_added() {
    // Home + строка с переводом.
    let mut d = doc(ABC);
    d.insert(4, b"new\n").unwrap();
    let (c, v) = both(&d, 12);
    assert_eq!((c.added, c.modified, c.deleted), (1, 0, 0));
    assert_eq!(v, vec![(1, Change::Added)]);
    // End + Enter + набор.
    let mut d = doc(ABC);
    d.insert(3, b"\n").unwrap();
    d.insert(4, b"x").unwrap();
    d.insert(5, b"yz").unwrap();
    let (c, v) = both(&d, 12);
    assert_eq!((c.added, c.modified, c.deleted), (1, 0, 0), "{v:?}");
    assert_eq!(v, vec![(1, Change::Added)]);
    // Пустая строка Enter'ом в конце.
    let mut d = doc(ABC);
    d.insert(7, b"\n").unwrap();
    let (_, v) = both(&d, 12);
    assert_eq!(v, vec![(2, Change::Added)]);
}

#[test]
fn deletes_and_edits() {
    // Строка удалена целиком — метка удаления перед следующей.
    let mut d = doc(ABC);
    d.remove(4..8);
    assert_eq!(both(&d, 12).1, vec![(1, Change::Deleted)]);
    // Удалена последняя строка — метка в конце.
    let mut d = doc(ABC);
    d.remove(8..12);
    assert_eq!(both(&d, 12).1, vec![(2, Change::Deleted)]);
    // Удалена первая строка.
    let mut d = doc(ABC);
    d.remove(0..4);
    assert_eq!(both(&d, 12).1, vec![(0, Change::Deleted)]);
    // Символ внутри строки заменён.
    let mut d = doc(ABC);
    d.replace(5..6, b"Z").unwrap();
    assert_eq!(both(&d, 12).1, vec![(1, Change::Modified)]);
    // Удалён символ внутри строки — шов внутри — изменена.
    let mut d = doc(ABC);
    d.remove(5..6);
    assert_eq!(both(&d, 12).1, vec![(1, Change::Modified)]);
    // Строки склеены.
    let mut d = doc(ABC);
    d.remove(3..4);
    assert_eq!(both(&d, 12).1, vec![(0, Change::Modified)]);
    // Всё удалено.
    let mut d = doc(ABC);
    d.remove(0..12);
    assert_eq!(both(&d, 12).1, vec![(0, Change::Deleted)]);
}

#[test]
fn new_file_lines_are_added() {
    let mut d = doc(b"");
    d.insert(0, b"one\ntwo\nthree").unwrap();
    let (c, v) = both(&d, 0);
    assert_eq!(c.added, 3);
    assert_eq!(
        v,
        vec![(0, Change::Added), (1, Change::Added), (2, Change::Added)]
    );
}

#[test]
fn utf16_lines() {
    let k = Kind::Le;
    let orig = k.encode(b"ab\ncd\n");
    let mut d = doc_of(&orig, k.sep());
    d.insert(6, &k.encode(b"new\n")).unwrap();
    let (c, v) = both(&d, orig.len() as u64);
    assert_eq!((c.added, c.modified), (1, 0));
    assert_eq!(v, vec![(1, Change::Added)]);
}

/// Начала строк документа.
fn starts(doc: &TextDoc) -> Vec<u64> {
    let mut b = vec![0; doc.len() as usize];
    doc.snap().read_at(0, &mut b).unwrap();
    let mut v = vec![0];
    scan(&b, 0, doc.sep(), |s| v.push(s));
    v
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Счётчик по снимку = метки целиком; метки диапазона строк = метки целиком в нём.
    #[test]
    fn count_and_visible_agree(
        k in kind(),
        orig in proptest::collection::vec(prop_oneof![Just(b'x'), Just(b'\n')], 0..200),
        edits in proptest::collection::vec((0.0..=1.0f64, 0..8usize, text()), 0..40),
        a in 0.0..=1.0f64,
        b in 0.0..=1.0f64,
    ) {
        let orig = k.encode(&orig);
        let olen = orig.len() as u64;
        let mut d = doc_of(&orig, k.sep());
        let (c, v) = both(&d, olen);
        prop_assert!(c.is_empty() && v.is_empty());
        let u = k.unit();
        let mut len = orig.len();
        for (f, n, t) in &edits {
            let at = pos(*f, len, u);
            let end = (at + n * u).min(len);
            let t = k.encode(t);
            d.replace(at as u64..end as u64, &t).unwrap();
            len = len - (end - at) + t.len();
        }
        let (_, full) = both(&d, olen);
        let st = starts(&d);
        let (mut l1, mut l2) = (pos(a, st.len() - 1, 1), pos(b, st.len() - 1, 1));
        if l1 > l2 {
            std::mem::swap(&mut l1, &mut l2);
        }
        let end = if l2 + 1 >= st.len() { d.len() } else { st[l2] };
        let part = visible(&d, st[l1]..end, 0..olen).unwrap();
        let hi = if end == d.len() { u32::MAX } else { l2 as u32 };
        let want: Vec<(u32, Change)> = full
            .iter()
            .filter(|(l, _)| *l >= l1 as u32 && *l < hi)
            .map(|(l, c)| (l - l1 as u32, *c))
            .collect();
        prop_assert_eq!(part, want);
    }
}
