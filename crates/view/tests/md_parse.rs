//! Ребро view → pulldown-cmark (S7-00b): разбор с расширениями GFM и смещения
//! в исходнике. Модель документа — S7-22 (S7 §3.10).

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

#[test]
fn gfm_events_carry_source_offsets() {
    let src = "# Урфин\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] ~~готово~~\n";
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES;
    let events: Vec<_> = Parser::new_ext(src, opts).into_offset_iter().collect();

    let heading = events
        .iter()
        .find(|(e, _)| matches!(e, Event::Start(Tag::Heading { .. })))
        .expect("заголовок");
    assert_eq!(&src[heading.1.clone()], "# Урфин\n");
    let has = |f: fn(&Event) -> bool| events.iter().any(|(e, _)| f(e));
    assert!(has(|e| matches!(e, Event::End(TagEnd::Table))));
    assert!(has(|e| matches!(e, Event::TaskListMarker(true))));
    assert!(has(|e| matches!(e, Event::Start(Tag::Strikethrough))));
}
