//! Групповое переименование: шаблоны, regex, проверки плана, применение в две
//! фазы (приёмка S5-23).

use std::time::{Duration, Instant};

use urfin_core::JobState;

use super::*;
use crate::engine::tests::H;

fn t(name: &str) -> Target {
    Target {
        name: name.into(),
        mtime: 1_700_000_000_000_000_000,
        dir: false,
    }
}

fn spec(p: &str, regex: bool) -> RenameSpec {
    RenameSpec {
        pattern: p.into(),
        regex,
    }
}

#[allow(clippy::unnecessary_wraps, reason = "сигнатура дат плана")]
fn date(_: i64) -> Option<(u16, u8, u8)> {
    Some((2026, 10, 21))
}

fn none(_: &str) -> bool {
    false
}

fn names(p: &RenamePlan) -> Vec<(&str, &str)> {
    p.rows
        .iter()
        .map(|r| (r.from.as_str(), r.to.as_str()))
        .collect()
}

#[test]
fn template_tokens() {
    let ts = [t("report.final.pdf"), t("Makefile"), t("a.TXT")];
    let run = |p: &str| plan(&ts, &spec(p, false), &none, &date).unwrap();
    assert!(
        run("[N].[E]").rows.is_empty(),
        "identity; no dot for Makefile"
    );
    assert_eq!(
        names(&run("[N]_[C:1:1:3].[E]")),
        [
            ("report.final.pdf", "report.final_001.pdf"),
            ("Makefile", "Makefile_002"),
            ("a.TXT", "a_003.TXT")
        ]
    );
    assert_eq!(
        names(&run("[N2-4].[E]"))[0],
        ("report.final.pdf", "epo.pdf")
    );
    assert_eq!(names(&run("[N1].[E]"))[0], ("report.final.pdf", "r.pdf"));
    assert_eq!(
        names(&run("[N8-].[E]"))[0],
        ("report.final.pdf", "final.pdf")
    );
    assert_eq!(names(&run("[U][N].[L][E]"))[2], ("a.TXT", "A.txt"));
    assert_eq!(
        names(&run("[Y]-[M]-[D] [N].[E]"))[1],
        ("Makefile", "2026-10-21 Makefile")
    );
    assert_eq!(names(&run("[[[N]].[E]"))[2], ("a.TXT", "[a].TXT"));
    for bad in ["[Q]", "[N", "[N5-2]", "[C:x]"] {
        assert!(plan(&ts, &spec(bad, false), &none, &date).is_err(), "{bad}");
    }
}

#[test]
fn regex_replace() {
    let ts = [t("img_001.jpg"), t("img_002.jpg"), t("notes.txt")];
    let p = plan(&ts, &spec(r"^img_(\d+)/photo-$1", true), &none, &date).unwrap();
    assert_eq!(
        names(&p),
        [
            ("img_001.jpg", "photo-001.jpg"),
            ("img_002.jpg", "photo-002.jpg")
        ]
    );
    assert!(plan(&ts, &spec("(unclosed/x", true), &none, &date).is_err());
    assert!(plan(&ts, &spec("noslash", true), &none, &date).is_err());
}

#[test]
fn checks_duplicates_existing_invalid_and_chains() {
    let ts = [t("a.txt"), t("b.txt")];
    let dup = plan(&ts, &spec("x.[E]", false), &none, &date).unwrap();
    assert_eq!(dup.problems, 2);
    assert!(
        dup.rows
            .iter()
            .all(|r| r.problem == Some(Problem::Duplicate))
    );
    let taken = |n: &str| n.eq_ignore_ascii_case("C.txt");
    let ex = plan(&[t("a.txt")], &spec("c.[E]", false), &taken, &date).unwrap();
    assert_eq!(ex.rows[0].problem, Some(Problem::Exists));
    let inv = plan(&[t("a.txt")], &spec("a:b.[E]", false), &none, &date).unwrap();
    assert_eq!(inv.rows[0].problem, Some(Problem::Invalid));
    // Цепочка: b занято, но само переименовывается — не проблема.
    let all = |n: &str| n == "a.txt" || n == "b.txt" || n == "c.txt";
    let chain = plan(&ts, &spec("(a|b)/${1}x", true), &all, &date).unwrap();
    assert_eq!(chain.problems, 0, "{:?}", chain.rows);
    // Смена регистра — своё же имя.
    let case = plan(&[t("a.txt")], &spec("[U][N].[E]", false), &all, &date).unwrap();
    assert_eq!((case.problems, names(&case)), (0, vec![("a.txt", "A.TXT")]));
}

/// Приёмка S5-23: превью 10k — меньше 50 мс (release; в debug — с запасом).
#[test]
fn preview_10k_is_fast() {
    let ts: Vec<Target> = (0..10_000)
        .map(|i| t(&format!("photo_{i:05}.jpg")))
        .collect();
    let t0 = Instant::now();
    let p = plan(
        &ts,
        &spec("[Y][M][D]_[N]_[C:1:1:5].[E]", false),
        &none,
        &date,
    )
    .unwrap();
    let dt = t0.elapsed();
    assert_eq!((p.rows.len(), p.problems), (10_000, 0));
    let limit = if cfg!(debug_assertions) { 1000 } else { 50 };
    assert!(dt < Duration::from_millis(limit), "plan 10k: {dt:?}");
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("urfin-").tempdir().unwrap()
}

fn job(h: &mut H, dir: &Path, rows: &[(&str, &str)]) -> JobState {
    let plan = RenamePlan {
        rows: rows
            .iter()
            .map(|(f, t)| Row {
                from: (*f).into(),
                to: (*t).into(),
                problem: None,
            })
            .collect(),
        problems: 0,
    };
    job_with(h, dir, &plan, None)
}

/// Job по плану; `undo` — журнал для отката.
fn job_with(h: &mut H, dir: &Path, plan: &RenamePlan, undo: Option<Arc<UndoLog>>) -> JobState {
    let mr = MultiRename {
        base: Location::local(dir),
        rows: plan
            .rows
            .iter()
            .map(|r| (r.from.clone(), r.to.clone()))
            .collect(),
        undo,
        reverting: false,
    };
    let id = h.start(1, mr);
    h.wait_done(id)
}

fn plan_of(rows: &[(&str, &str)]) -> RenamePlan {
    RenamePlan {
        rows: rows
            .iter()
            .map(|(f, t)| Row {
                from: (*f).into(),
                to: (*t).into(),
                problem: None,
            })
            .collect(),
        problems: 0,
    }
}

/// Откат журнала `log`: job с теми же строками наоборот.
fn undo_job(h: &mut H, log: &Arc<UndoLog>) -> JobState {
    let plan = log.load().expect("rename log");
    let id = h.start(1, MultiRename::revert(&plan, Arc::clone(log)));
    h.wait_done(id)
}

/// Приёмка S5-23: a ↔ b (цикл) — через одно временное имя, данные на месте.
#[test]
fn swap_via_one_temp_name() {
    let d = tmp();
    std::fs::write(d.path().join("a.txt"), b"A").unwrap();
    std::fs::write(d.path().join("b.txt"), b"B").unwrap();
    let mut h = H::new(8);
    let st = job(&mut h, d.path(), &[("a.txt", "b.txt"), ("b.txt", "a.txt")]);
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 2 && s.errors == 0),
        "{st:?}"
    );
    assert_eq!(std::fs::read(d.path().join("a.txt")).unwrap(), b"B");
    assert_eq!(std::fs::read(d.path().join("b.txt")).unwrap(), b"A");
    assert_eq!(
        std::fs::read_dir(d.path()).unwrap().count(),
        2,
        "no temp names left"
    );
}

fn rows(pairs: &[(&str, &str)]) -> Vec<(CompactString, CompactString)> {
    pairs
        .iter()
        .map(|(f, t)| ((*f).into(), (*t).into()))
        .collect()
}

/// Порядок (S0 №93): цепочка — с хвоста, цикл — отдельно, смена регистра —
/// сама по себе.
#[test]
fn order_tails_then_cycles() {
    // 0: a→b, 1: b→c (цепочка, хвост 1); 2: x→y, 3: y→z, 4: z→x (цикл);
    // 5: Q→q (регистр); 6: m→n (одиночка).
    let o = Order::new(&rows(&[
        ("a", "b"),
        ("b", "c"),
        ("x", "y"),
        ("y", "z"),
        ("z", "x"),
        ("Q", "q"),
        ("m", "n"),
    ]));
    assert_eq!(o.tails, [1, 5, 6]);
    assert_eq!(o.pred[1], Some(0), "a→b after b→c");
    assert_eq!(o.cycles, [2]);
    assert_eq!(o.rest(2).collect::<Vec<_>>(), [4, 3]);
    // Имена без учёта регистра: B — старое имя строки «b».
    let o = Order::new(&rows(&[("a", "B"), ("b", "c")]));
    assert_eq!((o.tails.as_slice(), o.pred[1]), ([1].as_slice(), Some(0)));
}

fn names_in(dir: &Path) -> Vec<(String, String)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

fn put(dir: &Path, names: &[&str]) {
    for n in names {
        std::fs::write(dir.join(n), n).unwrap();
    }
}

/// Цепочка, цикл, регистр и одиночка в одном job — всё на месте, временных нет.
#[test]
fn mixed_chains_and_cycles() {
    let d = tmp();
    put(d.path(), &["a", "b", "x", "y", "z", "Q", "m"]);
    let mut h = H::new(8);
    let st = job(
        &mut h,
        d.path(),
        &[
            ("a", "b"),
            ("b", "c"),
            ("x", "y"),
            ("y", "z"),
            ("z", "x"),
            ("Q", "q"),
            ("m", "n"),
        ],
    );
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 7 && s.errors == 0),
        "{st:?}"
    );
    let want = [
        ("b", "a"),
        ("c", "b"),
        ("n", "m"),
        ("q", "Q"),
        ("x", "z"),
        ("y", "x"),
        ("z", "y"),
    ];
    let want: Vec<_> = want
        .iter()
        .map(|(n, c)| ((*n).to_owned(), (*c).to_owned()))
        .collect();
    assert_eq!(names_in(d.path()), want);
}

/// Хвост цепочки упёрся в чужое имя: вся цепочка — ошибки, файлы не тронуты.
#[test]
fn blocked_chain_keeps_files() {
    let d = tmp();
    put(d.path(), &["a", "b", "c"]);
    let before = names_in(d.path());
    let mut h = H::new(8);
    let st = job(&mut h, d.path(), &[("a", "b"), ("b", "c")]);
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 0 && s.errors == 2),
        "{st:?}"
    );
    assert_eq!(names_in(d.path()), before);
}

/// Цикл, где один файл не переименовать (открыт без `FILE_SHARE_DELETE`):
/// откат всего цикла — имена прежние, временных нет. И когда держат первый
/// (временное имя не взять), и когда середину.
#[cfg(windows)]
#[test]
fn broken_cycle_rolls_back() {
    use std::os::windows::fs::OpenOptionsExt;
    for locked in ["a", "b"] {
        let d = tmp();
        put(d.path(), &["a", "b", "c"]);
        let before = names_in(d.path());
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ
            .open(d.path().join(locked))
            .unwrap();
        let mut h = H::new(8);
        let st = job(&mut h, d.path(), &[("a", "b"), ("b", "c"), ("c", "a")]);
        drop(hold);
        assert!(
            matches!(&st, JobState::Done(s) if s.items == 0 && s.errors == 3),
            "{locked}: {st:?}"
        );
        assert_eq!(names_in(d.path()), before, "{locked}");
    }
}

/// Приёмка S5-23: цепочка из 10k (`f_i → f_{i+1}`) — корректно.
#[test]
fn chain_of_10k() {
    let d = tmp();
    let n: u32 = 10_000;
    for i in 0..n {
        std::fs::write(d.path().join(format!("f{i}")), i.to_string()).unwrap();
    }
    let names: Vec<(String, String)> = (0..n)
        .map(|i| (format!("f{i}"), format!("f{}", i + 1)))
        .collect();
    let rows: Vec<(&str, &str)> = names
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut h = H::new(8);
    // 10k `MoveFileExW`: ~10 с в одиночку, под нагрузкой — в разы дольше.
    h.deadline = Duration::from_secs(180);
    let st = job(&mut h, d.path(), &rows);
    assert!(
        matches!(&st, JobState::Done(s) if s.items == u64::from(n) && s.errors == 0),
        "{st:?}"
    );
    for i in [0, 1, 5000, n - 1] {
        assert_eq!(
            std::fs::read_to_string(d.path().join(format!("f{}", i + 1))).unwrap(),
            i.to_string()
        );
    }
    assert!(!d.path().join("f0").exists());
}

/// Приёмка S5-29: переименование → откат возвращает имена: обмен a ↔ b, цепочка,
/// цикл из трёх, смена регистра и одиночка. План лежит в журнале (файл), после
/// отката журнал пуст.
#[test]
fn undo_restores_swap_chain_and_cycle() {
    let d = tmp();
    let jnl = tmp();
    put(
        d.path(),
        &["a.txt", "b.txt", "c1", "c2", "x", "y", "z", "Q", "m"],
    );
    let before = names_in(d.path());
    let log = UndoLog::new(Some(jnl.path().to_path_buf()));
    let rows = [
        ("a.txt", "b.txt"),
        ("b.txt", "a.txt"),
        ("c1", "c2"),
        ("c2", "c3"),
        ("x", "y"),
        ("y", "z"),
        ("z", "x"),
        ("Q", "q"),
        ("m", "n"),
    ];
    let mut h = H::new(8);
    let st = job_with(&mut h, d.path(), &plan_of(&rows), Some(Arc::clone(&log)));
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 9 && s.errors == 0),
        "{st:?}"
    );
    assert_ne!(names_in(d.path()), before);
    let saved = crate::journal::load_rename(jnl.path()).expect("plan is in the journal");
    assert_eq!(saved.base, Location::local(d.path()));
    assert_eq!(saved.rows.len(), 9);
    // «После перезапуска»: новый журнал над тем же каталогом читает план из файла.
    let log = UndoLog::new(Some(jnl.path().to_path_buf()));
    let st = undo_job(&mut h, &log);
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 9 && s.errors == 0),
        "{st:?}"
    );
    assert_eq!(names_in(d.path()), before);
    assert!(log.load().is_none(), "nothing left to undo");
    assert!(crate::journal::load_rename(jnl.path()).is_none());
}

/// В журнал идут только переименованные строки; строка, которую не удалось вернуть
/// (старое имя занято), остаётся в журнале — откат повторяется, когда имя свободно.
#[test]
fn undo_keeps_rows_it_could_not_restore() {
    let d = tmp();
    put(d.path(), &["a", "b", "k", "taken"]);
    let log = UndoLog::new(None);
    let mut h = H::new(8);
    // k → taken упрётся в занятое имя: в журнале — только a → a2 и b → b2.
    let rows = [("a", "a2"), ("b", "b2"), ("k", "taken")];
    let st = job_with(&mut h, d.path(), &plan_of(&rows), Some(Arc::clone(&log)));
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 2 && s.errors == 1),
        "{st:?}"
    );
    let mut saved = log.load().unwrap().rows;
    saved.sort();
    assert_eq!(
        saved,
        [
            ("a".to_owned(), "a2".to_owned()),
            ("b".to_owned(), "b2".to_owned())
        ]
    );
    // Старое имя «a» занято чужим файлом: b2 вернётся, a2 — нет.
    std::fs::write(d.path().join("a"), "foreign").unwrap();
    let st = undo_job(&mut h, &log);
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 1 && s.errors == 1),
        "{st:?}"
    );
    assert_eq!(
        log.load().unwrap().rows,
        [("a".to_owned(), "a2".to_owned())]
    );
    assert_eq!(std::fs::read_to_string(d.path().join("b")).unwrap(), "b");
    std::fs::remove_file(d.path().join("a")).unwrap();
    let st = undo_job(&mut h, &log);
    assert!(
        matches!(&st, JobState::Done(s) if s.items == 1 && s.errors == 0),
        "{st:?}"
    );
    assert_eq!(std::fs::read_to_string(d.path().join("a")).unwrap(), "a");
    assert!(log.load().is_none());
    // Переименование, которое ничего не изменило, прежний план не затирает.
    let st = job_with(
        &mut h,
        d.path(),
        &plan_of(&[("a", "a3")]),
        Some(Arc::clone(&log)),
    );
    assert!(matches!(&st, JobState::Done(s) if s.items == 1), "{st:?}");
    let st = job_with(
        &mut h,
        d.path(),
        &plan_of(&[("k", "taken")]),
        Some(Arc::clone(&log)),
    );
    assert!(matches!(&st, JobState::Done(s) if s.errors == 1), "{st:?}");
    assert_eq!(
        log.load().unwrap().rows,
        [("a".to_owned(), "a3".to_owned())]
    );
}
