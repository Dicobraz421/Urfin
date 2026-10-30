//! HIR-перевод для однобайтовых кодировок (1251, 866, KOI8, 1252…): Unicode-литералы и
//! классы → байтовые через таблицу `байт → символ` на 256 значений, без перекодирования
//! файла (S7 §3.6).
//!
//! Символ, которого нет в кодировке, — пустой класс (ветка не совпадает никогда).
//! Ограничение: `\b` и прочие границы слов становятся ASCII-границами — буквы вне ASCII
//! (кириллица) для них не «слово». Опция «Слово» текстового поиска от этого не зависит:
//! её проверяет [`super::Matcher`] по таблице.

use regex_syntax::hir::{
    Capture, Class, ClassBytes, ClassBytesRange, ClassUnicode, Hir, HirKind, Look, Repetition,
};

use crate::enc::ByteTable;

/// Переводит Unicode-HIR в байтовый для однобайтовой кодировки `t`.
pub fn translate(h: &Hir, t: &ByteTable) -> Hir {
    match h.kind() {
        HirKind::Empty => Hir::empty(),
        HirKind::Literal(lit) => literal(&lit.0, t),
        HirKind::Class(Class::Unicode(c)) => class(c, t),
        HirKind::Class(Class::Bytes(c)) => Hir::class(Class::Bytes(c.clone())),
        HirKind::Look(l) => Hir::look(look(*l)),
        HirKind::Repetition(r) => Hir::repetition(Repetition {
            min: r.min,
            max: r.max,
            greedy: r.greedy,
            sub: Box::new(translate(&r.sub, t)),
        }),
        HirKind::Capture(c) => Hir::capture(Capture {
            index: c.index,
            name: c.name.clone(),
            sub: Box::new(translate(&c.sub, t)),
        }),
        HirKind::Concat(v) => Hir::concat(v.iter().map(|h| translate(h, t)).collect()),
        HirKind::Alternation(v) => Hir::alternation(v.iter().map(|h| translate(h, t)).collect()),
    }
}

/// Байты, которые кодировка `t` декодирует в `c`.
fn bytes_of(c: char, t: &ByteTable) -> impl Iterator<Item = u8> + '_ {
    (0..=255u8).filter(move |&b| t.0[b as usize] == c)
}

fn byte_set(bytes: impl Iterator<Item = u8>) -> Hir {
    let mut ranges: Vec<ClassBytesRange> = Vec::new();
    for b in bytes {
        match ranges.last_mut() {
            Some(r) if r.end() + 1 == b => *r = ClassBytesRange::new(r.start(), b),
            _ => ranges.push(ClassBytesRange::new(b, b)),
        }
    }
    match ranges.as_slice() {
        [] => Hir::fail(),
        [r] if r.start() == r.end() => Hir::literal([r.start()]),
        _ => Hir::class(Class::Bytes(ClassBytes::new(ranges))),
    }
}

/// Литерал: валидный UTF-8 — по символам через таблицу; байты `(?-u)\xNN` — как есть.
fn literal(bytes: &[u8], t: &ByteTable) -> Hir {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return Hir::literal(bytes);
    };
    Hir::concat(s.chars().map(|c| byte_set(bytes_of(c, t))).collect())
}

fn class(c: &ClassUnicode, t: &ByteTable) -> Hir {
    let r = c.ranges();
    let has = |ch: char| {
        let i = r.partition_point(|x| x.end() < ch);
        r.get(i).is_some_and(|x| x.start() <= ch)
    };
    byte_set((0..=255u8).filter(|&b| has(t.0[b as usize])))
}

fn look(l: Look) -> Look {
    match l {
        Look::WordUnicode => Look::WordAscii,
        Look::WordUnicodeNegate => Look::WordAsciiNegate,
        Look::WordStartUnicode => Look::WordStartAscii,
        Look::WordEndUnicode => Look::WordEndAscii,
        Look::WordStartHalfUnicode => Look::WordStartHalfAscii,
        Look::WordEndHalfUnicode => Look::WordEndHalfAscii,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use encoding_rs::{IBM866, KOI8_R, WINDOWS_1251};
    use regex_automata::meta;
    use regex_syntax::ParserBuilder;

    use super::*;

    fn re(p: &str, case: bool, enc: &'static encoding_rs::Encoding) -> meta::Regex {
        let hir = ParserBuilder::new()
            .utf8(false)
            .case_insensitive(!case)
            .build()
            .parse(p)
            .unwrap();
        let hir = translate(&hir, ByteTable::of(enc));
        meta::Builder::new()
            .configure(meta::Config::new().utf8_empty(false))
            .build_from_hir(&hir)
            .unwrap()
    }

    #[test]
    fn cyrillic_classes_and_case() {
        for enc in [WINDOWS_1251, IBM866, KOI8_R] {
            let (hay, _, _) = enc.encode("Ошибка: ПРИВЕТ мир 42");
            let r = re("привет\\s+[а-я]+", false, enc);
            let m = r.find(&*hay).unwrap();
            let (want, _, _) = enc.encode("ПРИВЕТ мир");
            assert_eq!(&hay[m.range()], &*want, "{}", enc.name());
            assert!(re("привет", true, enc).find(&*hay).is_none());
            assert!(re("\\d+$", true, enc).find(&*hay).is_some());
            // Символа нет в кодировке — пустой класс.
            assert!(re("日|本", true, enc).find(&*hay).is_none());
        }
    }
}
