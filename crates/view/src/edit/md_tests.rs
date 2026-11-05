//! Приёмка S7-24: команды `editor.md.*` — текст и курсор по §5.9, команда дважды = исходный
//! текст, один undo на команду; контекст кнопок; предпросмотр — разбор после паузы,
//! синхронная прокрутка без эха, флажок задачи.

use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use encoding_rs::UTF_16LE;
use proptest::prelude::*;
use urfin_core::{Command, Core};

use super::*;
use crate::edit::input::tests::{cmd, ed, editor, eng, input, mv, pump_until, send, text};
use crate::edit::preview::{Lead, PAUSE};
use crate::{EditorCfg, InboxMsg};

fn heads() -> [String; 3] {
    ["A".to_owned(), "B".to_owned(), "C".to_owned()]
}

/// Текст и выделение после команды.
fn run(act: &MdAct, t: &str, sel: Range<usize>) -> (String, Range<usize>) {
    let Some(o) = apply(act, t, &sel, "\n", &heads()) else {
        return (t.to_owned(), sel);
    };
    let mut s = t.to_owned();
    for e in o.edits.iter().rev() {
        s.replace_range(e.at.clone(), &e.text);
    }
    (s, o.sel)
}

/// `|` в строке — курсор, выделение — `<`…`>`.
fn marked(s: &str) -> (String, Range<usize>) {
    if let Some(c) = s.find('|') {
        return (s.replacen('|', "", 1), c..c);
    }
    let a = s.find('<').expect("метка");
    let b = s.find('>').expect("метка") - 1;
    (s.replacen('<', "", 1).replacen('>', "", 1), a..b)
}

fn show(t: &str, r: &Range<usize>) -> String {
    let mut s = t.to_owned();
    if r.is_empty() {
        s.insert(r.start, '|');
    } else {
        s.insert(r.end, '>');
        s.insert(r.start, '<');
    }
    s
}

fn check(act: &MdAct, from: &str, to: &str) {
    let (t, sel) = marked(from);
    let (s, r) = run(act, &t, sel);
    assert_eq!(show(&s, &r), to, "{act:?} на {from:?}");
}

#[test]
fn bold_and_italic_by_spec() {
    check(&MdAct::Bold, "a <word> b", "a **<word>** b");
    check(&MdAct::Bold, "a **<word>** b", "a <word> b");
    check(&MdAct::Bold, "a <**word**> b", "a <word> b");
    check(&MdAct::Italic, "<word>", "*<word>*");
    check(&MdAct::Italic, "*<word>*", "<word>");
    check(&MdAct::Bold, "сло|во", "**сло|во**");
    check(&MdAct::Bold, "**сло|во**", "сло|во");
    check(&MdAct::Bold, "a | b", "a **|** b");
    check(&MdAct::Bold, "a **|** b", "a | b");
    check(&MdAct::Italic, "a | b", "a *|* b");
    // Весь жирный отрезок снимается и с курсора в середине.
    check(&MdAct::Bold, "x **two wo|rds** y", "x two wo|rds y");
    // Курсив и жирный вместе — снимается только свой.
    check(&MdAct::Italic, "***<a>***", "**<a>**");
    check(&MdAct::Bold, "***<a>***", "*<a>*");
    // Пробелы по краям выделения — за маркерами.
    check(&MdAct::Bold, "a< word >b", "a **<word>** b");
}

#[test]
fn heading_by_spec() {
    check(&MdAct::Heading(2), "ti|tle", "## ti|tle");
    check(&MdAct::Heading(2), "## ti|tle", "ti|tle");
    check(&MdAct::Heading(1), "### ti|tle", "# ti|tle");
    check(&MdAct::Heading(0), "### ti|tle", "ti|tle");
    check(&MdAct::Heading(3), "  ti|tle", "  ### ti|tle");
    check(&MdAct::Heading(1), "> ti|tle", "> # ti|tle");
    check(&MdAct::Heading(1), "#no|tag", "# #no|tag");
}

#[test]
fn lists_by_spec() {
    check(&MdAct::List, "<a\nb>\n", "<- a\n- b>\n");
    check(&MdAct::List, "<- a\n- b>", "<a\nb>");
    check(&MdAct::List, "<1. a\n2. b>", "<- a\n- b>");
    check(&MdAct::Numbered, "<a\n\nb>", "<1. a\n\n2. b>");
    check(&MdAct::Numbered, "<- a\n- b>", "<1. a\n2. b>");
    check(&MdAct::Task, "<a\n- b>", "<- [ ] a\n- [ ] b>");
    check(&MdAct::Task, "<- [ ] a\n- [x] b>", "<a\nb>");
    check(&MdAct::List, "|", "- |");
    check(&MdAct::List, "- |", "|");
    check(&MdAct::Task, "  - a|", "  - [ ] a|");
    // Строка, перед которой выделение кончается, не задета.
    check(&MdAct::List, "<a\n>b", "<- a\n>b");
}

#[test]
fn link_code_table_by_spec() {
    check(&MdAct::Link, "<text>", "[text](|)");
    check(&MdAct::Link, "<https://a.b/c>", "[|](https://a.b/c)");
    check(&MdAct::Link, "<docs/decisions/>", "[|](docs/decisions/)");
    check(&MdAct::Link, "<README.md>", "[|](README.md)");
    check(&MdAct::Link, "|", "[](|)");
    check(&MdAct::Code, "a <b c> d", "a `<b c>` d");
    check(&MdAct::Code, "a `<b c>` d", "a <b c> d");
    check(&MdAct::Code, "<a`b>", "``<a`b>``");
    check(&MdAct::Code, "<one\ntwo>\n", "```|\none\ntwo\n```\n");
    check(&MdAct::Code, "```\n<one\ntwo>\n```\n", "<one\ntwo>\n");
    check(&MdAct::Code, "<```\none\n```>", "<one>");
    let (s, r) = run(&MdAct::Table, "", 0..0);
    assert_eq!(s, "| A | B | C |\n|---|---|---|\n|   |   |   |");
    assert_eq!(&s[r], "A", "курсор в первой ячейке");
    let (s, r) = run(&MdAct::Table, "text", 4..4);
    assert_eq!(s, "text\n\n| A | B | C |\n|---|---|---|\n|   |   |   |");
    assert_eq!(r.start, 8);
}

#[test]
fn context_of_line() {
    let line = "- [ ] **Markdown как документ** — [решение](docs/decisions/)";
    let c = context(line, 16);
    assert!(c.bold && c.task && !c.italic && !c.code && !c.list);
    assert!(!context(line, 0).bold);
    assert_eq!(context("## заголовок", 4).heading, 2);
    assert!(context("1. пункт", 4).numbered);
    assert!(context("a `код` b", 4).code);
    assert!(context("*кур*", 3).italic);
    assert!(context("- пункт", 3).list);
    assert_eq!(context("обычный", 3), MdCtx::default());
}

#[test]
fn check_finds_task_mark() {
    assert_eq!(check_of(b"- [ ] a"), Some((3, b'x')));
    assert_eq!(check_of(b"* [x] a"), Some((3, b' ')));
    assert_eq!(check_of(b"12. [X] a"), Some((5, b' ')));
    assert_eq!(check_of(b"- a"), None);
    assert_eq!(check_of(b"text"), None);
}

/// Строки из слов без разметки.
fn plain() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("слово"),
            Just("word"),
            Just("a1"),
            Just(" "),
            Just("\n"),
            Just(","),
        ],
        0..14,
    )
    .prop_map(|v| v.concat())
}

/// Текст со звёздочками — вокруг выделения бывает что угодно.
fn starry() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("*"),
            Just("**"),
            Just("слово"),
            Just("a"),
        ],
        0..14,
    )
    .prop_map(|v| v.concat())
}

fn boundary(t: &str, x: f64) -> usize {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "доля строки"
    )]
    let mut i = (t.len() as f64 * x) as usize;
    while !t.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn pair(t: &str, a: f64, b: f64) -> Range<usize> {
    let (a, b) = (boundary(t, a), boundary(t, b));
    a.min(b)..a.max(b)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Команда дважды — исходный текст: выделение, текст с любыми звёздочками. Выделение
    /// внутри уже жирного отрезка снимает весь отрезок — такое вернуть нечем.
    #[test]
    fn twice_on_selection(t in starry(), x in 0.0f64..1.0, y in 0.0f64..1.0, bold in any::<bool>()) {
        let sel = pair(&t, x, y);
        prop_assume!(!sel.is_empty());
        let st = if bold { Style::Strong } else { Style::Em };
        prop_assume!(t[sel.clone()].chars().any(|c| c != '*'));
        // Граница выделения посреди серии звёздочек — маркер не отличить от текста.
        let b = t.as_bytes();
        let mid = |i: usize| i > 0 && i < b.len() && b[i - 1] == b'*' && b[i] == b'*';
        prop_assume!(!mid(sel.start) && !mid(sel.end));
        prop_assume!(span_of(&t, &lines(&t), &sel, st).is_none());
        let act = if bold { MdAct::Bold } else { MdAct::Italic };
        let (s1, r1) = run(&act, &t, sel);
        let (s2, _) = run(&act, &s1, r1);
        prop_assert_eq!(s2, t);
    }

    /// То же с пробелами и переводами строк: края выделения подрезаются.
    #[test]
    fn twice_on_spaces(t in plain(), a in 0.0f64..1.0, b in 0.0f64..1.0, bold in any::<bool>()) {
        let sel = pair(&t, a, b);
        prop_assume!(!sel.is_empty());
        let act = if bold { MdAct::Bold } else { MdAct::Italic };
        let (s1, r1) = run(&act, &t, sel);
        let (s2, _) = run(&act, &s1, r1);
        prop_assert_eq!(s2, t);
    }

    /// Команда дважды — исходный текст: курсор, текст без разметки.
    #[test]
    fn twice_on_caret(t in plain(), a in 0.0f64..1.0, bold in any::<bool>()) {
        let c = boundary(&t, a);
        let act = if bold { MdAct::Bold } else { MdAct::Italic };
        let (s1, r1) = run(&act, &t, c..c);
        let (s2, _) = run(&act, &s1, r1);
        prop_assert_eq!(s2, t);
    }

    /// Команда дважды — исходный текст: префиксы строк.
    #[test]
    fn twice_on_lines(t in plain(), a in 0.0f64..1.0, b in 0.0f64..1.0, k in 0usize..6) {
        let act = [
            MdAct::Heading(1),
            MdAct::Heading(2),
            MdAct::Heading(3),
            MdAct::List,
            MdAct::Numbered,
            MdAct::Task,
        ][k].clone();
        let sel = pair(&t, a, b);
        let (s1, r1) = run(&act, &t, sel);
        prop_assert!(s1 != t, "первая команда меняет текст");
        let (s2, _) = run(&act, &s1, r1);
        prop_assert_eq!(s2, t);
    }
}

fn md_editor(dir: &Path, bytes: &[u8], cfg: EditorCfg) -> Core {
    editor(dir, "a.md", bytes, 80, cfg)
}

fn doc_text(c: &Core) -> String {
    String::from_utf8(text(c)).unwrap()
}

/// Каждая команда — один шаг отмены; ввод вокруг не склеивается с ней.
#[test]
fn one_undo_per_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = md_editor(dir.path(), b"one\ntwo\nthree\n", EditorCfg::default());
    cmd(&mut c, "editor.select_all");
    let was = "one\ntwo\nthree\n";
    for (id, want) in [
        ("editor.md.list", "- one\n- two\n- three\n"),
        ("editor.md.numbered", "1. one\n2. two\n3. three\n"),
        ("editor.md.task", "- [ ] one\n- [ ] two\n- [ ] three\n"),
    ] {
        cmd(&mut c, id);
        assert_eq!(doc_text(&c), want, "{id}");
        assert!(ed(&c).dirty);
        cmd(&mut c, "editor.undo");
        assert_eq!(doc_text(&c), was, "{id}: один шаг");
        assert!(!ed(&c).dirty);
        assert!(eng(&c).sel.stream().is_some(), "выделение вернулось");
    }
    cmd(&mut c, "editor.redo");
    assert_eq!(doc_text(&c), "- [ ] one\n- [ ] two\n- [ ] three\n");
    cmd(&mut c, "editor.undo");
    // Ввод + команда + отмена: ввод остаётся.
    mv(&mut c, "doc_start");
    input(&mut c, "x");
    cmd(&mut c, "editor.md.bold");
    assert_eq!(doc_text(&c), "**xone**\ntwo\nthree\n");
    cmd(&mut c, "editor.undo");
    assert_eq!(doc_text(&c), "xone\ntwo\nthree\n");
}

/// Уровень приходит аргументом, контекст кнопки — из строки курсора.
#[test]
fn heading_level_and_context() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = md_editor(dir.path(), b"title\n", EditorCfg::default());
    c.dispatch(Command::new("editor.md.heading").arg("level", 2));
    c.pump(Duration::from_millis(1));
    assert_eq!(doc_text(&c), "## title\n");
    assert_eq!(eng(&c).md_ctx().unwrap().heading, 2);
    c.dispatch(Command::new("editor.md.heading").arg("level", 2));
    c.pump(Duration::from_millis(1));
    assert_eq!(doc_text(&c), "title\n");
}

/// Не UTF-8: правки — в кодировке документа, курсор — в её байтах.
#[test]
fn commands_in_utf16() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend("слово\n".encode_utf16().flat_map(u16::to_le_bytes));
    let mut c = md_editor(dir.path(), &bytes, EditorCfg::default());
    assert_eq!(eng(&c).enc.enc, UTF_16LE);
    mv(&mut c, "word_right");
    let caret = eng(&c).sel.caret;
    cmd(&mut c, "editor.md.bold");
    let got: Vec<u16> = text(&c)
        .chunks(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(String::from_utf16(&got).unwrap(), "**слово**\n");
    assert_eq!(eng(&c).sel.caret, caret + 4, "курсор — за маркером");
    cmd(&mut c, "editor.undo");
    assert_eq!(text(&c), bytes[2..]);
}

fn with_preview() -> EditorCfg {
    EditorCfg {
        preview: true,
        ..EditorCfg::default()
    }
}

fn parsed(c: &Core) -> bool {
    ed(c)
        .preview
        .as_ref()
        .is_some_and(|p| p.idle() && p.md.doc.is_some())
}

/// Разбор — один на серию правок и только после паузы; документ — по последнему тексту.
#[test]
fn preview_parses_after_pause() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = md_editor(dir.path(), b"# one\n\n", with_preview());
    pump_until(&mut c, parsed);
    let doc = ed(&c).preview.as_ref().unwrap().md.doc.clone().unwrap();
    assert_eq!(doc.block_text(0), "one");
    mv(&mut c, "doc_end");
    let t = Instant::now();
    for ch in ["a", "b", "c"] {
        input(&mut c, ch);
        assert!(t.elapsed() < PAUSE, "тест медленнее паузы");
        let p = ed(&c).preview.as_ref().unwrap();
        assert!(!p.idle(), "разбор ждёт паузы");
        assert!(
            std::sync::Arc::ptr_eq(p.md.doc.as_ref().unwrap(), &doc),
            "документ не перестраивается на клавишу"
        );
    }
    let was = ed(&c).preview.as_ref().unwrap().md.pass();
    pump_until(&mut c, parsed);
    let p = ed(&c).preview.as_ref().unwrap();
    assert!(t.elapsed() >= PAUSE);
    assert_eq!(p.md.pass(), was + 1, "один разбор на серию");
    assert_eq!(p.md.doc.as_ref().unwrap().block_text(1), "abc");
    assert!(
        p.reuse.is_some_and(|r| r.head == 1),
        "первый блок не менялся"
    );
}

/// Файл не Markdown и выключенный ключ — предпросмотра нет; переключатель включает его.
#[test]
fn preview_only_when_on() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = editor(dir.path(), "a.txt", b"text\n", 80, with_preview());
    c.pump(Duration::from_millis(5));
    assert!(ed(&c).preview.is_none());
    let mut c = md_editor(dir.path(), b"text\n", EditorCfg::default());
    assert!(ed(&c).preview.is_none());
    cmd(&mut c, "editor.preview");
    pump_until(&mut c, parsed);
    cmd(&mut c, "editor.preview");
    assert!(ed(&c).preview.is_none());
}

fn long_md() -> Vec<u8> {
    let mut s = String::new();
    for i in 0..60 {
        s.push_str(&format!("## Раздел {i}\n\nТекст {i}\nвторая строка\n\n"));
    }
    s.into_bytes()
}

/// Прокрутка: ведёт та сторона, которую прокрутили последней, эха нет.
#[test]
fn scroll_sync_without_echo() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = md_editor(dir.path(), &long_md(), with_preview());
    pump_until(&mut c, parsed);
    let doc = ed(&c).id;
    // Текст ведёт: верх документа — блок верхней строки.
    let off = eng(&c).text.offset_of_line(50).unwrap().get();
    send(&mut c, InboxMsg::ScrollTo { doc, off });
    let p = ed(&c).preview.as_ref().unwrap();
    assert_eq!(p.lead, Lead::Text);
    let d = p.md.doc.clone().unwrap();
    let (b, _) = d.top_at(off);
    assert_eq!(p.md.top, b);
    // Список догнал и сообщил своё (край, округление) — текст не двигается.
    let top = eng(&c).top;
    send(
        &mut c,
        InboxMsg::MdTop {
            doc,
            block: b + 3,
            frac: 0.0,
        },
    );
    assert_eq!(eng(&c).top, top, "эхо списка не двигает текст");
    // Колесо над документом: ведёт он, текст идёт за ним.
    send(&mut c, InboxMsg::MdLead { doc });
    let want = d.off_at(b + 8, 0.0);
    send(
        &mut c,
        InboxMsg::MdTop {
            doc,
            block: b + 8,
            frac: 0.0,
        },
    );
    let e = eng(&c);
    assert!(e.top <= want && want < e.top + 64, "{} {want}", e.top);
    let p = ed(&c).preview.as_ref().unwrap();
    assert_eq!(p.md.top, b + 8);
    assert_eq!(p.lead, Lead::Doc, "текст догнал, лидерство не перехватил");
    // Клавиша снова отдаёт лидерство тексту.
    mv(&mut c, "down");
    assert_eq!(ed(&c).preview.as_ref().unwrap().lead, Lead::Text);
}

/// Клик по флажку: `[ ]` ↔ `[x]` одной группой, курсор на месте; устаревший разбор — нет.
#[test]
fn task_checkbox() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = md_editor(
        dir.path(),
        b"text\n\n- [ ] one\n- [x] two\n",
        with_preview(),
    );
    pump_until(&mut c, parsed);
    let doc = ed(&c).id;
    let d = ed(&c).preview.as_ref().unwrap().md.doc.clone().unwrap();
    let item = |n: usize| {
        d.blocks
            .iter()
            .filter(|b| matches!(b.kind, crate::md::BlockKind::Item { .. }))
            .nth(n)
            .unwrap()
            .src
            .start
    };
    let caret = eng(&c).sel.caret;
    send(&mut c, InboxMsg::MdCheck { doc, off: item(0) });
    assert_eq!(doc_text(&c), "text\n\n- [x] one\n- [x] two\n");
    assert_eq!(eng(&c).sel.caret, caret);
    cmd(&mut c, "editor.undo");
    assert_eq!(doc_text(&c), "text\n\n- [ ] one\n- [x] two\n");
    pump_until(&mut c, parsed);
    send(&mut c, InboxMsg::MdCheck { doc, off: item(1) });
    assert_eq!(doc_text(&c), "text\n\n- [ ] one\n- [ ] two\n");
    // Разбор ещё по прежнему тексту — клик не применяется.
    send(&mut c, InboxMsg::MdCheck { doc, off: item(0) });
    assert_eq!(doc_text(&c), "text\n\n- [ ] one\n- [ ] two\n");
}
