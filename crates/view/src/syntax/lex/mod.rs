//! Возобновляемые лексеры подсветки (ADR-0024): состояние — несколько байт, проход по
//! файлу потоком, окно — от ближайшей контрольной точки. Без gpui и `Cx` ядра.
//!
//! - **Лексеры** JSON, TOML, INI, XML красят сами (`Out::span`); их вывод равен tree-sitter
//!   + `highlights.scm` на корректных документах (дифференциальные тесты).
//! - **Трекеры** YAML и Markdown не красят: отдают строки, с которых tree-sitter можно
//!   начать заново (`Out::line`), и события структуры для свёртки. Трекеры пакетных языков
//!   (`code`) — так же: границы кусков и заголовки уровней (ADR-0024 §8).
//! - **Единицы**: лексер видит по байту на символ — ASCII как есть, прочее — `0x80`;
//!   позиция байта — смещение начала символа в источнике (`Map`). UTF-8 и однобайтовые
//!   кодировки — байты источника как есть (≥ 0x80 — «прочий символ»), UTF-16 — единица
//!   кода, многобайтовые — посимвольный декодер.
//! - **Стек**: роли контейнеров (1 бит на уровень) у верхних [`TOP`] уровней — в
//!   состоянии; глубже — по сводкам точек (`marks::resolve`), без хранения всего стека.

pub(crate) mod code;
pub(crate) mod ini;
pub(crate) mod json;
pub(crate) mod md;
pub(crate) mod toml;
pub(crate) mod xml;
pub(crate) mod yaml;

use std::io;

use crate::enc::{Enc, Step};

use super::Tag;
pub use code::Code;
pub(crate) use code::Frame;

/// Уровней стека в состоянии точки.
pub(crate) const TOP: u64 = 64;
/// Известных уровней стека в памяти прохода больше — нижняя половина забывается.
const KNOWN_MAX: usize = 1 << 16;

/// Язык с окном (ADR-0024 §1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Json,
    Toml,
    Ini,
    Xml,
    Yaml,
    Md,
    /// Пакетный язык: трекер и tree-sitter по кускам (ADR-0024 §8).
    Code(Code),
}

impl Lang {
    /// По имени `urfin_view::lang`.
    pub fn of(name: &str) -> Option<Lang> {
        Some(match name {
            "json" => Lang::Json,
            "toml" => Lang::Toml,
            "ini" => Lang::Ini,
            "xml" => Lang::Xml,
            "yaml" => Lang::Yaml,
            "markdown" => Lang::Md,
            _ => return Code::of(name).map(Lang::Code),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Json => "json",
            Lang::Toml => "toml",
            Lang::Ini => "ini",
            Lang::Xml => "xml",
            Lang::Yaml => "yaml",
            Lang::Md => "markdown",
            Lang::Code(c) => c.name(),
        }
    }

    /// Байт начала текста, от которых зависит его первая строка (`Lex::begin`): правка в них
    /// меняет состояние с самого начала.
    pub(crate) fn head(self) -> u64 {
        if self == Lang::Md { md::HEAD } else { 0 }
    }

    /// Красит tree-sitter по отрезку (YAML, Markdown), а не лексер.
    pub fn by_tree(self) -> bool {
        matches!(self, Lang::Yaml | Lang::Md | Lang::Code(_))
    }

    /// Лексер (трекер) в состоянии `st`.
    pub(crate) fn lexer(self, st: St) -> Box<dyn Lex> {
        match self {
            Lang::Json => Box::new(json::Json::resume(st)),
            Lang::Toml => Box::new(toml::Toml::resume(st)),
            Lang::Ini => Box::new(ini::Ini::resume(st)),
            Lang::Xml => Box::new(xml::Xml::resume(st)),
            Lang::Yaml => Box::new(yaml::Yaml::resume(st)),
            Lang::Md => Box::new(md::Md::resume(st)),
            Lang::Code(c) => c.lexer(st),
        }
    }
}

/// Состояние в точке: режим языка (`mode`, `a`, `b`, `c`, `x` — по месту) и роли
/// верхних `n` уровней стека (`top`, бит 0 — верхний).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct St {
    pub mode: u8,
    pub a: u8,
    pub b: u16,
    pub c: u32,
    pub n: u8,
    pub top: u64,
    pub x: u64,
    /// Ещё байт режима (Markdown: HTML-блок в контейнере).
    pub d: u8,
    /// Трекеры кода (ADR-0024 §8).
    pub ext: Ext,
}

/// Состояние трекера кода: виды уровней, цепочка, расстояния до границы и кандидата.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ext {
    pub kinds: [u64; 2],
    pub bad: u64,
    pub cand_lvl: u64,
    /// Кандидат уже был.
    pub any: bool,
    /// Байт назад до границы.
    pub cut: u64,
    /// Байт назад до кандидата.
    pub cand: u64,
    pub k: u32,
    pub lt: i16,
    /// Уровень заголовка (`match`).
    pub h: u32,
    /// Прошлый знак.
    pub pc: u8,
    /// Хеши raw-строки C++ (разделитель и сверяемый хвост).
    pub hash: u32,
    pub rh: u32,
    /// Прошлый токен, флаги заголовка, директива, префикс строки C#.
    pub prev: u8,
    pub hdr: u8,
    pub hdr2: u8,
    pub cond: u8,
    pub pfx: u8,
    /// Python: отступы блоков и флаги строк под f-выражениями.
    pub kinds2: [u64; 2],
    pub fs: u64,
}

/// Байты, правка которых не меняет лексику (ADR-0024 §4): внутренность строки или
/// комментария; `deny` — байты, которых во вставке быть не должно (кавычка, `\`,
/// перевод строки, символы конца).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Inert {
    pub start: u64,
    pub end: u64,
    pub deny: &'static [u8],
    /// Удалять внутри можно (конец — один символ, его в зоне нет).
    pub del: bool,
}

/// Вид строки YAML/Markdown для трекера (ADR-0024 §3).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Ключ неявный или `? `.
    Key,
    /// `: ` значения ключа `? ` (не начало).
    QVal,
    Item,
    /// Прочая содержательная строка (скаляр, директива).
    Other,
    /// `---` / `...`.
    Doc,
    /// Markdown: безопасная строка.
    Safe,
}

/// Куда лексер пишет.
pub(crate) trait Out {
    /// Отрезок роли (байты источника); `Plain` не приходит.
    fn span(&mut self, _start: u64, _end: u64, _tag: Tag) {}
    fn inert(&mut self, _z: Inert) {}
    /// Открыт уровень структуры роли `kind` (глубина после — `depth`).
    fn open(&mut self, _at: u64, _depth: u64, _kind: bool) {}
    /// Закрыт уровень (глубина после — `depth`).
    fn close(&mut self, _at: u64, _depth: u64) {}
    /// Строка трекера: начало, отступ, вид.
    fn line(&mut self, _at: u64, _ind: u32, _kind: LineKind) {}
    /// Якорь внутри строки: роли до него от текста после не зависят (ничего не начато,
    /// ADR-0024 §4) — у строк без переводов (сжатый JSON, XML).
    fn anchor(&mut self, _at: u64) {}
    /// Markdown: от `at` (столбец содержимого контейнеров) до конца строки — HTML-блок
    /// по `CommonMark` (ADR-0024 §6 п. 4).
    fn html(&mut self, _at: u64) {}
    /// Markdown: `>` внутри HTML-блока, байт `at` (маркер цитаты тоже часть блока).
    fn html_quote(&mut self, _at: u64) {}
    /// Markdown: от `at` до конца строки — подчёркивание под абзацем из определений ссылок,
    /// Setext оно не делает (грамматика думает иначе).
    fn no_setext(&mut self, _at: u64) {}
    /// Код: с начала строки `at` начинается новый кусок разбора (ADR-0024 §8).
    fn cut(&mut self, _at: u64) {}
    /// Код: заголовок уровня, который сейчас откроется (`open`).
    fn frame(&mut self, _f: Frame) {}
}

/// Ничего не пишет.
pub(crate) struct NoOut;
impl Out for NoOut {}

/// Роли уровней глубже известных.
pub(crate) trait Resolve {
    /// Роли уровней `(lo, hi]` (снизу вверх, не пусто) на текущей позиции прохода;
    /// `min` — минимум глубины с последней точки прохода (окно — с начала).
    fn kinds(&mut self, hi: u64, min: u64) -> Vec<bool>;
}

/// Стек ролей: глубина и известные верхние уровни.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stack {
    depth: u64,
    /// Роли уровней `(depth - known.len(), depth]`, снизу вверх.
    known: Vec<bool>,
}

impl Stack {
    pub(crate) fn at(depth: u64, st: &St) -> Stack {
        let n = u64::from(st.n).min(depth);
        let known = (0..n).rev().map(|i| st.top >> i & 1 == 1).collect();
        Stack { depth, known }
    }

    /// Верхние уровни в состояние: `(n, top)`.
    pub(crate) fn top_bits(&self) -> (u8, u64) {
        let n = (self.known.len() as u64).min(TOP).min(self.depth);
        let mut top = 0u64;
        for i in 0..n {
            if self.known[self.known.len() - 1 - i as usize] {
                top |= 1 << i;
            }
        }
        #[allow(clippy::cast_possible_truncation, reason = "n ≤ 64")]
        (n as u8, top)
    }
}

/// Контекст лексера: стек, минимум глубины, вывод.
pub(crate) struct Cx<'a> {
    pub(crate) stack: Stack,
    /// Минимум глубины с последнего сброса (сводка куска точки; окно — с начала).
    pub(crate) min: u64,
    pub(crate) out: &'a mut dyn Out,
    pub(crate) resolve: Option<&'a mut dyn Resolve>,
    /// Забыта нижняя половина известных уровней (проход ставит точку).
    pub(crate) dropped: bool,
    /// Забывать нижние уровни (проход); окно помнит все.
    pub(crate) cap: bool,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(stack: Stack, out: &'a mut dyn Out) -> Cx<'a> {
        let d = stack.depth;
        Cx {
            stack,
            min: d,
            out,
            resolve: None,
            dropped: false,
            cap: true,
        }
    }

    pub(crate) fn depth(&self) -> u64 {
        self.stack.depth
    }

    pub(crate) fn push(&mut self, at: u64, kind: bool) {
        self.stack.depth += 1;
        self.stack.known.push(kind);
        if self.cap && self.stack.known.len() > KNOWN_MAX {
            self.stack.known.drain(..KNOWN_MAX / 2);
            self.dropped = true;
        }
        self.out.open(at, self.stack.depth, kind);
    }

    /// Закрыть уровень; `false` — стек пуст (ошибка документа: глубина не меняется).
    pub(crate) fn pop(&mut self, at: u64) -> bool {
        if self.stack.depth == 0 {
            return false;
        }
        self.stack.depth -= 1;
        self.stack.known.pop();
        self.min = self.min.min(self.stack.depth);
        self.out.close(at, self.stack.depth);
        true
    }

    /// Роль верхнего уровня; `None` — корень. Неизвестная — по сводкам точек (без них —
    /// `false`: проход «только глубина» ролей не читает).
    pub(crate) fn top(&mut self) -> Option<bool> {
        if self.stack.depth == 0 {
            return None;
        }
        if let Some(k) = self.stack.known.last() {
            return Some(*k);
        }
        let Some(r) = self.resolve.as_deref_mut() else {
            return Some(false);
        };
        let v = r.kinds(self.stack.depth, self.min);
        let k = v.last().copied().unwrap_or(false);
        self.stack.known = v;
        Some(k)
    }

    pub(crate) fn span(&mut self, start: u64, end: u64, tag: Tag) {
        if start < end && tag != Tag::Plain {
            self.out.span(start, end, tag);
        }
    }

    pub(crate) fn anchor(&mut self, at: u64) {
        self.out.anchor(at);
    }

    pub(crate) fn inert(&mut self, start: u64, end: u64, deny: &'static [u8], del: bool) {
        if start <= end {
            self.out.inert(Inert {
                start,
                end,
                deny,
                del,
            });
        }
    }
}

/// Позиция единицы в источнике.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Map<'a> {
    /// Байт за байтом с `base`.
    Id(u64),
    /// Единица UTF-16 — два байта с `base`.
    Wide(u64),
    /// Посимвольно: начало символа `i`; `t[len]` — конец.
    Table(&'a [u64]),
}

impl Map<'_> {
    #[inline]
    pub(crate) fn raw(&self, i: usize) -> u64 {
        match self {
            Map::Id(b) => b + i as u64,
            Map::Wide(b) => b + 2 * i as u64,
            Map::Table(t) => t[i],
        }
    }

    /// Байт в единице ASCII.
    #[inline]
    pub(crate) fn unit(&self) -> u64 {
        if matches!(self, Map::Wide(_)) { 2 } else { 1 }
    }
}

/// Лексер или трекер.
pub(crate) trait Lex: Send {
    /// Состояние без стека (`n`, `top` ставит проход).
    fn state(&self) -> St;
    /// Состояние на позиции `at` (`Scanner::at`): расстояния — относительно неё.
    fn state_at(&self, _at: u64) -> St {
        self.state()
    }
    /// Состояние полно: точка ставится (не внутри короткого токена).
    fn can_mark(&self) -> bool;
    /// До первого `feed` с начала текста: его первые `Lang::head` байт единицами.
    fn begin(&mut self, _head: &[u8]) {}
    /// Единицы `b` (позиции — `m`), все.
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>);
    /// Остановка на `end` (`eof` — конец текста): докрасить начатый токен; в конце текста
    /// — закрыть то, что закрывает конец (таблица TOML, разделы).
    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>);
}

/// Текст, по которому идёт проход: источник вьювера или снимок редактора.
pub(crate) trait Text: Send + Sync {
    fn len(&self) -> u64;
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize>;
}

/// Как байты источника становятся единицами.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Feed {
    /// UTF-8 и однобайтовые: байт — единица.
    Bytes,
    Utf16Le,
    Utf16Be,
    /// Многобайтовые (`Shift_JIS`, GBK, Big5, EUC-*): посимвольно.
    Multi(Enc),
}

impl Feed {
    /// `None` — кодировка с состоянием между символами (ISO-2022-JP): без подсветки.
    pub fn of(enc: Enc) -> Option<Feed> {
        if enc.is_utf8() || enc.is_single_byte() {
            Some(Feed::Bytes)
        } else if enc.is_utf16() {
            Some(if enc.is_utf16be() {
                Feed::Utf16Be
            } else {
                Feed::Utf16Le
            })
        } else if enc.enc == encoding_rs::ISO_2022_JP || enc.enc == encoding_rs::REPLACEMENT {
            None
        } else {
            Some(Feed::Multi(enc))
        }
    }

    /// Байт в единице (выравнивание позиций).
    pub fn unit(self) -> u64 {
        match self {
            Feed::Utf16Le | Feed::Utf16Be => 2,
            _ => 1,
        }
    }
}

/// Источник прохода: текст, кодировка, начало текста (за BOM).
pub(crate) struct Src<'a> {
    pub text: &'a dyn Text,
    pub feed: Feed,
    pub start: u64,
}

impl Src<'_> {
    pub(crate) fn len(&self) -> u64 {
        self.text.len()
    }
}

/// Блок чтения.
const BLOCK: usize = 1 << 20;

/// Чтение блоков и перевод в единицы.
#[derive(Default)]
pub(crate) struct Reader {
    raw: Vec<u8>,
    units: Vec<u8>,
    table: Vec<u64>,
}

/// Кусок единиц.
pub(crate) struct Chunk<'r> {
    pub bytes: &'r [u8],
    pub map: Map<'r>,
    /// Позиция источника после куска.
    pub next: u64,
}

impl Reader {
    /// Единицы с `at` (граница символа): не больше `max` байт источника (≥ одного
    /// символа). Пусто — конец текста.
    pub(crate) fn chunk<'r>(
        &'r mut self,
        src: &Src<'_>,
        at: u64,
        max: usize,
    ) -> io::Result<Chunk<'r>> {
        let len = src.len();
        // Байты — ровно `max`; UTF-16 — чётно; многобайтовые — с запасом на символ
        // (дальше `max` новый символ не начинается).
        let room = match src.feed {
            Feed::Bytes => max.max(1),
            Feed::Utf16Le | Feed::Utf16Be => max.max(2),
            Feed::Multi(_) => max.max(1).saturating_add(4),
        };
        let want = usize::try_from(len.saturating_sub(at))
            .unwrap_or(usize::MAX)
            .min(room)
            .min(BLOCK);
        self.raw.resize(want, 0);
        let mut n = 0;
        while n < want {
            match src.text.read_at(at + n as u64, &mut self.raw[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.raw.truncate(n);
        let eof = at + n as u64 >= len;
        match src.feed {
            Feed::Bytes => Ok(Chunk {
                bytes: &self.raw,
                map: Map::Id(at),
                next: at + n as u64,
            }),
            Feed::Utf16Le | Feed::Utf16Be => {
                let be = src.feed == Feed::Utf16Be;
                let k = (n / 2).max(usize::from(n >= 2));
                self.units.clear();
                self.units
                    .extend(self.raw[..2 * k].as_chunks::<2>().0.iter().map(|c| {
                        let u = if be {
                            u16::from_be_bytes(*c)
                        } else {
                            u16::from_le_bytes(*c)
                        };
                        #[allow(clippy::cast_possible_truncation, reason = "u < 0x80")]
                        if u < 0x80 { u as u8 } else { 0x80 }
                    }));
                // Нечётный хвост в конце файла — не единица.
                let next = if eof && n % 2 == 1 {
                    at + n as u64
                } else {
                    at + 2 * k as u64
                };
                Ok(Chunk {
                    bytes: &self.units,
                    map: Map::Wide(at),
                    next,
                })
            }
            Feed::Multi(enc) => {
                let mut dec = enc.decoder();
                self.units.clear();
                self.table.clear();
                let mut i = 0;
                // Не в конце файла символ у края блока может быть неполным: до края — с
                // запасом в 4 байта (длиннее символов у этих кодировок нет).
                let lim = max.max(1).min(n);
                while i < lim && (eof || i + 4 <= n || i == 0) {
                    let (c, k) = match dec.next(&self.raw[i..n]) {
                        Step::Char(c, k) => (Some(c), usize::from(k)),
                        Step::Bad(k) => (None, usize::from(k)),
                    };
                    let k = k.max(1);
                    self.table.push(at + i as u64);
                    #[allow(clippy::cast_possible_truncation, reason = "ASCII")]
                    self.units.push(match c {
                        Some(c) if c.is_ascii() => c as u8,
                        _ => 0x80,
                    });
                    i += k;
                }
                let next = at + i as u64;
                self.table.push(next);
                Ok(Chunk {
                    bytes: &self.units,
                    map: Map::Table(&self.table),
                    next,
                })
            }
        }
    }
}

/// Проход лексера по тексту с позиции `at`.
pub(crate) struct Scanner<'s> {
    src: &'s Src<'s>,
    rd: Reader,
    /// Позиция источника следующей единицы.
    pub at: u64,
    pub lex: Box<dyn Lex>,
    /// Начала строк и первый непробельный байт куска — тому, кто попросил (`lines`).
    pub lines: Option<Vec<u64>>,
    /// Сколько байт показать `Lex::begin` до первого шага (0 — не показывать).
    head: u64,
}

impl<'s> Scanner<'s> {
    pub(crate) fn new(src: &'s Src<'s>, lang: Lang, at: u64, st: St) -> Scanner<'s> {
        Scanner {
            src,
            rd: Reader::default(),
            at,
            lex: lang.lexer(st),
            lines: None,
            head: if at == src.start && st == St::default() {
                lang.head()
            } else {
                0
            },
        }
    }

    pub(crate) fn eof(&self) -> bool {
        self.at >= self.src.len()
    }

    /// Единицы до позиции `to` (≥ `to`, граница символа) или до конца текста. `stop` —
    /// отмена (проверяется на каждом блоке).
    pub(crate) fn run_to(
        &mut self,
        to: u64,
        cx: &mut Cx<'_>,
        stop: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        while self.at < to && !self.eof() {
            if stop() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let max = usize::try_from(to - self.at).unwrap_or(usize::MAX);
            self.step(max, cx)?;
        }
        Ok(())
    }

    /// Единицы, пока лексер не сможет поставить точку (обычно — сразу).
    pub(crate) fn settle(&mut self, cx: &mut Cx<'_>) -> io::Result<()> {
        while !self.lex.can_mark() && !self.eof() {
            let unit = usize::try_from(self.src.feed.unit()).unwrap_or(1);
            self.step(unit, cx)?;
        }
        Ok(())
    }

    fn step(&mut self, max: usize, cx: &mut Cx<'_>) -> io::Result<()> {
        let head = std::mem::take(&mut self.head);
        if head > 0 {
            let c = self.rd.chunk(
                self.src,
                self.at,
                usize::try_from(head).unwrap_or(usize::MAX),
            )?;
            self.lex.begin(c.bytes);
        }
        let c = self.rd.chunk(self.src, self.at, max)?;
        if c.next == self.at {
            // Ничего не прочитано (текст укоротился): дальше — конец.
            self.at = self.src.len();
            return Ok(());
        }
        if let Some(l) = &mut self.lines {
            for i in memchr::memchr_iter(b'\n', c.bytes) {
                l.push(c.map.raw(i + 1));
            }
        }
        self.lex.feed(c.bytes, c.map, cx);
        self.at = c.next;
        Ok(())
    }

    /// Состояние с верхом стека.
    pub(crate) fn state(&self, cx: &Cx<'_>) -> St {
        let mut st = self.lex.state_at(self.at);
        let (n, top) = cx.stack.top_bits();
        st.n = n;
        st.top = top;
        st
    }

    pub(crate) fn flush(&mut self, cx: &mut Cx<'_>) {
        let eof = self.eof();
        self.lex.flush(self.at, eof, cx);
    }
}

/// Байты `ws` — пробел, табуляция, перевод строки.
#[inline]
pub(crate) fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod tree_tests;
