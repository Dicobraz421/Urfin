//! S7-17 / S7-19: сохранение на диск (атомарная замена, атрибуты, ACL, жёсткие ссылки,
//! сбой посреди записи), вопросы и ответы через core, вето выхода, задача больше 8 мебибайт,
//! автосохранение, новый файл, F6 туда-обратно, внешний редактор. Всё — во временных
//! каталогах.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use encoding_rs::{UTF_8, WINDOWS_1251};
use ropey::Rope;
use urfin_core::{
    ArgSpec, Boot, CmdError, Command, CommandMeta, Core, Cx, Focus, Handler, Location,
    QuitDecision, Services, UiEvent, ValueKind,
};

use super::super::save::{Fault, How, Outcome, ReadOnly, SaveError, SaveReq, Unmappable, save};
use super::super::{DiskStamp, EdAsk, EdState};
use super::{NoWake, ed, open, pump_until, send, view};
use crate::config::Autosave;
use crate::inbox::After;
use crate::{EditorCfg, InboxMsg, ViewerCfg, register_with};

fn req(path: &Path, text: &str) -> SaveReq {
    SaveReq {
        text: Rope::from_str(text),
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
    r.text = Rope::from_str("again");
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
    r.text = Rope::from_str("Привет, 日本");
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

/// Просьбы снимка из UI-событий (остальные события отбрасываются).
fn snapshots(core: &mut Core) -> Vec<(crate::DocId, After)> {
    core.take_ui_events()
        .into_iter()
        .filter_map(|e| match e {
            UiEvent::Ext { id, args } if id.as_str() == "editor.snapshot" => Some((
                crate::handler::ext_doc(&args).unwrap(),
                After::from_args(&args).unwrap(),
            )),
            _ => None,
        })
        .collect()
}

/// Ждёт просьбы снимка (автосохранение), не дольше 5 с.
fn wait_snapshots(core: &mut Core) -> Vec<(crate::DocId, After)> {
    let t = Instant::now();
    loop {
        core.pump(Duration::from_millis(5));
        let got = snapshots(core);
        if !got.is_empty() {
            return got;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "нет снимка");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Отвечает на просьбы снимка текстом `text`, как фича UI; возвращает их число.
fn answer_snapshots(core: &mut Core, text: &str) -> usize {
    let asks = snapshots(core);
    let n = asks.len();
    for (doc, then) in asks {
        send(
            core,
            InboxMsg::Snapshot {
                doc,
                text: Rope::from_str(text),
                then,
            },
        );
    }
    n
}

fn edited(core: &mut Core, dirty: bool, cursor: (u32, u32)) {
    let doc = ed(core).id;
    send(core, InboxMsg::Edited { doc, dirty, cursor });
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

/// F2: снимок у UI → запись → «сохранено»; внешнее изменение → строка, «Сохранить
/// поверх»; Esc с правками → вопрос, «Сохранить» → снимок `save_close` → закрыт; выход
/// с правками — вето.
#[test]
fn save_flow_questions_and_quit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "one\r\n").unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    open(&mut core, &path);
    let doc = ed(&core).id;
    send(&mut core, InboxMsg::Adopted(doc));
    edited(&mut core, true, (0, 3));
    assert!(matches!(core.quit_decision(), QuitDecision::Veto(_)));

    cmd(&mut core, Command::new("editor.save"));
    assert_eq!(answer_snapshots(&mut core, "two\r\n"), 1);
    pump_until(&mut core, |c| ed(c).state == EdState::Ready && !ed(c).dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\r\n");
    assert_eq!(ed(&core).stamp, stamp(&path));
    assert!(matches!(core.quit_decision(), QuitDecision::Allow));

    // Чужая запись → «Файл изменён на диске».
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "outside\r\n").unwrap();
    edited(&mut core, true, (0, 1));
    cmd(&mut core, Command::new("editor.save"));
    answer_snapshots(&mut core, "three\r\n");
    pump_until(&mut core, |c| ed(c).ask.is_some());
    assert_eq!(ed(&core).ask, Some(EdAsk::Changed));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "outside\r\n");
    cmd(
        &mut core,
        Command::new("editor.answer").arg("choice", "overwrite"),
    );
    pump_until(&mut core, |c| settled(c) && !ed(c).dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\r\n");

    // Esc с правками: вопрос; «Отмена» убирает; «Сохранить» — снимок и закрытие.
    edited(&mut core, true, (0, 2));
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
    let asks = snapshots(&mut core);
    assert_eq!(asks, vec![(doc, After::SaveClose)]);
    send(
        &mut core,
        InboxMsg::Snapshot {
            doc,
            text: Rope::from_str("four\r\n"),
            then: After::SaveClose,
        },
    );
    pump_until(&mut core, |c| view(c).is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "four\r\n");
    assert_eq!(core.state().focus, Focus::Panel);
}

/// Непредставимые символы через core: строка вопроса, «Сохранить в UTF-8».
#[test]
fn unmappable_to_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ru.txt");
    std::fs::write(&path, WINDOWS_1251.encode("Привет мир, как дела").0).unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    core.dispatch(
        Command::new("editor.open")
            .arg("path", Location::local(&path))
            .arg("internal", true),
    );
    pump_until(&mut core, |c| {
        view(c).active_editor().is_some() && ed(c).is_ready()
    });
    cmd(
        &mut core,
        Command::new("editor.codepage").arg("cp", "windows-1251"),
    );
    pump_until(&mut core, |c| ed(c).is_ready());
    edited(&mut core, true, (0, 0));
    cmd(&mut core, Command::new("editor.save"));
    answer_snapshots(&mut core, "Привет, 日本");
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

/// Больше 8 мебибайт — задача job-бара; до конца документ в `Saving`.
#[test]
fn big_save_is_a_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    std::fs::write(&path, "x\n").unwrap();
    let mut core = core_cfg(dir.path(), EditorCfg::default());
    open(&mut core, &path);
    let doc = ed(&core).id;
    edited(&mut core, true, (0, 0));
    let text = "0123456789abcdef\n".repeat((9 << 20) / 17);
    send(
        &mut core,
        InboxMsg::Snapshot {
            doc,
            text: Rope::from_str(&text),
            then: After::Save,
        },
    );
    let EdState::Saving(Some(job)) = ed(&core).state else {
        panic!("ожидалась задача: {:?}", ed(&core).state);
    };
    assert_eq!(core.state().jobs.get(job).unwrap().kind, "editor.save");
    let t = Instant::now();
    pump_until(&mut core, |c| ed(c).state == EdState::Ready);
    eprintln!("save 9 MiB job: {:?}", t.elapsed());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), text.len() as u64);
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
    edited(&mut core, true, (0, 1));
    let t = Instant::now();
    let asks = wait_snapshots(&mut core);
    assert!(
        t.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(asks[0].1, After::Autosave);
    send(
        &mut core,
        InboxMsg::Snapshot {
            doc: asks[0].0,
            text: Rope::from_str("v1"),
            then: After::Autosave,
        },
    );
    pump_until(&mut core, |c| settled(c) && !ed(c).dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");

    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "outside").unwrap();
    edited(&mut core, true, (0, 2));
    let asks = wait_snapshots(&mut core);
    send(
        &mut core,
        InboxMsg::Snapshot {
            doc: asks[0].0,
            text: Rope::from_str("v2"),
            then: After::Autosave,
        },
    );
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
    assert_eq!(d.loaded.as_ref().unwrap().len(), 0);
    assert_eq!(d.lang, Some("markdown"));
    let path = dir.path().join("notes.md");
    assert!(!path.exists());
    edited(&mut core, true, (0, 0));
    cmd(&mut core, Command::new("editor.save"));
    answer_snapshots(&mut core, "# Notes\r\n");
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
    assert_eq!(ed(&core).start, Some((119, 0)));
    assert_eq!(view(&core).len(), 1, "вьювер закрыт");
    let doc = ed(&core).id;
    send(&mut core, InboxMsg::Adopted(doc));
    edited(&mut core, false, (119, 0));

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
