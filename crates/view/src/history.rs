//! История просмотра и правки: позиция, кодировка, режим и перенос по файлу (S7 §3.1 п. 3,
//! §3.9, №36). Запись — при закрытии, переходе к соседнему файлу и выходе; восстановление —
//! при открытии. Файл `state_dir/view-history.json` (массив записей `{uri, kind, pos, enc,
//! mode, wrap, ts}`), пишет handler с debounce 2 с на Bulk lane ([`HISTORY_DEBOUNCE`]);
//! список Alt+F11 — `viewer.history`.
//!
//! JSON здесь свой, маленький (плоские записи): `serde_json` во вьювере нет.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use urfin_core::Location;

use crate::layout::{Mode, WrapKind};

/// Имя файла истории в `state_dir`.
pub const HISTORY_FILE: &str = "view-history.json";
/// Запись файла — через столько после последнего изменения.
pub const HISTORY_DEBOUNCE: Duration = Duration::from_secs(2);

/// Просмотр или правка (Alt+F11 показывает оба).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistKind {
    #[default]
    View,
    Edit,
}

/// Что помнится о файле.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewMemo {
    pub loc: Location,
    /// Начало верхней строки (`ViewerDoc::top`).
    pub top: u64,
    pub hscroll: u32,
    /// Имя WHATWG кодировки, выбранной вручную; `None` — детект.
    pub enc: Option<CompactString>,
    pub mode: Mode,
    pub wrap: Option<WrapKind>,
    /// Документ Markdown: доля высоты верхнего блока ×10 000 (`top` — его начало, S7 §3.10).
    pub frac: u16,
    pub kind: HistKind,
    /// Время записи, секунды Unix; 0 — ставит [`ViewHistory::record`].
    pub ts: u64,
}

impl ViewMemo {
    /// Начало файла, режим и кодировка — по детекту.
    pub fn start(loc: Location) -> ViewMemo {
        ViewMemo {
            loc,
            top: 0,
            hscroll: 0,
            enc: None,
            mode: Mode::Text,
            wrap: None,
            frac: 0,
            kind: HistKind::View,
            ts: 0,
        }
    }
}

/// Последние `max` файлов; новые — в конце.
#[derive(Clone, Debug)]
pub struct ViewHistory {
    items: VecDeque<ViewMemo>,
    max: usize,
    /// Растёт на каждой записи: handler сравнивает с записанным в файл.
    rev: u64,
    /// Файл прочитан (`merge`): до этого handler файл не пишет.
    loaded: bool,
}

impl Default for ViewHistory {
    fn default() -> Self {
        ViewHistory::new(500)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl ViewHistory {
    pub fn new(max: usize) -> ViewHistory {
        ViewHistory {
            items: VecDeque::new(),
            max: max.max(1),
            rev: 0,
            loaded: false,
        }
    }

    /// Запись о файле, если есть.
    pub fn get(&self, loc: &Location) -> Option<&ViewMemo> {
        self.items.iter().rev().find(|m| m.loc == *loc)
    }

    /// Запоминает (прежняя запись о файле уходит, новая — в конец).
    pub fn record(&mut self, mut memo: ViewMemo) {
        if memo.ts == 0 {
            memo.ts = now_secs();
        }
        self.items.retain(|m| m.loc != memo.loc);
        self.items.push_back(memo);
        self.trim();
        self.rev += 1;
    }

    /// Предел записей (`viewer.history_max`, перечитывание конфига).
    pub fn set_max(&mut self, max: usize) {
        let max = max.max(1);
        if max != self.max {
            self.max = max;
            if self.items.len() > max {
                self.trim();
                self.rev += 1;
            }
        }
    }

    fn trim(&mut self) {
        while self.items.len() > self.max {
            self.items.pop_front();
        }
    }

    /// Записи из файла: старше тех, что уже в памяти (их записали после запуска).
    pub fn merge(&mut self, loaded: Vec<ViewMemo>) {
        let mut old: VecDeque<ViewMemo> = VecDeque::with_capacity(loaded.len());
        for m in loaded {
            if self.items.iter().all(|x| x.loc != m.loc) {
                old.retain(|x| x.loc != m.loc);
                old.push_back(m);
            }
        }
        old.append(&mut self.items);
        self.items = old;
        self.trim();
        self.loaded = true;
    }

    /// Записи из файла уже здесь (`merge`).
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Поколение записей (растёт на `record`).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// От старых к новым.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ViewMemo> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Текст файла: массив записей, по одной в строке.
    pub fn to_json(&self) -> String {
        let mut s = String::from("[\n");
        for (i, m) in self.items.iter().enumerate() {
            if i > 0 {
                s.push_str(",\n");
            }
            s.push_str("{\"uri\":");
            json_str(&mut s, &m.loc.to_uri());
            let kind = match m.kind {
                HistKind::View => "view",
                HistKind::Edit => "edit",
            };
            let _ = write!(
                s,
                ",\"kind\":\"{kind}\",\"pos\":{},\"hscroll\":{},\"frac\":{},\"enc\":",
                m.top, m.hscroll, m.frac
            );
            match &m.enc {
                Some(e) => json_str(&mut s, e),
                None => s.push_str("null"),
            }
            let mode = match m.mode {
                Mode::Text => "text",
                Mode::Markup => "markup",
                Mode::Hex => "hex",
            };
            let wrap = match m.wrap {
                None => "null",
                Some(WrapKind::Words) => "\"words\"",
                Some(WrapKind::Chars) => "\"chars\"",
            };
            let _ = write!(s, ",\"mode\":\"{mode}\",\"wrap\":{wrap},\"ts\":{}}}", m.ts);
        }
        s.push_str("\n]\n");
        s
    }

    /// Записи из текста файла: битые пропускаются, битый файл — пусто.
    pub fn from_json(text: &str) -> Vec<ViewMemo> {
        let Some(Json::Arr(items)) = json::parse(text) else {
            return Vec::new();
        };
        items.iter().filter_map(memo_of).collect()
    }
}

fn memo_of(v: &Json) -> Option<ViewMemo> {
    let Json::Obj(f) = v else {
        return None;
    };
    let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    let num = |k: &str| match get(k) {
        Some(Json::Num(n)) => Some(*n),
        _ => None,
    };
    let text = |k: &str| match get(k) {
        Some(Json::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    let loc = Location::parse_uri(text("uri")?).ok()?;
    let mode = match text("mode") {
        Some("markup") => Mode::Markup,
        Some("hex") => Mode::Hex,
        _ => Mode::Text,
    };
    let wrap = match text("wrap") {
        Some("words") => Some(WrapKind::Words),
        Some("chars") => Some(WrapKind::Chars),
        _ => None,
    };
    let kind = match text("kind") {
        Some("edit") => HistKind::Edit,
        _ => HistKind::View,
    };
    Some(ViewMemo {
        loc,
        top: num("pos").unwrap_or(0),
        hscroll: u32::try_from(num("hscroll").unwrap_or(0)).unwrap_or(0),
        enc: text("enc").map(CompactString::from),
        mode,
        wrap,
        frac: u16::try_from(num("frac").unwrap_or(0))
            .unwrap_or(0)
            .min(10_000),
        kind,
        ts: num("ts").unwrap_or(0),
    })
}

/// Строка JSON с экранированием.
fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

use json::Json;

/// Разбор JSON: значения, что пишет [`ViewHistory::to_json`], и любой корректный JSON
/// вокруг них (лишние поля пропускаются). Числа — целые без знака, прочие — 0.
mod json {
    #[derive(Debug, PartialEq)]
    pub(super) enum Json {
        Null,
        Bool,
        Num(u64),
        Str(String),
        Arr(Vec<Json>),
        Obj(Vec<(String, Json)>),
    }

    /// Вложенность — не глубже (стек не растёт от чужого файла).
    const DEPTH: u32 = 32;

    pub(super) fn parse(s: &str) -> Option<Json> {
        let mut p = P {
            b: s.as_bytes(),
            i: 0,
        };
        let v = p.value(0)?;
        p.ws();
        (p.i == p.b.len()).then_some(v)
    }

    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }

    impl P<'_> {
        fn ws(&mut self) {
            while self
                .b
                .get(self.i)
                .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
            {
                self.i += 1;
            }
        }

        fn eat(&mut self, c: u8) -> bool {
            self.ws();
            if self.b.get(self.i) == Some(&c) {
                self.i += 1;
                true
            } else {
                false
            }
        }

        fn lit(&mut self, w: &[u8]) -> bool {
            if self.b[self.i..].starts_with(w) {
                self.i += w.len();
                true
            } else {
                false
            }
        }

        fn value(&mut self, depth: u32) -> Option<Json> {
            if depth > DEPTH {
                return None;
            }
            self.ws();
            match *self.b.get(self.i)? {
                b'{' => {
                    self.i += 1;
                    let mut f = Vec::new();
                    if self.eat(b'}') {
                        return Some(Json::Obj(f));
                    }
                    loop {
                        self.ws();
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        f.push((k, self.value(depth + 1)?));
                        if self.eat(b'}') {
                            return Some(Json::Obj(f));
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                b'[' => {
                    self.i += 1;
                    let mut a = Vec::new();
                    if self.eat(b']') {
                        return Some(Json::Arr(a));
                    }
                    loop {
                        a.push(self.value(depth + 1)?);
                        if self.eat(b']') {
                            return Some(Json::Arr(a));
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                b'"' => self.string().map(Json::Str),
                b'n' => self.lit(b"null").then_some(Json::Null),
                b't' => self.lit(b"true").then_some(Json::Bool),
                b'f' => self.lit(b"false").then_some(Json::Bool),
                b'-' | b'0'..=b'9' => {
                    let st = self.i;
                    while self.b.get(self.i).is_some_and(|c| {
                        matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                    }) {
                        self.i += 1;
                    }
                    let t = std::str::from_utf8(&self.b[st..self.i]).ok()?;
                    Some(Json::Num(t.parse::<u64>().unwrap_or(0)))
                }
                _ => None,
            }
        }

        fn string(&mut self) -> Option<String> {
            if self.b.get(self.i) != Some(&b'"') {
                return None;
            }
            self.i += 1;
            let mut out: Vec<u8> = Vec::new();
            loop {
                let c = *self.b.get(self.i)?;
                self.i += 1;
                match c {
                    b'"' => return String::from_utf8(out).ok(),
                    b'\\' => {
                        let ch = self.escape()?;
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    c if c < 0x20 => return None,
                    c => out.push(c),
                }
            }
        }

        /// Символ после `\`.
        fn escape(&mut self) -> Option<char> {
            let e = *self.b.get(self.i)?;
            self.i += 1;
            Some(match e {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let hi = self.hex4()?;
                    if (0xD800..0xDC00).contains(&hi) {
                        if !self.lit(b"\\u") {
                            return None;
                        }
                        let lo = self.hex4()?;
                        if !(0xDC00..0xE000).contains(&lo) {
                            return None;
                        }
                        char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))?
                    } else {
                        char::from_u32(hi)?
                    }
                }
                _ => return None,
            })
        }

        fn hex4(&mut self) -> Option<u32> {
            let h = self.b.get(self.i..self.i + 4)?;
            let v = u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?;
            self.i += 4;
            Some(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memo(p: &str, top: u64) -> ViewMemo {
        ViewMemo {
            top,
            ts: 1,
            ..ViewMemo::start(Location::local(p))
        }
    }

    #[test]
    fn record_replaces_and_trims() {
        let mut h = ViewHistory::new(2);
        h.record(memo(r"C:\a", 1));
        h.record(memo(r"C:\b", 2));
        h.record(memo(r"C:\a", 3));
        assert_eq!(h.get(&Location::local(r"C:\a")).map(|m| m.top), Some(3));
        h.record(memo(r"C:\c", 4));
        assert_eq!(h.len(), 2);
        assert!(h.get(&Location::local(r"C:\b")).is_none());
        let order: Vec<u64> = h.iter().map(|m| m.top).collect();
        assert_eq!(order, [3, 4]);
        assert_eq!(h.rev(), 4);
    }

    #[test]
    fn record_stamps_time() {
        let mut h = ViewHistory::new(5);
        h.record(ViewMemo::start(Location::local(r"C:\a")));
        assert!(h.iter().next().unwrap().ts > 1_700_000_000);
    }

    #[test]
    fn json_roundtrip() {
        let mut h = ViewHistory::new(10);
        h.record(ViewMemo {
            hscroll: 7,
            enc: Some("windows-1251".into()),
            mode: Mode::Hex,
            wrap: Some(WrapKind::Chars),
            frac: 2500,
            kind: HistKind::Edit,
            ..memo("C:\\Мои файлы\\a\"b\"\\c.txt", 123_456_789_012)
        });
        h.record(ViewMemo {
            mode: Mode::Markup,
            wrap: Some(WrapKind::Words),
            ..memo(r"C:\docs\README.md", 0)
        });
        let text = h.to_json();
        let back = ViewHistory::from_json(&text);
        let mine: Vec<ViewMemo> = h.iter().cloned().collect();
        assert_eq!(back, mine);
    }

    #[test]
    fn json_tolerates_junk() {
        assert_eq!(ViewHistory::from_json(""), []);
        assert_eq!(ViewHistory::from_json("{\"uri\":1}"), []);
        assert_eq!(ViewHistory::from_json("[{\"uri\":\"local:C:\\\\a\""), []);
        let deep = "[".repeat(100) + &"]".repeat(100);
        assert_eq!(ViewHistory::from_json(&deep), []);
        // Чужие поля, битая запись и unicode-экранирование.
        let t = r#"[ {"uri":"local:C:\\\u0430.txt","pos":5,"x":[1,{"y":null}],"mode":"hex"},
                    {"uri":"nope"}, 3 ]"#;
        let v = ViewHistory::from_json(t);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].loc, Location::local("C:\\а.txt"));
        assert_eq!((v[0].top, v[0].mode), (5, Mode::Hex));
        let surrogate = r#"[{"uri":"local:C:\\\ud83d\ude00"}]"#;
        assert_eq!(
            ViewHistory::from_json(surrogate)[0].loc,
            Location::local("C:\\😀")
        );
    }

    #[test]
    fn merge_keeps_newer_in_memory() {
        let mut h = ViewHistory::new(3);
        h.record(memo(r"C:\a", 10));
        h.merge(vec![memo(r"C:\a", 1), memo(r"C:\b", 2), memo(r"C:\c", 3)]);
        let order: Vec<u64> = h.iter().map(|m| m.top).collect();
        assert_eq!(order, [2, 3, 10]);
        h.set_max(2);
        assert_eq!(h.len(), 2);
    }
}
