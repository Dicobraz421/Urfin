//! Движок в документе редактора (ADR-0023; S7-29): открытие задачей, страницы кэшей
//! кусков, фоновый индекс файла и счёт переводов строк, действия с итогом в документ,
//! массовые правки задачей (перевод в UTF-8, `editor.eol` → `Rewrite`), «Править копию»,
//! ответы на вопросы движка.

use std::io;
use std::sync::Arc;
use std::time::Instant;

use urfin_core::{CmdError, Cx, Lane, Level, Msg, Runtime};
use urfin_vfs::VfsRegistry;

use super::input::{Act, Done, Engine, Env, Sel, Selections};
use super::load::{LoadError, LoadOpts};
use super::open::{Guard, Opened};
use super::piece::{Piece, Src, Tree, create_temp};
use super::undo::{Kind, Op};
use super::write::{RewriteReq, Rewritten};
use super::{EdAsk, EdState, EditorDoc, LineChanges};
use crate::cache::Loaded;
use crate::detect::Eol;
use crate::enc::Enc;
use crate::index::{IndexDelta, IndexMode, Indexer, Sep, spawn_index};
use crate::source::{ByteSource, read_full};
use crate::{DocId, EditorCfg, ViewState};

/// Каталог временных файлов документа (`Add`, `Rewrite`, копия).
fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("Urfin")
}

fn temp_name(doc: DocId) -> String {
    format!("edit-{}-{}", std::process::id(), doc.0)
}

/// Открытие на движке (Interactive lane): итог — [`OpenedMsg`].
pub(crate) fn spawn_open(d: &mut EditorDoc, rt: &Runtime, reg: Arc<VfsRegistry>, opts: LoadOpts) {
    let (doc, gen_, loc) = (d.id, d.load_gen, d.loc.clone());
    rt.spawn(
        Lane::Interactive,
        "edit.open",
        d.load_cancel.clone(),
        move |cx| {
            let res = super::open::open(&reg, &loc, doc.0, &opts, &cx.cancel);
            if matches!(res, Err(LoadError::Cancelled)) {
                return;
            }
            let _ = cx
                .sink
                .send(Box::new(OpenedMsg { doc, gen_, res }), &cx.cancel);
        },
    );
}

/// Итог открытия → документ.
struct OpenedMsg {
    doc: DocId,
    gen_: u32,
    res: Result<Opened, LoadError>,
}

impl std::fmt::Debug for OpenedMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedMsg")
            .field("doc", &self.doc)
            .field("gen", &self.gen_)
            .field("ok", &self.res.is_ok())
            .finish()
    }
}

impl Msg for OpenedMsg {
    fn name(&self) -> &'static str {
        "edit.opened"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let (cfg, max_line) = (view.editor.clone(), view.viewer.max_line);
        let Some(d) = view.editor_mut(self.doc) else {
            return;
        };
        if let Some(err) = d.on_opened(self.gen_, self.res, rt, &cfg, max_line) {
            super::cmd::fail_load(cx, self.doc, &err);
        }
    }
}

impl EditorDoc {
    /// Фоновой работы над видимым нет: открыт, страницы на месте, счётчик изменений и
    /// поиск досчитаны (сцены `--screens`, замеры).
    pub fn idle(&self) -> bool {
        let Some(e) = self.engine.as_deref() else {
            return false;
        };
        let search = self
            .search
            .as_ref()
            .is_none_or(|s| !s.st.running && !s.st.counting);
        !matches!(self.state, EdState::Loading { .. } | EdState::Replacing)
            && !e.busy
            && e.need.is_none()
            && e.wait.is_none()
            && !self.changes_busy
            && self.changes_edits == e.text.edits()
            && search
    }

    /// Документ на движке принимает правку: готов или пишется (снимок, ADR-0023 §8).
    pub(crate) fn editable(&self) -> bool {
        matches!(self.state, EdState::Ready | EdState::Saving(_))
    }

    /// Что документ даёт действию движка.
    pub(crate) fn env(&self) -> Env {
        Env {
            eol_new: self.eol_new,
            readonly: self.readonly || !self.editable() || self.ask.is_some(),
            now: Instant::now(),
        }
    }

    /// Сохранено потоком (S7-31): журнал помнит сохранённое состояние снимка; корень не
    /// менялся и кодировка та же — документ переходит на новый файл (один кусок
    /// `Original`, индекс — готовый), прежний исходник — источник отмены `Rewrite(n)`.
    /// Иначе куски остаются на прежней версии (её handle жив), ● — по журналу.
    pub(crate) fn on_engine_saved(
        &mut self,
        stamp: super::DiskStamp,
        s: &super::flow::Snapped,
        reopened: Option<super::save::Reopened>,
        rt: &Runtime,
    ) {
        self.stamp = stamp;
        self.stamp_gen = self.stamp_gen.wrapping_add(1);
        self.is_new = false;
        self.edited_at = None;
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        e.undo.mark_saved_at(s.mark);
        let same = e.text.edits() == s.edits && s.enc == e.enc.enc;
        if let (true, Some(r)) = (same, reopened) {
            let slot = e.text.rebase(r.src, r.bom, r.index);
            e.undo.rename_src(Src::Original, slot);
            e.guard = r.guard;
            e.orig = r.bom..e.text.orig().len();
            e.read_failed = false;
            // Индекс и счёт строк прежнего файла больше не нужны.
            self.load_cancel.cancel();
            self.load_cancel = self.cancel.child();
            self.index_epoch = self.index_epoch.wrapping_add(1);
            self.lf_busy = false;
            self.changes = LineChanges::default();
            self.changes_edits = e.text.edits();
            tracing::debug!(?slot, guard = ?e.guard, "editor: document on the saved file");
        }
        self.dirty = e.undo.is_modified();
        self.cursor = e.cursor();
        self.bump();
        self.settle(rt);
    }

    /// Открыто движком: документ — на кусках, индекс большого файла — в фоне.
    fn on_opened(
        &mut self,
        gen_: u32,
        res: Result<Opened, LoadError>,
        rt: &Runtime,
        cfg: &EditorCfg,
        max_line: u32,
    ) -> Option<String> {
        if gen_ != self.load_gen {
            return None;
        }
        self.bump();
        let o = match res {
            Ok(o) => o,
            Err(LoadError::Cancelled) => return None,
            Err(e) => {
                self.state = EdState::Failed;
                return Some(e.to_string());
            }
        };
        let len = o.text.orig().len();
        let orig = u64::from(o.enc.bom_len).min(len)..len;
        let mut e = Engine::new(o.text, o.enc, o.eol, o.guard, orig);
        (e.cols, e.rows) = self.view_size;
        e.tab = cfg.tab_size;
        e.expand_tabs = cfg.expand_tabs;
        e.wrap = cfg.wrap;
        e.max_line = max_line;
        if let Some((line, col)) = self.start.take() {
            let off = e
                .text
                .offset_of_line(u64::from(line))
                .map_or(e.len(), crate::index::LineGuess::get);
            let env = Env {
                eol_new: o.eol_new,
                readonly: true,
                now: Instant::now(),
            };
            // Страниц нет — переход ждёт их (`Engine::wait`).
            let _ = e.act(Act::Goto { off, col }, &env);
        }
        self.enc = o.enc;
        self.detected = o.detected;
        self.bom = o.bom;
        self.eol = o.eol;
        self.eol_new = o.eol_new;
        self.lang = o.lang;
        self.lossy = o.lossy;
        self.stamp = o.stamp;
        self.stamp_gen = self.stamp_gen.wrapping_add(1);
        self.readonly = o.readonly;
        self.is_new = o.new;
        self.changes = LineChanges::default();
        self.changes_edits = e.text.edits();
        self.changes_busy = false;
        self.dirty = false;
        self.ask = None;
        self.pending = None;
        self.force = false;
        self.edited_at = None;
        self.state = EdState::Ready;
        self.cursor = e.cursor();
        let index = (!e.text.index().done && e.text.orig().mapped().is_none())
            .then(|| (e.text.orig().clone(), e.text.sep()));
        self.engine = Some(Box::new(e));
        self.index_epoch = self.index_epoch.wrapping_add(1);
        self.lf_busy = false;
        if let Some((src, sep)) = index {
            let mode = IndexMode::of(src.kind(), None);
            spawn_index(
                rt,
                self.id,
                self.index_epoch,
                src,
                Indexer::new(sep),
                mode,
                self.load_cancel.clone(),
            );
        }
        self.settle(rt);
        None
    }

    /// Просьба кэшам (видимое или то, чего ждёт действие) и запуск loader'ов; текст
    /// менялся — счётчик изменений шапки фоном.
    pub(crate) fn settle(&mut self, rt: &Runtime) {
        self.start_changes(rt);
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        e.want_visible();
        for (src, loader, req) in e.text.take_requests() {
            let doc = self.id;
            rt.spawn(
                Lane::Interactive,
                "edit.pages",
                self.cancel.clone(),
                move |cx| {
                    let loaded = loader.load(&req, &cx.cancel);
                    let _ = cx
                        .sink
                        .send(Box::new(PagesMsg { doc, src, loaded }), &cx.cancel);
                },
            );
        }
    }

    /// Итог действия → документ: признак правки, курсор для истории, автосохранение.
    pub(super) fn after(&mut self, done: &Done) -> bool {
        let Some(e) = self.engine.as_deref() else {
            return false;
        };
        let edited = match done {
            Done::Same => return false,
            Done::Edited => {
                self.dirty = e.undo.is_modified();
                self.edited_at = Some(Instant::now());
                true
            }
            Done::Moved => false,
            Done::Ask(a) => {
                self.ask = Some(*a);
                false
            }
        };
        self.cursor = e.cursor();
        if edited {
            self.search_edited();
        }
        self.bump();
        true
    }

    /// Действие движка (команда, ввод, мышь); `true` — документ изменился или действие
    /// ждёт страниц.
    pub(crate) fn act(&mut self, a: Act, rt: &Runtime) -> bool {
        let env = self.env();
        let Some(e) = self.engine.as_deref_mut() else {
            return false;
        };
        let changed = match e.act(a, &env) {
            Ok(d) => self.after(&d),
            Err(_) => true,
        };
        self.settle(rt);
        changed
    }

    /// Вырезание после копирования: удаление выделения одной группой.
    pub(crate) fn cut(&mut self, rt: &Runtime) -> bool {
        let env = self.env();
        let Some(e) = self.engine.as_deref_mut() else {
            return false;
        };
        let changed = e.cut(&env).is_ok_and(|d| self.after(&d));
        self.settle(rt);
        changed
    }

    /// `editor.undo` / `editor.redo` на движке.
    pub(crate) fn undo(&mut self, redo: bool, rt: &Runtime) -> bool {
        if !self.editable() {
            return false;
        }
        let Some(e) = self.engine.as_deref_mut() else {
            return false;
        };
        let Some((enc, eol)) = e.undo(redo) else {
            return false;
        };
        if let Some(enc) = enc {
            self.enc = enc;
        }
        if let Some(eol) = eol {
            self.eol = eol;
        }
        self.after(&Done::Edited);
        self.settle(rt);
        true
    }

    /// Страницы пришли: ждавшее действие или вывод курсора — снова.
    fn on_pages(&mut self, src: Src, loaded: Loaded, rt: &Runtime) {
        let env = self.env();
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        e.on_pages(src, loaded);
        if let Some(Ok(d)) = e.retry(&env) {
            self.after(&d);
        }
        self.bump();
        self.settle(rt);
    }

    /// Счётчик шапки («+2 добавлено · 1 изменена») по снимку — фоном (Bulk), один за
    /// раз; правили, пока считали, — заново по итогу.
    fn start_changes(&mut self, rt: &Runtime) {
        if self.changes_busy {
            return;
        }
        let Some(e) = self.engine.as_deref() else {
            return;
        };
        let edits = e.text.edits();
        if edits == self.changes_edits {
            return;
        }
        self.changes_busy = true;
        self.changes_edits = edits;
        let (doc, epoch, snap, orig) = (self.id, self.index_epoch, e.text.snap(), e.orig.clone());
        rt.spawn(Lane::Bulk, "edit.changes", self.cancel.clone(), move |cx| {
            match super::changes::count(&snap, orig, &cx.cancel) {
                Ok(c) => {
                    let msg = ChangesMsg {
                        doc,
                        epoch,
                        edits,
                        changes: c,
                    };
                    let _ = cx.sink.send(Box::new(msg), &cx.cancel);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => tracing::warn!(error = %e, "editor: change count failed"),
            }
        });
    }

    /// Итог счёта изменений: свежий — в шапку; текст с тех пор менялся — снова.
    fn on_changes(&mut self, epoch: u32, edits: u64, c: LineChanges, rt: &Runtime) -> bool {
        self.changes_busy = false;
        let fresh = epoch == self.index_epoch && edits == self.changes_edits;
        let changed = fresh && c != self.changes;
        if changed {
            self.changes = c;
            self.bump();
        }
        let now = self.engine.as_deref().map(|e| e.text.edits());
        if now != Some(self.changes_edits) {
            self.changes_edits = edits;
            self.start_changes(rt);
        }
        changed
    }

    /// Дельта индекса файла (`IndexMsg`): номера строк; неизвестные куски — счётом.
    pub(crate) fn on_index(&mut self, epoch: u32, delta: IndexDelta, rt: &Runtime) {
        if epoch != self.index_epoch {
            return;
        }
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        e.text.on_index(delta);
        self.start_lf(rt);
        self.bump();
    }

    /// Переводы строк кусков с `Lf::Unknown`, до которых дошёл индекс, — фоном по
    /// снимку (`lf_patch`); один счёт за раз.
    fn start_lf(&mut self, rt: &Runtime) {
        if self.lf_busy {
            return;
        }
        let Some(e) = self.engine.as_deref() else {
            return;
        };
        let ix = e.text.index().clone();
        let ready = e
            .text
            .tree()
            .unknown()
            .iter()
            .any(|(_, p)| p.src != Src::Original || p.end() <= ix.upto);
        if !ready {
            return;
        }
        self.lf_busy = true;
        let (doc, epoch, snap) = (self.id, self.index_epoch, e.text.snap());
        rt.spawn(
            Lane::Bulk,
            "edit.lf",
            self.cancel.clone(),
            move |cx| match super::lines::lf_patch(&snap, &ix, &cx.cancel) {
                Ok(patch) => {
                    let msg = LfMsg { doc, epoch, patch };
                    let _ = cx.sink.send(Box::new(msg), &cx.cancel);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => tracing::warn!(error = %e, "editor: line count failed"),
            },
        );
    }

    /// Итог счёта переводов строк.
    pub(crate) fn on_lf(&mut self, epoch: u32, patch: &super::lines::LfPatch, rt: &Runtime) {
        if epoch != self.index_epoch {
            return;
        }
        self.lf_busy = false;
        if let Some(e) = self.engine.as_deref_mut()
            && e.text.apply_lf(patch) > 0
        {
            self.cursor = e.cursor();
            self.bump();
        }
        self.start_lf(rt);
    }

    /// Массовая правка задачей (Bulk): документ — в кодировке `to` с концами строк `eol`
    /// (перевод в UTF-8, `editor.eol`); на время — только для чтения. Итог — одна группа
    /// `Op::Root`.
    pub(crate) fn start_rewrite(&mut self, rt: &Runtime, to: Enc, eol: Option<Eol>) {
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        if e.busy {
            return;
        }
        e.busy = true;
        let snap = e.text.snap();
        let from = e.enc;
        let anchor = match e.sel.sel {
            Sel::Stream { anchor } => anchor,
            _ => e.sel.caret,
        };
        let marks = [e.sel.caret, anchor];
        let doc = self.id;
        self.bump();
        rt.spawn(Lane::Bulk, "edit.rewrite", self.cancel.clone(), move |cx| {
            let (dir, name) = (temp_dir(), temp_name(doc));
            let req = RewriteReq {
                from,
                to: Enc { bom_len: 0, ..to },
                eol,
                marks: &marks,
                dir: &dir,
                name: &name,
            };
            let res = super::write::rewrite(&snap, &req, &|| cx.cancel.check());
            if matches!(res, Err(super::save::SaveError::Cancelled)) {
                return;
            }
            let res = res.map_err(|e| e.to_string());
            let msg = RewriteMsg { doc, to, eol, res };
            let _ = cx.sink.send(Box::new(msg), &cx.cancel);
        });
    }

    /// Итог массовой правки: новый корень одной группой отмены, затем ждавший ввод.
    pub(crate) fn on_rewrite(
        &mut self,
        to: Enc,
        eol: Option<Eol>,
        res: Result<Rewritten, String>,
        rt: &Runtime,
    ) -> Option<String> {
        let bom = self.bom;
        let (old_enc, old_eol) = (self.enc, self.eol);
        let e = self.engine.as_deref_mut()?;
        e.busy = false;
        let rw = match res {
            Ok(rw) => rw,
            Err(err) => {
                e.held = None;
                self.bump();
                return Some(err);
            }
        };
        let n = e.text.push_rewrite(rw.src);
        let after = Tree::from_pieces(rw.pieces.into_iter().map(|p| Piece { src: n, ..p }));
        let before = e.text.tree().clone();
        let sel_before = e.sel;
        e.text.set_tree(after.clone());
        let enc_changed = to.enc != e.enc.enc;
        let new_enc = if enc_changed {
            let bom_len = match (bom, to.is_utf8(), to.is_utf16()) {
                (true, true, _) => 3,
                (true, _, true) => 2,
                _ => 0,
            };
            Enc {
                enc: to.enc,
                auto: false,
                bom_len,
            }
        } else {
            old_enc
        };
        e.enc = Enc {
            bom_len: 0,
            ..new_enc
        };
        if let Some(l) = eol {
            e.eol = l;
        }
        e.text.set_sep(Sep::of(&e.enc, e.eol));
        let caret = rw.marks.first().copied().unwrap_or(0);
        e.sel = match sel_before.sel {
            Sel::Stream { .. } => Selections {
                caret,
                sel: Sel::Stream {
                    anchor: rw.marks.get(1).copied().unwrap_or(caret),
                },
            },
            _ => Selections::at(caret),
        };
        e.undo.begin_group(Kind::Command);
        e.undo.record(
            Op::Root {
                before,
                after,
                enc: enc_changed.then_some((old_enc, new_enc)),
                eol: eol.map(|l| (old_eol, l)),
            },
            Kind::Command,
            &sel_before,
            &e.sel,
            Instant::now(),
        );
        let held = e.held.take();
        self.enc = new_enc;
        self.bom = new_enc.bom_len > 0;
        if let Some(l) = eol {
            self.eol = l;
        }
        self.after(&Done::Edited);
        if let Some(a) = held {
            self.act(a, rt);
        }
        self.settle(rt);
        None
    }

    /// «Править копию»: исходник копируется во `%TEMP%` задачей (Bulk), куски — на копию.
    pub(crate) fn start_copy(&mut self, rt: &Runtime) {
        let Some(e) = self.engine.as_deref_mut() else {
            return;
        };
        if e.busy {
            return;
        }
        e.busy = true;
        let src = e.text.orig().clone();
        let doc = self.id;
        self.bump();
        rt.spawn(Lane::Bulk, "edit.copy", self.cancel.clone(), move |cx| {
            let res = copy_source(&*src, doc, &|| cx.cancel.is_cancelled());
            if matches!(&res, Err(e) if e.kind() == io::ErrorKind::Interrupted) {
                return;
            }
            let res = res.map_err(|e| e.to_string());
            let msg = CopyMsg { doc, res };
            let _ = cx.sink.send(Box::new(msg), &cx.cancel);
        });
    }

    /// Копия готова: исходник — она, ждавшая правка — снова.
    pub(crate) fn on_copy(
        &mut self,
        res: Result<Arc<dyn ByteSource>, String>,
        rt: &Runtime,
    ) -> Option<String> {
        let e = self.engine.as_deref_mut()?;
        e.busy = false;
        let held = e.held.take();
        match res {
            Ok(src) => {
                e.text.replace_orig(src);
                e.guard = Guard::Copy;
                self.bump();
                if let Some(a) = held {
                    self.act(a, rt);
                }
                self.settle(rt);
                None
            }
            Err(err) => {
                self.bump();
                Some(err)
            }
        }
    }
}

/// Копия источника во временный файл (блоки по 4 МБ, отмена между ними).
fn copy_source(
    src: &dyn ByteSource,
    doc: DocId,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Arc<dyn ByteSource>> {
    let file = create_temp(&temp_dir(), &temp_name(doc), "copy")?;
    let len = src.len();
    let mut buf = vec![0u8; super::write::BLOCK.min(usize::try_from(len).unwrap_or(1).max(1))];
    let mut at = 0u64;
    while at < len {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let k = buf.len().min((len - at) as usize);
        let n = read_full(src, at, &mut buf[..k])?;
        if n == 0 {
            break;
        }
        crate::sys::write_all_at(&file, &buf[..n], at)?;
        at += n as u64;
    }
    Ok(Arc::new(super::piece::SpillSource::sealed(file, at)))
}

/// Страницы кэша кусков → документ.
#[derive(Debug)]
struct PagesMsg {
    doc: DocId,
    src: Src,
    loaded: Loaded,
}

impl Msg for PagesMsg {
    fn name(&self) -> &'static str {
        "edit.pages"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let PagesMsg { doc, src, loaded } = *self;
        match cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(doc))
        {
            Some(d) => d.on_pages(src, loaded, rt),
            None => cx.keep_rev(),
        }
    }
}

/// Переводы строк кусков посчитаны.
#[derive(Debug)]
struct LfMsg {
    doc: DocId,
    epoch: u32,
    patch: super::lines::LfPatch,
}

impl Msg for LfMsg {
    fn name(&self) -> &'static str {
        "edit.lf"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        match cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc))
        {
            Some(d) => d.on_lf(self.epoch, &self.patch, rt),
            None => cx.keep_rev(),
        }
    }
}

/// Массовая правка готова.
#[derive(Debug)]
struct RewriteMsg {
    doc: DocId,
    to: Enc,
    eol: Option<Eol>,
    res: Result<Rewritten, String>,
}

impl Msg for RewriteMsg {
    fn name(&self) -> &'static str {
        "edit.rewrite"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let RewriteMsg { doc, to, eol, res } = *self;
        let err = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(doc))
            .and_then(|d| d.on_rewrite(to, eol, res, rt));
        if let Some(err) = err {
            report_failed(cx, "editor.rewrite", err);
        }
    }
}

/// Копия исходника готова («Править копию»).
struct CopyMsg {
    doc: DocId,
    res: Result<Arc<dyn ByteSource>, String>,
}

impl std::fmt::Debug for CopyMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyMsg")
            .field("doc", &self.doc)
            .field("ok", &self.res.is_ok())
            .finish()
    }
}

impl Msg for CopyMsg {
    fn name(&self) -> &'static str {
        "edit.copy"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let CopyMsg { doc, res } = *self;
        let err = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(doc))
            .and_then(|d| d.on_copy(res, rt));
        if let Some(err) = err {
            report_failed(cx, "editor.copy", err);
        }
    }
}

/// Ответ на вопрос движка (`editor.answer` при `EdAsk::NoChar` / `Writer` / `NoSpace`).
pub(crate) fn answer(
    cx: &mut Cx<'_>,
    doc: DocId,
    ask: EdAsk,
    choice: &str,
) -> Result<(), CmdError> {
    let rt = cx.rt;
    let view = crate::handler::view_mut(cx)?;
    let Some(d) = view.editor_mut(doc) else {
        return Ok(());
    };
    if choice != "cancel" && !ask.choices().iter().any(|c| c.id == choice) {
        return Err(CmdError::Arg {
            name: "choice".into(),
            msg: format!("unexpected answer {choice} to {ask:?}").into(),
        });
    }
    d.ask = None;
    d.rev += 1;
    match (ask, choice) {
        (EdAsk::NoSpace { .. }, "cancel") => {
            d.pending = None;
            d.then_view = None;
        }
        (_, "cancel") => {
            if let Some(e) = d.engine.as_deref_mut() {
                e.held = None;
            }
        }
        (EdAsk::NoChar { .. }, "utf8") => d.start_rewrite(rt, Enc::utf8(), None),
        (EdAsk::NoChar { .. }, _) => {
            if let Some(a) = d.engine.as_deref_mut().and_then(|e| e.held.take()) {
                d.act(a.with_replace(), rt);
            }
        }
        (EdAsk::NoSpace { .. }, "anyway") => {
            if let Some(mut p) = d.pending.take() {
                let super::save::Body::Snap(b) = &mut p.text;
                b.space = false;
                super::flow::start_save(cx, doc, p.text, p.then, p.flags);
            }
        }
        (EdAsk::NoSpace { .. }, _) => {
            d.pending = None;
            d.then_view = None;
        }
        (EdAsk::Writer, "copy") => d.start_copy(rt),
        (EdAsk::Writer, "view") => {
            if let Some(e) = d.engine.as_deref_mut() {
                e.held = None;
            }
            d.then_view = Some(d.cursor.0);
            super::flow::close_doc(cx, doc);
        }
        // Вопросы сохранения и закрытия отвечает `flow::answer`.
        _ => {}
    }
    Ok(())
}

/// Счётчик изменений посчитан.
#[derive(Debug)]
struct ChangesMsg {
    doc: DocId,
    epoch: u32,
    edits: u64,
    changes: LineChanges,
}

impl Msg for ChangesMsg {
    fn name(&self) -> &'static str {
        "edit.changes"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let ChangesMsg {
            doc,
            epoch,
            edits,
            changes,
        } = *self;
        let changed = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(doc))
            .is_some_and(|d| d.on_changes(epoch, edits, changes, rt));
        if !changed {
            cx.keep_rev();
        }
    }
}

/// Тост об ошибке задачи движка.
pub(crate) fn report_failed(cx: &mut Cx<'_>, id: &str, err: String) {
    tracing::warn!(error = %err, id, "editor task failed");
    let mut args = urfin_i18n::FluentArgs::new();
    args.set("id", id.to_owned());
    args.set("error", err);
    let text = urfin_i18n::current().fmt("core-command-failed", &args);
    cx.toast(Level::Error, text);
}
