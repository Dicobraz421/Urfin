//! Ход правки удалённого файла в handler `editor` (F03 §4.3, F03-08): скачивание копии,
//! закачка после сохранения, конфликт «файл на сервере изменился», «ждёт связи», отмена
//! и удаление временной папки. Блокирующие шаги — [`super::remote`], здесь — задачи и
//! сообщения в документ.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use urfin_core::{
    CancelToken, CmdError, Command, Cx, JobSpec, JobSummary, Lane, Level, Location, Phase, Runtime,
};
use urfin_vfs::VfsError;

use super::flow::{self, EdAsk};
use super::remote::{self, Fetched, UpOutcome, UpReq, UpState, Upload};
use super::{EdState, EditorDoc, LoadOpts};
use crate::inbox::After;
use crate::{DocId, ViewState};

/// Повторов закачки после возврата связи — не больше.
const RETRIES: u32 = 20;

fn tr(key: &str, args: &[(&str, String)]) -> String {
    let mut a = urfin_i18n::FluentArgs::new();
    for (k, v) in args {
        a.set(*k, v.clone());
    }
    urfin_i18n::current().fmt(key, &a)
}

fn doc_mut<'a>(cx: &'a mut Cx<'_>, doc: DocId) -> Option<&'a mut EditorDoc> {
    cx.state
        .ext
        .get_mut::<ViewState>()
        .and_then(|v| v.editor_mut(doc))
}

/// Временная папка — в фоне (UI-поток I/O не делает): копия могла быть открыта задачами
/// документа — несколько попыток.
pub(crate) fn drop_dir(rt: &Runtime, dir: std::path::PathBuf) {
    rt.spawn(Lane::Bulk, "edit.temp", CancelToken::new(), move |_| {
        remote::remove_dir(&dir);
    });
}

/// Скачивание копии документа `doc` (после вставки в `ViewState`); затем — обычное
/// открытие с `opts`. `size` — размер из листинга панели: больше 1 `МиБ` — задача
/// job-бара (F03 §4.3 п. 1).
pub(crate) fn start_fetch(cx: &mut Cx<'_>, doc: DocId, opts: LoadOpts, size: Option<u64>) {
    let rt = cx.rt;
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let reg = view.vfs.clone();
    let Some(d) = view.editor_mut(doc) else {
        return;
    };
    let (file, name) = (d.loc.path.clone(), d.name());
    let Some(r) = d.remote.as_deref_mut() else {
        return;
    };
    r.fetching = true;
    let (loc, server) = (r.loc.clone(), r.label());
    d.state = EdState::Loading {
        done: 0,
        total: size.unwrap_or(0),
    };
    d.rev += 1;
    let tok = d.cancel.child();
    let job = size.filter(|s| *s > remote::FETCH_JOB_MIN).map(|_| {
        let spec = JobSpec {
            kind: "editor.fetch",
            title: tr(
                "ui-edit-fetching-job",
                &[("name", name), ("server", server)],
            ),
            lane: Lane::Bulk,
            priority: 0,
            owner_ns: "editor",
        };
        cx.state.jobs.start(spec, rt.sink())
    });
    let tok = job.as_ref().map_or(tok, |h| h.cancel_token().clone());
    rt.spawn_dedicated("edit.fetch".into(), tok.clone(), move |t| {
        let Ok(vfs) = reg.get(&loc).cloned() else {
            return;
        };
        if let Some(h) = &job {
            let p = h.progress();
            p.items_total.store(1, Ordering::Relaxed);
            p.set_phase(Phase::Transfer);
            p.set_current(&loc.to_uri());
        }
        let res = remote::fetch(&*vfs, &loc, &file, &tok, &mut |done, total| {
            if let Some(h) = &job {
                let p = h.progress();
                p.bytes_total.store(total, Ordering::Relaxed);
                p.total_final.store(true, Ordering::Relaxed);
                p.bytes_done.store(done, Ordering::Relaxed);
            }
            let _ = t.sink.send(
                urfin_core::msg("edit.fetch.progress", move |cx| match doc_mut(cx, doc) {
                    Some(d) if matches!(d.state, EdState::Loading { .. }) => {
                        d.state = EdState::Loading { done, total };
                        d.rev += 1;
                    }
                    _ => cx.keep_rev(),
                }),
                &t.cancel,
            );
        });
        if let Some(h) = job {
            let summary = match &res {
                Ok(_) => Ok(JobSummary {
                    items: 1,
                    bytes: h.progress().bytes_done.load(Ordering::Relaxed),
                    ..JobSummary::default()
                }),
                Err(e) => Err(e.to_string()),
            };
            h.finish(summary);
        }
        let _ = t.sink.send(
            urfin_core::msg("edit.fetched", move |cx| fetched(cx, doc, res, opts)),
            &t.cancel,
        );
    });
}

/// Копия скачана: открытие локального файла; нет файла и это `editor.new` — новый.
fn fetched(cx: &mut Cx<'_>, doc: DocId, res: Result<Fetched, VfsError>, opts: LoadOpts) {
    let rt = cx.rt;
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let reg = view.vfs.clone();
    let Some(d) = view.editor_mut(doc) else {
        return;
    };
    let Some(r) = d.remote.as_deref_mut() else {
        return;
    };
    r.fetching = false;
    match res {
        Ok(f) => {
            r.target = f.target;
            r.stamp = Some(f.stamp);
            r.mode = f.mode;
            d.load(rt, &reg, opts);
        }
        Err(VfsError::NotFound(_)) if opts.new.is_some() => {
            r.stamp = None;
            d.load(rt, &reg, opts);
        }
        Err(VfsError::Cancelled) => cx.keep_rev(),
        Err(e) => {
            let dir = r.dir.clone();
            drop_dir(rt, dir);
            super::cmd::fail_load(cx, doc, &e.to_string());
        }
    }
}

/// Сохранено локально: закачка копии на сервер. `force` — без сверки отметки
/// («Перезаписать»). `then` — что сделать после (закрыть — `SaveClose`).
pub(crate) fn start_upload(cx: &mut Cx<'_>, doc: DocId, force: bool, then: Option<After>) {
    let rt = cx.rt;
    let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
        return;
    };
    let reg = view.vfs.clone();
    let Some(d) = view.editor_mut(doc) else {
        return;
    };
    let local = d.loc.path.clone();
    let total = d.stamp.size;
    let cancel = d.cancel.child();
    let Some(r) = d.remote.as_deref_mut() else {
        return;
    };
    if let Some(u) = r.up.take() {
        u.cancel.cancel();
    }
    r.up_gen = r.up_gen.wrapping_add(1);
    r.then = then;
    r.unsynced = true;
    let gen_ = r.up_gen;
    r.up = Some(Upload {
        state: UpState::Running,
        done: 0,
        total,
        elapsed: Duration::ZERO,
        gen_,
        cancel: cancel.clone(),
    });
    let req = UpReq {
        local,
        target: r.target.clone(),
        expect: (!force).then_some(r.stamp),
        mode: r.mode,
    };
    d.rev += 1;
    rt.spawn_dedicated("edit.upload".into(), cancel.clone(), move |t| {
        let Ok(vfs) = reg.get(&req.target).cloned() else {
            return;
        };
        let send = |f: Box<dyn FnOnce(&mut Upload) + Send>| {
            let _ = t.sink.send(
                urfin_core::msg("edit.upload.state", move |cx| {
                    let Some(d) = doc_mut(cx, doc) else {
                        cx.keep_rev();
                        return;
                    };
                    match d.remote.as_deref_mut().and_then(|r| r.up.as_mut()) {
                        Some(u) if u.gen_ == gen_ => {
                            f(u);
                            d.rev += 1;
                        }
                        _ => cx.keep_rev(),
                    }
                }),
                &t.cancel,
            );
        };
        let t0 = Instant::now();
        let mut tries = 0;
        let res = loop {
            let r = remote::upload(&*vfs, &req, &cancel, &mut |n| {
                send(Box::new(move |u: &mut Upload| u.done = n));
            });
            match r {
                Err(e)
                    if remote::maybe_net(&e)
                        && tries < RETRIES
                        && remote::wait_link(&*vfs, &req.target, &cancel, &mut || {
                            send(Box::new(|u: &mut Upload| u.state = UpState::Waiting));
                        }) =>
                {
                    tracing::info!(error = %e, "remote edit: connection back, upload again");
                    tries += 1;
                    send(Box::new(|u: &mut Upload| {
                        u.state = UpState::Running;
                        u.done = 0;
                    }));
                }
                r => break r,
            }
        };
        let elapsed = t0.elapsed();
        let _ = t.sink.send(
            urfin_core::msg("edit.uploaded", move |cx| {
                uploaded(cx, doc, gen_, res, elapsed);
            }),
            &t.cancel,
        );
    });
}

/// Итог закачки → документ: готово, конфликт или ошибка; закрытие после `SaveClose`,
/// «Сохранить все» при выходе.
fn uploaded(
    cx: &mut Cx<'_>,
    doc: DocId,
    gen_: u32,
    res: Result<UpOutcome, VfsError>,
    elapsed: Duration,
) {
    let Some(d) = doc_mut(cx, doc) else {
        flow::quit_saved(cx, doc, true);
        return;
    };
    let name = d.name();
    let Some(r) = d.remote.as_deref_mut() else {
        return;
    };
    if r.up_gen != gen_ {
        cx.keep_rev();
        return;
    }
    d.rev += 1;
    let then = r.then.take();
    let ok = match res {
        Ok(UpOutcome::Done { stamp, bytes }) => {
            r.stamp = Some(stamp);
            r.unsynced = false;
            if let Some(u) = r.up.as_mut() {
                u.state = UpState::Done;
                u.done = bytes;
                u.total = bytes;
                u.elapsed = elapsed;
            }
            tracing::info!(bytes, ?elapsed, "remote edit: uploaded");
            true
        }
        Ok(UpOutcome::Changed) => {
            r.up = None;
            // «Перезаписать» продолжит то же: закрыть после закачки.
            r.then.clone_from(&then);
            d.ask = Some(EdAsk::RemoteChanged);
            false
        }
        Err(VfsError::Cancelled) => {
            r.up = None;
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "remote edit: upload failed");
            let text = e.to_string();
            let server = r.label();
            if let Some(u) = r.up.as_mut() {
                u.state = UpState::Failed(text.clone());
            }
            cx.toast(
                Level::Error,
                tr(
                    "ui-edit-upload-failed",
                    &[("name", name), ("server", server), ("error", text)],
                ),
            );
            false
        }
    };
    if ok && then == Some(After::SaveClose) {
        flow::close_doc(cx, doc);
    }
    flow::quit_saved(cx, doc, ok);
}

/// `editor.upload {stop?}`: без `stop` — закачать сохранённую копию ещё раз (после
/// ошибки или отменённого конфликта; с правками — сначала сохранить); `stop=true` —
/// остановить закачку или убрать строку «готово».
pub(crate) fn command(cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
    let view = crate::handler::view_mut(cx)?;
    let doc = crate::handler::ext_doc(&cmd.args).or_else(|| view.active_editor());
    let Some(d) = doc.and_then(|id| view.editor_mut(id)) else {
        cx.keep_rev();
        return Ok(());
    };
    let doc = d.id;
    let (dirty, ready) = (d.dirty, d.is_ready());
    let Some(r) = d.remote.as_deref_mut() else {
        cx.keep_rev();
        return Ok(());
    };
    if cmd.args.bool("stop").unwrap_or(false) {
        if let Some(u) = r.up.take()
            && u.is_active()
        {
            u.cancel.cancel();
            r.unsynced = true;
            r.then = None;
            tracing::info!("remote edit: upload stopped");
        }
        r.up_gen = r.up_gen.wrapping_add(1);
        d.rev += 1;
        flow::quit_saved(cx, doc, false);
        return Ok(());
    }
    if r.uploading() || r.fetching || !ready {
        cx.keep_rev();
        return Ok(());
    }
    if dirty {
        flow::request_snapshot(cx, doc, &After::Save);
    } else {
        start_upload(cx, doc, false, None);
    }
    Ok(())
}

/// Ответ на «Файл на сервере изменился»: Перезаписать — закачка без сверки; Сравнить —
/// вьювер на файле сервера (строка остаётся); Отмена — копия не закачана.
pub(crate) fn answer(cx: &mut Cx<'_>, doc: DocId, choice: &str) {
    let Some(d) = doc_mut(cx, doc) else {
        return;
    };
    let Some(r) = d.remote.as_deref_mut() else {
        return;
    };
    match choice {
        "overwrite" => {
            let then = r.then.take();
            start_upload(cx, doc, true, then);
        }
        "compare" => {
            let loc = r.loc.clone();
            d.ask = Some(EdAsk::RemoteChanged);
            d.rev += 1;
            open_server_copy(cx, &loc);
        }
        _ => r.then = None,
    }
}

/// Файл, как он сейчас на сервере, — во вьювере (страницы по запросу, без копии).
fn open_server_copy(cx: &mut Cx<'_>, loc: &Location) {
    cx.dispatch(
        Command::new("viewer.open")
            .arg("path", loc.clone())
            .arg("internal", true),
    );
}

/// Удаляет копии документа при закрытии: временная папка — в фоне.
pub(crate) fn on_close(rt: &Runtime, d: &mut EditorDoc) {
    if let Some(r) = d.remote.take() {
        if let Some(u) = &r.up {
            u.cancel.cancel();
        }
        drop_dir(rt, r.dir);
    }
}

/// Сохранено «как» в другой локальный путь: документ больше не копия сервера.
pub(crate) fn detach(rt: &Runtime, d: &mut EditorDoc) {
    on_close(rt, d);
}
