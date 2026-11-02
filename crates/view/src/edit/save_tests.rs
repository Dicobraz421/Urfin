//! S7-17 / S7-19: сохранение на диск (атомарная замена, атрибуты, ACL, жёсткие ссылки,
//! сбой посреди записи), вопросы и ответы через core, вето выхода, задача больше 8 мебибайт,
//! автосохранение, новый файл, F6 туда-обратно, внешний редактор. Всё — во временных
//! каталогах.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use encoding_rs::{UTF_8, WINDOWS_1251};
use urfin_core::{
    ArgSpec, Boot, CmdError, Command, CommandMeta, Core, Cx, Focus, Handler, Location,
    QuitDecision, Services, ValueKind,
};

use super::super::save::{
    Body, Fault, How, Outcome, ReadOnly, SaveError, SaveReq, SnapBody, Unmappable, save,
};
use super::super::{DiskStamp, EdAsk, EdState};
use super::{NoWake, ed, open, pump_until, send, set_text, view};
use crate::config::Autosave;
use crate::{EditorCfg, InboxMsg, ViewerCfg, register_with};

/// Снимок документа из UTF-8 текста (один кусок исходника в памяти).
fn body(text: &str) -> Body {
    let src: Arc<dyn crate::source::ByteSource> =
        Arc::new(crate::source::OwnedSource::new(text.as_bytes().to_vec()));
    let sep = crate::index::Sep::of(&crate::enc::Enc::utf8(), crate::detect::Eol::Lf);
    let doc = super::super::piece::TextDoc::new(
        src,
        0,
        sep,
        super::super::piece::AddBuf::new(0),
        crate::index::LineIndex::default(),
    );
    Body::Snap(SnapBody {
        snap: doc.snap(),
        from: crate::enc::Enc::utf8(),
        eol: crate::detect::Eol::Lf,
        space: true,
        reopen: false,
    })
}

fn req(path: &Path, text: &str) -> SaveReq {
    SaveReq {
        text: body(text),
        enc: UTF_8,
        bom: false,
        path: path.to_path_buf(),
        check: None,
        unmappable: Unmappable::Ask,
        readonly: ReadOnly::Ask,
        fault: None,
    }
}

fn run(r: &SaveReq) -> Result<Outcome, SaveError> {
    save(r, &|| Ok(()), &mut |_| {})
}

fn stamp(path: &Path) -> DiskStamp {
    super::super::save::stamp_of(path).unwrap().unwrap()
}

fn icacls(path: &Path) -> String {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `ReplaceFileW`: содержимое новое, ACL (лишняя запись «Все: чтение») и атрибут
/// «скрытый» — прежние; временного файла нет.
#[test]
fn replace_keeps_acl_and_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf.ini");
    std::fs::write(&path, "a=1\r\n").unwrap();
    let st = std::process::Command::new("icacls")
        .arg(&path)
        .arg("/grant")
        .arg("*S-1-1-0:(R)")
        .output()
        .unwrap();
    assert!(st.status.success(), "icacls /grant");
    crate::sys::set_attrs(&path, crate::sys::ATTR_HIDDEN | 0x20).unwrap();
    let acl = icacls(&path);
    let mut r = req(&path, "a=2\r\nb=3\r\n");
    r.check = Some(stamp(&path));
    let out = run(&r).unwrap();
    assert!(
        matches!(
            out,
            Outcome::Saved {
                how: How::Replaced,
                readonly: false,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\r\nb=3\r\n");
    let attrs = crate::sys::attrs(&path).unwrap();
    assert_ne!(attrs & crate::sys::ATTR_HIDDEN, 0, "скрытый остался");
    assert_eq!(icacls(&path), acl, "ACL прежний");
    assert!(!super::super::save::temp_path(&path).exists());
    let Outcome::Saved { stamp: s, .. } = out else {
        unreachable!()
    };
    assert_eq!(s, stamp(&path));
}

/// Обрыв после записи временного файла: исходный файл не тронут; следующее сохранение
/// перезаписывает брошенный временный файл.
#[test]
fn fault_after_temp_keeps_original() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.txt");
    std::fs::write(&path, "old").unwrap();
    let mut r = req(&path, "new text");
    r.fault = Some(Fault::AfterTemp);
    assert!(matches!(run(&r), Err(SaveError::Io(_))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    let temp = super::super::save::temp_path(&path);
    assert_eq!(std::fs::read_to_string(&temp).unwrap(), "new text");
    r.fault = None;
    r.text = body("again");
    assert!(matches!(run(&r).unwrap(), Outcome::Saved { .. }));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "again");
    assert!(!temp.exists());
}

/// Жёсткая ссылка остаётся ссылкой (запись на месте); новый файл — через временный.
#[test]
fn hardlink_in_place_and_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "one").unwrap();
    std::fs::hard_link(&a, &b).unwrap();
    let out = run(&req(&a, "two, longer")).unwrap();
    assert!(
        matches!(
            out,
            Outcome::Saved {
                how: How::InPlace,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "two, longer");
    let f = std::fs::File::open(&a).unwrap();
    assert_eq!(crate::sys::file_info(&f).unwrap().nlink, 2);
    drop(f);

    let n = dir.path().join("new.md");
    let out = run(&req(&n, "# t\n")).unwrap();
    assert!(
        matches!(
            out,
            Outcome::Saved {
                how: How::Moved,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(std::fs::read_to_string(&n).unwrap(), "# t\n");
}

/// Symlink: пишется цель, ссылка остаётся ссылкой. Без права создавать ссылки
/// (Windows без режима разработчика) — пропуск.
#[test]
fn symlink_writes_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.txt");
    let link = dir.path().join("link.txt");
    std::fs::write(&target, "t0").unwrap();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file("real.txt", &link);
    #[cfg(not(windows))]
    let made = std::os::unix::fs::symlink("real.txt", &link);
    if made.is_err() {
        eprintln!("symlink: нет права, пропуск");
        return;
    }
    let mut r = req(&link, "t1");
    r.check = Some(stamp(&link));
    assert!(matches!(run(&r).unwrap(), Outcome::Saved { .. }));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "t1");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// Сверка `DiskStamp`, атрибут «только чтение» и непредставимые символы: вопрос, файл
/// не тронут; по ответу — запись.
#[test]
fn asks_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ru.txt");
    std::fs::write(&path, "old").unwrap();
    let before = stamp(&path);
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "changed outside").unwrap();
    let mut r = req(&path, "mine");
    r.check = Some(before);
    assert_eq!(run(&r).unwrap(), Outcome::Changed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed outside");
    r.check = None;

    crate::sys::set_attrs(&path, crate::sys::ATTR_READONLY).unwrap();
    assert_eq!(run(&r).unwrap(), Outcome::ReadOnly);
    r.readonly = ReadOnly::Keep;
    assert!(matches!(
        run(&r).unwrap(),
        Outcome::Saved { readonly: true, .. }
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
    assert_ne!(
        crate::sys::attrs(&path).unwrap() & crate::sys::ATTR_READONLY,
        0
    );
    r.readonly = ReadOnly::Clear;
    r.text = body("Привет, 日本");
    r.enc = WINDOWS_1251;
    assert_eq!(run(&r).unwrap(), Outcome::Unmappable(2));
    r.unmappable = Unmappable::Replace;
    assert!(matches!(
        run(&r).unwrap(),
        Outcome::Saved {
            readonly: false,
            ..
        }
    ));
    let want = WINDOWS_1251.encode("Привет, ??").0.into_owned();
    assert_eq!(std::fs::read(&path).unwrap(), want);
    assert_eq!(
        crate::sys::attrs(&path).unwrap() & crate::sys::ATTR_READONLY,
        0
    );
}

// ---------------------------------------------------------------- через core

fn core_cfg(dir: &Path, cfg: EditorCfg) -> Core {
    let root = Location::local(dir);
    let mut core = Core::new(
        Boot::new(root.clone(), root),
        Services::null(),
        Arc::new(NoWake),
    );
    register_with(&mut core, ViewerCfg::default(), cfg);
    core.pump(Duration::from_millis(5));
    core
}

/// Весь текст активного документа — `text` (как набор после «выделить всё»).
fn edit(core: &mut Core, text: &str) {
    let doc = ed(core).id;
    set_text(core, doc, text);
}

fn cmd(core: &mut Core, c: Command) {
    core.dispatch(c);
    core.pump(Duration::from_millis(5));
}

fn settled(core: &Core) -> bool {
    view(core)
        .active_editor()
        .is_none_or(|_| ed(core).state == EdState::Ready)
}

/// F2: снимок → запись → «сохранено»; внешнее изменение → строка, «Сохранить поверх»;
/// Esc с правками → вопрос, «Сохранить» → запись и закрытие; выход с правками — вето.
#[test]
fn save_flow_questions_and_quit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "one\r\n").unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    open(&mut core, &path);
    edit(&mut core, "two\r\n");
    assert!(matches!(core.quit_decision(), QuitDecision::Veto(_)));

    cmd(&mut core, Command::new("editor.save"));
    pump_until(&mut core, |c| ed(c).state == EdState::Ready && !ed(c).dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\r\n");
    assert_eq!(ed(&core).stamp, stamp(&path));
    assert!(matches!(core.quit_decision(), QuitDecision::Allow));

    // Чужая запись → «Файл изменён на диске».
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "outside\r\n").unwrap();
    edit(&mut core, "three\r\n");
    cmd(&mut core, Command::new("editor.save"));
    pump_until(&mut core, |c| ed(c).ask.is_some());
    assert_eq!(ed(&core).ask, Some(EdAsk::Changed));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "outside\r\n");
    cmd(
        &mut core,
        Command::new("editor.answer").arg("choice", "overwrite"),
    );
    pump_until(&mut core, |c| settled(c) && !ed(c).dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\r\n");

    // Esc с правками: вопрос; «Отмена» убирает; «Сохранить» — запись и закрытие.
    edit(&mut core, "four\r\n");
    cmd(&mut core, Command::new("editor.close"));
    assert_eq!(ed(&core).ask, Some(EdAsk::Close));
    cmd(
        &mut core,
        Command::new("editor.answer").arg("choice", "cancel"),
    );
    assert_eq!(ed(&core).ask, None);
    cmd(&mut core, Command::new("editor.close"));
    cmd(
        &mut core,
        Command::new("editor.answer").arg("choice", "save"),
    );
    pump_until(&mut core, |c| view(c).is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "four\r\n");
    assert_eq!(core.state().focus, Focus::Panel);
}

/// Ответ на вопрос выхода (`prompt.reply` открытого core вопроса).
fn quit_reply(core: &mut Core, choice: &str) {
    let id = core.state().prompts.active().expect("вопрос выхода").id;
    cmd(
        core,
        Command::new("prompt.reply")
            .arg("id", i64::try_from(id.0).unwrap())
            .arg("choice", choice),
    );
}

/// Вопрос выхода с правками (решение 2026-10-31 «quit-reply»): «Сохранить все» —
/// запись тем же путём, что `editor.save_all`; вопрос документа («изменён на диске»)
/// отменяет выход; без вопросов — все сохранены, выход. «Отмена» — остаёмся,
/// «Не сохранять» — выход.
#[test]
fn quit_save_all() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "a1\r\n").unwrap();
    std::fs::write(&b, "b1\r\n").unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    open(&mut core, &a);
    let doc_a = ed(&core).id;
    edit(&mut core, "a2\r\n");
    open(&mut core, &b);
    let doc_b = ed(&core).id;
    edit(&mut core, "b2\r\n");

    cmd(&mut core, Command::new("app.quit"));
    let ids: Vec<String> = core
        .state()
        .prompts
        .active()
        .unwrap()
        .spec
        .choices
        .iter()
        .map(|c| c.id.to_string())
        .collect();
    assert_eq!(ids, ["save", "quit", "cancel"]);
    quit_reply(&mut core, "cancel");
    assert!(!core.state().quit);
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "a1\r\n",
        "не сохранено"
    );

    // «a» изменён на диске: вопрос документа — выход отменён.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&a, "outside\r\n").unwrap();
    cmd(&mut core, Command::new("app.quit"));
    quit_reply(&mut core, "save");
    pump_until(&mut core, |c| {
        view(c).editor(doc_a).is_some_and(|d| d.ask.is_some())
            && view(c).editor(doc_b).is_some_and(|d| !d.dirty)
    });
    assert!(view(&core).quit_save.is_none());
    assert!(!core.state().quit, "question cancels quit");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "outside\r\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "b2\r\n");
    cmd(
        &mut core,
        Command::new("editor.answer")
            .arg("choice", "overwrite")
            .arg("doc", i64::from(doc_a.0)),
    );
    pump_until(&mut core, |c| {
        view(c).editor(doc_a).is_some_and(|d| !d.dirty)
    });
    assert!(!core.state().quit, "quit stays cancelled");

    // Без вопросов: все сохранены — снова `app.quit`, выход.
    set_text(&mut core, doc_b, "b3\r\n");
    cmd(&mut core, Command::new("app.quit"));
    quit_reply(&mut core, "save");
    pump_until(&mut core, |c| c.state().quit);
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "b3\r\n");

    // «Не сохранять» — выход сразу.
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    open(&mut core, &a);
    edit(&mut core, "a4\r\n");
    cmd(&mut core, Command::new("app.quit"));
    quit_reply(&mut core, "quit");
    assert!(core.state().quit);
}

/// Непредставимые символы через core: правка в кодировке документа UTF-8, сохранение
/// в 1251 (F8 с правками) — строка вопроса, «Сохранить в UTF-8».
#[test]
fn unmappable_to_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ru.txt");
    std::fs::write(&path, "Привет мир, как дела").unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    core.dispatch(
        Command::new("editor.open")
            .arg("path", Location::local(&path))
            .arg("internal", true),
    );
    pump_until(&mut core, |c| {
        view(c).active_editor().is_some() && ed(c).is_ready()
    });
    assert!(ed(&core).enc.is_utf8());
    edit(&mut core, "Привет, 日本");
    cmd(
        &mut core,
        Command::new("editor.codepage").arg("cp", "windows-1251"),
    );
    assert_eq!(ed(&core).enc.enc, WINDOWS_1251, "кодировка сохранения");
    cmd(&mut core, Command::new("editor.save"));
    pump_until(&mut core, |c| ed(c).ask.is_some());
    let d = ed(&core);
    assert_eq!(d.ask, Some(EdAsk::Unmappable { count: 2 }));
    assert!(d.ask.unwrap().text(d).contains('2'));
    cmd(
        &mut core,
        Command::new("editor.answer").arg("choice", "utf8"),
    );
    pump_until(&mut core, |c| settled(c) && !ed(c).dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Привет, 日本");
    assert!(ed(&core).enc.is_utf8());
}

/// Больше 8 мебибайт — задача job-бара; правка во время записи разрешена.
#[test]
fn big_save_is_a_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    let text = "0123456789abcdef\n".repeat((9 << 20) / 17);
    std::fs::write(&path, &text).unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    open(&mut core, &path);
    let doc = ed(&core).id;
    send(
        &mut core,
        InboxMsg::Input {
            doc,
            text: "x".into(),
        },
    );
    cmd(&mut core, Command::new("editor.save"));
    let EdState::Saving(Some(job)) = ed(&core).state else {
        panic!("ожидалась задача: {:?}", ed(&core).state);
    };
    assert_eq!(core.state().jobs.get(job).unwrap().kind, "editor.save");
    let t = Instant::now();
    pump_until(&mut core, |c| ed(c).state == EdState::Ready);
    eprintln!("save 9 MiB job: {:?}", t.elapsed());
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        text.len() as u64 + 1
    );
    assert!(!core.state().jobs.get(job).unwrap().state.is_active());
}

/// Автосохранение `delay`: через секунду после правки; при конфликте `DiskStamp` — не
/// пишет, строка «изменён на диске».
#[test]
fn autosave_delay_and_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "v0").unwrap();
    let cfg = EditorCfg {
        autosave: Autosave::Delay,
        ..EditorCfg::default()
    };
    let mut core = core_cfg(dir.path(), cfg);
    open(&mut core, &path);
    let t = Instant::now();
    edit(&mut core, "v1");
    pump_until(&mut core, |c| !ed(c).dirty && settled(c));
    assert!(
        t.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");

    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "outside").unwrap();
    edit(&mut core, "v2");
    pump_until(&mut core, |c| ed(c).ask.is_some());
    assert_eq!(ed(&core).ask, Some(EdAsk::Changed));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "outside");
}

/// `editor.new`: без пути — черновик имени; с путём — пустой документ, файл появляется
/// при первом сохранении.
#[test]
fn new_file_created_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    cmd(&mut core, Command::new("editor.new"));
    assert!(view(&core).is_empty(), "без пути — только черновик");
    cmd(
        &mut core,
        Command::new("editor.new").arg("path", "notes.md"),
    );
    pump_until(&mut core, |c| {
        view(c).active_editor().is_some() && ed(c).is_ready()
    });
    let d = ed(&core);
    assert!(d.is_new);
    assert!(d.engine.as_ref().is_some_and(|e| e.is_empty()));
    assert_eq!(d.lang, Some("markdown"));
    let path = dir.path().join("notes.md");
    assert!(!path.exists());
    edit(&mut core, "# Notes\n");
    cmd(&mut core, Command::new("editor.save"));
    pump_until(&mut core, |c| settled(c) && !ed(c).is_new);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Notes\r\n");
}

/// F6 туда-обратно: строка верха вьювера → курсор редактора → строка вьювера.
#[test]
fn f6_round_trip_keeps_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines.txt");
    let text = (1..=300).fold(String::new(), |mut s, i| {
        s.push_str("line ");
        s.push_str(&i.to_string());
        s.push_str("\r\n");
        s
    });
    std::fs::write(&path, &text).unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    core.dispatch(Command::new("viewer.open").arg("path", Location::local(&path)));
    let top_line = |c: &Core| {
        let v = view(c);
        v.viewer(v.active_viewer()?)
            .and_then(crate::viewer::visible_lines)
            .map(|(f, _)| f)
    };
    pump_until(&mut core, |c| top_line(c).is_some());
    cmd(&mut core, Command::new("viewer.goto").arg("to", "120"));
    pump_until(&mut core, |c| top_line(c) == Some(119));

    cmd(&mut core, Command::new("viewer.to_editor"));
    pump_until(&mut core, |c| {
        view(c).active_editor().is_some() && ed(c).is_ready()
    });
    assert_eq!(ed(&core).cursor, (119, 0));
    assert_eq!(view(&core).len(), 1, "вьювер закрыт");

    cmd(&mut core, Command::new("editor.to_viewer"));
    pump_until(&mut core, |c| top_line(c) == Some(119));
    assert_eq!(view(&core).len(), 1, "редактор закрыт");
    assert_eq!(core.state().focus, Focus::Viewer);
}

/// Команды `term.*` без терминала копятся для проверки.
struct Recorder(Arc<Mutex<Vec<Command>>>);

const fn arg(name: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        kind: ValueKind::Str,
        required: false,
    }
}

static TERM: [CommandMeta; 2] = [
    CommandMeta {
        id: "term.open",
        title: "open",
        category: "term",
        args: &[arg("path"), arg("verb")],
        repeatable: false,
    },
    CommandMeta {
        id: "term.exec",
        title: "exec",
        category: "term",
        args: &[arg("cmd"), arg("cwd")],
        repeatable: false,
    },
];

impl Handler for Recorder {
    fn namespace(&self) -> &'static str {
        "term"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &TERM
    }

    fn handle(&mut self, cmd: &Command, _cx: &mut Cx<'_>) -> Result<(), CmdError> {
        self.0.lock().unwrap().push(cmd.clone());
        Ok(())
    }
}

/// Внешний редактор: консольный (`vim`) — строкой в терминал, GUI (`code -w`, здесь —
/// скрипт-фейк) — отсоединённым процессом; `editor.default = "external"` меняет F4 и
/// Alt+Shift+F4 местами.
#[test]
fn external_console_and_detached() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a b.txt");
    std::fs::write(&path, "x").unwrap();
    let cfg = EditorCfg {
        external: "vim +{line} {path}".into(),
        ..EditorCfg::default()
    };
    let mut core = core_cfg(dir.path(), cfg);
    let rec = Arc::new(Mutex::new(Vec::new()));
    core.register(Box::new(Recorder(Arc::clone(&rec))));
    cmd(
        &mut core,
        Command::new("editor.open")
            .arg("path", Location::local(&path))
            .arg("alt", true)
            .arg("line", 7),
    );
    assert!(view(&core).is_empty(), "встроенный не открылся");
    let got = rec.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id.as_str(), "term.exec");
    assert_eq!(
        got[0].args.str("cmd"),
        Some(format!("vim +7 \"{}\"", path.display()).as_str())
    );

    // Фейковый GUI-редактор `.cmd`: пишет свои аргументы в файл-метку.
    let fake = dir.path().join("fakeed.cmd");
    let marker = dir.path().join("marker.txt");
    std::fs::write(
        &fake,
        format!("@echo off\r\necho %*> \"{}\"\r\n", marker.display()),
    )
    .unwrap();
    let cfg = EditorCfg {
        default: crate::config::EditorDefault::External,
        external: format!("\"{}\" -w {{path}}", fake.display()),
        external_console: "false".into(),
        ..EditorCfg::default()
    };
    let mut core = core_cfg(dir.path(), cfg);
    cmd(
        &mut core,
        Command::new("editor.open").arg("path", Location::local(&path)),
    );
    assert!(view(&core).is_empty(), "F4 при default = external — наружу");
    let t = Instant::now();
    while !marker.exists() || std::fs::read_to_string(&marker).unwrap().is_empty() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "фейковый редактор не запустился"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let args = std::fs::read_to_string(&marker).unwrap();
    assert!(args.starts_with("-w "), "{args}");
    assert!(args.contains("a b.txt"), "{args}");
    // Alt+Shift+F4 при default = external — встроенный.
    open_alt(&mut core, &path);
    assert!(ed(&core).is_ready());
}

fn open_alt(core: &mut Core, path: &Path) {
    core.dispatch(
        Command::new("editor.open")
            .arg("path", Location::local(path))
            .arg("alt", true),
    );
    pump_until(core, |c| {
        view(c).active_editor().is_some() && !matches!(ed(c).state, EdState::Loading { .. })
    });
}
