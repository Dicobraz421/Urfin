//! Линты i18n (S2 §7), идут в CI вместе с тестами:
//! - parity en/ru: те же файлы, сообщения, атрибуты и переменные;
//! - все файлы `locales/` вшиты (`EMBEDDED`);
//! - `keys_used`: каждый ключ-литерал в коде workspace есть в en;
//! - `no_raw_strings`: сырые литералы в `toast(`, `Banner {text}`, `PromptSpec {title, body}`,
//!   `Choice {label}`.
//!
//! `commands_i18n` (ключ `cmd-<id>` у каждой команды реестра) — в `urfin-ui`, где
//! собирается реестр.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};

/// Что видно у сообщения снаружи: атрибуты и переменные (по значению и атрибутам).
#[derive(Debug, Default, PartialEq, Eq)]
struct Shape {
    attrs: BTreeSet<String>,
    vars: BTreeSet<String>,
}

fn shapes(source: &str) -> BTreeMap<String, Shape> {
    let resource = match fluent_syntax::parser::parse(source) {
        Ok(r) => r,
        Err((r, errors)) => panic!("parse errors {errors:?} in {r:?}"),
    };
    let mut out = BTreeMap::new();
    for entry in resource.body {
        let Entry::Message(m) = entry else { continue };
        let mut shape = Shape::default();
        if let Some(p) = &m.value {
            vars(p, &mut shape.vars);
        }
        for a in &m.attributes {
            shape.attrs.insert(a.id.name.to_owned());
            vars(&a.value, &mut shape.vars);
        }
        out.insert(m.id.name.to_owned(), shape);
    }
    out
}

fn vars(p: &Pattern<&str>, out: &mut BTreeSet<String>) {
    for el in &p.elements {
        if let PatternElement::Placeable { expression } = el {
            expr_vars(expression, out);
        }
    }
}

fn expr_vars(e: &Expression<&str>, out: &mut BTreeSet<String>) {
    match e {
        Expression::Select { selector, variants } => {
            inline_vars(selector, out);
            for v in variants {
                vars(&v.value, out);
            }
        }
        Expression::Inline(i) => inline_vars(i, out),
    }
}

fn inline_vars(e: &InlineExpression<&str>, out: &mut BTreeSet<String>) {
    match e {
        InlineExpression::VariableReference { id } => {
            out.insert(id.name.to_owned());
        }
        InlineExpression::FunctionReference { arguments, .. } => {
            for a in &arguments.positional {
                inline_vars(a, out);
            }
            for a in &arguments.named {
                inline_vars(&a.value, out);
            }
        }
        InlineExpression::Placeable { expression } => expr_vars(expression, out),
        _ => {}
    }
}

/// Расхождения перевода `other` с эталоном `en` в одном файле.
fn parity(file: &str, en: &str, other: &str) -> Vec<String> {
    let (en, other) = (shapes(en), shapes(other));
    let mut problems = Vec::new();
    for (id, want) in &en {
        match other.get(id) {
            None => problems.push(format!("{file}: missing {id}")),
            Some(got) if got != want => {
                problems.push(format!("{file}: {id}: {got:?} != en {want:?}"));
            }
            Some(_) => {}
        }
    }
    for id in other.keys().filter(|id| !en.contains_key(*id)) {
        problems.push(format!("{file}: {id} is not in en"));
    }
    problems
}

fn locales() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("locales")
}

fn files(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| {
            Path::new(n)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("ftl"))
        })
        .collect()
}

#[test]
fn parity_en_ru() {
    let en_files = files(&locales().join("en"));
    let mut problems = Vec::new();
    for lang in ["ru"] {
        let dir = locales().join(lang);
        let lang_files = files(&dir);
        if lang_files != en_files {
            problems.push(format!("{lang}: files {lang_files:?} != en {en_files:?}"));
        }
        for file in en_files.intersection(&lang_files) {
            let en = std::fs::read_to_string(locales().join("en").join(file)).unwrap();
            let other = std::fs::read_to_string(dir.join(file)).unwrap();
            problems.extend(parity(&format!("{lang}/{file}"), &en, &other));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn parity_catches_removed_key_and_changed_vars() {
    let en = "a = A\nb = { $n } B\n    .tip = T\n";
    assert_eq!(parity("x", en, en), Vec::<String>::new());
    let ru_missing = "b = { $n } Б\n    .tip = Т\n";
    assert_eq!(parity("x", en, ru_missing), ["x: missing a"]);
    let ru_vars = "a = А\nb = { $count } Б\n    .tip = Т\n";
    assert_eq!(parity("x", en, ru_vars).len(), 1);
    let ru_attr = "a = А\nb = { $n } Б\n";
    assert_eq!(parity("x", en, ru_attr).len(), 1);
    let ru_extra = "a = А\nb = { $n } Б\n    .tip = Т\nc = В\n";
    assert_eq!(parity("x", en, ru_extra), ["x: c is not in en"]);
}

#[test]
fn every_locale_file_is_embedded() {
    let embedded: BTreeSet<(String, String)> = urfin_i18n::EMBEDDED
        .iter()
        .map(|(lang, file, _)| ((*lang).to_owned(), (*file).to_owned()))
        .collect();
    let mut on_disk = BTreeSet::new();
    for lang in ["en", "ru"] {
        for file in files(&locales().join(lang)) {
            on_disk.insert((lang.to_owned(), file));
        }
    }
    assert_eq!(embedded, on_disk);
}

// ---- Исходники workspace -------------------------------------------------------------

/// Исходник без комментариев: строковые литералы заменены на `"#N"`, их тексты — в списке.
struct Masked {
    code: String,
    literals: Vec<String>,
}

/// Мини-лексер Rust: комментарии, строки (`"…"`, `r#"…"#`, `b"…"`), символы.
fn mask(src: &str) -> Masked {
    let b = src.as_bytes();
    let mut code = String::with_capacity(src.len());
    let mut literals = Vec::new();
    let mut i = 0;
    let mut push_literal = |code: &mut String, text: String| {
        let _ = write!(code, "\"#{}\"", literals.len());
        literals.push(text);
    };
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'r' if matches!(b.get(i + 1), Some(b'"' | b'#'))
                && !b[..i]
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') =>
            {
                let mut j = i + 1;
                let mut hashes = 0;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    code.push('r');
                    i += 1;
                    continue;
                }
                let start = j + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let end = src[start..].find(&close).map_or(src.len(), |e| start + e);
                push_literal(&mut code, src[start..end].to_owned());
                i = (end + close.len()).min(src.len());
            }
            b'"' => {
                let mut j = i + 1;
                let mut text = String::new();
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                text.push_str(&src[i + 1..j.min(src.len())]);
                push_literal(&mut code, text);
                i = j + 1;
            }
            // Символ `'"'` / `'\''`; время жизни `'a` — обычный код.
            b'\'' if b.get(i + 2) == Some(&b'\'') || b.get(i + 1) == Some(&b'\\') => {
                let end = src[i + 1..].find('\'').map_or(src.len(), |e| i + 1 + e);
                let end = if b.get(i + 1) == Some(&b'\\') {
                    src[i + 3..].find('\'').map_or(src.len(), |e| i + 3 + e)
                } else {
                    end
                };
                code.push_str("'c'");
                i = end + 1;
            }
            _ => {
                let ch = src[i..].chars().next().unwrap();
                code.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Masked { code, literals }
}

/// Исходники `crates/*/src/**/*.rs` и `crates/ui/legacy/**/*.rs` (egui до G-06,
/// ADR-0018) без тестов (`tests.rs`, `*_tests.rs`, хвост после
/// `#[cfg(test)]`) и фикстур (`*_fixtures.rs`: демо-данные снапшотов и скриншотов —
/// имена файлов, заголовки задач — не строки интерфейса).
fn sources() -> Vec<(PathBuf, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = std::fs::read_dir(&crates)
        .unwrap()
        .flat_map(|e| {
            let p = e.unwrap().path();
            [p.join("src"), p.join("legacy")]
        })
        .filter(|p| p.is_dir())
        .collect();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let path = e.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
                && name != "tests.rs"
                && !name.ends_with("_tests.rs")
                && !name.ends_with("_fixtures.rs")
            {
                let text = std::fs::read_to_string(&path).unwrap();
                let text = match text.find("\n#[cfg(test)]") {
                    Some(at) => text[..at].to_owned(),
                    None => text,
                };
                out.push((path, text));
            }
        }
    }
    out.sort();
    out
}

const PREFIXES: [&str; 11] = [
    "ui-", "cmd-", "keybar-", "core-", "vfs-", "ops-", "err-", "fmt-", "keymap-", "net-", "pkg-",
];

/// Литералы с префиксом ключа, но не ключи: имена семейств шрифтов (S2 §1.1).
const NOT_KEYS: [&str; 3] = ["ui-strong", "ui-caption", "ui-subtitle"];

/// Литерал — ключ Fluent: известный префикс и `[a-z0-9_-]`.
fn is_key(s: &str) -> bool {
    !NOT_KEYS.contains(&s)
        && PREFIXES
            .iter()
            .any(|p| s.starts_with(p) && s.len() > p.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        && !s.ends_with('-')
}

#[test]
fn keys_used_exist_in_en() {
    let en = urfin_i18n::Catalog::embedded()
        .build(&"en".parse().unwrap())
        .0;
    let mut missing = Vec::new();
    let mut seen = 0;
    for (path, text) in sources() {
        for lit in mask(&text).literals {
            if is_key(&lit) {
                seen += 1;
                if !en.has(&lit) {
                    missing.push(format!("{}: {lit}", path.display()));
                }
            }
        }
    }
    assert!(seen > 10, "сканер не нашёл ключей: {seen}");
    assert!(
        missing.is_empty(),
        "keys not in en:\n{}",
        missing.join("\n")
    );
}

/// Нарушения `no_raw_strings` в одном исходнике.
fn raw_strings(src: &str) -> Vec<String> {
    let m = mask(src);
    let code = &m.code;
    let literal_at = |pos: usize| -> Option<&str> {
        let rest = code[pos..].trim_start();
        let num = rest.strip_prefix("\"#")?;
        let end = num.find('"')?;
        let idx: usize = num[..end].parse().ok()?;
        Some(m.literals[idx].as_str()).filter(|s| !s.is_empty())
    };
    let mut found = Vec::new();
    // toast(Level::X, "…")
    for (at, _) in code.match_indices("toast(") {
        let args = &code[at + "toast(".len()..];
        if let Some(comma) = args.find(',').filter(|c| !args[..*c].contains(')')) {
            let pos = at + "toast(".len() + comma + 1;
            if let Some(lit) = literal_at(pos) {
                found.push(format!("toast({lit:?})"));
            }
        }
    }
    // Поля структур: Banner {text}, PromptSpec {title, body}, Choice {label}.
    for (ty, fields) in [
        ("Banner {", &["text:"][..]),
        ("PromptSpec {", &["title:", "body:"][..]),
        ("Choice {", &["label:"][..]),
    ] {
        for (at, _) in code.match_indices(ty) {
            let body = block(&code[at + ty.len()..]);
            for field in fields {
                for (f, _) in body.match_indices(field) {
                    let pos = at + ty.len() + f + field.len();
                    if let Some(lit) = literal_at(pos) {
                        found.push(format!("{ty}{field} {lit:?}"));
                    }
                }
            }
        }
    }
    found
}

/// Текст до парной `}`.
fn block(s: &str) -> &str {
    let mut depth = 1;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..i];
                }
            }
            _ => {}
        }
    }
    s
}

#[test]
fn no_raw_strings_in_sources() {
    let mut found = Vec::new();
    for (path, text) in sources() {
        for v in raw_strings(&text) {
            found.push(format!("{}: {v}", path.display()));
        }
    }
    assert!(found.is_empty(), "raw UI strings:\n{}", found.join("\n"));
}

#[test]
fn no_raw_strings_catches_literals() {
    assert_eq!(
        raw_strings(r#"cx.toast(Level::Info, "x");"#),
        [r#"toast("x")"#]
    );
    assert_eq!(
        raw_strings(r#"cx.toast(Level::Warn, "y".to_owned());"#).len(),
        1
    );
    assert_eq!(
        raw_strings(r#"cx.toast(Level::Info, tr("core-x", &[]));"#),
        Vec::<String>::new()
    );
    let spec = r#"PromptSpec { title: "T".into(), body: tr("core-b", &[]), choices: vec![Choice { id: "yes".into(), label: "Yes".into() }] }"#;
    assert_eq!(
        raw_strings(spec),
        [r#"PromptSpec {title: "T""#, r#"Choice {label: "Yes""#]
    );
    assert_eq!(
        raw_strings(r#"Banner { level, text: "boom".into(), actions }"#).len(),
        1
    );
    // Комментарии и строки внутри строк не считаются.
    assert_eq!(
        raw_strings("// cx.toast(Level::Info, \"x\")\nlet s = \"toast(Level::Info, \\\"x\\\")\";"),
        Vec::<String>::new()
    );
}

#[test]
fn mask_handles_rust_literals() {
    let m = mask(
        r##"let a = "x\"y"; let b = r#"q"z"#; let c = '"'; // "c"
let d = 'a'; fn f<'a>() {} /* "e" */ let g = "ui-key";"##,
    );
    assert_eq!(m.literals, [r#"x\"y"#, r#"q"z"#, "ui-key"]);
    assert!(is_key("ui-key") && is_key("cmd-panel-cursor-page_up"));
    assert!(!is_key("ui-") && !is_key("keybar-{part}") && !is_key("panel.go"));
}
