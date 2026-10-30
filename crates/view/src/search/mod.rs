//! Поиск во вьювере: литерал, regex по байтам, однобайтовые и многобайтовые кодировки, hex
//! (S7 §0, §3.6). Карточка S7-09.
//!
//! [`compile`] строит [`Matcher`] по образцу и кодировке документа:
//! - UTF-8: литерал с учётом регистра — `memchr::memmem`, иначе `regex-automata::meta` по
//!   байтам (Unicode, `(?i)`);
//! - однобайтовые (1251, 866, KOI8, 1252…): HIR Unicode → [`hir_bytes::translate`] → meta,
//!   без перекодирования;
//! - UTF-16 и прочие многобайтовые: окно перекодируется в UTF-8 с картой смещений
//!   ([`transcode`]);
//! - hex: `DE AD ?? EF` — memmem, с `??` — байтовый regex.
//!
//! «Слово» (текстовый поиск) — проверка соседних символов совпадения по кодировке, а не
//! `\b`: так кириллица в 1251/866/KOI8 тоже «слово». Чанковый проход по файлу —
//! [`scan`], фоновая задача и `SearchState` документа — [`job`].

pub mod hex;
pub mod hir_bytes;
pub mod job;
pub mod scan;
pub mod transcode;

#[cfg(test)]
mod tests;

use std::ops::Range;

use memchr::memmem;
use regex_automata::{Input, meta};
use regex_syntax::ParserBuilder;
use regex_syntax::hir::{Class, ClassBytes, ClassBytesRange, Hir};

use crate::enc::{ByteTable, Enc};
use transcode::OffMap;

pub use job::{CountState, SearchMsg, SearchReq, SearchState};
pub use scan::{Hay, SliceHay, SourceHay};

/// Перекрытие окон regex — не больше этого (S7 §3.6); совпадение длиннее обрезается.
pub const OVERLAP_MAX: usize = 64 << 10;

/// Образец поиска.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Текст; `case` — с учётом регистра, `word` — целое слово.
    Text {
        s: String,
        case: bool,
        word: bool,
    },
    Regex {
        s: String,
        case: bool,
    },
    /// Байты; `None` — `??`.
    Hex(Vec<Option<u8>>),
}

/// Ошибка образца (показывается в строке поиска).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("empty pattern")]
    Empty,
    /// `pos` — символ образца (с 0), где ошибка.
    #[error("regex error at {pos}: {msg}")]
    Regex { pos: usize, msg: String },
    #[error("bad hex pattern at {pos}")]
    Hex { pos: usize },
    #[error("pattern is too big")]
    TooBig,
}

/// Движок над байтами (исходными или перекодированными в UTF-8).
#[derive(Clone, Debug)]
enum Engine {
    Lit(Box<(memmem::Finder<'static>, memmem::FinderRev<'static>)>),
    Re(meta::Regex),
}

impl Engine {
    fn lit(b: &[u8]) -> Engine {
        Engine::Lit(Box::new((
            memmem::Finder::new(b).into_owned(),
            memmem::FinderRev::new(b).into_owned(),
        )))
    }

    /// Самое левое совпадение, начинающееся не раньше `at`.
    fn find_at(&self, hay: &[u8], at: usize) -> Option<Range<usize>> {
        if at > hay.len() {
            return None;
        }
        match self {
            Engine::Lit(f) => {
                let fwd = &f.0;
                let n = fwd.needle().len();
                fwd.find(&hay[at..]).map(|i| at + i..at + i + n)
            }
            Engine::Re(re) => re.search(&Input::new(hay).range(at..)).map(|m| m.range()),
        }
    }

    /// Совпадение с наибольшим началом в `[lo, hi)` (у regex — самое левое от этого начала).
    fn rfind(&self, hay: &[u8], lo: usize, hi: usize) -> Option<Range<usize>> {
        let hi = hi.min(hay.len());
        if lo >= hi {
            return None;
        }
        match self {
            Engine::Lit(f) => {
                let rev = &f.1;
                let n = rev.needle().len();
                let end = (hi - 1 + n).min(hay.len());
                rev.rfind(&hay[lo..end]).map(|i| lo + i..lo + i + n)
            }
            Engine::Re(_) => {
                let mut last = None;
                let mut at = lo;
                while let Some(m) = self.find_at(hay, at) {
                    if m.start >= hi {
                        break;
                    }
                    at = m.start + 1;
                    last = Some(m);
                }
                last
            }
        }
    }
}

/// Как определяется «буква слова» для опции «Слово».
#[derive(Copy, Clone, Debug)]
enum Word {
    Utf8,
    Table(&'static ByteTable),
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Word {
    fn before(self, hay: &[u8], at: usize) -> bool {
        if at == 0 {
            return false;
        }
        match self {
            Word::Table(t) => is_word(t.0[hay[at - 1] as usize]),
            Word::Utf8 => {
                let lo = at.saturating_sub(4);
                (lo..at)
                    .find_map(|i| std::str::from_utf8(&hay[i..at]).ok())
                    .and_then(|s| s.chars().next_back())
                    .is_some_and(is_word)
            }
        }
    }

    fn after(self, hay: &[u8], at: usize) -> bool {
        let Some(&b) = hay.get(at) else {
            return false;
        };
        match self {
            Word::Table(t) => is_word(t.0[b as usize]),
            Word::Utf8 => {
                let hi = (at + 4).min(hay.len());
                (at + 1..=hi)
                    .rev()
                    .find_map(|i| std::str::from_utf8(&hay[at..i]).ok())
                    .and_then(|s| s.chars().next())
                    .is_some_and(is_word)
            }
        }
    }
}

/// Скомпилированный образец.
#[derive(Clone, Debug)]
pub struct Matcher {
    eng: Engine,
    /// Окно перекодируется в UTF-8 (UTF-16, многобайтовые).
    trans: Option<Enc>,
    word: Option<Word>,
    /// Наибольшая длина совпадения в байтах источника; `None` — не ограничена.
    max_len: Option<usize>,
    unit: u8,
}

/// Окно, готовое к поиску: исходные байты или их UTF-8 с картой.
#[derive(Debug)]
pub struct Prepared<'a> {
    m: &'a Matcher,
    raw: &'a [u8],
    dec: Option<(Vec<u8>, OffMap)>,
}

impl Prepared<'_> {
    fn hay(&self) -> &[u8] {
        self.dec.as_ref().map_or(self.raw, |(u, _)| u)
    }

    fn to_hay(&self, s: usize) -> usize {
        self.dec.as_ref().map_or(s, |(u, m)| m.to_utf8(u, s))
    }

    fn to_src(&self, r: Range<usize>) -> Range<usize> {
        match &self.dec {
            None => r,
            Some((u, m)) => m.to_src(u, r.start)..m.to_src(u, r.end),
        }
    }

    fn word_ok(&self, r: &Range<usize>) -> bool {
        let Some(w) = self.m.word else {
            return true;
        };
        let hay = self.hay();
        !w.before(hay, r.start) && !w.after(hay, r.end)
    }

    /// Самое левое совпадение, начинающееся не раньше `at` (смещения — в окне).
    pub fn find_at(&self, at: usize) -> Option<Range<usize>> {
        let hay = self.hay();
        let mut at = self.to_hay(at);
        loop {
            let r = self.m.eng.find_at(hay, at)?;
            if self.word_ok(&r) {
                return Some(self.to_src(r));
            }
            at = r.start + 1;
        }
    }

    /// Совпадение с наибольшим началом в `[lo, hi)` (смещения — в окне).
    pub fn rfind(&self, lo: usize, hi: usize) -> Option<Range<usize>> {
        let hay = self.hay();
        let lo = self.to_hay(lo);
        let mut hi = self.to_hay(hi);
        loop {
            let r = self.m.eng.rfind(hay, lo, hi)?;
            if self.word_ok(&r) {
                return Some(self.to_src(r));
            }
            hi = r.start;
        }
    }
}

impl Matcher {
    /// Готовит окно `hay` (у перекодирования — декодирует его целиком).
    pub fn prepare<'a>(&'a self, hay: &'a [u8]) -> Prepared<'a> {
        let dec = self.trans.map(|enc| {
            let mut out = Vec::new();
            let mut map = OffMap::default();
            transcode::decode(hay, enc, &mut out, &mut map);
            (out, map)
        });
        Prepared {
            m: self,
            raw: hay,
            dec,
        }
    }

    /// Совпадение в `hay` (абсолютное смещение `hay` — `base`): вперёд — первое, назад —
    /// последнее.
    pub fn find(&self, hay: &[u8], base: u64, back: bool) -> Option<Range<u64>> {
        let p = self.prepare(hay);
        let r = if back {
            p.rfind(0, hay.len())
        } else {
            p.find_at(0)
        }?;
        Some(base + r.start as u64..base + r.end as u64)
    }

    /// Перекрытие окон: наибольшая длина совпадения, не больше [`OVERLAP_MAX`].
    pub fn max_len(&self) -> usize {
        self.max_len.map_or(OVERLAP_MAX, |n| n.min(OVERLAP_MAX))
    }

    /// Выравнивание начала окна (2 у UTF-16).
    pub fn unit(&self) -> u8 {
        self.unit
    }
}

/// Позиция символа по байтовому смещению в образце.
fn char_pos(s: &str, byte: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < byte).count()
}

/// Разбор regex в Unicode-HIR; ошибка — с позицией символа.
fn parse(s: &str, case: bool) -> Result<Hir, SearchError> {
    ParserBuilder::new()
        .utf8(false)
        .case_insensitive(!case)
        .build()
        .parse(s)
        .map_err(|e| {
            let (off, msg) = match &e {
                regex_syntax::Error::Parse(p) => (p.span().start.offset, p.kind().to_string()),
                regex_syntax::Error::Translate(t) => (t.span().start.offset, t.kind().to_string()),
                other => (0, other.to_string()),
            };
            SearchError::Regex {
                pos: char_pos(s, off),
                msg,
            }
        })
}

fn build(hir: &Hir) -> Result<meta::Regex, SearchError> {
    meta::Builder::new()
        .configure(meta::Config::new().utf8_empty(false))
        .build_from_hir(hir)
        .map_err(|e| {
            if e.size_limit().is_some() {
                SearchError::TooBig
            } else {
                SearchError::Regex {
                    pos: 0,
                    msg: e.to_string(),
                }
            }
        })
}

/// Совпадение нулевой длины запрещено: «найти далее» стояло бы на месте.
fn check_len(hir: &Hir) -> Result<(), SearchError> {
    if hir.properties().minimum_len().unwrap_or(0) == 0 {
        return Err(SearchError::Empty);
    }
    Ok(())
}

/// Компилирует образец под кодировку документа (S7 §3.6).
pub fn compile(p: &Pattern, enc: Enc) -> Result<Matcher, SearchError> {
    let multi = !enc.is_utf8() && !enc.is_single_byte();
    let table = enc.table();
    let trans = multi.then_some(enc);
    let unit = enc.unit();
    // Перекодированное окно: UTF-8 до 4 байт на символ, источник — до 4 (UTF-16 —
    // 2 или 4, многобайтовые — 1–4); длина совпадения в источнике — не больше
    // длины в UTF-8 × 4 / 1 — берём с запасом.
    let scale = |n: usize| if multi { n.saturating_mul(4) } else { n };
    match p {
        Pattern::Hex(bytes) => {
            if bytes.is_empty() {
                return Err(SearchError::Empty);
            }
            let eng = if bytes.iter().all(Option::is_some) {
                let lit: Vec<u8> = bytes.iter().flatten().copied().collect();
                Engine::lit(&lit)
            } else {
                let any = Class::Bytes(ClassBytes::new([ClassBytesRange::new(0, 255)]));
                let hir = Hir::concat(
                    bytes
                        .iter()
                        .map(|b| match b {
                            Some(b) => Hir::literal([*b]),
                            None => Hir::class(any.clone()),
                        })
                        .collect(),
                );
                Engine::Re(build(&hir)?)
            };
            Ok(Matcher {
                eng,
                trans: None,
                word: None,
                max_len: Some(bytes.len()),
                unit: 1,
            })
        }
        Pattern::Text { s, case, word } => {
            if s.is_empty() {
                return Err(SearchError::Empty);
            }
            let w = word.then_some(match table {
                Some(t) => Word::Table(t),
                None => Word::Utf8,
            });
            let (eng, max) = if *case && table.is_none() {
                (Engine::lit(s.as_bytes()), s.len())
            } else {
                let hir = parse(&regex_syntax::escape(s), *case)?;
                let max = hir.properties().maximum_len().unwrap_or(s.len() * 4);
                let hir = match table {
                    Some(t) => hir_bytes::translate(&hir, t),
                    None => hir,
                };
                (Engine::Re(build(&hir)?), max)
            };
            Ok(Matcher {
                eng,
                trans,
                word: w,
                max_len: Some(match table {
                    // Символ однобайтовой кодировки — один байт.
                    Some(_) => s.chars().count(),
                    None => scale(max),
                }),
                unit,
            })
        }
        Pattern::Regex { s, case } => {
            if s.is_empty() {
                return Err(SearchError::Empty);
            }
            let hir = parse(s, *case)?;
            check_len(&hir)?;
            let hir = match table {
                Some(t) => hir_bytes::translate(&hir, t),
                None => hir,
            };
            let max = hir.properties().maximum_len().map(scale);
            Ok(Matcher {
                eng: Engine::Re(build(&hir)?),
                trans,
                word: None,
                max_len: max,
                unit,
            })
        }
    }
}
