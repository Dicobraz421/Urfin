//! Загрузка документа редактора (S7 §5.1) на Interactive lane: выборка 64 КБ → детект
//! (§3.2) или кодировка, выбранная вручную → потоковое декодирование кусками по мебибайту в
//! `ropey::RopeBuilder`. Попутно — хэши строк для полосы изменений и счёт концов строк
//! (EOL «как в файле» и доминирующий для новых строк). Недекодируемые байты — `lossy`.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use encoding_rs::{CoderResult, Decoder, Encoding};
use ropey::{Rope, RopeBuilder};
use urfin_core::{CancelToken, Cancelled, Cx, Lane, Location, Msg, Runtime};
use urfin_vfs::{VfsError, VfsRegistry};

use super::DiskStamp;
use super::diff::{EolCount, LineScan};
use crate::config::EolCfg;
use crate::detect::{Eol, detect};
use crate::enc::Enc;
use crate::viewer::{SAMPLE, bom_len};
use crate::{DocId, ViewState};

/// Кусок чтения и декодирования.
pub const CHUNK: usize = 1 << 20;
/// Прогресс загрузки — не чаще.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Как загружать.
#[derive(Clone, Debug)]
pub struct LoadOpts {
    /// Кодировка, выбранная вручную (F8, история); `None` — детект.
    pub enc: Option<&'static Encoding>,
    /// `editor.max_size_mb` в байтах.
    pub limit: u64,
    /// Подсказка детекту по языку интерфейса.
    pub tld: Option<&'static [u8]>,
    /// `editor.eol`: EOL новых строк.
    pub eol: EolCfg,
    /// Имя файла: язык подсветки.
    pub name: String,
    /// `editor.new`: файла нет — пустой документ с этой кодировкой (иначе — ошибка).
    pub new: Option<NewDoc>,
}

/// Новый файл (`editor.new`): кодировка и BOM из `[editor] new_codepage` / `new_bom`.
#[derive(Clone, Copy, Debug)]
pub struct NewDoc {
    pub enc: &'static Encoding,
    pub bom: bool,
}

/// Почему не загрузилось.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// Больше `editor.max_size_mb`: строка в шапке «Файл слишком большой» (§5.1).
    #[error("file is too large for the editor: {0} bytes")]
    TooLarge(u64),
    #[error("{0}")]
    Io(String),
    #[error("cancelled")]
    Cancelled,
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        LoadError::Io(e.to_string())
    }
}

impl From<VfsError> for LoadError {
    fn from(e: VfsError) -> Self {
        match e {
            VfsError::Cancelled => LoadError::Cancelled,
            e => LoadError::Io(e.to_string()),
        }
    }
}

impl From<Cancelled> for LoadError {
    fn from(_: Cancelled) -> Self {
        LoadError::Cancelled
    }
}

/// Декодированный текст.
#[derive(Debug)]
pub struct Decoded {
    pub text: Rope,
    /// Кодировка текста (`auto` — по детекту); `bom_len` — BOM файла.
    pub enc: Enc,
    /// Что дал детект (кольцо F8: «авто»).
    pub detected: Enc,
    pub bom: bool,
    /// Концы строк файла (`Mixed` — разные).
    pub eol: Eol,
    /// Конец новых строк: `editor.eol` или доминирующий в файле.
    pub eol_new: Eol,
    pub lossy: bool,
    pub lang: Option<&'static str>,
    /// Хэши строк (полоса изменений).
    pub hashes: Arc<[u64]>,
    /// Прочитано байт.
    pub bytes: u64,
}

/// Читает, пока буфер не полон или поток не кончился.
fn read_fill(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match r.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// Декодер в Rope со счётом строк.
struct Sink {
    builder: RopeBuilder,
    scan: LineScan,
    buf: String,
    lossy: bool,
}

impl Sink {
    fn feed(&mut self, dec: &mut Decoder, mut input: &[u8], last: bool) {
        loop {
            let cap = dec
                .max_utf8_buffer_length(input.len())
                .unwrap_or(input.len() * 3 + 16);
            self.buf.clear();
            self.buf.reserve(cap);
            let (r, read, replaced) = dec.decode_to_string(input, &mut self.buf, last);
            self.lossy |= replaced;
            input = &input[read..];
            if !self.buf.is_empty() {
                self.scan.feed(&self.buf);
                self.builder.append(&self.buf);
            }
            if r == CoderResult::InputEmpty {
                return;
            }
        }
    }
}

/// EOL по счёту: один вид — он, разные — `Mixed`, нет — `CrLf` (как у детекта).
pub fn eol_of(c: EolCount) -> Eol {
    let kinds = [(c.lf, Eol::Lf), (c.crlf, Eol::CrLf), (c.cr, Eol::Cr)];
    let mut present = kinds.iter().filter(|(n, _)| *n > 0);
    match (present.next(), present.next()) {
        (None, _) => Eol::default(),
        (Some((_, e)), None) => *e,
        _ => Eol::Mixed,
    }
}

/// Конец новых строк: из `editor.eol` или доминирующий в файле (поровну — CRLF; новый
/// файл и файл без переводов строк — CRLF).
pub fn eol_new(cfg: EolCfg, c: EolCount) -> Eol {
    match cfg {
        EolCfg::Lf => Eol::Lf,
        EolCfg::Crlf => Eol::CrLf,
        EolCfg::File => {
            if c.lf > c.crlf && c.lf >= c.cr {
                Eol::Lf
            } else if c.cr > c.crlf && c.cr > c.lf {
                Eol::Cr
            } else {
                Eol::CrLf
            }
        }
    }
}

/// Декодирует поток `r` (размер `size`, если известен). `progress` — прочитано байт.
pub fn decode(
    r: &mut dyn Read,
    size: Option<u64>,
    o: &LoadOpts,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u64),
) -> Result<Decoded, LoadError> {
    let _span = tracing::debug_span!("edit.decode", ?size).entered();
    if let Some(s) = size
        && s > o.limit
    {
        return Err(LoadError::TooLarge(s));
    }
    let mut head = vec![0u8; SAMPLE];
    let n = read_fill(r, &mut head)?;
    head.truncate(n);
    let at_eof = n < SAMPLE;
    let det = detect(&head, at_eof, o.tld);
    let enc = match o.enc {
        Some(e) => Enc {
            bom_len: bom_len(e, &head),
            ..Enc::new(e)
        },
        None => det.enc,
    };
    let lang = if det.binary {
        None
    } else {
        crate::lang::detect(&o.name, &head)
    };
    let mut dec = enc.enc.new_decoder_without_bom_handling();
    let mut sink = Sink {
        builder: RopeBuilder::new(),
        scan: LineScan::default(),
        buf: String::new(),
        lossy: false,
    };
    let skip = usize::from(enc.bom_len).min(head.len());
    sink.feed(&mut dec, &head[skip..], false);
    let mut total = head.len() as u64;
    progress(total);
    if !at_eof {
        let mut buf = vec![0u8; CHUNK];
        loop {
            cancel.check()?;
            let n = read_fill(r, &mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > o.limit {
                return Err(LoadError::TooLarge(size.unwrap_or(total).max(total)));
            }
            sink.feed(&mut dec, &buf[..n], false);
            progress(total);
            if n < buf.len() {
                break;
            }
        }
    }
    sink.feed(&mut dec, &[], true);
    let Sink {
        builder,
        scan,
        lossy,
        ..
    } = sink;
    let (hashes, count) = scan.finish();
    Ok(Decoded {
        text: builder.finish(),
        enc,
        detected: det.enc,
        bom: enc.bom_len > 0,
        eol: eol_of(count),
        eol_new: eol_new(o.eol, count),
        lossy,
        lang,
        hashes: hashes.into(),
        bytes: total,
    })
}

/// Загруженный документ с отметкой диска.
#[derive(Debug)]
pub struct Loaded {
    pub decoded: Decoded,
    pub stamp: DiskStamp,
    /// Атрибут «только чтение».
    pub readonly: bool,
    /// Файла нет: новый документ (`editor.new`).
    pub new: bool,
}

/// Пустой документ нового файла: EOL новых строк — по `editor.eol` (как в файле — CRLF).
pub fn new_doc(o: &LoadOpts, n: NewDoc) -> Loaded {
    let unicode = n.enc == encoding_rs::UTF_8
        || n.enc == encoding_rs::UTF_16LE
        || n.enc == encoding_rs::UTF_16BE;
    let bom = n.bom && unicode;
    let bom_len = match (bom, n.enc == encoding_rs::UTF_8) {
        (false, _) => 0,
        (true, true) => 3,
        (true, false) => 2,
    };
    let enc = Enc {
        bom_len,
        ..Enc::new(n.enc)
    };
    let text = Rope::new();
    let hashes = super::diff::line_hashes(&text);
    Loaded {
        decoded: Decoded {
            text,
            enc,
            detected: enc,
            bom,
            eol: Eol::default(),
            eol_new: eol_new(o.eol, EolCount::default()),
            lossy: false,
            lang: crate::lang::detect(&o.name, &[]),
            hashes: hashes.into(),
            bytes: 0,
        },
        stamp: DiskStamp::default(),
        readonly: false,
        new: true,
    }
}

/// Локальный файл: последовательное чтение по своему handle (вне UI-потока).
pub fn load_path(
    path: &Path,
    o: &LoadOpts,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u64),
) -> Result<Loaded, LoadError> {
    let mut f: File = match crate::sys::open_seq(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound && o.new.is_some() => {
            return Ok(new_doc(o, o.new.expect("checked")));
        }
        Err(e) => return Err(e.into()),
    };
    let meta = f.metadata()?;
    let stamp = DiskStamp {
        size: meta.len(),
        mtime: super::save::mtime_ns(&meta),
        file_id: crate::sys::file_id(&f).ok(),
    };
    let readonly = meta.permissions().readonly();
    let decoded = decode(&mut f, Some(meta.len()), o, cancel, progress)?;
    Ok(Loaded {
        decoded,
        stamp,
        readonly,
        new: false,
    })
}

/// Любой VFS: локальный путь — [`load_path`], иначе поток `open_read`.
pub fn load(
    reg: &VfsRegistry,
    loc: &Location,
    o: &LoadOpts,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u64),
) -> Result<Loaded, LoadError> {
    let vfs = reg.get(loc)?;
    if let Some(path) = vfs.local_path(loc) {
        return load_path(&path, o, cancel, progress);
    }
    let meta = vfs.stat(loc, true).ok();
    let mut r = vfs.open_read(loc)?;
    let size = meta.as_ref().map(|m| m.size).or_else(|| r.len());
    let stamp = DiskStamp {
        size: size.unwrap_or(0),
        mtime: meta.as_ref().map_or(i64::MIN, |m| m.mtime),
        file_id: None,
    };
    let decoded = decode(&mut r, size, o, cancel, progress)?;
    Ok(Loaded {
        decoded,
        stamp,
        readonly: false,
        new: false,
    })
}

/// Задача загрузки.
pub(crate) struct LoadJob {
    pub doc: DocId,
    /// Поколение загрузки документа: ответы прежней (перечитывание) отбрасываются.
    pub gen_: u32,
    pub loc: Location,
    pub reg: Arc<VfsRegistry>,
    pub opts: LoadOpts,
}

/// Задача `edit.load` на Interactive lane: прогресс — `ProgressMsg` не чаще 10 раз в
/// секунду, итог — `LoadedMsg`.
pub(crate) fn spawn_load(rt: &Runtime, job: LoadJob, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "edit.load", cancel, move |cx| {
        let LoadJob {
            doc,
            gen_,
            loc,
            reg,
            opts,
        } = job;
        let mut last = Instant::now();
        let sink = cx.sink.clone();
        let tok = cx.cancel.clone();
        let mut progress = |done: u64| {
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                let _ = sink.send(Box::new(ProgressMsg { doc, gen_, done }), &tok);
            }
        };
        let res = load(&reg, &loc, &opts, &cx.cancel, &mut progress);
        if matches!(res, Err(LoadError::Cancelled)) {
            return;
        }
        let _ = cx
            .sink
            .send(Box::new(LoadedMsg { doc, gen_, res }), &cx.cancel);
    });
}

/// Прогресс загрузки → `EdState::Loading`.
#[derive(Debug)]
struct ProgressMsg {
    doc: DocId,
    gen_: u32,
    done: u64,
}

impl Msg for ProgressMsg {
    fn name(&self) -> &'static str {
        "edit.progress"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        if let Some(d) = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc))
        {
            d.on_progress(self.gen_, self.done);
        }
    }
}

/// Итог загрузки → документ.
struct LoadedMsg {
    doc: DocId,
    gen_: u32,
    res: Result<Loaded, LoadError>,
}

impl std::fmt::Debug for LoadedMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedMsg")
            .field("doc", &self.doc)
            .field("gen", &self.gen_)
            .field("ok", &self.res.is_ok())
            .finish_non_exhaustive()
    }
}

impl Msg for LoadedMsg {
    fn name(&self) -> &'static str {
        "edit.loaded"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let Some(d) = cx
            .state
            .ext
            .get_mut::<ViewState>()
            .and_then(|v| v.editor_mut(self.doc))
        else {
            return;
        };
        if let Some(err) = d.on_loaded(self.gen_, self.res) {
            super::cmd::fail_load(cx, self.doc, &err);
        }
    }
}
