//! Экран поиска на фикстуре (S8-07): сцены `--screens` и замер кадра `--bench-search`, без
//! обхода настоящей ФС. Строки — как на макете `screen-search`.
//!
//! `screens.search` — результаты по тексту, курсор в списке; `kind=names`, `kind=dirs` — поиск
//! по имени; `kind=form` — фокус в поле «Текст»; `rows=N` — ряд из N строк для замера.
//!
//! Предпросмотр (S8-09) читает настоящий файл: `copy.rs` макета пишется во временную папку
//! `urfin-screens\search\crates`, она — корень стора; в форме и статусе — `C:\Dev\urfin\crates`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use urfin_core::search::{Hit, PastQuery, SearchModel, SearchStore};
use urfin_core::{Args, Attrs, Command, Cx, Entry, EntryKind, Location, UiEvent};

const ROOT: &str = r"C:\Dev\urfin\crates";
const WORD: &str = "spawn_blocking";
/// Файл, строка, текст до совпадения и после.
const HITS: [(&str, u32, &str, &str); 9] = [
    (
        r"urfin-core\src\ops\copy.rs",
        142,
        "handles.push(pool.",
        "(move || copy_file(src, dst, &cancel)));",
    ),
    (
        r"urfin-core\src\ops\delete.rs",
        57,
        "pool.",
        "(move || remove_tree(path, &cancel))",
    ),
    (
        r"urfin-core\src\ops\rename.rs",
        74,
        "let h = pool.",
        "(move || rename_or_copy(src, dst, &c));",
    ),
    (
        r"urfin-core\src\fs\scan.rs",
        88,
        "io.",
        "(move || read_dir_window(dir, range))",
    ),
    (
        r"urfin-core\src\fs\scan.rs",
        131,
        "io.",
        "(move || stat_batch(paths))",
    ),
    (
        r"urfin-core\src\fs\watch.rs",
        31,
        "self.io.",
        "(move || watch_loop(rx, tx));",
    ),
    (
        r"urfin-view\src\hex.rs",
        64,
        "io.",
        "(move || read_chunk(file, off, len))",
    ),
    (
        r"urfin-edit\src\load.rs",
        23,
        "pool.",
        "(move || Rope::from_reader(f))",
    ),
    (
        r"urfin-edit\src\save.rs",
        40,
        "pool.",
        "(move || atomic_write(path, text))",
    ),
];
const NAMES: [&str; 5] = [
    r"urfin-core\Cargo.toml",
    r"urfin-view\Cargo.toml",
    r"urfin-edit\Cargo.toml",
    r"urfin-ui\Cargo.toml",
    r"urfin-search\Cargo.toml",
];
const DIRS: [&str; 2] = [r"urfin-core\src", r"urfin-view\src"];
/// Кусок ряда замера.
const BENCH_CHUNK: usize = 4096;
/// Строки 130–153 `copy.rs` — как предпросмотр макета; совпадение — строка 142.
const COPY_RS: [&str; 24] = [
    "pub fn copy_many(",
    "    jobs: Vec<CopyJob>,",
    "    pool: &IoPool,",
    "    cancel: CancelToken,",
    "    progress: &Progress,",
    ") -> TaskHandle {",
    "    // UI-поток не делает I/O: всё уходит в io-pool",
    "    let mut handles = Vec::with_capacity(jobs.len());",
    "    for job in jobs {",
    "        let cancel = cancel.clone();",
    "        let progress = progress.clone();",
    "        let (src, dst) = (job.src, job.dst);",
    "        handles.push(pool.spawn_blocking(move || copy_file(src, dst, &cancel)));",
    "        progress.add_total(job.size);",
    "    }",
    "    TaskHandle::join_all(handles, cancel)",
    "}",
    "",
    "fn copy_file(src: PathBuf, dst: PathBuf, cancel: &CancelToken) -> io::Result<u64> {",
    "    if cancel.is_cancelled() {",
    "        return Err(io::ErrorKind::Interrupted.into());",
    "    }",
    "    sys::copy_file_ex(&src, &dst, cancel)",
    "}",
];

fn entry(name: &str, kind: EntryKind) -> Entry {
    Entry {
        name: name.into(),
        size: 2048,
        mtime: 0,
        ctime: 0,
        atime: 0,
        attrs: Attrs::default(),
        kind,
    }
}

fn hit(file: &str, line: u32, pre: &str, post: &str) -> Hit {
    let from = u16::try_from(pre.len()).unwrap_or(0);
    let to = from + u16::try_from(WORD.len()).unwrap_or(0);
    Hit {
        e: entry(file, EntryKind::File),
        root: 0,
        line,
        hl: (from, to),
        preview: Some(format!("{pre}{WORD}{post}").into()),
        status: 0,
    }
}

fn name_hit(name: &str, kind: EntryKind) -> Hit {
    Hit {
        e: entry(name, kind),
        root: 0,
        line: 0,
        hl: (0, 0),
        preview: None,
        status: 0,
    }
}

/// Текст `copy.rs`: до строки 129 — заглушки, дальше — [`COPY_RS`].
fn copy_rs() -> String {
    let mut lines: Vec<String> = vec![
        "//! Копирование: пул ввода-вывода, отмена и прогресс.".into(),
        String::new(),
        "use std::io;".into(),
        "use std::path::PathBuf;".into(),
        String::new(),
    ];
    let mut n = 0;
    while lines.len() < 129 {
        n += 1;
        for l in [
            format!("/// Шаг {n}."),
            format!("fn step_{n}() {{}}"),
            String::new(),
        ] {
            if lines.len() < 129 {
                lines.push(l);
            }
        }
    }
    lines.extend(COPY_RS.iter().map(|l| (*l).to_owned()));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Корень стора фикстуры: папка `crates` с `copy.rs` макета. Не записалось — `C:\Dev\…`
/// (предпросмотр покажет ошибку открытия).
fn fixture_root() -> Location {
    let root = std::env::temp_dir()
        .join("urfin-screens")
        .join("search")
        .join("crates");
    let dir = root.join(r"urfin-core\src\ops");
    let written =
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join("copy.rs"), copy_rs()));
    match written {
        Ok(()) => Location::local(root),
        Err(e) => {
            tracing::warn!(error = %e, "demo search file");
            Location::local(ROOT)
        }
    }
}

/// Ряд замера: все строки разные, кэш раскладки текста не подыгрывает.
fn bench_rows(store: &SearchStore, rows: usize) {
    let mut done = 0;
    while done < rows {
        let n = BENCH_CHUNK.min(rows - done);
        store.append(
            (done..done + n)
                .map(|i| {
                    let (file, line, pre, post) = HITS[i % HITS.len()];
                    let file = format!("{file}.{}", i / 7);
                    let post = format!("{post} // {i}");
                    hit(
                        &file,
                        line + u32::try_from(i % 1000).unwrap_or(0),
                        pre,
                        &post,
                    )
                })
                .collect(),
        );
        done += n;
    }
}

/// Запрос, стор и экран фикстуры.
pub(crate) fn run(cmd: &Command, cx: &mut Cx<'_>) {
    let kind = cmd.args.str("kind").unwrap_or_default();
    let rows = cmd.args.int("rows").and_then(|v| usize::try_from(v).ok());
    if cx.state.ext.get::<SearchModel>().is_none() {
        cx.state.ext.insert(SearchModel::default());
    }
    let (name, text, kinds) = match kind {
        "names" => ("*.toml", "", "files"),
        "dirs" => ("src", "", "dirs"),
        _ => ("*.rs", WORD, "files"),
    };
    let Some(m) = cx.state.ext.get_mut::<SearchModel>() else {
        return;
    };
    m.show(None);
    // Файл предпросмотра — только сценам с текстом; замеру и поиску по имени он не нужен.
    let root = if matches!(kind, "names" | "dirs") || rows.is_some() {
        Location::local(ROOT)
    } else {
        fixture_root()
    };
    let store = m.open(vec![root]);
    let hits: Vec<Hit> = match kind {
        "names" => NAMES.iter().map(|n| name_hit(n, EntryKind::File)).collect(),
        "dirs" => DIRS.iter().map(|n| name_hit(n, EntryKind::Dir)).collect(),
        _ if rows.is_some() => Vec::new(),
        _ => HITS
            .iter()
            .map(|&(file, line, pre, post)| hit(file, line, pre, post))
            .collect(),
    };
    store.append(hits);
    if let Some(n) = rows {
        bench_rows(&store, n);
    }
    store.info.files.store(1284, Ordering::Relaxed);
    store.finish(400);
    m.remember(PastQuery {
        name: name.into(),
        text: text.into(),
        place: ROOT.into(),
        case: true,
        regex: false,
        recursive: true,
    });
    m.show(Some(store.id));
    let id = Command::new("search.screen").id;
    // Сцены идут подряд: форма — заново, с полями своего запроса.
    cx.ui(UiEvent::Ext {
        id: id.clone(),
        args: Args::default().with("close", true),
    });
    let args = Args::default()
        .with("name", name)
        .with("text", text)
        .with("where", ROOT)
        .with("case", true)
        .with("regex", false)
        .with("recursive", true)
        .with("kinds", kinds)
        .with("zone", if kind == "form" { "text" } else { "list" });
    cx.ui(UiEvent::Ext { id, args });
}

// ---------------------------------------------------------------------------
// Панель результатов `search:` (S8-11): `screen-searchpanel`, `screen-searchgrep`.

/// Сторы сцен — в реестр панели `search:`. Реестр держит `urfin-app`: `urfin-ui` от
/// `urfin-search` не зависит (решение 2026-11-06 «s8-answers», В1).
pub type Adopt = Box<dyn Fn(&Arc<SearchStore>, bool) + Send + Sync>;

static ADOPT: OnceLock<Adopt> = OnceLock::new();
/// Стор сцены, где поиск «идёт»: следующая сцена его закончит (листинг ждёт новых строк).
static RUNNING: Mutex<Option<Arc<SearchStore>>> = Mutex::new(None);

/// `--screens`: чем стор сцены попадает в реестр панели `search:` (зовёт `urfin-app`).
pub fn set_adopt(f: Adopt) {
    let _ = ADOPT.set(f);
}

const PANEL_ROOT: &str = r"C:\Dev";
/// Путь от корня, размер, дата и время — как на `screen-searchpanel`.
const PANEL_HITS: [(&str, u64, &str); 23] = [
    (r"pets\.cargo\config.toml", 307, "02.09.26 14:20"),
    (r"pets\Cargo.toml", 922, "20.10.26 18:44"),
    (
        r"pets\crates\pets-commands\Cargo.toml",
        410,
        "14.10.26 10:02",
    ),
    (r"pets\crates\pets-ui\Cargo.toml", 717, "20.10.26 18:44"),
    (r"pets\rust-toolchain.toml", 96, "02.09.26 14:20"),
    (r"postman\Cargo.toml", 819, "19.10.26 21:05"),
    (
        r"postman\crates\postman-imap\Cargo.toml",
        512,
        "19.10.26 21:05",
    ),
    (
        r"postman\crates\postman-store\Cargo.toml",
        410,
        "17.10.26 16:30",
    ),
    (r"postman\rust-toolchain.toml", 96, "02.09.26 14:20"),
    (r"urfin\.config\nextest.toml", 614, "11.10.26 12:15"),
    (r"urfin\Cargo.toml", 1229, "19.10.26 22:40"),
    (r"urfin\clippy.toml", 102, "15.10.26 10:11"),
    (r"urfin\crates\urfin-app\Cargo.toml", 1126, "20.10.26 13:50"),
    (r"urfin\crates\urfin-arc\Cargo.toml", 819, "21.10.26 09:30"),
    (r"urfin\crates\urfin-core\Cargo.toml", 922, "19.10.26 22:40"),
    (r"urfin\crates\urfin-i18n\Cargo.toml", 410, "12.10.26 18:20"),
    (r"urfin\crates\urfin-ops\Cargo.toml", 717, "18.10.26 17:05"),
    (
        r"urfin\crates\urfin-search\Cargo.toml",
        922,
        "21.10.26 09:30",
    ),
    (r"urfin\crates\urfin-ui\Cargo.toml", 1024, "20.10.26 13:50"),
    (r"urfin\crates\urfin-vfs\Cargo.toml", 1024, "19.10.26 22:40"),
    (r"urfin\crates\urfin-view\Cargo.toml", 614, "16.10.26 11:47"),
    (r"urfin\deny.toml", 2048, "15.10.26 10:11"),
    (r"urfin\rust-toolchain.toml", 96, "02.09.26 14:20"),
];
const GREP_ROOT: &str = r"C:\Dev\urfin\crates";
const GREP_WORD: &str = "Lane::Bulk";
/// Путь, размер, дата, строка, текст до совпадения и после — как на `screen-searchgrep`.
const GREP_HITS: [(&str, u64, &str, u32, &str, &str); 9] = [
    (
        r"urfin-arc\src\extract.rs",
        9318,
        "21.10.26 09:30",
        57,
        "rt.spawn(",
        ", move |c| extract_plan(arc, sel, dest, c))",
    ),
    (
        r"urfin-core\src\runtime.rs",
        12_698,
        "19.10.26 22:41",
        42,
        "/// ",
        ": хэши, размеры папок, F5/F8 — не вытесняет Interactive",
    ),
    (
        r"urfin-ops\src\compare.rs",
        7885,
        "20.10.26 11:02",
        91,
        "let lane = if recursive { ",
        " } else { Lane::Interactive };",
    ),
    (
        r"urfin-ops\src\copy.rs",
        21_811,
        "20.10.26 13:55",
        118,
        "let job = rt.spawn(",
        ", move |c| copy_tree(plan, c));",
    ),
    (
        r"urfin-ops\src\delete.rs",
        8397,
        "18.10.26 17:05",
        64,
        "rt.spawn(",
        ", move |c| remove_tree(items, c))",
    ),
    (
        r"urfin-search\src\svfs.rs",
        10_854,
        "21.10.26 09:30",
        133,
        "rt.spawn(",
        ", move |c| restat_hits(store, 8, c));",
    ),
    (
        r"urfin-ui\src\info.rs",
        6451,
        "20.10.26 19:58",
        88,
        "self.rt.spawn(",
        ", move |c| owner_and_acl(path, c));",
    ),
    (
        r"urfin-ui\src\panel.rs",
        35_635,
        "20.10.26 19:58",
        512,
        "self.rt.spawn(",
        ", move |c| dir_sizes(sel, c));",
    ),
    (
        r"urfin-vfs\src\local\size.rs",
        3482,
        "19.10.26 22:41",
        29,
        "let h = rt.spawn(",
        ", move |c| dir_size(path, c));",
    ),
];

/// `дд.мм.гг чч:мм` по московскому времени (стенд владельца) — наносекунды Unix.
pub(crate) fn stamp(text: &str) -> i64 {
    let n: Vec<i64> = text
        .split(['.', ' ', ':'])
        .filter_map(|p| p.parse().ok())
        .collect();
    let [d, m, y, hh, mm] = n[..] else {
        return 0;
    };
    // Дни от 1970-01-01 (алгоритм «days from civil»).
    let y = 2000 + y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + hh * 3600 + mm * 60 - 3 * 3600) * 1_000_000_000
}

fn panel_hit(name: &str, size: u64, when: &str) -> Hit {
    let mut h = name_hit(name, EntryKind::File);
    h.e.size = size;
    h.e.mtime = stamp(when);
    h
}

/// Панель результатов на фикстуре: `panel` — поиск по имени идёт (`screen-searchpanel`),
/// `grep` — поиск по тексту готов (`screen-searchgrep`). Новая вкладка активной панели.
pub(crate) fn panel(kind: &str, cx: &mut Cx<'_>) {
    if cx.state.ext.get::<SearchModel>().is_none() {
        cx.state.ext.insert(SearchModel::default());
    }
    let Some(m) = cx.state.ext.get_mut::<SearchModel>() else {
        return;
    };
    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(st) = running.take() {
        st.finish(0);
    }
    let grep = kind == "grep";
    let root = if grep { GREP_ROOT } else { PANEL_ROOT };
    let store = m.open(vec![Location::local(root)]);
    let hits: Vec<Hit> = if grep {
        GREP_HITS
            .iter()
            .map(|&(name, size, when, line, pre, post)| {
                let mut h = panel_hit(name, size, when);
                let from = u16::try_from(pre.len()).unwrap_or(0);
                h.line = line;
                h.hl = (from, from + u16::try_from(GREP_WORD.len()).unwrap_or(0));
                h.preview = Some(format!("{pre}{GREP_WORD}{post}").into());
                h
            })
            .collect()
    } else {
        PANEL_HITS
            .iter()
            .map(|&(name, size, when)| panel_hit(name, size, when))
            .collect()
    };
    store.append(hits);
    store.info.files.store(4812, Ordering::Relaxed);
    if grep {
        store.finish(600);
    } else {
        *running = Some(Arc::clone(&store));
    }
    drop(running);
    if let Some(adopt) = ADOPT.get() {
        adopt(&store, grep);
    }
    let id = store.id;
    let (name, text) = if grep {
        ("*.rs", GREP_WORD)
    } else {
        ("*.toml", "")
    };
    cx.dispatch(
        Command::new("ui.search.label")
            .arg("id", i64::from(id.0))
            .arg("name", name)
            .arg("text", text)
            .arg("where", root)
            .arg("kinds", "files"),
    );
    cx.dispatch(Command::new("tab.new"));
    let loc = Location {
        backend: crate::search_panel::SCHEME.into(),
        authority: None,
        path: format!("/{}", id.0).into(),
        outer: None,
    };
    cx.dispatch(Command::new("panel.go").arg("path", loc));
}
