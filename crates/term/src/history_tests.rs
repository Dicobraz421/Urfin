//! История (S6-16): дедуп, предел, исключения, jsonl, нативные форматы.

use super::*;

fn cmds(h: &History) -> Vec<&str> {
    h.entries().iter().map(|e| e.cmd.as_str()).collect()
}

/// Приёмка: дубль переносится в конец.
#[test]
fn duplicate_moves_to_end() {
    let mut h = History::default();
    for c in ["dir", "cd ..", "git status", "dir"] {
        h.push(Entry::new(c, 1), 100);
    }
    assert_eq!(cmds(&h), ["cd ..", "git status", "dir"]);
}

#[test]
fn cap_keeps_locked() {
    let mut h = History::default();
    h.push(Entry::new("keep", 1), 3);
    assert!(h.toggle_lock(0));
    for c in ["a", "b", "c", "d"] {
        h.push(Entry::new(c, 1), 3);
    }
    assert_eq!(cmds(&h), ["keep", "c", "d"]);
    // Повтор закреплённой — в конец, закрепление остаётся.
    h.push(Entry::new("keep", 2), 3);
    assert!(h.entries().last().unwrap().lock);
    assert!(h.remove(0));
    assert_eq!(cmds(&h), ["d", "keep"]);
}

/// Приёмка: `  secret` и `*password*` не пишутся.
#[test]
fn excluded_lines() {
    let cfg = HistoryConfig::default();
    assert!(excluded(&cfg, "  secret"));
    assert!(excluded(&cfg, " ls"));
    assert!(excluded(&cfg, "mysql --PASSWORD=x"));
    assert!(excluded(&cfg, "set GITHUB_TOKEN=1"));
    assert!(excluded(&cfg, ""));
    assert!(!excluded(&cfg, "git status"));
    let off = HistoryConfig {
        ignore_space: false,
        exclude: Vec::new(),
        ..HistoryConfig::default()
    };
    assert!(!excluded(&off, " ls"));
    assert!(glob("a*b?c", "axxbyc") && !glob("a*b?c", "axxbc"));
    assert!(glob("*", "") && glob("**x*", "yyx"));
}

#[test]
fn jsonl_round_trip() {
    let mut e = Entry::new("echo \"привет\"\t\\ \u{1}😀\nnext", 1_700_000_000);
    e.cwd = Some(r"C:\Program Files".into());
    e.exit = Some(-1);
    e.lock = true;
    let line = to_line(&e);
    assert!(!line.contains('\n'));
    assert_eq!(from_line(&line), Some(e));
    let plain = from_line(r#"{"t":5,"cmd":"ls","cwd":null,"exit":null,"lock":false}"#).unwrap();
    assert_eq!((plain.cmd.as_str(), plain.t, plain.cwd), ("ls", 5, None));
    // Суррогатная пара и пробелы вокруг.
    let e2 = from_line(r#" { "cmd" : "\ud83d\ude00" , "t" : 1 } "#).unwrap();
    assert_eq!(e2.cmd, "😀");
}

/// Приёмка: битый jsonl → пропуск строк (и предупреждение в лог).
#[test]
fn broken_lines_skipped() {
    let text = "{\"t\":1,\"cmd\":\"a\"}\nnot json\n{\"t\":2}\n{\"t\":3,\"cmd\":\"b\"\n\n{\"t\":4,\"cmd\":\"c\"}\n";
    let (entries, lines) = parse(text, 100, Path::new("x.jsonl"));
    assert_eq!(lines, 5);
    let got: Vec<&str> = entries.iter().map(|e| e.cmd.as_str()).collect();
    assert_eq!(got, ["a", "c"]);
}

/// Приёмка: перезапуск сохраняет историю по shell (файл на диске, повтор — один раз,
/// компакция при росте файла).
#[test]
fn restart_keeps_history_per_shell() {
    let dir = tempfile::tempdir().unwrap();
    let pwsh = dir.path().join(file_name("pwsh"));
    let wsl = dir.path().join(file_name("wsl:Ubuntu"));
    for c in ["Get-Date", "ls", "Get-Date"] {
        append(&pwsh, &Entry::new(c, 1)).unwrap();
    }
    append(&wsl, &Entry::new("uname -a", 1)).unwrap();
    let got: Vec<String> = load_file(&pwsh, 100).into_iter().map(|e| e.cmd).collect();
    assert_eq!(got, ["ls", "Get-Date"]);
    assert_eq!(load_file(&wsl, 100)[0].cmd, "uname -a");
    assert_eq!(key_of("wsl%3AUbuntu"), "wsl:Ubuntu");
    assert_eq!(
        key_of(file_name("my_shell").trim_end_matches(".jsonl")),
        "my_shell"
    );
    // 3 строки > 2 × 1: файл сжат до одной записи.
    let one = load_file(&pwsh, 1);
    assert_eq!(one.len(), 1);
    let text = std::fs::read_to_string(&pwsh).unwrap();
    assert_eq!(text.lines().count(), 1);
    // Удаление и закрепление — перезапись целиком.
    let mut h = History::default();
    h.load(load_file(&pwsh, 100), 100);
    h.push(Entry::new("new", 2), 100);
    h.toggle_lock(0);
    rewrite(&pwsh, &h.own()).unwrap();
    let back = load_file(&pwsh, 100);
    assert_eq!(back.len(), 2);
    assert!(back[0].lock);
}

#[test]
fn load_merges_with_session_entries() {
    let mut h = History::default();
    h.push(Entry::new("typed now", 9), 10);
    h.import(vec![Entry::new("native", 0)], 10);
    h.load(vec![Entry::new("old", 1), Entry::new("typed now", 1)], 10);
    assert_eq!(cmds(&h), ["native", "old", "typed now"]);
    assert!(h.entries()[0].native);
    // Нативную нельзя закрепить; удаление нативной файл не трогает.
    assert!(!h.toggle_lock(0));
    assert!(!h.remove(0));
}

#[test]
fn prefix_navigation() {
    let mut h = History::default();
    for c in ["git status", "dir", "git log", "cls"] {
        h.push(Entry::new(c, 1), 10);
    }
    let n = h.entries().len();
    assert_eq!(h.prev("git", n), Some(2));
    assert_eq!(h.prev("git", 2), Some(0));
    assert_eq!(h.prev("git", 0), None);
    assert_eq!(h.next("git", 0), Some(2));
    assert_eq!(h.prev("", n), Some(3));
}

#[test]
fn native_formats() {
    assert_eq!(
        parse_psreadline("Get-Date\nif ($x) {`\n  1 }\n\nls\n"),
        ["Get-Date", "if ($x) {\n  1 }", "ls"]
    );
    assert_eq!(
        parse_bash("ls\n#1700000000\ncd /tmp\n#comment\n"),
        ["ls", "cd /tmp", "#comment"]
    );
    assert_eq!(
        parse_zsh(": 1700000000:0;ls -la\n: 1700000001:0;echo a\\\nb\nplain\n"),
        ["ls -la", "echo a\nb", "plain"]
    );
    assert_eq!(
        parse_fish("- cmd: ls\n  when: 1\n- cmd: echo a\\nb\n  when: 2\n"),
        ["ls", "echo a\nb"]
    );
}
