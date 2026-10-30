//! Задача разбора Markdown на `Lane::Interactive` (S7 §3.10): байты источника →
//! декодирование целиком в текущей кодировке → разбор → разрешение путей → `MdMsg`. У файла
//! больше [`SPLIT_MIN`] сначала уходит разбор первых [`FIRST_CHUNK`] (первый экран), затем
//! весь документ. Отмена — токеном (новый разбор, смена кодировки, закрытие).
//!
//! [`ProjSource`] — проекция как `ByteSource` (`SourceKind::Owned`, без копии): по ней
//! работает поиск S7-09 (F7 в документе, решение §13, вопрос 1).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use urfin_core::{CancelToken, Cx, Lane, Msg, Runtime};

use super::build::{ParseError, parse_with};
use super::resolve::{RealFs, Resolver, resolve};
use super::{FIRST_CHUNK, MdDoc, SPLIT_MIN};
use crate::enc::Enc;
use crate::search::transcode::{OffMap, decode as transcode};
use crate::source::{ByteSource, SourceKind, read_full};
use crate::{DocId, ViewState};

/// Проекция документа как источник байтов (поиск F7).
#[derive(Debug, Clone)]
pub struct ProjSource(pub Arc<MdDoc>);

impl ByteSource for ProjSource {
    fn len(&self) -> u64 {
        self.0.text.len() as u64
    }

    fn is_final(&self) -> bool {
        true
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let b = self.0.text.as_bytes();
        let at = usize::try_from(off).unwrap_or(usize::MAX).min(b.len());
        let n = buf.len().min(b.len() - at);
        buf[..n].copy_from_slice(&b[at..at + n]);
        Ok(n)
    }

    fn mapped(&self) -> Option<&[u8]> {
        Some(self.0.text.as_bytes())
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Owned
    }

    fn refresh_len(&self) -> io::Result<u64> {
        Ok(self.len())
    }
}

/// Текст для разбора и перевод его смещений в байты источника.
#[derive(Debug)]
pub struct Decoded {
    pub text: String,
    bom: u64,
    /// `None` — UTF-8 без замен: смещение + BOM.
    map: Option<OffMap>,
}

impl Decoded {
    /// Декодирует `bytes` (весь файл, с BOM) в кодировке `enc`.
    pub fn new(bytes: &[u8], enc: Enc) -> Decoded {
        let bom = usize::from(enc.bom_len).min(bytes.len());
        let body = &bytes[bom..];
        if enc.is_utf8()
            && let Ok(s) = std::str::from_utf8(body)
        {
            return Decoded {
                text: s.to_owned(),
                bom: bom as u64,
                map: None,
            };
        }
        let mut out = Vec::new();
        let mut map = OffMap::default();
        transcode(body, enc, &mut out, &mut map);
        let text = String::from_utf8(out)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        Decoded {
            text,
            bom: bom as u64,
            map: Some(map),
        }
    }

    /// Байт источника для смещения в тексте.
    pub fn to_src(&self, u: u64) -> u64 {
        let u = usize::try_from(u).unwrap_or(usize::MAX);
        let s = match &self.map {
            None => u.min(self.text.len()),
            Some(m) => m.to_src(self.text.as_bytes(), u),
        };
        s as u64 + self.bom
    }

    /// Смещения документа (в тексте) → байты источника.
    pub fn remap(&self, doc: &mut MdDoc) {
        if self.map.is_none() && self.bom == 0 {
            return;
        }
        for b in &mut doc.blocks {
            b.src = self.to_src(b.src.start)..self.to_src(b.src.end);
        }
        for r in &mut doc.runs {
            r.src = u32::try_from(self.to_src(u64::from(r.src))).unwrap_or(u32::MAX);
        }
        doc.src_len = self.to_src(doc.src_len);
    }
}

/// Граница начала большого файла: последняя пустая строка до `max` (иначе — конец
/// строки, иначе граница символа).
pub fn cut_point(text: &str, max: usize) -> usize {
    if text.len() <= max {
        return text.len();
    }
    let mut m = max;
    while !text.is_char_boundary(m) {
        m -= 1;
    }
    let head = &text.as_bytes()[..m];
    if let Some(i) = memchr::memmem::rfind(head, b"\n\n") {
        return i + 2;
    }
    memchr::memrchr(b'\n', head).map_or(m, |i| i + 1)
}

/// Ошибка задачи.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("cancelled")]
    Cancelled,
}

impl From<ParseError> for LoadError {
    fn from(_: ParseError) -> Self {
        LoadError::Cancelled
    }
}

/// Читает источник, разбирает (у большого файла — сначала начало), разрешает пути;
/// каждый готовый документ — в `emit`.
pub fn load(
    src: &dyn ByteSource,
    enc: Enc,
    r: &Resolver<'_>,
    cancel: &CancelToken,
    mut emit: impl FnMut(MdDoc),
) -> Result<(), LoadError> {
    let len = src.len();
    let owned;
    let bytes = if let Some(m) = src.mapped() {
        &m[..usize::try_from(len).unwrap_or(m.len()).min(m.len())]
    } else {
        let mut v = vec![0u8; usize::try_from(len).unwrap_or(0)];
        let n = read_full(src, 0, &mut v)?;
        v.truncate(n);
        owned = v;
        &owned[..]
    };
    if cancel.is_cancelled() {
        return Err(LoadError::Cancelled);
    }
    let dec = Decoded::new(bytes, enc);
    let full = |dec: &Decoded, upto: usize| -> Result<MdDoc, LoadError> {
        let mut doc = parse_with(&dec.text[..upto], cancel)?;
        doc.complete = upto == dec.text.len();
        dec.remap(&mut doc);
        resolve(&mut doc, r);
        Ok(doc)
    };
    if len > SPLIT_MIN {
        let cut = cut_point(&dec.text, FIRST_CHUNK);
        if cut < dec.text.len() {
            emit(full(&dec, cut)?);
        }
    }
    emit(full(&dec, dec.text.len())?);
    Ok(())
}

/// Что нужно задаче разбора.
#[derive(Debug)]
pub(crate) struct MdJob {
    pub doc: DocId,
    pub pass: u64,
    pub src: Arc<dyn ByteSource>,
    pub enc: Enc,
    pub dir: Option<PathBuf>,
    pub image_max: u64,
}

/// Задача `view.md` на `Lane::Interactive`.
pub(crate) fn spawn_parse(rt: &Runtime, job: MdJob, cancel: CancelToken) {
    rt.spawn(Lane::Interactive, "view.md", cancel, move |cx| {
        let _span = tracing::debug_span!("view.md").entered();
        let r = Resolver {
            dir: job.dir.clone(),
            image_max: job.image_max,
            fs: &RealFs,
        };
        let (doc, pass) = (job.doc, job.pass);
        let res = load(&*job.src, job.enc, &r, &cx.cancel, |d| {
            let msg = MdMsg {
                doc,
                pass,
                res: Ok(Arc::new(d)),
            };
            let _ = cx.sink.send(Box::new(msg), &cx.cancel);
        });
        match res {
            Ok(()) | Err(LoadError::Cancelled) => {}
            Err(LoadError::Io(e)) => {
                let msg = MdMsg {
                    doc,
                    pass,
                    res: Err(e.to_string()),
                };
                let _ = cx.sink.send(Box::new(msg), &cx.cancel);
            }
        }
    });
}

/// Ответ разбора → документ вьювера.
#[derive(Debug)]
pub struct MdMsg {
    pub doc: DocId,
    pub pass: u64,
    pub res: Result<Arc<MdDoc>, String>,
}

impl Msg for MdMsg {
    fn name(&self) -> &'static str {
        "view.md"
    }

    fn apply(self: Box<Self>, cx: &mut Cx<'_>) {
        let rt = cx.rt;
        let Some(view) = cx.state.ext.get_mut::<ViewState>() else {
            return;
        };
        let Some(d) = view.viewer_mut(self.doc) else {
            return;
        };
        d.md_apply(self.pass, self.res, rt);
    }
}
