//! Определения ссылок Markdown (`[r]: /u "t"`, `CommonMark` §4.7) в начале абзаца. Они не
//! текст: абзац из одних определений заголовком Setext не становится — строка `===` под ним
//! сама текст абзаца (пример спецификации: `[foo]: /url` / `===` / `[foo]`), и `<a>` за ней
//! HTML-блока не начинает.
//!
//! Определение бывает в несколько строк (метка, адрес, заголовок), поэтому разбор — автомат
//! по строкам абзаца, состояние [`Def`] — в точке. Строка подчёркивания сверяется с уже
//! прочитанным: незакрытые метка и заголовок — текст, и она — заголовок Setext.

/// Длина метки на строке (спецификация — 999 символов; между строками не считается).
const LABEL_MAX: usize = 999;
/// Вложенность скобок адреса (как у `cmark`).
const PARENS_MAX: usize = 32;

/// Куда дошёл разбор определений в абзаце.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Def {
    /// В абзаце есть текст (или абзаца нет): определений дальше нет.
    #[default]
    Text,
    /// Определения полны, следующая строка начинает новое.
    Done,
    /// То же, но у последнего нет заголовка: он может быть на следующей строке.
    NoTitle,
    /// Метка не закрыта; `some` — в ней уже есть непробельный символ.
    Label { some: bool },
    /// За `[r]:` конец строки: адрес — на следующей.
    Dest,
    /// Заголовок не закрыт: символ его конца.
    Title(u8),
}

fn at_end(t: &[u8], i: usize) -> bool {
    t.get(i..)
        .is_none_or(|r| r.iter().all(u8::is_ascii_whitespace))
}

fn skip_ws(t: &[u8], mut i: usize) -> usize {
    while matches!(t.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    i
}

impl Def {
    /// В состояние точки (4 бита).
    pub(super) fn code(self) -> u8 {
        match self {
            Def::Text => 0,
            Def::Done => 1,
            Def::NoTitle => 2,
            Def::Label { some: false } => 3,
            Def::Label { some: true } => 4,
            Def::Dest => 5,
            Def::Title(b'"') => 6,
            Def::Title(b'\'') => 7,
            Def::Title(_) => 8,
        }
    }

    pub(super) fn of_code(c: u8) -> Def {
        match c {
            1 => Def::Done,
            2 => Def::NoTitle,
            3 => Def::Label { some: false },
            4 => Def::Label { some: true },
            5 => Def::Dest,
            6 => Def::Title(b'"'),
            7 => Def::Title(b'\''),
            8 => Def::Title(b')'),
            _ => Def::Text,
        }
    }

    /// Абзац пока — только полные определения: подчёркивание Setext ему не положено.
    pub(super) fn only_defs(self) -> bool {
        matches!(self, Def::Done | Def::NoTitle)
    }

    /// Новый абзац, первая строка `t` (без отступа).
    pub(super) fn first(t: &[u8]) -> Def {
        Def::Done.next(t)
    }

    /// Следующая строка абзаца `t` (без отступа).
    pub(super) fn next(self, t: &[u8]) -> Def {
        match self {
            Def::Text => Def::Text,
            Def::NoTitle if matches!(t.first(), Some(b'"' | b'\'' | b'(')) => Self::title(t, 0),
            Def::Done | Def::NoTitle => Self::open(t),
            Def::Label { some } => Self::label(t, 0, some),
            Def::Dest => Self::dest(t, 0),
            Def::Title(q) => Self::title_rest(t, 0, q),
        }
    }

    /// Начало определения.
    fn open(t: &[u8]) -> Def {
        if t.first() == Some(&b'[') {
            Self::label(t, 1, false)
        } else {
            Def::Text
        }
    }

    /// Метка с `i`, `some` — в ней уже есть непробельный символ.
    fn label(t: &[u8], mut i: usize, mut some: bool) -> Def {
        let from = i;
        while let Some(&c) = t.get(i) {
            if i - from > LABEL_MAX {
                return Def::Text;
            }
            match c {
                b'\\' => {
                    some = true;
                    i += 2;
                }
                b'[' => return Def::Text,
                b']' => {
                    if !some || t.get(i + 1) != Some(&b':') {
                        return Def::Text;
                    }
                    let j = skip_ws(t, i + 2);
                    return if at_end(t, j) {
                        Def::Dest
                    } else {
                        Self::dest(t, j)
                    };
                }
                _ => {
                    some |= !c.is_ascii_whitespace();
                    i += 1;
                }
            }
        }
        Def::Label { some }
    }

    /// Адрес с `i` и что за ним.
    fn dest(t: &[u8], i: usize) -> Def {
        let Some(e) = Self::dest_end(t, i) else {
            return Def::Text;
        };
        if at_end(t, e) {
            return Def::NoTitle;
        }
        let j = skip_ws(t, e);
        if j == e {
            return Def::Text;
        }
        Self::title(t, j)
    }

    /// Конец адреса с `i`.
    fn dest_end(t: &[u8], mut i: usize) -> Option<usize> {
        if t.get(i) == Some(&b'<') {
            i += 1;
            while let Some(&c) = t.get(i) {
                match c {
                    b'\\' => i += 2,
                    b'<' => return None,
                    b'>' => return Some(i + 1),
                    _ => i += 1,
                }
            }
            return None;
        }
        let from = i;
        let mut depth = 0usize;
        while let Some(&c) = t.get(i) {
            match c {
                b'\\' if t.get(i + 1).is_some_and(u8::is_ascii_punctuation) => i += 1,
                b'(' => {
                    depth += 1;
                    if depth > PARENS_MAX {
                        return None;
                    }
                }
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                c if c <= b' ' || c == 0x7F => break,
                _ => {}
            }
            i += 1;
        }
        (i > from && depth == 0).then_some(i)
    }

    /// Заголовок с `i` — открывающий символ.
    fn title(t: &[u8], i: usize) -> Def {
        match t.get(i) {
            Some(&q @ (b'"' | b'\'')) => Self::title_rest(t, i + 1, q),
            Some(b'(') => Self::title_rest(t, i + 1, b')'),
            _ => Def::Text,
        }
    }

    /// Заголовок с `i` до символа конца `q`: за ним — только пробелы.
    fn title_rest(t: &[u8], mut i: usize, q: u8) -> Def {
        while let Some(&c) = t.get(i) {
            match c {
                b'\\' => i += 2,
                c if c == q => {
                    return if at_end(t, i + 1) {
                        Def::Done
                    } else {
                        Def::Text
                    };
                }
                b'(' if q == b')' => return Def::Text,
                _ => i += 1,
            }
        }
        Def::Title(q)
    }
}
