use std::sync::Arc;

use encoding_rs::{UTF_16BE, UTF_16LE, WINDOWS_1251};

use super::*;
use crate::syntax::{Tag, grammar, highlight};

const NEVER: fn() -> bool = || false;

const TOML: &str = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n\
[workspace.package]\nversion = \"0.4.2\" # версия\nedition = \"2024\"\n\n\
[profile.dev]\ndebug = \"line-tables-only\"\n";

fn full(lang: &str, text: &[u8], feed: Feed) -> (Parsed, Vec<Span>) {
    let g = grammar(lang).unwrap();
    let p = parse(&g, None, feed_text(text.to_vec(), feed), feed, &NEVER).unwrap();
    let s = spans(&g, &p, 0..text.len(), &NEVER).unwrap();
    (p, s)
}

/// Роль каждого символа текста (по байтам отрезков `list` и границам символов `starts`).
fn tags_by_char(list: &[Span], starts: &[usize]) -> Vec<Tag> {
    starts
        .iter()
        .map(|&b| {
            list.iter()
                .find(|s| s.start as usize <= b && b < s.end as usize)
                .map_or(Tag::Plain, |s| s.tag)
        })
        .collect()
}

#[test]
fn one_edit_points() {
    let old = b"ab\ncd\nef";
    let new = b"ab\nc-X\nY-d\nef";
    let e = one_edit(old, new, Feed::Utf8).unwrap();
    assert_eq!((e.start_byte, e.old_end_byte, e.new_end_byte), (4, 4, 9));
    assert_eq!(e.start_position, Point { row: 1, column: 1 });
    assert_eq!(e.old_end_position, Point { row: 1, column: 1 });
    assert_eq!(e.new_end_position, Point { row: 2, column: 2 });
    assert!(one_edit(old, old, Feed::Utf8).is_none());
    // Граница символа UTF-8: «я» → «ю» правит оба байта.
    let e = one_edit("xя".as_bytes(), "xю".as_bytes(), Feed::Utf8).unwrap();
    assert_eq!((e.start_byte, e.old_end_byte), (1, 3));
    // UTF-16: перевод строки — единица 0x000A, колонки — в байтах.
    let le = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let e = one_edit(&le("a\nb"), &le("a\nbc"), Feed::Utf16Le).unwrap();
    assert_eq!(e.start_position, Point { row: 1, column: 2 });
    assert_eq!(e.new_end_position, Point { row: 1, column: 4 });
}

/// Разбор с прежним деревом после правок даёт те же отрезки, что разбор с нуля.
#[test]
fn incremental_matches_full() {
    let g = grammar("toml").unwrap();
    let mut prev = parse(
        &g,
        None,
        Arc::new(TOML.as_bytes().to_vec()),
        Feed::Utf8,
        &NEVER,
    )
    .unwrap();
    let steps = [
        TOML.replace("0.4.2", "0.4.3"),
        TOML.replace("0.4.2", "0.4.3")
            .replace("[profile.dev]", "[profile.dev]\nopt = 1"),
        TOML.replace("\"2024\"", "\"2024"),
        TOML.to_owned(),
    ];
    for t in steps {
        let p = parse(
            &g,
            Some(&prev),
            Arc::new(t.as_bytes().to_vec()),
            Feed::Utf8,
            &NEVER,
        )
        .unwrap();
        let inc = spans(&g, &p, 0..t.len(), &NEVER).unwrap();
        let (_, fresh) = full("toml", t.as_bytes(), Feed::Utf8);
        assert_eq!(inc, fresh, "{t}");
        // Тот же отрезок, что у подсветки вьювера.
        assert_eq!(inc, highlight(&g, &t, &NEVER).unwrap());
        prev = p;
    }
}

/// Запрос по диапазону — те же отрезки, что у всего текста, обрезанные по нему.
#[test]
fn range_query_is_a_window() {
    let (p, all) = full("toml", TOML.as_bytes(), Feed::Utf8);
    let g = grammar("toml").unwrap();
    let r = 30..90;
    let part = spans(&g, &p, r.clone(), &NEVER).unwrap();
    let cut: Vec<Span> = all
        .iter()
        .filter(|s| (s.end as usize) > r.start && (s.start as usize) < r.end)
        .map(|s| Span {
            start: s.start.max(r.start as u64),
            end: s.end.min(r.end as u64),
            tag: s.tag,
        })
        .collect();
    assert_eq!(part, cut);
}

/// 1251 с кириллицей в строках и комментариях подсвечен как его UTF-8-копия; UTF-16 —
/// тоже.
#[test]
fn single_byte_and_utf16_like_utf8() {
    let text =
        "# комментарий: Ёж\nname = \"Привет, мир\"\nn = 42 # число\n[\"таблица\"]\nk = 'ключ'\n";
    let (_, u8s) = full("toml", text.as_bytes(), Feed::Utf8);
    let u8_starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let want = tags_by_char(&u8s, &u8_starts);
    assert!(want.contains(&Tag::Comment) && want.contains(&Tag::String));

    let (cp, _, bad) = WINDOWS_1251.encode(text);
    assert!(!bad);
    let (_, cps) = full("toml", &cp, Feed::of(WINDOWS_1251));
    let cp_starts: Vec<usize> = (0..cp.len()).collect();
    assert_eq!(tags_by_char(&cps, &cp_starts), want);

    for (enc, be) in [(UTF_16LE, false), (UTF_16BE, true)] {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
            .collect();
        let (_, s16) = full("toml", &bytes, Feed::of(enc));
        let mut starts = Vec::new();
        let mut at = 0;
        for c in text.chars() {
            starts.push(at);
            at += c.len_utf16() * 2;
        }
        assert_eq!(tags_by_char(&s16, &starts), want, "{}", enc.name());
    }
}

/// Места свёртки: таблицы TOML (заголовок и строки до следующей), у JSON закрывающая
/// скобка остаётся видна; диапазон — только заголовки в нём.
#[test]
fn fold_places() {
    let (p, _) = full("toml", TOML.as_bytes(), Feed::Utf8);
    let f = folds(&p, 0..TOML.len());
    let line = |s: &str| TOML.find(s).unwrap() as u64;
    let heads: Vec<u64> = f.iter().map(|f| f.head).collect();
    assert_eq!(
        heads,
        vec![
            line("[workspace]"),
            line("[workspace.package]"),
            line("[profile.dev]")
        ]
    );
    assert_eq!(f[0].start, line("resolver"));
    // Пустая строка перед следующей таблицей остаётся видна.
    assert_eq!(f[0].end, line("[workspace.package]") - 1);
    // Только заголовки в диапазоне.
    let mid = folds(&p, line("[workspace.package]") as usize..TOML.len());
    assert_eq!(mid.len(), 2);

    let json = "{\n  \"a\": {\n    \"b\": 1\n  },\n  \"c\": [\n    2\n  ]\n}\n";
    let (p, _) = full("json", json.as_bytes(), Feed::Utf8);
    let f = folds(&p, 0..json.len());
    let at = |s: &str| json.find(s).unwrap() as u64;
    assert_eq!(f[0].head, 0);
    assert_eq!(f[0].start, 2);
    // Последняя `}` видна: скрыто до начала её строки.
    assert_eq!(f[0].end, json.rfind('}').unwrap() as u64);
    let a = f.iter().find(|x| x.head == at("  \"a\"")).unwrap();
    assert_eq!(a.start, at("    \"b\""));
    assert_eq!(a.end, at("  },"));
}

/// 16 МБ без 16 байт: JSON записей или TOML таблиц.
fn big(lang: &str) -> Vec<u8> {
    big_n(lang, 16 << 20)
}

fn big_n(lang: &str, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 << 20);
    let mut i = 0u64;
    if lang == "json" {
        out.extend_from_slice(b"{\n  \"items\": [\n");
    }
    loop {
        let rec = if lang == "json" {
            format!(
                "    {{\"id\": {i}, \"name\": \"item-{i}\", \"tags\": [\"a\", \"b\"], \"price\": {}.5, \"ok\": true, \"note\": \"lorem ipsum dolor sit amet\"}},\n",
                i % 997
            )
        } else {
            format!(
                "\n[section{i}]\nkey = \"value {i}\" # comment\nnum = {i}\nflag = true\narr = [1, 2, 3]\n"
            )
        };
        if out.len() + rec.len() + 16 > size - 16 {
            break;
        }
        out.extend_from_slice(rec.as_bytes());
        i += 1;
    }
    if lang == "json" {
        out.extend_from_slice(b"    {}\n  ]\n}\n");
    }
    out
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "индекс перцентиля"
    )]
    let i = ((v.len() - 1) as f64 * q).round() as usize;
    v[i]
}

/// Замер S7-35: 16 МБ — полный разбор, затем правки внутри строки в середине: задача
/// целиком (копия текста, одна правка, разбор с прежним деревом, запрос и места свёртки
/// по видимой части ± 64 КБ). Бюджет — новые цвета ≤ 50 мс.
#[test]
#[ignore = "замер: cargo nextest run -p urfin-view --release --run-ignored only --no-capture -E 'test(hl_16mb)'"]
fn hl_16mb() {
    use std::time::Instant;
    for lang in ["json", "toml"] {
        let gram = grammar(lang).unwrap();
        let mut text = big(lang);
        let clock = Instant::now();
        let mut prev = parse(
            &gram,
            None,
            feed_text(text.clone(), Feed::Utf8),
            Feed::Utf8,
            &NEVER,
        )
        .unwrap();
        let full = clock.elapsed().as_secs_f64() * 1000.0;
        let mid = text.len() / 2;
        let at = mid
            + text[mid..]
                .windows(5)
                .position(|w| w == b"value" || w == b"lorem")
                .unwrap();
        let mut job = Vec::new();
        let mut parse_ms = Vec::new();
        for k in 0..40 {
            if k % 2 == 0 {
                text.insert(at, b'x');
            } else {
                text.remove(at);
            }
            let clock = Instant::now();
            let copy = feed_text(text.clone(), Feed::Utf8);
            let tp = Instant::now();
            let next = parse(&gram, Some(&prev), copy, Feed::Utf8, &NEVER).unwrap();
            parse_ms.push(tp.elapsed().as_secs_f64() * 1000.0);
            let r = at.saturating_sub(64 << 10)..at + (128 << 10);
            let s = spans(&gram, &next, r.clone(), &NEVER).unwrap();
            let f = folds(&next, r);
            assert_ne!(s, Vec::new());
            std::hint::black_box(f);
            job.push(clock.elapsed().as_secs_f64() * 1000.0);
            prev = next;
        }
        println!(
            "hl_16mb {lang}: full_parse_ms={full:.0} job_p50={:.1} job_p99={:.1} parse_p50={:.1} parse_p99={:.1}",
            pct(&mut job.clone(), 0.5),
            pct(&mut job, 0.99),
            pct(&mut parse_ms.clone(), 0.5),
            pct(&mut parse_ms, 0.99),
        );
    }
}
