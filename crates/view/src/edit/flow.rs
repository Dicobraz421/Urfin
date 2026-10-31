//! Сохранение в core (S7 §5.4, S7-17) и автосохранение (S7-19): снимок текста у UI,
//! запуск записи (задача job-бара больше 8 мебибайт), итог в документ, inline-вопросы
//! ([`EdAsk`]) и ответы `editor.answer`, закрытие с правками, вето на выход, отметка
//! диска по возврату на экран.
//!
//! Поток: команда (`editor.save` и др.) → `UiEvent::Ext { id: "editor.snapshot", args:
//! { doc, then, path? } }` → фича UI кладёт `InboxMsg::Snapshot` → `on_pump` →
//! [`start_save`] → воркер [`super::save::save`] → [`SavedMsg`] → документ: сохранено
//! (`UiEvent::Ext { id: "editor.saved" }`), вопрос или ошибка тостом.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ropey::Rope;
use urfin_core::{
    Anchor, Args, Choice, CmdError, Command, CommandId, Cx, JobId, JobSpec, JobSummary, Lane,
    Level, Location, Msg, Phase, PromptSpec, QuitDecision, UiEvent,
};

use super::save::{self, Outcome, ReadOnly, SaveError, SaveReq, Unmappable};
use super::{EdState, EditorDoc};
use crate::config::Autosave;
use crate::enc::Enc;
use crate::handler::refocus;
use crate::inbox::After;
use crate::{Doc, DocId, ViewState};

/// Больше — сохранение задачей в job-баре (S7 §5.4).
pub const JOB_MIN: usize = 8 << 20;
/// Автосохранение `delay`: пауза после правки.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// Inline-вопрос редактора (строка внизу экрана, не тост — №51). UI рисует
/// [`EdAsk::text`] и кнопки [`EdAsk::choices`]; кнопка — `editor.answer choice=<id>`,
/// Esc — `choice=cancel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdAsk {
    /// `editor.close` / F6 с правками: «“{name}” изменён. Сохранить?».
    Close,
    /// Файл изменён на диске (перед сохранением или при возврате на экран).
    Changed,
    /// Символы, которых нет в кодировке файла.
    Unmappable { count: usize },
    /// Атрибут «только чтение».
    ReadOnly,
}

/// Кнопка inline-вопроса: id ответа, ключ i18n подписи, клавиша.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AskChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub key: char,
}

const fn ch(id: &'static str, label: &'static str, key: char) -> AskChoice {
    AskChoice { id, label, key }
}

const CANCEL: AskChoice = ch("cancel", "ui-button-cancel", 'c');
const ASK_CLOSE: [AskChoice; 3] = [
    ch("save", "ui-edit-save", 's'),
    ch("discard", "ui-edit-dont-save", 'n'),
    CANCEL,
];
const ASK_CHANGED: [AskChoice; 3] = [
    ch("reload", "ui-edit-reload", 'r'),
    ch("overwrite", "ui-edit-overwrite", 'o'),
    CANCEL,
];
const ASK_UNMAPPABLE: [AskChoice; 3] = [
    ch("utf8", "ui-edit-save-utf8", 'u'),
    ch("replace", "ui-edit-replace-q", 'r'),
    CANCEL,
];
const ASK_READONLY: [AskChoice; 3] = [
    ch("clear", "ui-edit-clear-readonly", 'c'),
    ch("keep", "ui-edit-keep-readonly", 'k'),
    CANCEL,
];

impl EdAsk {
    /// Кнопки слева направо; первая — основная (Enter), последняя — «Отмена» (Esc).
    pub fn choices(&self) -> &'static [AskChoice] {
        match self {
            EdAsk::Close => &ASK_CLOSE,
            EdAsk::Changed => &ASK_CHANGED,
            EdAsk::Unmappable { .. } => &ASK_UNMAPPABLE,
            EdAsk::ReadOnly => &ASK_READONLY,
        }
    }

    /// Текст строки на языке интерфейса.
    pub fn text(&self, d: &EditorDoc) -> String {
        let i18n = urfin_i18n::current();
        let mut a = urfin_i18n::FluentArgs::new();
        match self {
            EdAsk::Close => {
                a.set("name", d.name());
                i18n.fmt("ui-edit-close-changed", &a)
            }
            EdAsk::Changed => i18n.get("ui-edit-disk-changed"),
            EdAsk::Unmappable { count } => {
                a.set("count", *count);
                a.set("enc", d.enc.enc.name());
                i18n.fmt("ui-edit-unmappable", &a)
            }
            EdAsk::ReadOnly => i18n.get("ui-edit-readonly-save"),
        }
    }
}

/// Ответы, накопленные по вопросам одного сохранения.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    /// Без сверки `DiskStamp` («Сохранить поверх»).
    pub force: bool,
    pub unmappable: Unmappable,
    pub readonly: ReadOnly,
}

/// Снимок, ждущий ответа на вопрос.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub text: Rope,
    pub then: After,
    pub flags: Flags,
}

/// Просит у UI снимок текста документа.
pub(crate) fn request_snapshot(cx: &mut Cx<'_>, doc: DocId, then: &After) {
    cx.ui(UiEvent::Ext {
        id: CommandId::from_static("editor.snapshot"),
        args: then.to_args().with("doc", doc.0),
    });
}

/// Черновик имени (`ui.draft`): `editor.save_as` / `editor.new` с аргументом `path`.
pub(crate) fn draft(cx: &mut Cx<'_>, command: &str, init: &str) {
    cx.dispatch(
        Command::new("ui.draft")
            .arg("command", command)
            .arg("arg", "path")
            .arg("init", init),
    );
}

#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "проверки, малая запись и задача — один путь сверху вниз"
)]
/// Снимок пришёл: запись в фоне. `false` — документа нет или он не готов (загрузка,
/// другое сохранение), снимок отброшен.
pub(crate) fn start_save(
    cx: &mut Cx<'_>,
    doc: DocId,
    text: Rope,
    then: After,
    flags: Flags,
) -> bool {
    let rt = cx.rt;
    let Some(d) = cx
        .state
        .ext
        .get_mut::<ViewState>()
        .and_then(|v| v.editor_mut(doc))
    else {
        tracing::debug!(?doc, "editor save: snapshot for a closed document");
        return false;
    };
    if !d.is_ready() || (then == After::Autosave && (d.ask.is_some() || !d.dirty)) {
        tracing::debug!(?doc, state = ?d.state, "editor save: snapshot dropped");
        return false;
    }
    let target = match &then {
        After::SaveAs(l) => l.clone(),
        _ => d.loc.clone(),
    };
    if !target.is_local() {
        // Не-локальные (S8): только «Сохранить как» в локальный путь.
        let name = d.name();
        let text = urfin_i18n::current().get("ui-edit-remote-save-as");
        cx.toast(Level::Warn, text);
        if then != After::Autosave {
            draft(cx, "editor.save_as", &name);
        }
        return true;
    }
    let same = target == d.loc;
    let force = flags.force || std::mem::take(&mut d.force);
    let req = SaveReq {
        text: text.clone(),
        enc: d.enc.enc,
        bom: d.bom,
        path: target.path.clone(),
        check: (same && !d.is_new && !force).then_some(d.stamp),
        unmappable: flags.unmappable,
        readonly: flags.readonly,
        fault: None,
    };
    d.pending = Some(Pending {
        text,
        then: then.clone(),
        flags: Flags { force, ..flags },
    });
    d.ask = None;
    d.rev += 1;
    let name = d.name();
    let len = req.text.len();
    if len <= JOB_MIN {
        d.state = EdState::Saving(None);
        rt.spawn(Lane::Interactive, "edit.save", rt.token(), move |t| {
            let res = save::save(&req, &|| t.cancel.check(), &mut |_| {});
            let msg = SavedMsg::new(doc, then, &req.text, res, None);
            let _ = t.sink.send(Box::new(msg), &t.cancel);
        });
        return true;
    }
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("name", name);
    let spec = JobSpec {
        kind: "editor.save",
        title: urfin_i18n::current().fmt("ui-edit-saving", &a),
        lane: Lane::Bulk,
        priority: 0,
        owner_ns: "editor",
    };
    let h = cx.state.jobs.start(spec, rt.sink());
    let job = h.id();
    if let Some(d) = cx
        .state
        .ext
        .get_mut::<ViewState>()
        .and_then(|v| v.editor_mut(doc))
    {
        d.state = EdState::Saving(Some(job));
    }
    let tok = h.cancel_token().clone();
    rt.spawn(Lane::Bulk, "edit.save", tok, move |t| {
        let p = h.progress();
        p.bytes_total.store(len as u64, Ordering::Relaxed);
        p.items_total.store(1, Ordering::Relaxed);
        p.total_final.store(true, Ordering::Relaxed);
        p.set_phase(Phase::Transfer);
        p.set_current(&req.path.to_string_lossy());
        let res = save::save(&req, &|| h.checkpoint(), &mut |n| {
            h.progress().bytes_done.store(n, Ordering::Relaxed);
        });
        let summary = match &res {
            Ok(Outcome::Saved { bytes, .. }) => Ok(JobSummary {
                items: 1,
                bytes: *bytes,
                ..JobSummary::default()
            }),
            Ok(_) => Ok(JobSummary::default()),
            Err(e) => Err(e.to_string()),
        };
        let msg = SavedMsg::new(doc, then, &req.text, res, Some(job));
        h.finish(summary);
        let _ = t.sink.send(Box::new(msg), &t.cancel);
    });
    true
}

/// Итог сохранения → документ.
#[derive(Debug)]
pub(crate) struct SavedMsg {
    doc: DocId,
    then: After,
    res: Result<Outcome, SaveError>,
    /// Хэши строк сохранённого текста (только при успехе).
    hashes: Option<Arc<[u64]>>,
    job: Option<JobId>,
}

impl SavedMsg {
    fn new(
        doc: DocId,
        then: After,
        text: &Rope,
        res: Result<Outcome, SaveError>,
        job: Option<JobId>,
    ) -> SavedMsg {
        let hashes = matches!(res, Ok(Outcome::Saved { .. }))
            .then(|| Arc::from(super::diff::line_hashes(text)));
        SavedMsg {
            doc,
            then,
            res,
            hashes,
            job,
        }
    }
}

impl Msg for SavedMsg {
    fn name(&self) -> &'static str {
        "edit.saved"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let SavedMsg {
            doc,
            then,
            res,
            hashes,
            job,
        } = *self;
        let Some(d) = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(doc))
        else {
            return;
        };
        if d.state == EdState::Saving(job) {
            d.state = EdState::Ready;
        }
        d.rev += 1;
        let auto = then == After::Autosave;
        match res {
            Ok(Outcome::Saved {
                stamp, readonly, ..
            }) => {
                d.pending = None;
                d.ask = None;
                if let After::SaveAs(loc) = &then
                    && *loc != d.loc
                {
                    d.loc = loc.clone();
                    d.lang = crate::lang::detect(&d.name(), &[]);
                }
                d.on_saved(stamp, hashes.unwrap_or_else(|| Arc::from(Vec::new())));
                d.readonly = readonly;
                cx.ui(UiEvent::Ext {
                    id: CommandId::from_static("editor.saved"),
                    args: Args::default()
                        .with("doc", doc.0)
                        .with("readonly", readonly),
                });
                if then == After::SaveClose {
                    close_doc(cx, doc);
                }
            }
            Ok(Outcome::Changed) => d.ask = Some(EdAsk::Changed),
            // Автосохранение вопросов не задаёт: попробует после следующей правки.
            Ok(Outcome::Unmappable(_) | Outcome::ReadOnly) if auto => d.pending = None,
            Ok(Outcome::Unmappable(count)) => d.ask = Some(EdAsk::Unmappable { count }),
            Ok(Outcome::ReadOnly) => d.ask = Some(EdAsk::ReadOnly),
            Err(SaveError::Cancelled) => {
                d.pending = None;
                d.then_view = None;
            }
            Err(SaveError::Io(e)) => {
                d.pending = None;
                d.then_view = None;
                tracing::warn!(error = %e, "editor save failed");
                let mut a = urfin_i18n::FluentArgs::new();
                a.set("name", d.name());
                a.set("error", e.to_string());
                let text = urfin_i18n::current().fmt("ui-edit-save-failed", &a);
                cx.toast(Level::Error, text);
            }
        }
    }
}

/// Закрывает документ: позиция — в историю правок, фокус — верхнему экрану; F6 с
/// вопросом — затем вьювер на строке курсора.
pub(crate) fn close_doc(cx: &mut Cx<'_>, id: DocId) {
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let remember = view.viewer.remember_pos;
    let Some(Doc::Editor(mut d)) = view.remove(id) else {
        return;
    };
    d.close();
    if remember && d.is_ready() && !d.is_new {
        view.history.record(d.memo());
    }
    refocus(cx);
    if let Some(line) = d.then_view {
        open_viewer(cx, &d.loc, line);
    }
}

/// Вьювер на строке `line` (с 0): F6 из редактора.
pub(crate) fn open_viewer(cx: &mut Cx<'_>, loc: &Location, line: u32) {
    cx.dispatch(
        Command::new("viewer.open")
            .arg("path", loc.clone())
            .arg("internal", true)
            .arg("line", i64::from(line) + 1),
    );
}

/// Документ ответа: `doc=` или активный редактор.
fn answer_doc(cmd: &Command, view: &ViewState) -> Option<DocId> {
    match crate::handler::ext_doc(&cmd.args) {
        Some(id) => view.editor(id).map(|_| id),
        None => view.active_editor(),
    }
}

/// Что сделать по ответу (после того, как документ отпущен).
enum Act {
    Nothing,
    Snapshot(After),
    Close,
    Reload,
    /// Снова записать ждущий снимок с уточнёнными ответами.
    Restart(fn(&mut Flags)),
}

/// `editor.answer {choice, doc?}`: ответ на inline-вопрос документа.
pub(crate) fn answer(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let choice = cmd.args.str("choice").unwrap_or_default().to_owned();
    let view = crate::handler::view_mut(cx)?;
    let Some(doc) = answer_doc(cmd, view) else {
        cx.keep_rev();
        return Ok(());
    };
    let Some(d) = view.editor_mut(doc) else {
        return Ok(());
    };
    let Some(ask) = d.ask else {
        cx.keep_rev();
        return Ok(());
    };
    let valid = choice == "cancel" || ask.choices().iter().any(|c| c.id == choice);
    if !valid {
        return Err(CmdError::Arg {
            name: "choice".into(),
            msg: format!("unexpected answer {choice} to {ask:?}").into(),
        });
    }
    d.ask = None;
    d.rev += 1;
    let pending = d.pending.take();
    let act = match (ask, choice.as_str()) {
        (_, "cancel") => {
            d.then_view = None;
            Act::Nothing
        }
        (EdAsk::Close, "save") => Act::Snapshot(After::SaveClose),
        (EdAsk::Close, _) => Act::Close,
        (EdAsk::Changed, "reload") => {
            d.then_view = None;
            Act::Reload
        }
        (EdAsk::Changed, _) if pending.is_some() => Act::Restart(|f| f.force = true),
        (EdAsk::Changed, _) => {
            // Строка по возврату на экран: снимка ещё нет.
            d.force = true;
            Act::Snapshot(After::Save)
        }
        (EdAsk::Unmappable { .. }, "utf8") => {
            d.enc = Enc::utf8();
            d.bom = false;
            Act::Restart(|_| {})
        }
        (EdAsk::Unmappable { .. }, _) => Act::Restart(|f| f.unmappable = Unmappable::Replace),
        (EdAsk::ReadOnly, "keep") => Act::Restart(|f| f.readonly = ReadOnly::Keep),
        (EdAsk::ReadOnly, _) => Act::Restart(|f| f.readonly = ReadOnly::Clear),
    };
    match act {
        Act::Nothing => {}
        Act::Snapshot(then) => request_snapshot(cx, doc, &then),
        Act::Close => close_doc(cx, doc),
        Act::Reload => super::cmd::reload(cx, doc),
        Act::Restart(f) => {
            if let Some(mut p) = pending {
                f(&mut p.flags);
                start_save(cx, doc, p.text, p.then, p.flags);
            }
        }
    }
    Ok(())
}

/// Несохранённые документы: правки или запись ещё идёт.
fn unsaved(view: &ViewState) -> Vec<String> {
    let mut names: Vec<String> = view
        .editors()
        .filter(|d| d.dirty || matches!(d.state, EdState::Saving(_)))
        .map(EditorDoc::name)
        .collect();
    names.sort();
    names
}

/// `Handler::on_quit`: несохранённые — вето «Не сохранено: N [Отмена] [Не сохранять]».
/// Ответ `quit` выходит (core); «Сохранить все» требует ответа вопроса handler'у — его
/// в core нет (CONTRACTS «Выход»), сохранить — F2 / `editor.save_all` и выйти снова.
pub(crate) fn quit_decision(cx: &mut Cx<'_>) -> QuitDecision {
    let Some(view) = cx.state.ext.get::<ViewState>() else {
        return QuitDecision::Allow;
    };
    let names = unsaved(view);
    if names.is_empty() {
        return QuitDecision::Allow;
    }
    let i18n = urfin_i18n::current();
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("count", names.len());
    let choice = |id: &str, key: &str, k: char| Choice {
        id: id.into(),
        label: i18n.get(key).into(),
        key: k,
    };
    QuitDecision::Veto(PromptSpec {
        anchor: Anchor::Cmdline,
        title: i18n.fmt("ui-edit-unsaved", &a),
        body: names.join(", "),
        choices: [
            choice("cancel", "ui-button-cancel", 'c'),
            choice("quit", "ui-edit-dont-save", 'n'),
        ]
        .into_iter()
        .collect(),
        default: 0,
        input: None,
        allow_all: false,
        detail: None,
        secret: false,
    })
}

/// Автосохранение `delay`: снимок через [`AUTOSAVE_DELAY`] после последней правки
/// (локальные файлы без вопроса на экране); пока ждём — будильник окна.
pub(crate) fn autosave_tick(cx: &mut Cx<'_>) {
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    if view.editor.autosave != Autosave::Delay {
        return;
    }
    let now = Instant::now();
    let mut due = Vec::new();
    let mut wait: Option<Duration> = None;
    for d in view.editors_mut() {
        let Some(t) = d.edited_at else {
            continue;
        };
        if !d.dirty || !d.loc.is_local() || d.ask.is_some() {
            d.edited_at = None;
            continue;
        }
        if !d.is_ready() {
            continue;
        }
        let gone = now.duration_since(t);
        if gone >= AUTOSAVE_DELAY {
            d.edited_at = None;
            due.push(d.id);
        } else {
            let left = AUTOSAVE_DELAY.saturating_sub(gone);
            wait = Some(wait.map_or(left, |w| w.min(left)));
        }
    }
    for id in due {
        request_snapshot(cx, id, &After::Autosave);
    }
    if let Some(w) = wait {
        cx.rt.waker().wake_after(w);
    }
}

/// Автосохранение `focus`: документ ушёл с экрана или окно потеряло фокус.
pub(crate) fn autosave_leave(cx: &mut Cx<'_>, doc: DocId) {
    let Some(view) = cx.state.ext.get::<ViewState>() else {
        return;
    };
    if view.editor.autosave != Autosave::Focus {
        return;
    }
    let go = view
        .editor(doc)
        .is_some_and(|d| d.dirty && d.is_ready() && d.loc.is_local() && d.ask.is_none());
    if go {
        request_snapshot(cx, doc, &After::Autosave);
    }
}

/// Возврат на экран редактора: отметка диска в фоне; изменён — строка «Файл изменён на
/// диске».
pub(crate) fn check_stamp(cx: &mut Cx<'_>, doc: DocId) {
    let Some(d) = cx.state.ext.get::<ViewState>().and_then(|v| v.editor(doc)) else {
        return;
    };
    if !d.is_ready() || d.is_new || !d.loc.is_local() || d.ask.is_some() {
        return;
    }
    let (path, want, gen_) = (d.loc.path.clone(), d.stamp, d.stamp_gen);
    cx.rt
        .spawn(Lane::Interactive, "edit.stamp", cx.rt.token(), move |t| {
            let now = match save::stamp_of(&path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(error = %e, "editor: stamp check failed");
                    return;
                }
            };
            if now.is_some_and(|s| save::same_stamp(&want, &s)) {
                return;
            }
            let _ = t.sink.send(Box::new(StampMsg { doc, gen_ }), &t.cancel);
        });
}

/// Файл изменён на диске (по возврату на экран).
#[derive(Debug)]
struct StampMsg {
    doc: DocId,
    gen_: u32,
}

impl Msg for StampMsg {
    fn name(&self) -> &'static str {
        "edit.stamp"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let d = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc));
        match d {
            Some(d) if d.stamp_gen == self.gen_ && d.is_ready() && d.ask.is_none() => {
                d.ask = Some(EdAsk::Changed);
                d.rev += 1;
            }
            _ => cx.keep_rev(),
        }
    }
}
