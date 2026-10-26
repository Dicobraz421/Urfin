//! Квотирование и разбор слов по правилам shell (S6 §6.3, S6-17).
//!
//! - cmd: `"…"`, если есть пробел или `&()[]{}^=;!'+,~`; `"` в имени файла не бывает.
//! - pwsh: `'…'` с удвоением `'`.
//! - posix (bash, zsh, fish, WSL): `'…'`, а `'` внутри — `'\''`.
//!
//! [`words`] — обратное: строка → слова с позициями (для комплита и roundtrip-теста).

use crate::shells::ShellKind;

/// Правила кавычек.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Cmd,
    Pwsh,
    Posix,
}

impl Style {
    pub fn of(kind: &ShellKind) -> Style {
        match kind {
            ShellKind::Cmd | ShellKind::Other => Style::Cmd,
            ShellKind::Pwsh | ShellKind::WinPowerShell => Style::Pwsh,
            _ => Style::Posix,
        }
    }
}

/// Нужны ли кавычки.
fn needs(s: &str, style: Style) -> bool {
    if s.is_empty() {
        return true;
    }
    let special: &str = match style {
        Style::Cmd => "&()[]{}^=;!'+,~%`",
        Style::Pwsh => "&()[]{}^=;!'+,~`$@#|<>\"",
        Style::Posix => "&()[]{}^=;!'+,~`$#|<>\"\\*?",
    };
    s.chars().any(|c| c.is_whitespace() || special.contains(c))
}

/// Слово в кавычках по правилам shell, только если нужно.
pub fn quote(s: &str, style: Style) -> String {
    if !needs(s, style) {
        return s.to_owned();
    }
    match style {
        Style::Cmd => format!("\"{}\"", s.replace('"', "")),
        Style::Pwsh => format!("'{}'", s.replace('\'', "''")),
        Style::Posix => format!("'{}'", s.replace('\'', r"'\''")),
    }
}

/// Слово строки: байтовые границы в исходной строке и текст без кавычек.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Разбить строку на слова (пробелы вне кавычек — разделители).
pub fn words(line: &str, style: Style) -> Vec<Word> {
    let mut out = Vec::new();
    let mut cur: Option<Word> = None;
    let mut quote: Option<char> = None;
    let mut it = line.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if quote.is_none() && c.is_whitespace() {
            if let Some(mut w) = cur.take() {
                w.end = i;
                out.push(w);
            }
            continue;
        }
        let w = cur.get_or_insert_with(|| Word {
            start: i,
            end: i,
            text: String::new(),
        });
        match (quote, c, style) {
            (None, '\'', Style::Pwsh | Style::Posix) | (None, '"', _) => quote = Some(c),
            (Some('\''), '\'', Style::Pwsh) if it.peek().is_some_and(|(_, n)| *n == '\'') => {
                it.next();
                w.text.push('\'');
            }
            (Some(q), c, _) if c == q => quote = None,
            // Экранирование вне кавычек: `\` у posix, `^` у cmd.
            (None, '\\', Style::Posix) | (None, '^', Style::Cmd) => {
                if let Some((_, n)) = it.next() {
                    w.text.push(n);
                }
            }
            (_, c, _) => w.text.push(c),
        }
    }
    if let Some(mut w) = cur {
        w.end = line.len();
        out.push(w);
    }
    out
}

/// Слово под курсором (или пустое у курсора, если он на пробеле) и номер слова.
pub fn word_at(line: &str, cursor: usize, style: Style) -> (usize, Word) {
    let ws = words(&line[..cursor.min(line.len())], style);
    match ws.last() {
        // Курсор сразу за словом — оно; после пробела — новое пустое.
        Some(w) if w.end == cursor => (ws.len() - 1, w.clone()),
        _ => (
            ws.len(),
            Word {
                start: cursor,
                end: cursor,
                text: String::new(),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Приёмка S6-17: `C:\Program Files` в pwsh и cmd.
    #[test]
    fn program_files() {
        assert_eq!(
            quote(r"C:\Program Files", Style::Pwsh),
            r"'C:\Program Files'"
        );
        assert_eq!(
            quote(r"C:\Program Files", Style::Cmd),
            r#""C:\Program Files""#
        );
        assert_eq!(quote(r"C:\Windows", Style::Cmd), r"C:\Windows");
        assert_eq!(quote("it's", Style::Pwsh), "'it''s'");
        assert_eq!(quote("it's", Style::Posix), r"'it'\''s'");
        assert_eq!(quote("a&b", Style::Cmd), "\"a&b\"");
    }

    #[test]
    fn word_under_cursor() {
        let line = r"cd 'C:\Program Files'\Com";
        let (n, w) = word_at(line, line.len(), Style::Pwsh);
        assert_eq!(n, 1);
        assert_eq!(w.text, r"C:\Program Files\Com");
        assert_eq!(&line[w.start..w.end], r"'C:\Program Files'\Com");
        let (n, w) = word_at("dir ", 4, Style::Cmd);
        assert_eq!((n, w.text.as_str(), w.start), (1, "", 4));
        let (n, w) = word_at("no", 2, Style::Cmd);
        assert_eq!((n, w.text.as_str()), (0, "no"));
    }

    fn name() -> impl Strategy<Value = String> {
        "[a-zA-Zа-я0-9 '&()^!;,.~_$\\\\-]{0,12}"
    }

    proptest! {
        /// Roundtrip: квотированное имя разбирается shell-правилами обратно в одно
        /// слово с тем же текстом.
        #[test]
        fn quote_round_trip(s in name(), style in prop_oneof![
            Just(Style::Cmd), Just(Style::Pwsh), Just(Style::Posix)
        ]) {
            // У cmd `^` — экранирование вне кавычек; в кавычки оно попадает целиком.
            let q = quote(&s, style);
            let ws = words(&q, style);
            prop_assert_eq!(ws.len(), 1, "{:?} → {:?}", s, q);
            prop_assert_eq!(&ws[0].text, &s);
            prop_assert_eq!((ws[0].start, ws[0].end), (0, q.len()));
        }
    }
}
