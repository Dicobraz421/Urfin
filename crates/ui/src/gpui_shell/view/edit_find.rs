//! Поиск, замена и переход в редакторе (S7-18, S7-30, S7 §5.2): строки «Найти» /
//! «Заменить на» и «Перейти к строке» — [`FindBar`] над статусом, пока фокус Core —
//! `Focus::Overlay(Find)`. Ищет и заменяет движок документа: строка шлёт
//! `editor.search text=…`, `editor.replace text=… with=… [all]`, переход —
//! `editor.goto to=…`; совпадение становится выделением, «N из M», подсветка и метки —
//! из `EditorDoc::search`, прогресс «Заменить все» — из `EditorDoc::replace`.

use urfin_core::Command;
use urfin_view::EditorDoc;
use urfin_view::search::Pattern;

use super::find::{BarAction, BarMode, FindBar, Flags};

/// Поиск и переход фичи `Editor`.
#[derive(Default)]
pub(super) struct Find {
    pub bar: Option<FindBar>,
    /// Образец и флажки последнего поиска (F7 снова, Shift+F7 после закрытия строки).
    pub last: Option<(String, Flags)>,
    pub last_repl: String,
    /// Сцена `--screens`: тексты новой строки (образец и «Заменить на»).
    pub demo: Option<(String, Option<String>)>,
}

/// `kind` образца по флажкам.
fn kind(f: Flags) -> &'static str {
    if f.regex { "regex" } else { "text" }
}

/// `editor.search` / `editor.replace` с образцом.
fn with_pattern(id: &'static str, text: &str, f: Flags, back: bool) -> Command {
    let mut c = Command::new(id)
        .arg("text", text)
        .arg("kind", kind(f))
        .arg("case", f.case);
    if !f.regex {
        c = c.arg("word", f.word);
    }
    if back {
        c = c.arg("back", true);
    }
    c
}

/// Поиск образца: тот же, что у документа, — следующее совпадение, иначе новый поиск.
pub(super) fn search_cmd(d: &EditorDoc, text: &str, f: Flags, back: bool) -> Command {
    let same = d.search.as_ref().is_some_and(|s| {
        s.st.error.is_none()
            && super::pattern_of(text, f).as_ref() == Some(&s.st.req.pat)
            && s.st.hit.is_some()
    });
    if same {
        again(back)
    } else {
        with_pattern("editor.search", text, f, back)
    }
}

fn again(back: bool) -> Command {
    Command::new(if back {
        "editor.search_prev"
    } else {
        "editor.search_next"
    })
}

/// Текст выделения документа для «Найти»: одна строка, не длиннее 256 символов.
pub(super) fn selected(d: &EditorDoc) -> Option<String> {
    let e = d.engine.as_deref()?;
    let r = e.sel.stream()?;
    if r.end - r.start > 1024 {
        return None;
    }
    let bytes = e.bytes(r)?;
    let t = e.enc.enc.decode_without_bom_handling(&bytes).0.into_owned();
    (!t.contains(['\n', '\r']) && t.chars().count() <= 256).then_some(t)
}

impl Find {
    /// Строка документа `id` открыта (фокус Core — у неё).
    pub(super) fn bar_for(&self, id: u32, find_focus: bool) -> Option<&FindBar> {
        self.bar.as_ref().filter(|b| find_focus && b.owner == id)
    }

    /// F7 / Ctrl+F7 / Alt+F8: открыть строку (или сменить ряд открытой). Текст «Найти» —
    /// сцена, выделение в одну строку, иначе прошлый образец; непустой — сразу поиск.
    pub(super) fn open(&mut self, d: &EditorDoc, mode: BarMode, replace: bool) -> Option<Command> {
        let demo = self.demo.take();
        let mut flags = self.last.as_ref().map(|l| l.1).unwrap_or_default();
        let (find_init, repl_init) = if let Some((text, repl)) = &demo {
            if repl.is_some() {
                flags.regex = true;
            }
            (text.clone(), repl.clone().unwrap_or_default())
        } else {
            (
                selected(d)
                    .or_else(|| self.last.as_ref().map(|l| l.0.clone()))
                    .unwrap_or_default(),
                self.last_repl.clone(),
            )
        };
        let is_demo = demo.is_some();
        let init = match mode {
            BarMode::Find => find_init.clone(),
            BarMode::Goto => demo.map(|d| d.0).unwrap_or_default(),
        };
        let id = d.id.0;
        // Открытая строка: другой ряд — с новым текстом; тот же — как есть.
        let fresh = if let Some(b) = self.bar.as_mut().filter(|b| b.owner == id) {
            let switch = b.mode != mode || is_demo;
            if switch {
                b.switch(mode, init);
            }
            if is_demo {
                b.flags = flags;
            }
            switch
        } else {
            self.bar = Some(FindBar::editor(mode, id, init, flags, None));
            true
        };
        if replace && let Some(b) = self.bar.as_mut() {
            // Фокус — «Найти», как на макете; Tab — к замене.
            b.open_replace(repl_init, false);
        }
        let b = self.bar.as_ref()?;
        (fresh && mode == BarMode::Find && !find_init.is_empty()).then(|| {
            self.last = Some((find_init.clone(), b.flags));
            with_pattern("editor.search", &find_init, b.flags, false)
        })
    }

    /// Действие строки → команды Core (окно — для фокуса полей, у вызывающего).
    pub(super) fn action(&mut self, d: &EditorDoc, act: BarAction) -> Vec<Command> {
        let Some(bar) = self.bar.as_mut() else {
            return Vec::new();
        };
        match act {
            BarAction::Close => vec![Command::new("editor.search_cancel")],
            BarAction::Swallow | BarAction::Tab => Vec::new(),
            BarAction::Toggle(f) => {
                bar.flags.toggle(f);
                Vec::new()
            }
            BarAction::OpenReplace => {
                if bar.mode != BarMode::Find {
                    let init = self.last.as_ref().map(|l| l.0.clone());
                    bar.switch(BarMode::Find, init.unwrap_or_default());
                }
                bar.open_replace(self.last_repl.clone(), true);
                Vec::new()
            }
            BarAction::Switch(mode) => {
                let init = match mode {
                    BarMode::Find => self.last.as_ref().map(|l| l.0.clone()),
                    BarMode::Goto => None,
                };
                bar.switch(mode, init.unwrap_or_default());
                Vec::new()
            }
            BarAction::Again { back } => match &self.last {
                Some((text, f)) => vec![search_cmd(d, text, *f, back)],
                None => Vec::new(),
            },
            BarAction::Commit { back } => {
                let text = bar.text();
                match bar.mode {
                    BarMode::Find => {
                        if text.is_empty() {
                            return Vec::new();
                        }
                        let f = bar.flags;
                        self.last = Some((text.clone(), f));
                        vec![search_cmd(d, &text, f, back)]
                    }
                    BarMode::Goto => {
                        if urfin_view::goto::parse(&text).is_err() {
                            return Vec::new();
                        }
                        vec![Command::new("editor.goto").arg("to", text.trim())]
                    }
                }
            }
            BarAction::Replace { all } => self.replace(all),
        }
    }

    /// «Заменить» (выделенное совпадение — и к следующему) или «Заменить все».
    pub(super) fn replace(&mut self, all: bool) -> Vec<Command> {
        let Some(bar) = self.bar.as_ref() else {
            return Vec::new();
        };
        let text = bar.text();
        if text.is_empty() {
            return Vec::new();
        }
        let (f, with) = (bar.flags, bar.repl_text());
        self.last = Some((text.clone(), f));
        self.last_repl.clone_from(&with);
        let mut c = with_pattern("editor.replace", &text, f, false).arg("with", with);
        if all {
            c = c.arg("all", true);
        }
        vec![c]
    }

    /// Строка закрылась: образец остаётся для F7 и Shift+F7.
    pub(super) fn closed(&mut self) {
        if let Some(b) = self.bar.take()
            && b.mode == BarMode::Find
            && !b.text().is_empty()
        {
            self.last = Some((b.text(), b.flags));
        }
    }
}

/// Образец документа текстом и флажками (F7 после закрытия строки).
pub(super) fn last_of(d: &EditorDoc) -> Option<(String, Flags)> {
    let s = d.search.as_ref()?;
    let text = match &s.st.req.pat {
        Pattern::Text { s, .. } | Pattern::Regex { s, .. } => s.clone(),
        Pattern::Hex(_) => return None,
    };
    Some((text, super::flags_of(&s.st.req.pat)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_commands() {
        let f = Flags {
            case: true,
            ..Flags::default()
        };
        let c = with_pattern("editor.search", "ab", f, true);
        assert_eq!(c.args.str("text"), Some("ab"));
        assert_eq!(c.args.str("kind"), Some("text"));
        assert_eq!(c.args.bool("case"), Some(true));
        assert_eq!(c.args.bool("back"), Some(true));
        let f = Flags {
            regex: true,
            ..Flags::default()
        };
        let c = with_pattern("editor.replace", "a+", f, false);
        assert_eq!(c.args.str("kind"), Some("regex"));
        assert_eq!(c.args.bool("word"), None);
    }
}
