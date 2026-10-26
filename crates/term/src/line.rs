//! Редактор командной строки (S6 §6, S6-15): буфер, курсор, undo/redo с группировкой
//! по словам, переходы по словам с учётом кавычек. Курсор — индекс байта на границе
//! символа. Чистая модель: команды `cmdline.*` правят её из handler.

/// Вид правки для группировки undo: подряд идущие правки одного вида (набор слова,
/// удаление символов) — один шаг.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Type,
    Delete,
    Other,
}

/// Строка ввода.
#[derive(Clone, Debug, Default)]
pub struct LineEditor {
    text: String,
    cursor: usize,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// Вид последней правки (`None` — после перемещения курсора или undo).
    last: Option<Edit>,
}

/// Сколько шагов undo помнить.
const UNDO_CAP: usize = 200;

impl LineEditor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Снимок до правки вида `kind`; подряд одного вида — без нового снимка.
    fn checkpoint(&mut self, kind: Edit) {
        if self.last != Some(kind) || kind == Edit::Other {
            self.undo.push((self.text.clone(), self.cursor));
            if self.undo.len() > UNDO_CAP {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = Some(kind);
    }

    fn moved(&mut self) {
        self.last = None;
    }

    /// Вставить текст у курсора. Слово набирается одним шагом undo; пробел, вставка
    /// нескольких символов — граница шага.
    pub fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let one = s.chars().count() == 1;
        let word = one && !s.chars().all(char::is_whitespace);
        self.checkpoint(if word { Edit::Type } else { Edit::Other });
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        if !word {
            // Пробел закрывает слово: следующий символ — новый шаг.
            self.last = None;
        }
    }

    /// Заменить всю строку (история, вставка из списка), курсор в конец.
    pub fn set(&mut self, s: &str) {
        if s == self.text {
            return;
        }
        self.checkpoint(Edit::Other);
        s.clone_into(&mut self.text);
        self.cursor = self.text.len();
    }

    /// Заменить строку и поставить курсор (комплит: курсор — за вставленным).
    pub fn set_at(&mut self, s: &str, cursor: usize) {
        if s != self.text {
            self.checkpoint(Edit::Other);
            s.clone_into(&mut self.text);
        }
        let mut c = cursor.min(self.text.len());
        while !self.text.is_char_boundary(c) {
            c -= 1;
        }
        self.cursor = c;
        self.last = None;
    }

    /// Забрать строку (submit): буфер пуст, история правок сброшена.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.undo.clear();
        self.redo.clear();
        self.last = None;
        std::mem::take(&mut self.text)
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i]
            .char_indices()
            .next_back()
            .map_or(0, |(p, _)| p)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..]
            .chars()
            .next()
            .map_or(self.text.len(), |c| i + c.len_utf8())
    }

    pub fn left(&mut self) {
        self.cursor = self.prev(self.cursor);
        self.moved();
    }

    pub fn right(&mut self) {
        self.cursor = self.next(self.cursor);
        self.moved();
    }

    pub fn home(&mut self) {
        self.cursor = 0;
        self.moved();
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
        self.moved();
    }

    /// Начало слова слева от `i`: пробелы пропускаются, слово в кавычках — целиком.
    fn word_start(&self, i: usize) -> usize {
        let chars: Vec<(usize, char)> = self.text[..i].char_indices().collect();
        let mut k = chars.len();
        while k > 0 && chars[k - 1].1.is_whitespace() {
            k -= 1;
        }
        if k == 0 {
            return 0;
        }
        if chars[k - 1].1 == '"' {
            // Закрывающая кавычка: до открывающей.
            k -= 1;
            while k > 0 && chars[k - 1].1 != '"' {
                k -= 1;
            }
            return chars.get(k.saturating_sub(1)).map_or(0, |c| c.0);
        }
        while k > 0 && !chars[k - 1].1.is_whitespace() && chars[k - 1].1 != '"' {
            k -= 1;
        }
        chars.get(k).map_or(i, |c| c.0)
    }

    /// Конец слова справа от `i`: пробелы пропускаются, слово в кавычках — целиком.
    fn word_end(&self, i: usize) -> usize {
        let mut it = self.text[i..]
            .char_indices()
            .map(|(p, c)| (i + p, c))
            .peekable();
        while it.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        match it.peek().copied() {
            None => self.text.len(),
            Some((_, '"')) => {
                it.next();
                for (p, c) in it {
                    if c == '"' {
                        return p + 1;
                    }
                }
                self.text.len()
            }
            Some(_) => {
                for (p, c) in it {
                    if c.is_whitespace() || c == '"' {
                        return p;
                    }
                }
                self.text.len()
            }
        }
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start(self.cursor);
        self.moved();
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end(self.cursor);
        self.moved();
    }

    fn remove(&mut self, from: usize, to: usize, kind: Edit) {
        if from == to {
            return;
        }
        self.checkpoint(kind);
        self.text.replace_range(from..to, "");
        self.cursor = from;
    }

    pub fn backspace(&mut self) {
        let p = self.prev(self.cursor);
        self.remove(p, self.cursor, Edit::Delete);
    }

    pub fn delete(&mut self) {
        let n = self.next(self.cursor);
        self.remove(self.cursor, n, Edit::Delete);
    }

    pub fn delete_word_left(&mut self) {
        let p = self.word_start(self.cursor);
        self.remove(p, self.cursor, Edit::Other);
    }

    pub fn delete_word_right(&mut self) {
        let n = self.word_end(self.cursor);
        self.remove(self.cursor, n, Edit::Other);
    }

    pub fn delete_to_end(&mut self) {
        self.remove(self.cursor, self.text.len(), Edit::Other);
    }

    pub fn clear(&mut self) {
        self.remove(0, self.text.len(), Edit::Other);
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.redo
                .push((std::mem::replace(&mut self.text, t), self.cursor));
            self.cursor = c;
        }
        self.last = None;
    }

    pub fn redo(&mut self) {
        if let Some((t, c)) = self.redo.pop() {
            self.undo
                .push((std::mem::replace(&mut self.text, t), self.cursor));
            self.cursor = c;
        }
        self.last = None;
    }
}

/// Имя для командной строки: в кавычках, если в нём пробел или спецсимвол shell
/// (`&()[]{}^=;!'+,~` — как у cmd и pwsh).
pub fn quote(name: &str) -> String {
    let special = |c: char| c.is_whitespace() || "&()[]{}^=;!'+,`~$@#%".contains(c);
    if name.chars().any(special) && !name.starts_with('"') {
        format!("\"{name}\"")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn ed(text: &str, cursor: usize) -> LineEditor {
        let mut e = LineEditor::default();
        e.insert(text);
        e.cursor = cursor;
        e.last = None;
        e
    }

    #[test]
    fn words_respect_quotes() {
        let s = r#"copy "My Documents\a b.txt" D:\x"#;
        let mut e = ed(s, s.len());
        e.word_left();
        assert_eq!(&s[e.cursor()..], r"D:\x");
        e.word_left();
        assert_eq!(&s[e.cursor()..], r#""My Documents\a b.txt" D:\x"#);
        e.word_left();
        assert_eq!(e.cursor(), 0);
        e.word_right();
        assert_eq!(&s[..e.cursor()], "copy");
        e.word_right();
        assert_eq!(&s[..e.cursor()], r#"copy "My Documents\a b.txt""#);
        e.delete_word_left();
        assert_eq!(e.text(), r"copy  D:\x");
    }

    #[test]
    fn undo_groups_by_word() {
        let mut e = LineEditor::default();
        for c in "dir /b".chars() {
            e.insert(&c.to_string());
        }
        e.undo();
        assert_eq!(e.text(), "dir ");
        e.undo();
        assert_eq!(e.text(), "dir");
        e.undo();
        assert_eq!(e.text(), "");
        e.redo();
        e.redo();
        assert_eq!(e.text(), "dir ");
        e.backspace();
        e.backspace();
        e.undo();
        assert_eq!(e.text(), "dir ");
        // Новая правка после undo сбрасывает redo.
        e.undo();
        e.insert("x");
        e.redo();
        assert_eq!(e.text(), "dirx");
    }

    #[test]
    fn cyrillic_cursor_on_char_boundaries() {
        let mut e = ed("файл", 0);
        e.end();
        e.backspace();
        e.left();
        e.insert("ё");
        assert_eq!(e.text(), "фаёй");
        e.home();
        e.delete();
        assert_eq!(e.text(), "аёй");
        assert_eq!(quote("Мои документы"), "\"Мои документы\"");
        assert_eq!(quote("a.txt"), "a.txt");
    }

    #[derive(Clone, Debug)]
    enum Op {
        Insert(String),
        Left,
        Right,
        Home,
        End,
        Backspace,
        Delete,
        WordLeft,
        WordRight,
        DelWordLeft,
        DelToEnd,
        Undo,
        Redo,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => "[a-zа-я \"]{1,3}".prop_map(Op::Insert),
            1 => Just(Op::Left),
            1 => Just(Op::Right),
            1 => Just(Op::Home),
            1 => Just(Op::End),
            1 => Just(Op::Backspace),
            1 => Just(Op::Delete),
            1 => Just(Op::WordLeft),
            1 => Just(Op::WordRight),
            1 => Just(Op::DelWordLeft),
            1 => Just(Op::DelToEnd),
            1 => Just(Op::Undo),
            1 => Just(Op::Redo),
        ]
    }

    proptest! {
        /// Модель: строка символов и позиция. Курсор всегда на границе символа и в
        /// пределах строки; простые правки совпадают с моделью; undo до упора даёт
        /// пустую строку.
        #[test]
        fn editor_matches_model(ops in proptest::collection::vec(op(), 0..60)) {
            let mut e = LineEditor::default();
            let mut model: Vec<char> = Vec::new();
            let mut pos = 0usize;
            let mut exact = true;
            for o in &ops {
                match o {
                    Op::Insert(s) => {
                        e.insert(s);
                        for (k, c) in s.chars().enumerate() {
                            model.insert(pos + k, c);
                        }
                        pos += s.chars().count();
                    }
                    Op::Left => { e.left(); pos = pos.saturating_sub(1); }
                    Op::Right => { e.right(); pos = (pos + 1).min(model.len()); }
                    Op::Home => { e.home(); pos = 0; }
                    Op::End => { e.end(); pos = model.len(); }
                    Op::Backspace => {
                        e.backspace();
                        if pos > 0 { pos -= 1; model.remove(pos); }
                    }
                    Op::Delete => {
                        e.delete();
                        if pos < model.len() { model.remove(pos); }
                    }
                    Op::WordLeft => { e.word_left(); exact = false; }
                    Op::WordRight => { e.word_right(); exact = false; }
                    Op::DelWordLeft => { e.delete_word_left(); exact = false; }
                    Op::DelToEnd => { e.delete_to_end(); exact = false; }
                    Op::Undo => { e.undo(); exact = false; }
                    Op::Redo => { e.redo(); exact = false; }
                }
                prop_assert!(e.text().is_char_boundary(e.cursor()));
                prop_assert!(e.cursor() <= e.text().len());
                if exact {
                    prop_assert_eq!(e.text(), model.iter().collect::<String>());
                    prop_assert_eq!(e.text()[..e.cursor()].chars().count(), pos);
                }
            }
            while !e.undo.is_empty() {
                e.undo();
            }
            prop_assert_eq!(e.text(), "");
        }
    }
}
