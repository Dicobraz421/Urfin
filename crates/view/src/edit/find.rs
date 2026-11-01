//! Поиск и замена в тексте редактора (S7 §5.2, S7-18): `regex-cursor` по Rope gpui-kit —
//! свой [`RopeCursor`] по чанкам ropey 2 (`ChunkCursor`), без копии текста.
//!
//! Образец: литерал экранируется, «Слово» — `\b…\b`, «Регистр» выключен — `(?i)`. Режимы
//! `m` и `R` всегда: `^`/`$` — по строкам, и на CRLF `$` стоит перед `\r\n` (текст редактора
//! хранит `\r`, S7-16). Замена: `$1`, `${name}`, `$$` — только в «Регулярном», литерал
//! подставляется как есть. «Заменить все» строит новый текст целиком — UI отдаёт его
//! `EditorState::replace_all` одной undo-группой.
//!
//! Всё здесь — чистые функции над Rope: UI зовёт их в фоне по снимку (клон Rope дёшев).

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use regex_cursor::engines::meta::Regex;
use regex_cursor::regex_automata::util::captures::Captures;
use regex_cursor::regex_automata::util::interpolate;
use regex_cursor::regex_automata::{Anchored, PatternID};
use regex_cursor::{Cursor, Input};
use regex_syntax::ParserBuilder;
use ropey::{ChunkCursor, Rope};

use crate::search::SearchError;

/// Совпадений в списке (счётчик «N из M», подсветка, метки) — не больше (S7 §5.2).
pub const COUNT_LIMIT: usize = 10_000;

/// Курсор `regex-cursor` по чанкам Rope (чанки ropey не режут символ UTF-8).
pub struct RopeCursor<'a> {
    cur: ChunkCursor<'a>,
    len: usize,
}

impl<'a> RopeCursor<'a> {
    pub fn new(rope: &'a Rope) -> RopeCursor<'a> {
        RopeCursor {
            cur: rope.chunk_cursor(),
            len: rope.len(),
        }
    }
}

impl std::fmt::Debug for RopeCursor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RopeCursor")
            .field("offset", &self.cur.byte_offset())
            .field("len", &self.len)
            .finish()
    }
}

impl Cursor for RopeCursor<'_> {
    fn chunk(&self) -> &[u8] {
        self.cur.chunk().as_bytes()
    }

    fn advance(&mut self) -> bool {
        self.cur.next()
    }

    fn backtrack(&mut self) -> bool {
        self.cur.prev()
    }

    fn total_bytes(&self) -> Option<usize> {
        Some(self.len)
    }

    fn offset(&self) -> usize {
        self.cur.byte_offset()
    }
}

/// Что ищется: текст строки «Найти» и флажки.
#[allow(clippy::struct_excessive_bools, reason = "флажки строки поиска")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    /// С учётом регистра.
    pub case: bool,
    /// Целое слово (только литерал).
    pub word: bool,
    pub regex: bool,
}

/// Итог поиска по документу.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Совпадения по порядку (байты UTF-8), не больше [`COUNT_LIMIT`].
    pub ranges: Vec<Range<usize>>,
    /// Есть ещё совпадения после последнего в списке.
    pub more: bool,
    /// Строки совпадений (с 0, по `\n`, без повторов) — метки мини-карты.
    pub lines: Vec<u32>,
}

/// Собранный образец.
#[derive(Clone, Debug)]
pub struct Finder {
    re: Regex,
    /// Замена с группами (`$1`) — только в «Регулярном».
    groups: bool,
}

/// Позиция символа по байтовому смещению в образце.
fn char_pos(s: &str, byte: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < byte).count()
}

impl Finder {
    /// Образец строки; пустой — `SearchError::Empty`, ошибка regex — с позицией символа.
    pub fn new(q: &Query) -> Result<Finder, SearchError> {
        if q.text.is_empty() {
            return Err(SearchError::Empty);
        }
        let pat = if q.regex {
            q.text.clone()
        } else {
            let lit = regex_syntax::escape(&q.text);
            if q.word {
                format!(r"\b(?:{lit})\b")
            } else {
                lit
            }
        };
        let hir = ParserBuilder::new()
            .case_insensitive(!q.case)
            .multi_line(true)
            .crlf(true)
            .build()
            .parse(&pat)
            .map_err(|e| {
                let (off, msg) = match &e {
                    regex_syntax::Error::Parse(p) => (p.span().start.offset, p.kind().to_string()),
                    regex_syntax::Error::Translate(t) => {
                        (t.span().start.offset, t.kind().to_string())
                    }
                    other => (0, other.to_string()),
                };
                // В литерале позиция ошибки ничего не скажет: образец экранирован.
                let pos = if q.regex { char_pos(&pat, off) } else { 0 };
                SearchError::Regex { pos, msg }
            })?;
        let re = Regex::builder()
            .build_from_hir::<&[u8]>(&hir)
            .map_err(|e| {
                if e.size_limit().is_some() {
                    SearchError::TooBig
                } else {
                    SearchError::Regex {
                        pos: 0,
                        msg: e.to_string(),
                    }
                }
            })?;
        Ok(Finder {
            re,
            groups: q.regex,
        })
    }

    /// Совпадения по курсору `c` (до `limit`); `stop` — новый поиск отменил этот.
    pub fn find_in<C: Cursor>(
        &self,
        c: C,
        limit: usize,
        stop: &AtomicBool,
    ) -> (Vec<Range<usize>>, bool) {
        let mut out = Vec::new();
        for (k, m) in self.re.find_iter(Input::new(c)).enumerate() {
            if out.len() >= limit {
                return (out, true);
            }
            if k % 1024 == 1023 && stop.load(Ordering::Relaxed) {
                return (out, false);
            }
            out.push(m.range());
        }
        (out, false)
    }

    /// Все совпадения документа (до [`COUNT_LIMIT`]) и их строки.
    pub fn find_all(&self, rope: &Rope, stop: &AtomicBool) -> Found {
        let (ranges, more) = self.find_in(RopeCursor::new(rope), COUNT_LIMIT, stop);
        let lines = lines_of(rope, &ranges);
        Found {
            ranges,
            more,
            lines,
        }
    }

    /// Первое совпадение, начатое не раньше `from` (за пределами списка — «найти далее»).
    pub fn find_from(&self, rope: &Rope, from: usize) -> Option<Range<usize>> {
        let mut input = Input::new(RopeCursor::new(rope));
        input.set_start(from.min(rope.len()));
        self.re.find(input).map(|m| m.range())
    }

    /// Совпадение ровно на `m` с группами (`None` — текст уже другой).
    fn captures_at(&self, rope: &Rope, m: &Range<usize>) -> Option<Captures> {
        let mut input = Input::new(RopeCursor::new(rope));
        input.set_start(m.start.min(rope.len()));
        input.set_anchored(Anchored::Yes);
        let mut caps = self.re.create_captures();
        self.re.search_captures(input, &mut caps);
        (caps.get_match()?.range() == *m).then_some(caps)
    }

    /// Чем заменить совпадение `m`: шаблон с группами из «Найти» (`$1`, `${name}`, `$$`)
    /// или литерал. `None` — на `m` уже нет совпадения.
    pub fn expand(&self, rope: &Rope, m: &Range<usize>, template: &str) -> Option<String> {
        if !self.groups || !template.contains('$') {
            let mut input = Input::new(RopeCursor::new(rope));
            input.set_start(m.start.min(rope.len()));
            input.set_anchored(Anchored::Yes);
            return (self.re.find(input)?.range() == *m).then(|| template.to_owned());
        }
        let caps = self.captures_at(rope, m)?;
        let mut out = String::new();
        self.interpolate(rope, &caps, template, &mut out);
        Some(out)
    }

    fn interpolate(&self, rope: &Rope, caps: &Captures, template: &str, dst: &mut String) {
        interpolate::string(
            template,
            |i, dst| {
                if let Some(sp) = caps.get_group(i) {
                    push_slice(rope, sp.range(), dst);
                }
            },
            |name| self.re.group_info().to_index(PatternID::ZERO, name),
            dst,
        );
    }

    /// Текст после замены всех совпадений и их число (0 — текст тот же).
    pub fn replace_all(&self, rope: &Rope, template: &str) -> (String, usize) {
        let mut out = String::with_capacity(rope.len());
        let mut at = 0usize;
        let mut n = 0usize;
        if self.groups && template.contains('$') {
            for caps in self.re.captures_iter(Input::new(RopeCursor::new(rope))) {
                let Some(m) = caps.get_match() else {
                    continue;
                };
                push_slice(rope, at..m.start(), &mut out);
                self.interpolate(rope, &caps, template, &mut out);
                at = m.end();
                n += 1;
            }
        } else {
            for m in self.re.find_iter(Input::new(RopeCursor::new(rope))) {
                push_slice(rope, at..m.start(), &mut out);
                out.push_str(template);
                at = m.end();
                n += 1;
            }
        }
        push_slice(rope, at..rope.len(), &mut out);
        (out, n)
    }
}

/// Кусок Rope — в строку (по чанкам).
fn push_slice(rope: &Rope, r: Range<usize>, dst: &mut String) {
    if r.start >= r.end {
        return;
    }
    for c in rope.slice(r).chunks() {
        dst.push_str(c);
    }
}

/// Строки (по `\n`) начал совпадений `ranges` (по порядку), без повторов.
pub fn lines_of(rope: &Rope, ranges: &[Range<usize>]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut line = 0u64;
    let mut pos = 0usize;
    let mut chunks = rope.chunks();
    let mut chunk: &str = chunks.next().unwrap_or("");
    let mut chunk_at = 0usize;
    for r in ranges {
        // Считать `\n` от `pos` до `r.start`.
        while pos < r.start {
            let end_of_chunk = chunk_at + chunk.len();
            let upto = r.start.min(end_of_chunk);
            let bytes = &chunk.as_bytes()[pos - chunk_at..upto - chunk_at];
            line += memchr::memchr_iter(b'\n', bytes).count() as u64;
            pos = upto;
            if pos == end_of_chunk {
                match chunks.next() {
                    Some(c) => {
                        chunk_at = end_of_chunk;
                        chunk = c;
                    }
                    None => break,
                }
            }
        }
        let l = u32::try_from(line).unwrap_or(u32::MAX);
        if out.last() != Some(&l) {
            out.push(l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use regex_automata::meta;
    use regex_automata::util::syntax;

    fn q(text: &str) -> Query {
        Query {
            text: text.to_owned(),
            case: true,
            ..Query::default()
        }
    }

    fn re(text: &str) -> Query {
        Query {
            regex: true,
            ..q(text)
        }
    }

    fn all(f: &Finder, s: &str) -> Vec<Range<usize>> {
        f.find_all(&Rope::from_str(s), &AtomicBool::new(false))
            .ranges
    }

    /// Курсор по кускам строки (границы — где скажут): чанковые стыки чаще, чем в Rope.
    struct Pieces<'a> {
        parts: Vec<&'a str>,
        i: usize,
        off: usize,
        len: usize,
    }

    impl<'a> Pieces<'a> {
        fn new(s: &'a str, cuts: &[usize]) -> Pieces<'a> {
            let mut parts = Vec::new();
            let mut at = 0;
            let mut cuts: Vec<usize> = cuts
                .iter()
                .map(|c| c % (s.len() + 1))
                .filter(|c| s.is_char_boundary(*c))
                .collect();
            cuts.sort_unstable();
            cuts.dedup();
            for c in cuts {
                if c > at {
                    parts.push(&s[at..c]);
                    at = c;
                }
            }
            if at < s.len() || parts.is_empty() {
                parts.push(&s[at..]);
            }
            Pieces {
                parts,
                i: 0,
                off: 0,
                len: s.len(),
            }
        }
    }

    impl Cursor for Pieces<'_> {
        fn chunk(&self) -> &[u8] {
            self.parts[self.i].as_bytes()
        }
        fn advance(&mut self) -> bool {
            if self.i + 1 >= self.parts.len() {
                return false;
            }
            self.off += self.parts[self.i].len();
            self.i += 1;
            true
        }
        fn backtrack(&mut self) -> bool {
            if self.i == 0 {
                return false;
            }
            self.i -= 1;
            self.off -= self.parts[self.i].len();
            true
        }
        fn total_bytes(&self) -> Option<usize> {
            Some(self.len)
        }
        fn offset(&self) -> usize {
            self.off
        }
    }

    /// Эталон — `regex-automata::meta` (движок `regex`) по `String` с теми же флагами.
    fn reference(pat: &str, s: &str) -> Vec<Range<usize>> {
        let r = meta::Regex::builder()
            .syntax(syntax::Config::new().multi_line(true).crlf(true))
            .build(pat)
            .unwrap();
        r.find_iter(s).map(|m| m.range()).collect()
    }

    #[test]
    fn literal_case_and_word() {
        let f = Finder::new(&q("ab")).unwrap();
        assert_eq!(all(&f, "ab Ab ab"), vec![0..2, 6..8]);
        let f = Finder::new(&Query {
            case: false,
            ..q("ab")
        })
        .unwrap();
        assert_eq!(all(&f, "ab Ab abc").len(), 3);
        let f = Finder::new(&Query {
            word: true,
            ..q("ab")
        })
        .unwrap();
        assert_eq!(all(&f, "ab abc cab ab"), vec![0..2, 11..13]);
        // Литерал экранирован: точка — точка.
        let f = Finder::new(&q("a.b")).unwrap();
        assert_eq!(all(&f, "a.b axb"), vec![0..3]);
        // Кириллица без учёта регистра.
        let f = Finder::new(&Query {
            case: false,
            ..q("ёж")
        })
        .unwrap();
        assert_eq!(all(&f, "ЁЖ ёж").len(), 2);
    }

    #[test]
    fn errors() {
        assert_eq!(Finder::new(&q("")).unwrap_err(), SearchError::Empty);
        match Finder::new(&re("ab(")).unwrap_err() {
            SearchError::Regex { pos, .. } => assert_eq!(pos, 2),
            e => panic!("{e}"),
        }
    }

    #[test]
    fn crlf_anchors() {
        let f = Finder::new(&re(r"^\w+$")).unwrap();
        let s = "ab\r\ncd\nef\r\n";
        assert_eq!(all(&f, s), vec![0..2, 4..6, 7..9]);
    }

    #[test]
    fn groups_replace_like_mockup() {
        let text = "[workspace]\nresolver = \"3\"\r\nmembers = [\"crates/*\"]\nrayon = \"1.10\"\n";
        let f = Finder::new(&re(r#"^(\w+) = "([\d.]+)""#)).unwrap();
        let rope = Rope::from_str(text);
        let found = f.find_all(&rope, &AtomicBool::new(false));
        assert_eq!(found.ranges.len(), 2);
        assert_eq!(found.lines, vec![1, 3]);
        let tpl = r#"$1 = { version = "$2" }"#;
        assert_eq!(
            f.expand(&rope, &found.ranges[1], tpl).as_deref(),
            Some(r#"rayon = { version = "1.10" }"#)
        );
        assert_eq!(f.expand(&rope, &(0..3), tpl), None, "не совпадение");
        let (out, n) = f.replace_all(&rope, tpl);
        assert_eq!(n, 2);
        assert_eq!(
            out,
            "[workspace]\nresolver = { version = \"3\" }\r\nmembers = [\"crates/*\"]\nrayon = { version = \"1.10\" }\n"
        );
        // `$$` — доллар; литерал — без групп.
        assert_eq!(f.replace_all(&rope, "$$").0.matches('$').count(), 2);
        let lit = Finder::new(&q("rayon")).unwrap();
        assert_eq!(lit.replace_all(&rope, "$1").0.matches("$1").count(), 1);
    }

    #[test]
    fn find_from_and_limit() {
        let f = Finder::new(&q("x")).unwrap();
        let rope = Rope::from_str(&"x".repeat(COUNT_LIMIT + 5));
        let found = f.find_all(&rope, &AtomicBool::new(false));
        assert_eq!(found.ranges.len(), COUNT_LIMIT);
        assert!(found.more);
        assert_eq!(found.lines, vec![0]);
        assert_eq!(
            f.find_from(&rope, COUNT_LIMIT + 2),
            Some(COUNT_LIMIT + 2..COUNT_LIMIT + 3)
        );
        assert_eq!(f.find_from(&rope, COUNT_LIMIT + 5), None);
    }

    /// Rope из многих чанков: совпадения на стыках и строки меток.
    #[test]
    fn across_rope_chunks() {
        let line = "alpha = \"1.0\"\r\nбета = 2\n";
        let text = line.repeat(2_000);
        let rope = Rope::from_str(&text);
        let f = Finder::new(&re(r"^\w+ = .*$")).unwrap();
        let found = f.find_all(&rope, &AtomicBool::new(false));
        assert_eq!(found.ranges, reference(r"^\w+ = .*$", &text));
        assert_eq!(found.lines.len(), 4_000);
        assert_eq!(found.lines[3], 3);
    }

    /// 100 000 замен в 50 МБ — не дольше секунды (приёмка S7-18; замер — в release).
    #[test]
    #[ignore = "замер: cargo nextest run -p urfin-view --release --run-ignored only replace_all_budget"]
    fn replace_all_budget() {
        let line = format!("{}needle{}\n", "a".repeat(250), "b".repeat(244));
        let text = line.repeat(100_000);
        assert!(text.len() >= 50_000_000);
        let rope = Rope::from_str(&text);
        let f = Finder::new(&q("needle")).unwrap();
        let t = std::time::Instant::now();
        let (out, n) = f.replace_all(&rope, "pin");
        let took = t.elapsed();
        assert_eq!(n, 100_000);
        assert_eq!(out.len(), text.len() - 300_000);
        assert!(took.as_secs_f64() <= 1.0, "{took:?}");
    }

    fn texts() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("a"),
                Just("b"),
                Just("ab"),
                Just(" "),
                Just("\r\n"),
                Just("\n"),
                Just("\r"),
                Just("я"),
            ],
            0..60,
        )
        .prop_map(|v| v.concat())
    }

    proptest! {
        /// `^`/`$` по строкам на CRLF — как у `regex` по `String`, при любых стыках чанков.
        #[test]
        fn anchors_match_reference(
            s in texts(),
            pat in prop_oneof![
                Just(r"^"), Just(r"$"), Just(r"^a"), Just(r"b$"), Just(r"^$"),
                Just(r"^.*$"), Just(r"^\w+$"), Just(r"a\r?$"), Just(r"(?s)b.a"), Just(r"\bab\b"),
            ],
            cuts in proptest::collection::vec(0usize..200, 0..12),
        ) {
            let f = Finder::new(&re(pat)).unwrap();
            let want = reference(pat, &s);
            let (got, _) = f.find_in(Pieces::new(&s, &cuts), usize::MAX, &AtomicBool::new(false));
            prop_assert_eq!(&got, &want);
            prop_assert_eq!(all(&f, &s), want);
        }
    }
}
