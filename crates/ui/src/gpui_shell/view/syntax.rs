//! Тема подсветки (S7 §5.7, S7-21): роли отрезков tree-sitter (`urfin_view::syntax::Tag`)
//! → цвета `[syntax]` темы (docs/THEMES.md). Разбор и грамматики — `urfin_view::syntax`
//! в фоне; здесь только цвет. В высокой контрастности роли `[syntax]` = текст окна.

use gpui_kit::Hsla;
use urfin_view::syntax::Tag;

use crate::gpui_shell::theme::{Theme, hsla};

/// Цвета ролей подсветки текущей темы.
#[derive(Clone, Copy, Debug)]
pub(super) struct SynPalette {
    keyword: Hsla,
    string: Hsla,
    number: Hsla,
    comment: Hsla,
}

impl SynPalette {
    pub(super) fn of(theme: &Theme) -> SynPalette {
        let s = &theme.tokens.theme.syntax;
        SynPalette {
            keyword: hsla(s.keyword),
            string: hsla(s.string),
            number: hsla(s.number),
            comment: hsla(s.comment),
        }
    }

    /// Цвет роли; `Plain` — `None` (цвет текста по месту).
    pub(super) fn color(&self, tag: Tag) -> Option<Hsla> {
        match tag {
            Tag::Plain => None,
            Tag::Keyword => Some(self.keyword),
            Tag::String => Some(self.string),
            Tag::Number => Some(self.number),
            Tag::Comment => Some(self.comment),
        }
    }
}
