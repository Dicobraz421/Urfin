//! Трекер Markdown (ADR-0024 §3): не красит — tree-sitter красит отрезок между
//! безопасными строками. Безопасная строка (`LineKind::Safe`) — с колонки 0, вне
//! ограждённого кода и HTML-блока, и: после пустой строки (кроме `---`/`+++`), или
//! заголовок ATX, или пункт маркированного списка, или `1.`/`1)`. Такая строка закрывает
//! все контейнеры выше, а начатое ею от текста выше не зависит.
//!
//! Ограждённый код и HTML-блоки с отступом 1–3 бывают внутри пункта списка верхнего уровня
//! (тогда их закрывает строка с колонки 0) — трекер ведёт открытый пункт (отступ
//! содержимого) и ленивое продолжение абзаца, как `CommonMark`.
//!
//! Свёртка: раздел заголовка ATX (до заголовка того же или старшего уровня), ограждённый
//! код верхнего уровня, списки и цитаты ([`nest`], решение 2026-11-03).

#![allow(
    clippy::many_single_char_names,
    reason = "автомат по байтам: b — единицы, m — позиции, i — индекс, c — байт"
)]

use super::{Cx, Lex, LineKind, Map, St};

#[cfg(test)]
pub(super) mod html_tests;
mod nest;

const CAP: usize = 64 << 10;

// Флаги (`St::a`).
const PREV_BLANK: u8 = 1;
/// HTML-блок типов 6–7: до пустой строки.
const HTML67: u8 = 2;
/// Ограждение или HTML-блок — внутри пункта списка.
const IN_ITEM: u8 = 4;
/// Ограждению открыт уровень свёртки.
const FENCE_FOLD: u8 = 8;
/// Прошлая строка — текст абзаца (ленивое продолжение возможно).
const PARA: u8 = 16;
/// Первая строка документа пройдена.
const SEEN: u8 = 32;
/// Внутри метаданных (`St::b` — символ ограды).
const META: u8 = 64;
/// Пункт без содержимого: пустая строка сразу после него его закрывает.
const BARE: u8 = 128;

/// Конец HTML-блока типов 1–5.
const HTML_END: [&[u8]; 6] = [b"", b"</", b"-->", b"?>", b">", b"]]>"];

pub(crate) struct Md {
    /// Ограждение: 0 — нет, иначе символ.
    fence: u8,
    flen: u16,
    flags: u8,
    /// HTML-блок 1–5 (0 — нет).
    html: u8,
    /// Отступ содержимого открытого пункта списка (0 — нет).
    item_w: u8,
    /// Открытые разделы: бит — уровень заголовка 1..6.
    secs: u64,
    /// Списки и цитаты (свёртка).
    nest: nest::Nest,
    line: Vec<u8>,
    raws: Vec<u64>,
    over: bool,
    done: bool,
}

fn lower_starts(t: &[u8], p: &[u8]) -> bool {
    t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p)
}

/// Тип HTML-блока по началу строки (1–7; 0 — не HTML-блок).
fn html_kind(t: &[u8]) -> u8 {
    if t.first() != Some(&b'<') {
        return 0;
    }
    for open in [&b"<script"[..], b"<pre", b"<style", b"<textarea"] {
        if lower_starts(t, open)
            && t.get(open.len())
                .is_none_or(|c| matches!(c, b' ' | b'>' | b'\t'))
        {
            return 1;
        }
    }
    if t.starts_with(b"<!--") {
        return 2;
    }
    if t.starts_with(b"<?") {
        return 3;
    }
    if t.starts_with(b"<![CDATA[") {
        return 5;
    }
    if t.starts_with(b"<!") && t.get(2).is_some_and(u8::is_ascii_alphabetic) {
        return 4;
    }
    if html_block_tag(t) {
        return 6;
    }
    if html_tag7(t) {
        return 7;
    }
    0
}

/// Тип 7: строка — целиком открывающий или закрывающий тег, дальше только пробелы. Теги
/// типа 1 сюда доходят только как `</pre>` и `<pre/>` — это тип 7 (как cmark, commonmark.js,
/// pulldown-cmark и tree-sitter).
fn html_tag7(t: &[u8]) -> bool {
    let ws = |c: &u8| matches!(c, b' ' | b'\t');
    let close = t.get(1) == Some(&b'/');
    let mut i = if close { 2 } else { 1 };
    if !t.get(i).is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    while t
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-')
    {
        i += 1;
    }
    if close {
        while t.get(i).is_some_and(ws) {
            i += 1;
        }
        if t.get(i) != Some(&b'>') {
            return false;
        }
        i += 1;
    } else {
        loop {
            let s = i;
            while t.get(i).is_some_and(ws) {
                i += 1;
            }
            match t.get(i) {
                Some(b'>') => {
                    i += 1;
                    break;
                }
                Some(b'/') if t.get(i + 1) == Some(&b'>') => {
                    i += 2;
                    break;
                }
                Some(c) if i > s && (c.is_ascii_alphabetic() || matches!(c, b'_' | b':')) => {
                    i += 1;
                    while t.get(i).is_some_and(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-')
                    }) {
                        i += 1;
                    }
                    // Значение атрибута.
                    let mut j = i;
                    while t.get(j).is_some_and(ws) {
                        j += 1;
                    }
                    if t.get(j) == Some(&b'=') {
                        j += 1;
                        while t.get(j).is_some_and(ws) {
                            j += 1;
                        }
                        match t.get(j) {
                            Some(&q @ (b'"' | b'\'')) => {
                                let Some(e) = t[j + 1..].iter().position(|&c| c == q) else {
                                    return false;
                                };
                                j += e + 2;
                            }
                            Some(_) => {
                                let v = j;
                                while t.get(j).is_some_and(|c| {
                                    !c.is_ascii_whitespace()
                                        && !matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                                }) {
                                    j += 1;
                                }
                                if j == v {
                                    return false;
                                }
                            }
                            None => return false,
                        }
                        i = j;
                    }
                }
                _ => return false,
            }
        }
    }
    t[i..].iter().all(ws)
}

/// HTML-блок типа 6 (тег блока CommonMark): прерывает абзац; тип 7 — нет.
fn html_block_tag(t: &[u8]) -> bool {
    const TAGS: &[&str] = &[
        "address",
        "article",
        "aside",
        "base",
        "basefont",
        "blockquote",
        "body",
        "caption",
        "center",
        "col",
        "colgroup",
        "dd",
        "details",
        "dialog",
        "dir",
        "div",
        "dl",
        "dt",
        "fieldset",
        "figcaption",
        "figure",
        "footer",
        "form",
        "frame",
        "frameset",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "header",
        "hr",
        "html",
        "iframe",
        "legend",
        "li",
        "link",
        "main",
        "menu",
        "menuitem",
        "nav",
        "noframes",
        "ol",
        "optgroup",
        "option",
        "p",
        "param",
        "search",
        "section",
        "summary",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "title",
        "tr",
        "track",
        "ul",
    ];
    if t.first() != Some(&b'<') {
        return false;
    }
    let s = if t.get(1) == Some(&b'/') { 2 } else { 1 };
    let n = t[s.min(t.len())..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric())
        .count();
    let name = &t[s.min(t.len())..(s + n).min(t.len())];
    let after = t.get(s + n);
    TAGS.iter().any(|g| name.eq_ignore_ascii_case(g.as_bytes()))
        && (after.is_none_or(|c| matches!(c, b' ' | b'\t' | b'>'))
            || t[(s + n).min(t.len())..].starts_with(b"/>"))
}

/// Конец HTML-блока `k` на строке.
fn html_closes(k: u8, t: &[u8]) -> bool {
    if k == 1 {
        let lo = t.to_ascii_lowercase();
        return [&b"</script>"[..], b"</pre>", b"</style>", b"</textarea>"]
            .iter()
            .any(|e| memchr::memmem::find(&lo, e).is_some());
    }
    memchr::memmem::find(t, HTML_END[usize::from(k)]).is_some()
}

/// Маркер списка в начале `t`: ширина маркера (без пробелов) и порядковый ли он.
fn list_marker(t: &[u8]) -> Option<(usize, bool, bool)> {
    let c = *t.first()?;
    if matches!(c, b'-' | b'+' | b'*') {
        return t
            .get(1)
            .is_none_or(|x| *x == b' ' || *x == b'\t')
            .then_some((1, false, false));
    }
    let d = t.iter().take_while(|b| b.is_ascii_digit()).count();
    if (1..=9).contains(&d)
        && matches!(t.get(d), Some(b'.' | b')'))
        && t.get(d + 1).is_none_or(|x| *x == b' ' || *x == b'\t')
    {
        let one = &t[..d] == b"1";
        return Some((d + 1, true, one));
    }
    None
}

/// Тематический разрыв (`***`, `- - -`, `___`).
fn thematic(t: &[u8]) -> bool {
    let Some(&c) = t.first() else {
        return false;
    };
    if !matches!(c, b'-' | b'*' | b'_') {
        return false;
    }
    let mut n = 0;
    for &b in t {
        if b == c {
            n += 1;
        } else if b != b' ' && b != b'\t' {
            return false;
        }
    }
    n >= 3
}

/// Подчёркивание Setext (`===`, `--`): строка абзаца, закрывающая его.
fn setext(t: &[u8]) -> bool {
    let Some(&c) = t.first() else {
        return false;
    };
    let k = t.iter().take_while(|&&b| b == c).count();
    matches!(c, b'=' | b'-') && t[k..].iter().all(|b| matches!(b, b' ' | b'\t'))
}

fn fence_open(t: &[u8]) -> Option<(u8, usize)> {
    let c = *t.first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let k = t.iter().take_while(|&&b| b == c).count();
    if k < 3 || (c == b'`' && t[k..].contains(&b'`')) {
        return None;
    }
    Some((c, k))
}

fn heading(t: &[u8]) -> Option<u8> {
    let k = t.iter().take_while(|&&b| b == b'#').count();
    (1..=6).contains(&k).then_some(())?;
    t.get(k)
        .is_none_or(|c| *c == b' ' || *c == b'\t')
        .then(|| u8::try_from(k).unwrap_or(6))
}

impl Md {
    pub(crate) fn resume(st: St) -> Md {
        Md {
            fence: st.mode,
            flen: st.b,
            flags: st.a,
            html: (st.c & 0xFF) as u8,
            item_w: (st.c >> 8 & 0xFF) as u8,
            secs: st.x & 0xFF,
            nest: nest::Nest::unpack(st.x, st.c),
            line: Vec::new(),
            raws: Vec::new(),
            over: false,
            done: false,
        }
    }

    fn set(&mut self, f: u8, on: bool) {
        if on {
            self.flags |= f;
        } else {
            self.flags &= !f;
        }
    }

    fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }

    fn close_fence(&mut self, at: u64, cx: &mut Cx<'_>) {
        if self.has(FENCE_FOLD) {
            cx.pop(at);
        }
        self.fence = 0;
        self.flen = 0;
        self.set(FENCE_FOLD | IN_ITEM, false);
    }

    fn classify(&mut self, cx: &mut Cx<'_>) {
        let b = std::mem::take(&mut self.line);
        let raws = std::mem::take(&mut self.raws);
        // Строки ограждения, HTML-блока верхнего уровня и метаданных — не контейнеры (их
        // начало закрыло все уровни, `Nest::reset`).
        let top_block =
            (self.fence != 0 || self.html != 0 || self.has(HTML67)) && !self.has(IN_ITEM);
        // Абзац открыт до строки (по контейнерам): тип 7 его не прерывает.
        let para = self.nest.para();
        if !top_block && !self.has(META) {
            self.nest
                .line(b.strip_suffix(b"\r").unwrap_or(&b), &raws, cx);
        }
        self.classify_line(&b, &raws, para, cx);
        // Абзаца нет (ограждение, HTML-блок и в пункте): продолжать лениво нечего.
        if !self.has(PARA) {
            self.nest.end_para();
        }
        self.line = b;
        self.raws = raws;
        self.done = true;
    }

    #[allow(clippy::too_many_lines, reason = "блоки CommonMark по строке")]
    fn classify_line(&mut self, b: &[u8], raws: &[u64], para_before: bool, cx: &mut Cx<'_>) {
        let Some(&start) = raws.first() else {
            return;
        };
        let b = b.strip_suffix(b"\r").unwrap_or(b);
        // Отступ: табуляция — до кратного 4.
        let mut ind = 0usize;
        let mut k = 0usize;
        while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
            ind = if b[k] == b'\t' {
                (ind / 4 + 1) * 4
            } else {
                ind + 1
            };
            k += 1;
        }
        let t = &b[k..];
        let blank = t.iter().all(u8::is_ascii_whitespace);
        let at = raws[k.min(raws.len() - 1)];
        // Метаданные в начале документа (`---`/`+++` … `---`/`+++`): внутри — не начало.
        let meta_line = |c: u8| {
            t.len() >= 3
                && t[..3].iter().all(|&x| x == c)
                && t[3..].iter().all(|&x| x == b' ' || x == b'\t')
        };
        if !self.has(SEEN) {
            self.set(SEEN, true);
            if ind <= 3 && (meta_line(b'-') || meta_line(b'+')) {
                self.nest.reset(at, cx);
                self.set(META, true);
                self.flen = u16::from(t[0]);
                return;
            }
        }
        if self.has(META) {
            #[allow(clippy::cast_possible_truncation, reason = "символ метаданных")]
            if k == 0 && meta_line(self.flen as u8) {
                self.set(META, false);
                self.flen = 0;
                self.set(PREV_BLANK, false);
            }
            return;
        }
        let col0 = ind == 0 && !blank;
        let w = usize::from(self.item_w);
        // Строка вне пункта: закрывает его (кроме ленивого продолжения абзаца).
        let in_item_line = w > 0 && (blank || ind >= w);
        let lazy = w > 0
            && !blank
            && ind < w
            && self.has(PARA)
            && !self.has(PREV_BLANK)
            && !Self::starts_block(t, ind);
        let item_ends = w > 0 && !blank && !in_item_line && !lazy;
        if self.fence != 0 {
            if self.has(IN_ITEM) && item_ends {
                self.close_fence(at, cx);
            } else {
                let n = t.iter().take_while(|&&c| c == self.fence).count();
                let place = if self.has(IN_ITEM) {
                    ind >= w && ind - w < 4
                } else {
                    ind < 4
                };
                if place
                    && n >= usize::from(self.flen)
                    && t[n..].iter().all(u8::is_ascii_whitespace)
                {
                    self.close_fence(at, cx);
                }
                self.set(PREV_BLANK | PARA, false);
                return;
            }
        }
        if self.html != 0 {
            if self.has(IN_ITEM) && item_ends {
                self.html = 0;
                self.set(IN_ITEM, false);
            } else {
                if html_closes(self.html, t) {
                    self.html = 0;
                    self.set(IN_ITEM, false);
                }
                self.set(PREV_BLANK | PARA, false);
                return;
            }
        }
        if self.has(HTML67) {
            if blank {
                self.set(HTML67 | IN_ITEM, false);
            } else if !(self.has(IN_ITEM) && item_ends) {
                self.set(PREV_BLANK | PARA, false);
                return;
            } else {
                self.set(HTML67 | IN_ITEM, false);
            }
        }
        if item_ends {
            self.item_w = 0;
        }
        let bare = self.has(BARE);
        self.set(BARE, false);
        if blank {
            if bare {
                self.item_w = 0;
            }
            self.set(PREV_BLANK, true);
            self.set(PARA, false);
            return;
        }
        let w = usize::from(self.item_w);
        // Безопасная строка.
        if col0 {
            let h = heading(t).is_some();
            let lm = list_marker(t);
            let bullet = lm.is_some_and(|(_, ord, _)| !ord)
                && !thematic(t)
                && t[1..].iter().any(|c| !c.is_ascii_whitespace());
            let one = lm.is_some_and(|(_, ord, one)| ord && one);
            let meta = t.starts_with(b"---") || t.starts_with(b"+++");
            if h || bullet || one || (self.has(PREV_BLANK) && !meta) {
                cx.out.line(start, 0, LineKind::Safe);
            }
        }
        let prev_blank = self.has(PREV_BLANK);
        self.set(PREV_BLANK, false);
        // Пункт списка верхнего уровня (отступ 0–3, вне пункта или левее содержимого).
        let mut rest = t;
        let mut rind = ind;
        let mut item_new = false;
        if ind < 4
            && (w == 0 || ind < w)
            && !thematic(t)
            && let Some((mw, ord, one)) = list_marker(t)
        {
            // Порядковый не с 1 и пустой пункт абзац не прерывают (`-` под ним — Setext).
            let empty = t[mw..].iter().all(u8::is_ascii_whitespace);
            let kept = w == 0 && (self.nest.under() || (empty && para_before && self.nest.para()));
            let interrupts = (!ord || one || prev_blank || !self.has(PARA) || w > 0) && !kept;
            if interrupts {
                let mut sp = 0;
                while mw + sp < t.len() && t[mw + sp] == b' ' && sp < 5 {
                    sp += 1;
                }
                let sp = if sp == 0 || sp > 4 || mw + sp >= t.len() {
                    1
                } else {
                    sp
                };
                self.item_w = u8::try_from(ind + mw + sp).unwrap_or(u8::MAX);
                rest = &t[(mw + sp).min(t.len())..];
                rind = ind + mw + sp;
                item_new = true;
                self.set(BARE, empty);
            }
        }
        let w = usize::from(self.item_w);
        let inside = w > 0 && rind >= w;
        let rel = if inside { rind - w } else { rind };
        // Ограждение и HTML: верхнего уровня (отступ < 4) или в пункте (отступ ≥ w).
        if rel < 4 {
            let rs = rest.iter().take_while(|&&c| c == b' ').count();
            let r = &rest[rs..];
            let rel = rel + rs;
            if rel < 4 {
                if let Some((c, n)) = fence_open(r) {
                    self.fence = c;
                    self.flen = u16::try_from(n).unwrap_or(u16::MAX);
                    self.set(IN_ITEM, inside);
                    if !inside && ind < 4 {
                        self.nest.reset(at, cx);
                        cx.push(at, false);
                        self.set(FENCE_FOLD, true);
                    }
                    self.set(PARA, false);
                    return;
                }
                let mut hk = html_kind(r);
                // Тип 7 абзац не прерывает (строка — его продолжение).
                if hk == 7 && self.has(PARA) && para_before && !item_new {
                    hk = 0;
                }
                if !inside && hk != 0 {
                    self.nest.reset(at, cx);
                }
                if hk >= 6 {
                    self.set(HTML67, true);
                    self.set(IN_ITEM, inside);
                    self.set(PARA, false);
                    return;
                }
                if hk != 0 {
                    if !html_closes(hk, r) {
                        self.html = hk;
                        self.set(IN_ITEM, inside);
                    }
                    self.set(PARA, false);
                    return;
                }
            }
        }
        // Разделы (свёртка): заголовок ATX верхнего уровня.
        if ind < 4
            && w == 0
            && let Some(h) = heading(t)
        {
            self.nest.reset(at, cx);
            let keep = (1u64 << h) - 1;
            let mut closing = self.secs & !keep;
            while closing != 0 {
                let top = closing.ilog2();
                cx.pop(at);
                closing &= !(1 << top);
            }
            self.secs &= keep;
            cx.push(at, false);
            self.secs |= 1 << h;
            self.set(PARA, false);
            return;
        }
        // Отступ кода абзац не начинает.
        let code = rel >= 4 && !self.has(PARA);
        let para = !thematic(t) && heading(t).is_none() && !self.nest.under() && !code;
        self.set(PARA, para);
    }

    /// Строка с отступом `ind` начинает блок (не ленивое продолжение).
    fn starts_block(t: &[u8], ind: usize) -> bool {
        if ind >= 4 {
            return false;
        }
        if heading(t).is_some() || thematic(t) || fence_open(t).is_some() {
            return true;
        }
        if t.first() == Some(&b'>') {
            return true;
        }
        if let Some((_, ord, one)) = list_marker(t) {
            return !ord || one;
        }
        !matches!(html_kind(t), 0 | 7)
    }
}

impl Lex for Md {
    fn state(&self) -> St {
        St {
            mode: self.fence,
            a: self.flags,
            b: self.flen,
            c: u32::from(self.html) | u32::from(self.item_w) << 8 | self.nest.pack().1,
            x: self.secs | self.nest.pack().0,
            ..St::default()
        }
    }

    fn can_mark(&self) -> bool {
        self.line.is_empty() && !self.over
    }

    fn feed(&mut self, b: &[u8], m: Map<'_>, cx: &mut Cx<'_>) {
        let mut i = 0;
        while i < b.len() {
            let j = memchr::memchr(b'\n', &b[i..]).map_or(b.len(), |k| i + k);
            let piece = &b[i..j];
            if !self.over {
                let room = CAP - self.line.len();
                let take = piece.len().min(room);
                self.line.extend_from_slice(&piece[..take]);
                self.raws.extend((i..i + take).map(|k| m.raw(k)));
                if take < piece.len() {
                    self.over = true;
                    self.classify(cx);
                }
            }
            if j < b.len() {
                if !self.done {
                    if self.raws.is_empty() {
                        self.raws.push(m.raw(j));
                    }
                    self.classify(cx);
                }
                self.line.clear();
                self.raws.clear();
                self.over = false;
                self.done = false;
                i = j + 1;
            } else {
                i = j;
            }
        }
    }

    fn flush(&mut self, end: u64, eof: bool, cx: &mut Cx<'_>) {
        if eof {
            if !self.done && !self.line.is_empty() {
                self.classify(cx);
            }
            self.line.clear();
            self.raws.clear();
            self.nest.reset(end, cx);
            if self.has(FENCE_FOLD) {
                cx.pop(end);
                self.set(FENCE_FOLD, false);
            }
            while self.secs != 0 {
                let top = self.secs.ilog2();
                cx.pop(end);
                self.secs &= !(1 << top);
            }
        }
    }
}
