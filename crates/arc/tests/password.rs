//! Вопрос пароля (S8-35, S8 §3.7) в core с настоящими архивами: F3/F5 по зашифрованной
//! записи zip, вход в 7z `-mhe` и rar `-hp`, три попытки, Esc; `arc.test` (отчёт с
//! именами битых файлов, пароль через `JobHandle::ask`) и `arc.commands`. Пароль не
//! попадает в логи (`tracing`), историю и сессию.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use urfin_arc::{ArcCache, ArcCfg, root_of};
use urfin_core::{
    Args, Boot, Command, Core, JobId, JobState, ListingStatus, Location, PromptId, PromptSpec,
    Services, Session, UiEvent, Value, Waker,
};
use urfin_vfs::{VfsCfg, VfsError, VfsListing, VfsRegistry};
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipWriter};

/// Пароли тестов: их не должно быть ни в логах, ни в сессии.
const GOOD: &str = "Pw-s3cr3t-Zq";
const BAD: &str = "Wr0ng-Pw-Qx";

// --- Логи: свой subscriber пишет все поля всех событий и span'ов в строку. ---

static LOG: Mutex<String> = Mutex::new(String::new());

struct Fields;

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        let mut g = LOG.lock().unwrap();
        let _ = write!(g, "{}={v:?} ", f.name());
    }
}

struct Capture;

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, a: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        a.record(&mut Fields);
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, v: &tracing::span::Record<'_>) {
        v.record(&mut Fields);
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, e: &tracing::Event<'_>) {
        LOG.lock().unwrap().push_str(e.metadata().name());
        e.record(&mut Fields);
        LOG.lock().unwrap().push('\n');
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

fn capture() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| tracing::subscriber::set_global_default(Capture).unwrap());
}

/// Пароль не попал в логи, сессию и состояние вопросов.
fn no_leak(core: &Core) {
    let log = LOG.lock().unwrap().clone();
    assert!(!log.is_empty(), "subscriber что-то записал");
    let session = toml::to_string(&Session::capture(core.state(), None)).unwrap();
    let prompts = format!("{:?}", core.state().prompts);
    for pw in [GOOD, BAD] {
        assert!(!log.contains(pw), "пароль в логе");
        assert!(!session.contains(pw), "пароль в сессии");
        assert!(!prompts.contains(pw), "пароль в вопросах");
    }
}

// --- Стенд ---

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
    fn wake_after(&self, _d: Duration) {}
}

struct Rig {
    core: Core,
    reg: Arc<VfsRegistry>,
    cache: Arc<ArcCache>,
    events: Vec<UiEvent>,
    last: Option<PromptId>,
}

fn rig(dir: &Path) -> Rig {
    capture();
    urfin_i18n::set_language("en");
    let cfg = ArcCfg::default();
    let cache = urfin_arc::cache(&cfg);
    let mut reg = VfsRegistry::with_local();
    for b in urfin_arc::backends(&cache, &cfg) {
        reg.register(b);
    }
    let reg = Arc::new(reg);
    let services = Services {
        listing: Arc::new(VfsListing::new(Arc::clone(&reg), VfsCfg::default())),
        open_resolvers: Vec::new(),
    };
    let here = Location::local(dir);
    let mut core = Core::new(Boot::new(here.clone(), here), services, Arc::new(NoWake));
    urfin_arc::register(&mut core, Arc::clone(&reg), Arc::clone(&cache), &cfg);
    let mut r = Rig {
        core,
        reg,
        cache,
        events: Vec::new(),
        last: None,
    };
    r.until("каталог", |r| !loading(r));
    r
}

fn loading(r: &Rig) -> bool {
    matches!(
        r.core.state().active_tab().listing.status,
        ListingStatus::Loading { .. } | ListingStatus::Idle
    )
}

impl Rig {
    fn until(&mut self, what: &str, mut done: impl FnMut(&Rig) -> bool) {
        let t = Instant::now();
        loop {
            self.core.pump(Duration::from_millis(5));
            let ev = self.core.take_ui_events();
            self.events.extend(ev);
            if done(self) {
                return;
            }
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "не дождались: {what}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Следующий вопрос: id, описание, задача.
    fn prompt(&mut self) -> (PromptId, PromptSpec, Option<JobId>) {
        let last = self.last;
        self.until("вопрос пароля", |r| {
            r.core
                .state()
                .prompts
                .active()
                .is_some_and(|p| Some(p.id) != last)
        });
        let p = self.core.state().prompts.active().unwrap();
        self.last = Some(p.id);
        (p.id, p.spec.clone(), p.job)
    }

    fn reply(&mut self, id: PromptId, pw: &str) {
        self.core.dispatch(
            Command::new("prompt.reply")
                .arg("id", i64::try_from(id.0).unwrap())
                .arg("choice", "ok")
                .arg("input", pw),
        );
    }

    fn esc(&mut self, id: PromptId) {
        self.core
            .dispatch(Command::new("prompt.cancel").arg("id", i64::try_from(id.0).unwrap()));
    }

    /// Пока ждём `h`, крутим core.
    fn join<T>(&mut self, h: std::thread::JoinHandle<T>) -> T {
        self.until("поток", |_| h.is_finished());
        h.join().unwrap()
    }

    fn go(&mut self, loc: Location) {
        self.core
            .dispatch(Command::new("panel.go").arg("path", loc));
    }

    fn point(&mut self, name: &str) {
        self.core
            .dispatch(Command::new("panel.cursor.to").arg("name", name));
    }

    /// Последнее `UiEvent::Ext` с `id`.
    fn ext(&mut self, id: &str) -> Args {
        self.until(id, |r| {
            r.events
                .iter()
                .any(|e| matches!(e, UiEvent::Ext { id: i, .. } if i.as_str() == id))
        });
        self.events
            .iter()
            .rev()
            .find_map(|e| match e {
                UiEvent::Ext { id: i, args } if i.as_str() == id => Some(args.clone()),
                _ => None,
            })
            .unwrap()
    }

    fn toasts(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| match e {
                UiEvent::Toast { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("urfin-arc-pw-")
        .tempdir()
        .unwrap()
}

fn make_zip(path: &Path, files: &[(&str, &[u8])], aes: Option<&str>, method: CompressionMethod) {
    let mut z = ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, body) in files {
        let mut o = SimpleFileOptions::default().compression_method(method);
        if let Some(pw) = aes {
            o = o.with_aes_encryption(AesMode::Aes256, pw);
        }
        z.start_file(*name, o).unwrap();
        z.write_all(body).unwrap();
    }
    z.finish().unwrap();
}

fn fixture(name: &str) -> Location {
    Location::local(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
}

fn entry(scheme: &str, file: &Path, path: &str) -> Location {
    Location {
        path: path.into(),
        ..root_of(scheme, Location::local(file))
    }
}

fn read(reg: &VfsRegistry, at: &Location) -> Result<Vec<u8>, VfsError> {
    let mut r = reg.get(at)?.open_read(at)?;
    let mut out = Vec::new();
    r.read_to_end(&mut out)
        .map_err(|e| VfsError::from_io(at.path.clone(), e))?;
    Ok(out)
}

fn names(r: &Rig) -> Vec<String> {
    let mut v: Vec<String> = r
        .core
        .state()
        .active_tab()
        .listing
        .entries
        .iter()
        .map(|e| e.name.to_string())
        .collect();
    v.sort();
    v
}

fn denied(e: &VfsError) -> bool {
    matches!(e, VfsError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied)
}

// --- F3 и F5 по зашифрованной записи ---

#[test]
fn f3_asks_three_times_and_keeps_the_password_out_of_logs_and_session() {
    let dir = tmp();
    let file = dir.path().join("s.zip");
    let big = vec![7u8; 5000];
    make_zip(
        &file,
        &[("big.bin", &big), ("t.txt", b"tiny")],
        Some(GOOD),
        CompressionMethod::Deflated,
    );
    let mut r = rig(dir.path());
    r.go(root_of("zip", Location::local(&file)));
    r.until("архив", |r| !loading(r) && names(r).len() == 2);

    // F3: вьювер читает запись в своём потоке.
    let (reg, at) = (Arc::clone(&r.reg), entry("zip", &file, "/big.bin"));
    let h = std::thread::spawn(move || read(&reg, &at));
    let (id, spec, job) = r.prompt();
    assert!(spec.secret, "секретный ввод");
    assert_eq!(spec.input.as_deref(), Some(""));
    assert_eq!(job, None, "вопрос потока VFS — без задачи");
    assert_eq!(spec.title, "Password for s.zip");
    assert!(spec.body.is_empty(), "{spec:?}");
    r.reply(id, BAD);
    let (id, spec, _) = r.prompt();
    assert_eq!(spec.body, "Wrong password — 2 attempts left");
    r.reply(id, "");
    let (id, spec, _) = r.prompt();
    assert_eq!(
        spec.body, "Wrong password — 2 attempts left",
        "пустой — не попытка"
    );
    r.reply(id, GOOD);
    assert_eq!(r.join(h).unwrap(), big);

    // Пароль — в индексе: вторая запись без вопроса.
    let idx = r.cache.latest(&Location::local(&file)).unwrap();
    assert!(idx.ctx().has_password());
    assert_eq!(
        read(&r.reg, &entry("zip", &file, "/t.txt")).unwrap(),
        b"tiny"
    );
    assert!(r.core.state().prompts.active().is_none());
    no_leak(&r.core);
}

#[test]
fn f5_plan_asks_once_and_esc_or_three_misses_are_remembered() {
    let dir = tmp();
    let (za, zb) = (dir.path().join("a.zip"), dir.path().join("b.zip"));
    let files: [(&str, &[u8]); 2] = [("1.txt", b"one"), ("2.txt", b"two")];
    make_zip(&za, &files, Some(GOOD), CompressionMethod::Deflated);
    make_zip(&zb, &files, Some(GOOD), CompressionMethod::Deflated);
    let mut r = rig(dir.path());
    r.go(root_of("zip", Location::local(&za)));
    r.until("архив", |r| !loading(r) && names(r).len() == 2);

    // F5: job ops берёт план выборки — вопрос один на все файлы.
    let reg = Arc::clone(&r.reg);
    let locs = [entry("zip", &za, "/1.txt"), entry("zip", &za, "/2.txt")];
    let h = std::thread::spawn(move || {
        let v = reg.get(&locs[0]).unwrap();
        let mut got = Vec::new();
        for mut s in v.extract_plan(&locs, 1).unwrap() {
            while let Some((i, mut rd)) = s.next(&urfin_core::CancelToken::new()).unwrap() {
                let mut buf = Vec::new();
                rd.read_to_end(&mut buf).unwrap();
                got.push((i, buf));
            }
        }
        got.sort();
        got
    });
    let (id, _, _) = r.prompt();
    r.reply(id, GOOD);
    assert_eq!(
        r.join(h),
        [(0, b"one".to_vec()), (1, b"two".to_vec())],
        "данные обоих файлов"
    );

    // Esc: запись не читается, и следующая сразу — без второго вопроса.
    r.go(root_of("zip", Location::local(&zb)));
    r.until("архив b", |r| !loading(r) && names(r).len() == 2);
    let (reg, at) = (Arc::clone(&r.reg), entry("zip", &zb, "/1.txt"));
    let h = std::thread::spawn(move || read(&reg, &at));
    let (id, _, _) = r.prompt();
    r.esc(id);
    assert!(denied(&r.join(h).unwrap_err()));
    let e = read(&r.reg, &entry("zip", &zb, "/2.txt")).unwrap_err();
    assert!(denied(&e), "{e}");
    assert!(r.core.state().prompts.active().is_none(), "отказ помнится");
    no_leak(&r.core);
}

#[test]
fn ui_thread_never_waits_for_a_password() {
    let dir = tmp();
    let file = dir.path().join("s.zip");
    make_zip(
        &file,
        &[("t.txt", b"tiny")],
        Some(GOOD),
        CompressionMethod::Deflated,
    );
    let mut r = rig(dir.path());
    r.go(root_of("zip", Location::local(&file)));
    r.until("архив", |r| !loading(r) && names(r).len() == 1);
    // Поток core (тест) — UI: вопроса нет, сразу ошибка.
    let e = read(&r.reg, &entry("zip", &file, "/t.txt")).unwrap_err();
    assert!(denied(&e), "{e}");
    r.core.pump(Duration::from_millis(5));
    assert!(r.core.state().prompts.active().is_none());
}

// --- Вход в архив с зашифрованными именами ---

fn enter_with_names_password(file: &str, scheme: &str, good: &str, want: &[&str]) {
    let outer = fixture(file);
    let dir = outer.path.parent().unwrap().to_path_buf();
    let mut r = rig(&dir);
    r.go(root_of(scheme, outer.clone()));
    let (id, spec, job) = r.prompt();
    assert!(spec.secret);
    assert_eq!(job, None);
    assert_eq!(
        spec.body,
        urfin_i18n::current().get("arc-error-no-password")
    );
    r.reply(id, BAD);
    let (id, spec, _) = r.prompt();
    assert_eq!(spec.body, "Wrong password — 2 attempts left");
    r.reply(id, good);
    r.until("листинг", |r| !loading(r) && !names(r).is_empty());
    let tab = r.core.state().active_tab();
    assert_eq!(tab.loc, root_of(scheme, outer.clone()));
    assert!(
        matches!(tab.listing.status, ListingStatus::Complete),
        "{:?}",
        tab.listing.status
    );
    assert_eq!(names(&r), want);
    let idx = r.cache.latest(&outer).unwrap();
    assert!(idx.ctx().has_password(), "пароль имён — и данным");
    no_leak(&r.core);
}

#[test]
fn entering_7z_with_encrypted_names_asks_before_listing() {
    enter_with_names_password(
        "aes-names.7z",
        "7z",
        "secret",
        &["b.bin", "dir", "empty.txt", "prog.exe", "Отчёт.txt"],
    );
}

#[cfg(feature = "rar")]
#[test]
fn entering_rar_with_encrypted_headers_asks_before_listing() {
    enter_with_names_password(
        "rar/comment-hpw-password.rar",
        "rar",
        "password",
        &[".gitignore"],
    );
}

#[test]
fn three_wrong_names_passwords_leave_the_error_and_a_toast() {
    let outer = fixture("aes-names.7z");
    let dir = outer.path.parent().unwrap().to_path_buf();
    let mut r = rig(&dir);
    r.go(root_of("7z", outer.clone()));
    for left in [3, 2, 1] {
        let (id, spec, _) = r.prompt();
        if left < 3 {
            assert!(spec.body.contains(&left.to_string()), "{spec:?}");
        }
        r.reply(id, BAD);
    }
    r.until("ошибка листинга", |r| {
        matches!(
            r.core.state().active_tab().listing.status,
            ListingStatus::Failed(_)
        )
    });
    assert!(
        r.toasts()
            .iter()
            .any(|t| t == "Wrong password — no attempts left"),
        "{:?}",
        r.toasts()
    );
    assert!(r.cache.latest(&outer).is_none());
    no_leak(&r.core);
}

// --- arc.test и arc.commands ---

/// Zip без сжатия: в `bad.txt` испорчен байт данных — CRC не сойдётся.
fn broken_zip(path: &Path) {
    let body = b"0123456789abcdefghij-BROKEN-PAYLOAD-0123456789";
    make_zip(
        path,
        &[
            ("ok/1.txt", b"first file"),
            ("bad.txt", body),
            ("ok/2.txt", b"second file"),
        ],
        None,
        CompressionMethod::Stored,
    );
    let mut bytes = std::fs::read(path).unwrap();
    let at = bytes
        .windows(body.len())
        .position(|w| w == body)
        .expect("данные stored лежат как есть");
    bytes[at + 25] ^= 0x20;
    std::fs::write(path, bytes).unwrap();
}

fn job_of(r: &Rig, kind: &str) -> Option<JobState> {
    r.core
        .state()
        .jobs
        .iter()
        .find(|j| j.kind == kind)
        .map(|j| j.state.clone())
}

#[test]
fn arc_test_reports_broken_files_by_name() {
    let dir = tmp();
    broken_zip(&dir.path().join("broken.zip"));
    make_zip(
        &dir.path().join("good.zip"),
        &[("a.txt", b"aaa"), ("b/c.txt", b"ccc")],
        None,
        CompressionMethod::Deflated,
    );
    let mut r = rig(dir.path());
    r.until("каталог", |r| names(r).len() == 2);

    r.point("broken.zip");
    r.core.dispatch(Command::new("arc.test"));
    let t = r.ext("arc.tested");
    assert_eq!(t.str("name"), Some("broken.zip"));
    assert_eq!(t.int("count"), Some(3));
    assert_eq!(t.int("errors"), Some(1));
    assert_eq!(
        t.get("bad"),
        Some(&Value::List(vec![Value::Str("bad.txt".into())].into()))
    );
    assert!(
        r.toasts()
            .iter()
            .any(|s| s == "Errors in 1 file of 3: bad.txt"),
        "{:?}",
        r.toasts()
    );
    r.until("итог задачи", |r| {
        matches!(job_of(r, "arc.test"), Some(JobState::Done(_)))
    });
    let Some(JobState::Done(s)) = job_of(&r, "arc.test") else {
        unreachable!()
    };
    assert_eq!((s.items, s.errors), (3, 1));

    // Внутри архива — сам архив.
    r.events.clear();
    r.go(root_of("zip", Location::local(dir.path().join("good.zip"))));
    r.until("архив", |r| !loading(r) && names(r).len() == 2);
    r.core.dispatch(Command::new("arc.test"));
    let t = r.ext("arc.tested");
    assert_eq!((t.int("count"), t.int("errors")), (Some(2), Some(0)));
    assert!(
        r.toasts().iter().any(|s| s == "Tested 2 files — no errors"),
        "{:?}",
        r.toasts()
    );
}

#[test]
fn arc_test_asks_the_password_through_its_job() {
    let dir = tmp();
    let file = dir.path().join("s.zip");
    make_zip(
        &file,
        &[("a.txt", b"aaa"), ("b.txt", b"bbbb")],
        Some(GOOD),
        CompressionMethod::Deflated,
    );
    let mut r = rig(dir.path());
    r.until("каталог", |r| names(r).len() == 1);
    r.point("s.zip");
    r.core.dispatch(Command::new("arc.test"));
    let (id, spec, job) = r.prompt();
    assert!(spec.secret);
    assert!(job.is_some(), "JobHandle::ask — вопрос задачи");
    assert!(matches!(job_of(&r, "arc.test"), Some(JobState::Waiting(_))));
    r.reply(id, BAD);
    let (id, _, _) = r.prompt();
    r.reply(id, GOOD);
    let t = r.ext("arc.tested");
    assert_eq!((t.int("count"), t.int("errors")), (Some(2), Some(0)));
    no_leak(&r.core);
}

#[test]
fn arc_commands_sends_the_menu() {
    let dir = tmp();
    make_zip(
        &dir.path().join("a.zip"),
        &[("1.txt", b"one")],
        None,
        CompressionMethod::Deflated,
    );
    let mut r = rig(dir.path());
    r.until("каталог", |r| names(r).len() == 1);
    r.point("a.zip");
    r.core.dispatch(Command::new("arc.commands"));
    let m = r.ext("arc.commands");
    let want: Vec<Value> = urfin_arc::MENU
        .iter()
        .map(|c| Value::Str((*c).into()))
        .collect();
    assert_eq!(m.get("commands"), Some(&Value::List(want.into())));
    assert_eq!(m.str("name"), Some("a.zip"));
    assert_eq!(m.bool("inside"), Some(false));
    assert_eq!(urfin_arc::MENU, ["arc.test", "arc.extract"]);
}
