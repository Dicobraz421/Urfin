//! HTML (`tree-sitter-html` + запрос крейта): имя тега — `tag` (`keyword`), значение
//! атрибута без кавычек — `string`, `<!-- -->` — `comment`; прочее — текст (`doctype`,
//! имена атрибутов, `script` и `style` запрос не красит). Лексер повторяет внешний сканер
//! грамматики, чтобы элементы закрывались там же, где у дерева:
//!
//! - **Стек тегов** (`Html::tags`): `<` перед именем снимает пустой элемент (`<br>`) и
//!   элементы, которые не могут содержать новый (`<li>` в `<li>`, блок в `<p>`, `<td>` в
//!   `<td>`); `</x>` снимает всё до `x` (у своих тегов — до любого своего, как сканер), а
//!   если `x` в стеке нет — закрывать нечего. Имя с пробелом (`< a`, `</ a`) этих правил
//!   не запускает. Стек хранится вне состояния точки: `Marks::tags` (ADR-0024 §2).
//! - **Уровни свёртки** — именованные узлы дерева, которые бывают многострочными:
//!   элемент, закрывающий тег, атрибут, значение в кавычках, комментарий, `doctype`,
//!   `raw_text`. Открытие — начало узла. Закрытие — начало последнего безымянного листа
//!   (`>`, `/>`, кавычка), как у дерева; без него — конец узла. Неявное закрытие стоит на
//!   `<` следующего тега.
//! - `<!--` закрывается на `-->` не раньше чем через два `-`, `</script` ищется автоматом
//!   сканера (несовпавший `<` не начинает новое совпадение).
//!
//! Спецификация, а не грамматика: `<!--` в начале значения в кавычках — значение (сканер
//! делает из него комментарий, только если `-->` есть где-то ниже, а это конец файла).
//!
//! Неверный документ (дерево — с `ERROR`): `<` без имени и `<!` не перед `--` и `doctype` —
//! текст, `<!…>` — текст до `>`; незакрытые комментарий, значение и `script` идут до конца
//! файла; конец файла закрывает все элементы; `<` внутри тега начинает новый тег;
//! `<script/>` закрыт (`.xhtml`); прочее лишнее в теге пропускается.

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use std::collections::VecDeque;

use super::{Cx, Lex, Map, St};
use crate::syntax::Tag;

// Режимы.
const TEXT: u8 = 0;
/// После `<`.
const LT: u8 = 1;
/// После `<` и пробела или комментария: ждём имя.
const LT_SP: u8 = 2;
/// Имя открывающего тега.
const NAME: u8 = 3;
/// Внутри открывающего тега.
const TAG: u8 = 4;
/// Имя атрибута.
const ATTR: u8 = 5;
/// За именем атрибута: пробелы, комментарии, `=`.
const AFTER: u8 = 6;
/// За `=`.
const EQ: u8 = 7;
/// Значение без кавычек.
const UNQ: u8 = 8;
/// Значение в кавычках (`b` — кавычка).
const QV: u8 = 9;
/// `/` в теге.
const SLASH: u8 = 10;
/// После `</`.
const END: u8 = 11;
/// После `</` и пробела или комментария.
const END_SP: u8 = 12;
/// Имя закрывающего тега.
const ENAME: u8 = 13;
/// За именем закрывающего тега: ждём `>`.
const ETAG: u8 = 14;
/// После `<!`.
const BANG: u8 = 15;
/// После `<!-`.
const BANG_D: u8 = 16;
/// `<` в месте, где разрешён комментарий (`ret` — куда вернуться): `<`, `<!`, `<!-`.
const CQ: u8 = 17;
const CQ1: u8 = 18;
const CQ2: u8 = 19;
/// Комментарий (`b` — подряд идущих `-`, до двух).
const COMMENT: u8 = 20;
/// Слово `doctype` (`b` — сколько букв совпало).
const DT_WORD: u8 = 21;
/// После `<!` и пробела.
const DT_SP: u8 = 22;
/// За словом `doctype`: пробелы, комментарии.
const DT_GAP: u8 = 23;
/// Содержимое `doctype` до `>`.
const DT_BODY: u8 = 24;
/// `<!…>`, которого нет в грамматике: текст до `>`.
const BOGUS: u8 = 25;
/// `script` и `style` (`b` — совпавшая часть `</SCRIPT`).
const RAW: u8 = 26;
/// `</script` найден: имя кончается здесь или это не конец.
const RAW_END: u8 = 27;

/// Внутри текста правка не должна вносить `<`.
const DENY_TEXT: &[u8] = b"<";
const DENY_COMMENT: &[u8] = b"->";
const DENY_DQ: &[u8] = b"\"";
const DENY_SQ: &[u8] = b"'";

const DOCTYPE: &[u8; 7] = b"DOCTYPE";
const END_SCRIPT: &[u8] = b"</SCRIPT";
const END_STYLE: &[u8] = b"</STYLE";

/// Уровней стека тегов, которые лексер помнит: глубже сканер грамматики и сам теряет теги
/// (буфер состояния на всё — килобайт), а браузеры режут дерево на 512.
pub(crate) const TAGS_MAX: usize = 2048;

/// Теги сканера (`tag.h`), по алфавиту: ключ тега — номер здесь + 1.
const NAMES: [&str; 125] = [
    "A",
    "ABBR",
    "ADDRESS",
    "AREA",
    "ARTICLE",
    "ASIDE",
    "AUDIO",
    "B",
    "BASE",
    "BASEFONT",
    "BDI",
    "BDO",
    "BGSOUND",
    "BLOCKQUOTE",
    "BODY",
    "BR",
    "BUTTON",
    "CANVAS",
    "CAPTION",
    "CITE",
    "CODE",
    "COL",
    "COLGROUP",
    "COMMAND",
    "DATA",
    "DATALIST",
    "DD",
    "DEL",
    "DETAILS",
    "DFN",
    "DIALOG",
    "DIV",
    "DL",
    "DT",
    "EM",
    "EMBED",
    "FIELDSET",
    "FIGCAPTION",
    "FIGURE",
    "FOOTER",
    "FORM",
    "FRAME",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "HEAD",
    "HEADER",
    "HGROUP",
    "HR",
    "HTML",
    "I",
    "IFRAME",
    "IMAGE",
    "IMG",
    "INPUT",
    "INS",
    "ISINDEX",
    "KBD",
    "KEYGEN",
    "LABEL",
    "LEGEND",
    "LI",
    "LINK",
    "MAIN",
    "MAP",
    "MARK",
    "MATH",
    "MENU",
    "MENUITEM",
    "META",
    "METER",
    "NAV",
    "NEXTID",
    "NOSCRIPT",
    "OBJECT",
    "OL",
    "OPTGROUP",
    "OPTION",
    "OUTPUT",
    "P",
    "PARAM",
    "PICTURE",
    "PRE",
    "PROGRESS",
    "Q",
    "RB",
    "RP",
    "RT",
    "RTC",
    "RUBY",
    "S",
    "SAMP",
    "SCRIPT",
    "SECTION",
    "SELECT",
    "SLOT",
    "SMALL",
    "SOURCE",
    "SPAN",
    "STRONG",
    "STYLE",
    "SUB",
    "SUMMARY",
    "SUP",
    "SVG",
    "TABLE",
    "TBODY",
    "TD",
    "TEMPLATE",
    "TEXTAREA",
    "TFOOT",
    "TH",
    "THEAD",
    "TIME",
    "TITLE",
    "TR",
    "TRACK",
    "U",
    "UL",
    "VAR",
    "VIDEO",
    "WBR",
];

/// Пустые элементы (`tag.h`).
const VOID: [&str; 23] = [
    "AREA", "BASE", "BASEFONT", "BGSOUND", "BR", "COL", "COMMAND", "EMBED", "FRAME", "HR", "IMAGE",
    "IMG", "INPUT", "ISINDEX", "KEYGEN", "LINK", "MENUITEM", "META", "NEXTID", "PARAM", "SOURCE",
    "TRACK", "WBR",
];

/// Что не может лежать в `<p>` (`tag.h`).
const NOT_IN_P: [&str; 26] = [
    "ADDRESS",
    "ARTICLE",
    "ASIDE",
    "BLOCKQUOTE",
    "DETAILS",
    "DIV",
    "DL",
    "FIELDSET",
    "FIGCAPTION",
    "FIGURE",
    "FOOTER",
    "FORM",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "HEADER",
    "HR",
    "MAIN",
    "NAV",
    "OL",
    "P",
    "PRE",
    "SECTION",
];

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Ключ имени из таблицы.
const fn id(s: &str) -> u32 {
    let mut i = 0;
    while i < NAMES.len() {
        if same(NAMES[i], s) {
            #[allow(clippy::cast_possible_truncation, reason = "таблица короткая")]
            return i as u32 + 1;
        }
        i += 1;
    }
    panic!("тега нет в таблице")
}

const fn table<const N: usize>(names: [&str; N]) -> [bool; 126] {
    let mut out = [false; 126];
    let mut i = 0;
    while i < N {
        out[id(names[i]) as usize] = true;
        i += 1;
    }
    out
}

static IS_VOID: [bool; 126] = table(VOID);
static IS_NOT_IN_P: [bool; 126] = table(NOT_IN_P);

const LI: u32 = id("LI");
const DT: u32 = id("DT");
const DD: u32 = id("DD");
const P: u32 = id("P");
const COL: u32 = id("COL");
const COLGROUP: u32 = id("COLGROUP");
const RB: u32 = id("RB");
const RT: u32 = id("RT");
const RP: u32 = id("RP");
const OPTGROUP: u32 = id("OPTGROUP");
const TR: u32 = id("TR");
const TD: u32 = id("TD");
const TH: u32 = id("TH");
const SCRIPT: u32 = id("SCRIPT");
const STYLE: u32 = id("STYLE");

/// Свой тег: старший бит, имя — хешем.
const CUSTOM: u32 = 1 << 31;

/// Пустой элемент.
fn is_void(key: u32) -> bool {
    key < 126 && IS_VOID[key as usize]
}

/// Один вид тега (`type` сканера: свои теги — один вид).
fn same_type(a: u32, b: u32) -> bool {
    a == b || a & b & CUSTOM != 0
}

/// `tag_can_contain` сканера.
fn can_contain(parent: u32, child: u32) -> bool {
    match parent {
        LI => child != LI,
        DT | DD => child != DT && child != DD,
        P => !(child < 126 && IS_NOT_IN_P[child as usize]),
        COLGROUP => child == COL,
        RB | RT | RP => child != RB && child != RT && child != RP,
        OPTGROUP => child != OPTGROUP,
        TR => child != TR,
        TD | TH => child != TD && child != TH && child != TR,
        _ => true,
    }
}

/// Байт имени тега (`iswalnum`, `-`, `:`; не-ASCII считается буквой).
#[inline]
fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b':') || c >= 0x80
}

/// Пробел `\s` (ASCII: остальные сюда единицами не попадают).
#[inline]
fn space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

/// Вклад уровня в хеш стека.
#[inline]
fn mix(lvl: u32, key: u32) -> u64 {
    let x = (u64::from(lvl) << 32 | u64::from(key)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (x ^ x >> 29).wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

/// Набранное имя.
#[derive(Copy, Clone, Default)]
struct Name {
    buf: [u8; 10],
    len: u32,
    hash: u32,
}

impl Name {
    #[inline]
    fn push(&mut self, c: u8) {
        let c = c.to_ascii_uppercase();
        if let Some(b) = self.buf.get_mut(self.len as usize) {
            *b = c;
        }
        self.len = self.len.saturating_add(1);
        self.hash = (self.hash ^ u32::from(c)).wrapping_mul(0x0100_0193);
    }

    fn key(&self) -> u32 {
        let n = self.len as usize;
        if n <= self.buf.len()
            && let Ok(i) = NAMES.binary_search_by(|x| x.as_bytes().cmp(&self.buf[..n]))
        {
            #[allow(clippy::cast_possible_truncation, reason = "таблица короткая")]
            return i as u32 + 1;
        }
        CUSTOM | self.hash & !CUSTOM
    }
}

pub(crate) struct Html {
    mode: u8,
    /// Куда вернуться из комментария.
    ret: u8,
    /// Счётчик режима или кавычка.
    b: u16,
    /// Имя вплотную к `<` или `</`.
    adj: bool,
    /// Уровень атрибута открыт.
    attr: bool,
    /// `>` закрывает и элемент.
    close: bool,
    /// Стек тегов, снизу вверх.
    tags: VecDeque<u32>,
    /// Глубина стека с забытыми уровнями и хеш всего стека.
    lvl: u32,
    hash: u64,
    /// `<` тега (открытие элемента, неявные закрытия).
    tok: u64,
    /// Начало имени.
    ntok: u64,
    /// Начало комментария, `/` в теге, начало `</script`.
    ctok: u64,
    /// Конец имени атрибута.
    attr_end: u64,
    /// Начало зоны: текст, значение, тело комментария (за `<!--`).
    zone: u64,
    /// Последний лист `>` или `/>`: начало и конец. Неявное закрытие вплотную за ним
    /// считается закрытием на нём, как у дерева: это конец узла.
    leaf: (u64, u64),
    name: Name,
    /// После возобновления: `tok`, `attr_end`, начало листа назад от первого куска.
    back: Option<(u64, u64, u64)>,
}

impl Html {
    pub(crate) fn resume(st: St) -> Html {
        Html {
            mode: st.mode,
            ret: st.a,
            b: st.b,
            adj: st.d & 1 != 0,
            attr: st.d & 2 != 0,
            close: st.d & 4 != 0,
            tags: VecDeque::new(),
            lvl: st.c,
            hash: st.x,
            tok: 0,
            ntok: 0,
            ctok: 0,
            attr_end: 0,
            zone: 0,
            leaf: (0, u64::MAX),
            name: Name::default(),
            back: Some((st.ext.cut, st.ext.cand, st.x2)),
        }
    }

    fn push_tag(&mut self, key: u32, cx: &mut Cx<'_>) {
        self.lvl = self.lvl.saturating_add(1);
        self.hash ^= mix(self.lvl, key);
        self.tags.push_back(key);
        if self.tags.len() > TAGS_MAX
            && let Some(k) = self.tags.pop_front()
        {
            #[allow(clippy::cast_possible_truncation, reason = "TAGS_MAX мал")]
            let low = self.lvl - TAGS_MAX as u32;
            self.hash ^= mix(low, k);
        }
        cx.push(self.tok, false);
    }

    fn pop_tag(&mut self) {
        if let Some(k) = self.tags.pop_back() {
            self.hash ^= mix(self.lvl, k);
            self.lvl -= 1;
        }
    }

    /// Неявное закрытие верхнего элемента на `<` тега.
    fn implicit(&mut self, cx: &mut Cx<'_>) {
        self.pop_tag();
        cx.pop(self.close_at(self.tok));
    }

    /// Закрытие узла, конец которого — `at`.
    fn close_at(&self, at: u64) -> u64 {
        if self.leaf.1 == at { self.leaf.0 } else { at }
    }

    /// Имя открывающего тега кончилось на `at`.
    fn start_tag(&mut self, at: u64, cx: &mut Cx<'_>) {
        let key = self.name.key();
        if self.adj {
            while self.tags.back().is_some_and(|&p| !can_contain(p, key)) {
                self.implicit(cx);
            }
        }
        cx.span(self.ntok, at, Tag::Keyword);
        self.push_tag(key, cx);
        self.name = Name::default();
        self.adj = false;
        self.mode = TAG;
    }

    /// Имя закрывающего тега кончилось на `at`.
    fn end_tag(&mut self, at: u64, cx: &mut Cx<'_>) {
        let key = self.name.key();
        if self.adj {
            while self.tags.back().is_some_and(|&t| t != key)
                && self.tags.iter().any(|&t| same_type(t, key))
            {
                self.implicit(cx);
            }
        }
        self.close = self.tags.back() == Some(&key);
        if self.close {
            self.pop_tag();
        }
        cx.span(self.ntok, at, Tag::Keyword);
        cx.push(self.tok, false);
        self.name = Name::default();
        self.adj = false;
        self.mode = ETAG;
    }

    /// Не комментарий: `<` из `ret` оказался лишним. В теге это начало нового тега: старый
    /// не дописан (так и восстанавливается дерево).
    fn no_comment(&mut self, cx: &mut Cx<'_>) {
        self.mode = match self.ret {
            DT_GAP => DT_BODY,
            DT_SP => BOGUS,
            LT_SP | END_SP => {
                self.tok = self.ctok;
                LT
            }
            TAG | AFTER | EQ => {
                let e = if self.ret == AFTER {
                    self.attr_end
                } else {
                    self.ctok
                };
                self.end_attr(e, cx);
                self.tok = self.ctok;
                LT
            }
            ETAG => {
                cx.pop(self.ctok);
                if std::mem::take(&mut self.close) {
                    cx.pop(self.ctok);
                }
                self.tok = self.ctok;
                LT
            }
            r => r,
        };
    }

    /// Комментарий начат в `ctok`, тело — с `body`.
    fn comment(&mut self, body: u64, cx: &mut Cx<'_>) {
        cx.push(self.ctok, false);
        self.zone = body;
        self.mode = COMMENT;
        self.b = 0;
    }

    /// `<` там, где разрешён комментарий: режим `ret`.
    fn maybe_comment(&mut self, at: u64, ret: u8) {
        self.ctok = at;
        self.ret = ret;
        self.mode = CQ;
    }

    /// Закрыть начатый атрибут в `at`.
    fn end_attr(&mut self, at: u64, cx: &mut Cx<'_>) {
        if std::mem::take(&mut self.attr) {
            cx.pop(at);
        }
    }

    /// Конец открывающего тега: `>` длиной `unit`, за ним `next`.
    fn tag_end(&mut self, next: u64, unit: u64, cx: &mut Cx<'_>) {
        self.zone = next;
        self.leaf = (next - unit, next);
        if matches!(self.tags.back(), Some(&SCRIPT | &STYLE)) {
            cx.push(next, false);
            self.mode = RAW;
            self.b = 0;
        } else {
            self.mode = TEXT;
        }
    }

    fn raw_delim(&self) -> &'static [u8] {
        if self.tags.back() == Some(&STYLE) {
            END_STYLE
        } else {
            END_SCRIPT
        }
    }
}

impl Lex for Html {
    fn state(&self) -> St {
        self.state_at(0)
    }

    fn state_at(&self, at: u64) -> St {
        let in_comment = self.mode == COMMENT;
        let mut st = St {
            mode: self.mode,
            a: if in_comment { self.ret } else { 0 },
            b: self.b,
            c: self.lvl,
            x: self.hash,
            d: u8::from(self.adj) | u8::from(self.attr) << 1 | u8::from(self.close) << 2,
            ..St::default()
        };
        // Позиции нужны дальше только здесь; иначе состояние не должно от них зависеть.
        let (keep_tok, keep_attr) = (
            in_comment && matches!(self.ret, LT_SP | END_SP | DT_SP),
            self.attr && (self.mode == AFTER || in_comment && self.ret == AFTER),
        );
        let (t, e, l) = self.back.unwrap_or((
            at.saturating_sub(self.tok),
            at.saturating_sub(self.attr_end),
            if self.leaf.1 == at {
                at - self.leaf.0
            } else {
                0
            },
        ));
        st.ext.cut = if keep_tok { t } else { 0 };
        st.ext.cand = if keep_attr { e } else { 0 };
        st.x2 = l;
        st
    }

    fn can_mark(&self) -> bool {
        matches!(
            self.mode,
            TEXT | TAG | ATTR | AFTER | EQ | UNQ | QV | ETAG | COMMENT | DT_GAP | DT_BODY | BOGUS
        ) || self.mode == RAW && self.b == 0
    }

    fn keyed(&self) -> bool {
        true
    }

    fn keys(&self) -> Option<Vec<u32>> {
        Some(self.tags.iter().copied().collect())
    }

    fn set_keys(&mut self, keys: &[u32]) {
        self.tags = keys.iter().copied().collect();
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "автомат одного языка"
    )]
    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        if b.is_empty() {
            return;
        }
        if let Some((t, e, l)) = self.back.take() {
            let at0 = m.raw(0);
            self.tok = at0.saturating_sub(t);
            self.attr_end = at0.saturating_sub(e);
            self.ctok = at0;
            self.zone = at0;
            if l > 0 {
                self.leaf = (at0 - l, at0);
            }
        }
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            let at = m.raw(i);
            match self.mode {
                TEXT => match memchr::memchr(b'<', &b[i..]) {
                    None => i = b.len(),
                    Some(k) => {
                        let j = i + k;
                        let p = m.raw(j);
                        cx.inert(self.zone, p, DENY_TEXT, true);
                        self.tok = p;
                        self.mode = LT;
                        i = j + 1;
                    }
                },
                LT => match c {
                    b'!' => {
                        self.mode = BANG;
                        i += 1;
                    }
                    b'/' => {
                        self.mode = END;
                        self.adj = true;
                        i += 1;
                    }
                    _ => {
                        // Пустой элемент снимает любой `<`, кроме `<!` и `</`.
                        if self.tags.back().is_some_and(|&t| is_void(t)) {
                            self.implicit(cx);
                        }
                        if name_char(c) {
                            self.adj = true;
                            self.ntok = at;
                            self.mode = NAME;
                        } else if space(c) {
                            self.adj = false;
                            self.mode = LT_SP;
                            i += 1;
                        } else if c == b'<' {
                            self.adj = false;
                            self.maybe_comment(at, LT_SP);
                            i += 1;
                        } else {
                            self.adj = false;
                            // Вставка перед этим байтом сделает тег: зона — со следующего.
                            self.zone = m.raw(i + 1);
                            self.mode = TEXT;
                        }
                    }
                },
                LT_SP => {
                    if name_char(c) {
                        self.adj = false;
                        self.ntok = at;
                        self.mode = NAME;
                    } else if space(c) {
                        i += 1;
                    } else if c == b'<' {
                        self.maybe_comment(at, LT_SP);
                        i += 1;
                    } else {
                        self.zone = m.raw(i + 1);
                        self.mode = TEXT;
                    }
                }
                NAME => {
                    if name_char(c) {
                        self.name.push(c);
                        i += 1;
                    } else {
                        self.start_tag(at, cx);
                    }
                }
                TAG => match c {
                    b'>' => {
                        self.tag_end(m.raw(i + 1), m.unit(), cx);
                        i += 1;
                    }
                    b'/' => {
                        self.ctok = at;
                        self.mode = SLASH;
                        i += 1;
                    }
                    b'<' => {
                        self.maybe_comment(at, TAG);
                        i += 1;
                    }
                    b'=' => {
                        self.mode = EQ;
                        i += 1;
                    }
                    b'"' | b'\'' => i += 1,
                    _ if space(c) => i += 1,
                    _ => {
                        cx.push(at, false);
                        self.attr = true;
                        self.mode = ATTR;
                        i += 1;
                    }
                },
                ATTR => {
                    if matches!(c, b'<' | b'>' | b'"' | b'\'' | b'/' | b'=') || space(c) {
                        self.attr_end = at;
                        self.mode = AFTER;
                    } else {
                        i += 1;
                    }
                }
                AFTER => {
                    if c == b'=' {
                        self.mode = EQ;
                        i += 1;
                    } else if space(c) {
                        i += 1;
                    } else if c == b'<' {
                        self.maybe_comment(at, AFTER);
                        i += 1;
                    } else {
                        let e = self.attr_end;
                        self.end_attr(e, cx);
                        self.mode = TAG;
                    }
                }
                EQ => match c {
                    b'"' | b'\'' => {
                        cx.push(at, false);
                        self.b = u16::from(c);
                        self.zone = m.raw(i + 1);
                        self.mode = QV;
                        i += 1;
                    }
                    b'<' => {
                        self.maybe_comment(at, EQ);
                        i += 1;
                    }
                    b'>' | b'=' => {
                        self.end_attr(at, cx);
                        self.mode = TAG;
                    }
                    _ if space(c) => i += 1,
                    _ => {
                        self.zone = at;
                        self.mode = UNQ;
                    }
                },
                UNQ => {
                    if matches!(c, b'<' | b'>' | b'"' | b'\'' | b'=') || space(c) {
                        cx.span(self.zone, at, Tag::String);
                        self.end_attr(at, cx);
                        self.mode = TAG;
                    } else {
                        i += 1;
                    }
                }
                QV => {
                    #[allow(clippy::cast_possible_truncation, reason = "кавычка — байт")]
                    let q = self.b as u8;
                    match memchr::memchr(q, &b[i..]) {
                        None => i = b.len(),
                        Some(k) => {
                            let j = i + k;
                            let p = m.raw(j);
                            cx.span(self.zone, p, Tag::String);
                            let deny = if q == b'"' { DENY_DQ } else { DENY_SQ };
                            cx.inert(self.zone, p, deny, true);
                            cx.pop(p);
                            self.end_attr(p, cx);
                            self.b = 0;
                            self.mode = TAG;
                            i = j + 1;
                        }
                    }
                }
                SLASH => {
                    self.mode = TAG;
                    if c == b'>' {
                        self.pop_tag();
                        cx.pop(self.ctok);
                        self.zone = m.raw(i + 1);
                        self.leaf = (self.ctok, self.zone);
                        self.mode = TEXT;
                        i += 1;
                    }
                }
                END => {
                    if name_char(c) {
                        self.ntok = at;
                        self.mode = ENAME;
                    } else if space(c) {
                        self.adj = false;
                        self.mode = END_SP;
                        i += 1;
                    } else if c == b'<' {
                        self.adj = false;
                        self.maybe_comment(at, END_SP);
                        i += 1;
                    } else {
                        self.adj = false;
                        self.zone = m.raw(i + 1);
                        self.mode = if c == b'>' { TEXT } else { BOGUS };
                    }
                }
                END_SP => {
                    if name_char(c) {
                        self.ntok = at;
                        self.mode = ENAME;
                    } else if space(c) {
                        i += 1;
                    } else if c == b'<' {
                        self.maybe_comment(at, END_SP);
                        i += 1;
                    } else {
                        self.zone = m.raw(i + 1);
                        self.mode = if c == b'>' { TEXT } else { BOGUS };
                    }
                }
                ENAME => {
                    if name_char(c) {
                        self.name.push(c);
                        i += 1;
                    } else {
                        self.end_tag(at, cx);
                    }
                }
                ETAG => {
                    if c == b'>' {
                        cx.pop(at);
                        if std::mem::take(&mut self.close) {
                            cx.pop(at);
                        }
                        self.zone = m.raw(i + 1);
                        self.leaf = (at, self.zone);
                        self.mode = TEXT;
                    } else if c == b'<' {
                        self.maybe_comment(at, ETAG);
                    }
                    i += 1;
                }
                BANG => {
                    if c == b'-' {
                        self.mode = BANG_D;
                        i += 1;
                    } else if space(c) {
                        self.mode = DT_SP;
                        i += 1;
                    } else if c == b'<' {
                        self.maybe_comment(at, DT_SP);
                        i += 1;
                    } else {
                        self.b = 0;
                        self.mode = DT_WORD;
                    }
                }
                BANG_D => {
                    if c == b'-' {
                        self.ctok = self.tok;
                        self.ret = TEXT;
                        self.comment(m.raw(i + 1), cx);
                        i += 1;
                    } else {
                        self.mode = BOGUS;
                    }
                }
                CQ => {
                    if c == b'!' {
                        self.mode = CQ1;
                        i += 1;
                    } else {
                        self.no_comment(cx);
                    }
                }
                CQ1 => {
                    if c == b'-' {
                        self.mode = CQ2;
                        i += 1;
                    } else {
                        self.no_comment(cx);
                    }
                }
                CQ2 => {
                    if c == b'-' {
                        self.comment(m.raw(i + 1), cx);
                        i += 1;
                    } else {
                        self.no_comment(cx);
                    }
                }
                COMMENT => match memchr::memchr2(b'-', b'>', &b[i..]) {
                    None => {
                        self.b = 0;
                        i = b.len();
                    }
                    Some(k) => {
                        let j = i + k;
                        if k > 0 {
                            self.b = 0;
                        }
                        if b[j] == b'-' {
                            self.b = (self.b + 1).min(2);
                        } else if self.b >= 2 {
                            let next = m.raw(j + 1);
                            cx.span(self.ctok, next, Tag::Comment);
                            // Конец — два `-` и `>`: вставка между ними его рвёт.
                            let dashes = m.raw(j) - 2 * m.unit();
                            cx.inert(self.zone, dashes, DENY_COMMENT, false);
                            cx.pop(next);
                            self.b = 0;
                            self.zone = next;
                            self.mode = self.ret;
                        } else {
                            self.b = 0;
                        }
                        i = j + 1;
                    }
                },
                DT_SP => {
                    if space(c) {
                        i += 1;
                    } else if c == b'<' {
                        self.maybe_comment(at, DT_SP);
                        i += 1;
                    } else {
                        self.b = 0;
                        self.mode = DT_WORD;
                    }
                }
                DT_WORD => {
                    if c.to_ascii_uppercase() == DOCTYPE[self.b as usize] {
                        self.b += 1;
                        i += 1;
                        if self.b as usize == DOCTYPE.len() {
                            self.b = 0;
                            cx.push(self.tok, false);
                            self.mode = DT_GAP;
                        }
                    } else {
                        self.b = 0;
                        self.mode = BOGUS;
                    }
                }
                DT_GAP => {
                    if space(c) {
                        i += 1;
                    } else if c == b'<' {
                        self.maybe_comment(at, DT_GAP);
                        i += 1;
                    } else {
                        self.mode = DT_BODY;
                    }
                }
                DT_BODY => match memchr::memchr(b'>', &b[i..]) {
                    None => i = b.len(),
                    Some(k) => {
                        let j = i + k;
                        cx.pop(m.raw(j));
                        self.zone = m.raw(j + 1);
                        self.leaf = (m.raw(j), self.zone);
                        self.mode = TEXT;
                        i = j + 1;
                    }
                },
                BOGUS => match memchr::memchr(b'>', &b[i..]) {
                    None => i = b.len(),
                    Some(k) => {
                        self.zone = m.raw(i + k + 1);
                        self.mode = TEXT;
                        i += k + 1;
                    }
                },
                RAW => {
                    if self.b == 0 {
                        match memchr::memchr(b'<', &b[i..]) {
                            None => i = b.len(),
                            Some(k) => {
                                self.ctok = m.raw(i + k);
                                self.ntok = self.ctok + 2 * m.unit();
                                self.b = 1;
                                i += k + 1;
                            }
                        }
                    } else {
                        let d = self.raw_delim();
                        if c.to_ascii_uppercase() == d[self.b as usize] {
                            self.b += 1;
                            i += 1;
                            if self.b as usize == d.len() {
                                self.b = 0;
                                self.mode = RAW_END;
                            }
                        } else {
                            // Как сканер: несовпавший байт съеден, даже `<`.
                            self.b = 0;
                            i += 1;
                        }
                    }
                }
                _ => {
                    // RAW_END: имя продолжилось — это не конец.
                    if name_char(c) {
                        self.mode = RAW;
                        self.b = 0;
                    } else {
                        let start = self.ctok;
                        cx.span(self.ntok, at, Tag::Keyword);
                        cx.pop(start);
                        self.tok = start;
                        self.pop_tag();
                        self.close = true;
                        cx.push(start, false);
                        self.mode = ETAG;
                    }
                }
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        match self.mode {
            NAME | ENAME | RAW_END => cx.span(self.ntok, end, Tag::Keyword),
            UNQ | QV => cx.span(self.zone, end, Tag::String),
            COMMENT => cx.span(self.ctok, end, Tag::Comment),
            TEXT if !eof => cx.inert(self.zone, end, DENY_TEXT, true),
            _ => {}
        }
        if eof {
            while cx.depth() > 0 {
                cx.pop(self.close_at(end));
            }
        }
    }
}

#[cfg(test)]
#[path = "html_tests.rs"]
mod tests;
