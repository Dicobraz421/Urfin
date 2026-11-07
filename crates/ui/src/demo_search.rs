//! Экран поиска на фикстуре (S8-07): сцены `--screens` и замер кадра `--bench-search`, без
//! обхода настоящей ФС. Строки — как на макете `screen-search`.
//!
//! `screens.search` — результаты по тексту, курсор в списке; `kind=names`, `kind=dirs` — поиск
//! по имени; `kind=form` — фокус в поле «Текст»; `rows=N` — ряд из N строк для замера.
//!
//! Предпросмотр (S8-09) читает настоящий файл: `copy.rs` макета пишется во временную папку
//! `urfin-screens\search\crates`, она — корень стора; в форме и статусе — `C:\Dev\urfin\crates`.

use std::sync::atomic::Ordering;

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
