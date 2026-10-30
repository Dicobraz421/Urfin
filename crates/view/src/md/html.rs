//! Маленький токенайзер HTML для событий `Html` / `InlineHtml` pulldown-cmark (S7 §3.10,
//! без html5ever): теги с атрибутами (имя и атрибуты без учёта регистра, значения в
//! кавычках или без), комментарии и текст. Битый тег (нет `>`, имя не с буквы) — текст.

use compact_str::CompactString;

/// Теги набора (решение §13, вопрос 3); прочие — текстом.
pub const KNOWN: &[&str] = &[
    "br", "img", "details", "summary", "p", "div", "kbd", "sub", "sup",
];

/// Тег.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlTag<'a> {
    /// Имя в нижнем регистре (`!doctype`, `?xml` — с первым знаком).
    pub name: CompactString,
    pub close: bool,
    pub self_close: bool,
    /// Атрибуты: имя в нижнем регистре, значение с раскрытыми сущностями.
    pub attrs: Vec<(CompactString, String)>,
    /// Тег как в тексте.
    pub raw: &'a str,
    /// Смещение `<` в разобранной строке.
    pub off: usize,
}

impl HtmlTag<'_> {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Тег из набора.
    pub fn known(&self) -> bool {
        KNOWN.contains(&self.name.as_str())
    }
}

/// Кусок HTML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok<'a> {
    /// Текст как есть (сущности не раскрыты) и его смещение.
    Text(&'a str, usize),
    Tag(HtmlTag<'a>),
    /// `<!-- … -->` — скрыт, как на GitHub.
    Comment,
}

/// Разбор строки на куски.
pub fn tokenize(s: &str) -> Vec<Tok<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut text = 0;
    let mut i = 0;
    while let Some(k) = memchr::memchr(b'<', &b[i..]) {
        let at = i + k;
        let got = if s[at..].starts_with("<!--") {
            let end = s[at + 4..].find("-->").map_or(s.len(), |e| at + 4 + e + 3);
            Some((Tok::Comment, end))
        } else {
            tag(s, at).map(|(t, end)| (Tok::Tag(t), end))
        };
        match got {
            Some((t, end)) => {
                if text < at {
                    out.push(Tok::Text(&s[text..at], text));
                }
                out.push(t);
                i = end;
                text = end;
            }
            None => i = at + 1,
        }
    }
    if text < s.len() {
        out.push(Tok::Text(&s[text..], text));
    }
    out
}

/// Один тег, начинающийся с `<` в `at`; `None` — это не тег.
pub fn tag(s: &str, at: usize) -> Option<(HtmlTag<'_>, usize)> {
    let b = s.as_bytes();
    let mut i = at + 1;
    let close = b.get(i) == Some(&b'/');
    if close {
        i += 1;
    }
    let first = *b.get(i)?;
    if !close && (first == b'!' || first == b'?') {
        // `<!DOCTYPE …>`, `<?xml …?>` — тег вне набора.
        let end = at + memchr::memchr(b'>', &b[at..])? + 1;
        let name = s[i..end - 1]
            .split(|c: char| c.is_ascii_whitespace())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        return Some((
            HtmlTag {
                name: name.into(),
                close: false,
                self_close: false,
                attrs: Vec::new(),
                raw: &s[at..end],
                off: at,
            },
            end,
        ));
    }
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let ns = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
        i += 1;
    }
    let name = CompactString::from(s[ns..i].to_ascii_lowercase());
    let mut attrs = Vec::new();
    let mut self_close = false;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match b.get(i)? {
            b'>' => {
                i += 1;
                break;
            }
            b'/' if b.get(i + 1) == Some(&b'>') => {
                self_close = true;
                i += 2;
                break;
            }
            b'/' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        if close {
            // У закрывающего тега атрибутов нет: до `>`.
            let e = memchr::memchr(b'>', &b[i..])?;
            i += e + 1;
            break;
        }
        let (attr, next) = attr(s, i)?;
        attrs.push(attr);
        i = next;
    }
    Some((
        HtmlTag {
            name,
            close,
            self_close,
            attrs,
            raw: &s[at..i],
            off: at,
        },
        i,
    ))
}

/// Атрибут `имя[=значение]` с `at`; `None` — битый. Возвращает его и позицию за ним.
fn attr(s: &str, at: usize) -> Option<((CompactString, String), usize)> {
    let b = s.as_bytes();
    let mut i = at;
    while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'/' | b'>' | b'=') {
        i += 1;
    }
    if at == i {
        // `=` без имени — битый тег.
        return None;
    }
    let name = CompactString::from(s[at..i].to_ascii_lowercase());
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut value = String::new();
    if b.get(i) == Some(&b'=') {
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let quote = *b.get(i)?;
        if quote == b'"' || quote == b'\'' {
            let end = memchr::memchr(quote, &b[i + 1..])?;
            value = entities(&s[i + 1..i + 1 + end]);
            i += end + 2;
        } else {
            let vs = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                i += 1;
            }
            value = entities(&s[vs..i]);
        }
    }
    Some(((name, value), i))
}

/// Раскрывает частые сущности: `&amp; &lt; &gt; &quot; &apos; &nbsp;`, `&#N;`, `&#xH;`.
/// Прочие остаются как есть.
pub fn entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find('&') {
        out.push_str(&rest[..k]);
        rest = &rest[k..];
        let end = rest.bytes().take(12).position(|c| c == b';');
        let ent = end.map(|e| &rest[1..e]);
        let ch = ent.and_then(|e| match e {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => {
                let n = e.strip_prefix('#')?;
                let v = match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok()?,
                    None => n.parse().ok()?,
                };
                char::from_u32(v)
            }
        });
        if let (Some(c), Some(e)) = (ch, end) {
            out.push(c);
            rest = &rest[e + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Размер в px из `width=` / `height=`: `120`, `120px`; проценты и прочее — `None`.
pub fn px(v: &str) -> Option<u32> {
    let v = v.trim();
    let v = v.strip_suffix("px").unwrap_or(v).trim();
    v.parse::<u32>().ok().filter(|n| *n > 0)
}
