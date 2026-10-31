//! История просмотра и правки: позиция, кодировка, режим и перенос по файлу (S7 §3.1 п. 3,
//! §3.9, №36). Запись — при закрытии, переходе к соседнему файлу и выходе; восстановление —
//! при открытии. Файл `state_dir/view-history.json` (массив записей `{uri, kind, pos, enc,
//! mode, wrap, ts}`), пишет handler с debounce 2 с на Bulk lane ([`HISTORY_DEBOUNCE`]);
//! список Alt+F11 — `viewer.history`.
//!
//! Записи правок (`HistKind::Edit`, редактор S7-15) живут отдельно от просмотров того же
//! файла: `top` у них — строка курсора, `hscroll` — колонка (с 0).

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use serde::{Deserialize, Deserializer, Serialize};
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

    /// Запись просмотра файла, если есть.
    pub fn get(&self, loc: &Location) -> Option<&ViewMemo> {
        self.get_kind(loc, HistKind::View)
    }

    /// Запись просмотра или правки файла.
    pub fn get_kind(&self, loc: &Location, kind: HistKind) -> Option<&ViewMemo> {
        self.items
            .iter()
            .rev()
            .find(|m| m.kind == kind && m.loc == *loc)
    }

    /// Запоминает (прежняя запись того же вида о файле уходит, новая — в конец).
    pub fn record(&mut self, mut memo: ViewMemo) {
        if memo.ts == 0 {
            memo.ts = now_secs();
        }
        self.items
            .retain(|m| m.kind != memo.kind || m.loc != memo.loc);
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
        let same = |a: &ViewMemo, b: &ViewMemo| a.kind == b.kind && a.loc == b.loc;
        for m in loaded {
            if self.items.iter().all(|x| !same(x, &m)) {
                old.retain(|x| !same(x, &m));
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
            match serde_json::to_string(&RecOut::of(m)) {
                Ok(line) => s.push_str(&line),
                Err(e) => tracing::warn!(error = %e, "view history record is not written"),
            }
        }
        s.push_str("\n]\n");
        s
    }

    /// Записи из текста файла: битые пропускаются, битый файл — пусто.
    pub fn from_json(text: &str) -> Vec<ViewMemo> {
        let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
            return Vec::new();
        };
        items
            .into_iter()
            .filter_map(|v| RecIn::deserialize(v).ok()?.memo())
            .collect()
    }
}

/// Запись файла: поля и порядок — как в S7-14 (`{uri, kind, pos, hscroll, frac, enc,
/// mode, wrap, ts}`).
#[derive(Serialize)]
struct RecOut<'a> {
    uri: String,
    kind: &'static str,
    pos: u64,
    hscroll: u32,
    frac: u16,
    enc: Option<&'a str>,
    mode: &'static str,
    wrap: Option<&'static str>,
    ts: u64,
}

impl<'a> RecOut<'a> {
    fn of(m: &'a ViewMemo) -> RecOut<'a> {
        RecOut {
            uri: m.loc.to_uri(),
            kind: match m.kind {
                HistKind::View => "view",
                HistKind::Edit => "edit",
            },
            pos: m.top,
            hscroll: m.hscroll,
            frac: m.frac,
            enc: m.enc.as_deref(),
            mode: match m.mode {
                Mode::Text => "text",
                Mode::Markup => "markup",
                Mode::Hex => "hex",
            },
            wrap: m.wrap.map(|w| match w {
                WrapKind::Words => "words",
                WrapKind::Chars => "chars",
            }),
            ts: m.ts,
        }
    }
}

/// Запись при чтении: чужие поля пропускаются, поле не того типа — значение по
/// умолчанию (как у прежнего разбора S7-14), без `uri` — запись битая.
#[derive(Deserialize)]
struct RecIn {
    uri: String,
    #[serde(default, deserialize_with = "lenient_str")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient_u64")]
    pos: u64,
    #[serde(default, deserialize_with = "lenient_u64")]
    hscroll: u64,
    #[serde(default, deserialize_with = "lenient_u64")]
    frac: u64,
    #[serde(default, deserialize_with = "lenient_str")]
    enc: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    mode: Option<String>,
    #[serde(default, deserialize_with = "lenient_str")]
    wrap: Option<String>,
    #[serde(default, deserialize_with = "lenient_u64")]
    ts: u64,
}

impl RecIn {
    fn memo(self) -> Option<ViewMemo> {
        let loc = Location::parse_uri(&self.uri).ok()?;
        let mode = match self.mode.as_deref() {
            Some("markup") => Mode::Markup,
            Some("hex") => Mode::Hex,
            _ => Mode::Text,
        };
        let wrap = match self.wrap.as_deref() {
            Some("words") => Some(WrapKind::Words),
            Some("chars") => Some(WrapKind::Chars),
            _ => None,
        };
        let kind = match self.kind.as_deref() {
            Some("edit") => HistKind::Edit,
            _ => HistKind::View,
        };
        Some(ViewMemo {
            loc,
            top: self.pos,
            hscroll: u32::try_from(self.hscroll).unwrap_or(0),
            enc: self.enc.map(CompactString::from),
            mode,
            wrap,
            frac: u16::try_from(self.frac.min(10_000)).unwrap_or(0),
            kind,
            ts: self.ts,
        })
    }
}

/// Целое без знака; другое значение (дробь, строка, `null`) — 0.
fn lenient_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_u64().unwrap_or(0))
}

/// Строка; другое значение — `None`.
fn lenient_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
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
    fn view_and_edit_records_are_separate() {
        let mut h = ViewHistory::new(10);
        h.record(memo(r"C:\a", 100));
        h.record(ViewMemo {
            kind: HistKind::Edit,
            ..memo(r"C:\a", 5)
        });
        let a = Location::local(r"C:\a");
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(&a).map(|m| m.top), Some(100));
        assert_eq!(h.get_kind(&a, HistKind::Edit).map(|m| m.top), Some(5));
        h.merge(vec![ViewMemo {
            kind: HistKind::Edit,
            ..memo(r"C:\a", 1)
        }]);
        assert_eq!(h.len(), 2, "в памяти новее");
    }

    /// Файл, записанный прежним разбором (S7-14), читается так же и пишется так же.
    #[test]
    fn reads_s7_14_file() {
        let t = concat!(
            "[\n",
            r#"{"uri":"local:C:\\x.txt","kind":"view","pos":42,"hscroll":3,"frac":0,"#,
            r#""enc":null,"mode":"text","wrap":"words","ts":1700000000}"#,
            "\n]\n"
        );
        let v = ViewHistory::from_json(t);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].loc, Location::local(r"C:\x.txt"));
        assert_eq!((v[0].top, v[0].hscroll), (42, 3));
        assert_eq!(v[0].wrap, Some(WrapKind::Words));
        let mut h = ViewHistory::new(5);
        h.merge(v);
        assert_eq!(h.to_json(), t, "формат записи тот же");
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
