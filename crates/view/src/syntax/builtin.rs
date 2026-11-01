//! Грамматики в exe (решение 2026-10-30 «M4»): конфиги и разметка. Запросы — свои,
//! `assets/syntax/<язык>/highlights.scm`, по макетам (`screen-quickview.png`).

use std::sync::Arc;

use tree_sitter::Language;

use super::{Grammar, GrammarError};

/// Языки в exe (имена `urfin_view::lang`).
pub(super) const NAMES: &[&str] = &["json", "toml", "yaml", "ini", "xml", "markdown"];

pub(super) fn has(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Вшитая грамматика; `None` — такой в exe нет.
pub(super) fn load(name: &str) -> Option<Result<Grammar, GrammarError>> {
    let (name, lang, query): (&'static str, Language, &str) = match name {
        "json" => (
            "json",
            tree_sitter_json::LANGUAGE.into(),
            include_str!("../../../../assets/syntax/json/highlights.scm"),
        ),
        "toml" => (
            "toml",
            tree_sitter_toml_ng::LANGUAGE.into(),
            include_str!("../../../../assets/syntax/toml/highlights.scm"),
        ),
        "yaml" => (
            "yaml",
            tree_sitter_yaml::LANGUAGE.into(),
            include_str!("../../../../assets/syntax/yaml/highlights.scm"),
        ),
        "ini" => (
            "ini",
            tree_sitter_ini::LANGUAGE.into(),
            include_str!("../../../../assets/syntax/ini/highlights.scm"),
        ),
        "xml" => (
            "xml",
            tree_sitter_xml::LANGUAGE_XML.into(),
            include_str!("../../../../assets/syntax/xml/highlights.scm"),
        ),
        "markdown" => return Some(markdown()),
        _ => return None,
    };
    Some(Grammar::new(name, lang, query))
}

/// Markdown: блочная грамматика и строчная — по диапазонам `inline`.
fn markdown() -> Result<Grammar, GrammarError> {
    let inline = Grammar::new(
        "markdown_inline",
        tree_sitter_md::INLINE_LANGUAGE.into(),
        include_str!("../../../../assets/syntax/markdown_inline/highlights.scm"),
    )?;
    let mut g = Grammar::new(
        "markdown",
        tree_sitter_md::LANGUAGE.into(),
        include_str!("../../../../assets/syntax/markdown/highlights.scm"),
    )?;
    g.inline = Some(Arc::new(inline));
    Ok(g)
}
