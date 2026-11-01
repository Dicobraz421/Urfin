//! Потоковый писатель снимка документа (ADR-0023 §3, §5, §8): куски по очереди блоками
//! ≤ 4 МБ (`read_at` источников, не кэш страниц), смена кодировки и концов строк —
//! потоком, память постоянная. Общий для перевода кодировки и `editor.eol` (S7-29),
//! сохранения (S7-31) и «Заменить все» (S7-30).
//!
//! Концы строк меняются по единицам кода без декодирования: в однобайтовых и
//! многобайтовых кодировках байты `0D`/`0A` бывают только символами CR/LF (хвостовые байты
//! `Shift_JIS`, GBK, Big5, EUC — от `0x40`), в UTF-16 — единицы `000D`/`000A`. Поэтому
//! недекодируемые байты исходника сохраняются как были, пока кодировка та же.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use encoding_rs::{
    CoderResult, Decoder, Encoder, EncoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE,
};
use urfin_core::Cancelled;

use super::piece::{Lf, PIECE_MAX, Piece, Src, TextSnap, create_temp};
use super::save::SaveError;
use crate::detect::Eol;
use crate::enc::Enc;
use crate::index::{Sep, scan};
use crate::source::{ByteSource, OwnedSource};

/// Блок чтения кусков.
pub const BLOCK: usize = 4 << 20;
/// Результат не длиннее — в памяти, иначе во временный файл (ADR-0023 §5).
pub const MEM_MAX: u64 = 16 << 20;

/// Байты снимка по порядку блоками ≤ [`BLOCK`]: `f(смещение в документе, байты)`.
/// `check` — точка отмены между блоками.
pub fn blocks(
    snap: &TextSnap,
    check: &dyn Fn() -> Result<(), Cancelled>,
    f: &mut dyn FnMut(u64, &[u8]) -> io::Result<()>,
) -> Result<(), SaveError> {
    let mut buf = vec![
        0u8;
        BLOCK
            .min(usize::try_from(snap.len()).unwrap_or(BLOCK))
            .max(1)
    ];
    let mut fill = 0usize;
    let mut base = 0u64;
    for (_, p) in snap.tree() {
        let mut at = p.start;
        while at < p.end() {
            if fill == buf.len() {
                check()?;
                f(base, &buf)?;
                base += fill as u64;
                fill = 0;
            }
            let k = (buf.len() - fill).min((p.end() - at) as usize);
            let n = snap.read_src_full(p.src, at, &mut buf[fill..fill + k])?;
            if n < k {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "piece source is shorter than the piece",
                )
                .into());
            }
            fill += k;
            at += k as u64;
        }
    }
    if fill > 0 {
        check()?;
        f(base, &buf[..fill])?;
    }
    Ok(())
}

/// Единица кода конца строки.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Unit {
    Cr,
    Lf,
    Other,
}

/// Перекодировщик потока байт документа: другая кодировка (декодирование →
/// кодирование) и/или концы строк (по единицам кода).
pub struct Recode {
    /// Кодировка документа (байты на входе).
    from: Enc,
    to: &'static Encoding,
    /// Новые концы строк (`None` — как есть).
    eol: Option<Eol>,
    dec: Option<Decoder>,
    enc: Option<Encoder>,
    /// Последний CR ещё не выведен (за ним может идти LF).
    cr: bool,
    /// Нечётный байт UTF-16 на границе блоков.
    carry: Option<u8>,
    /// Непредставимых в целевой кодировке символов.
    pub unmappable: usize,
    /// Декодированный кусок (переиспользуется).
    text: String,
    out: Vec<u8>,
}

impl std::fmt::Debug for Recode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recode")
            .field("from", &self.from.enc.name())
            .field("to", &self.to.name())
            .field("eol", &self.eol)
            .field("unmappable", &self.unmappable)
            .finish_non_exhaustive()
    }
}

fn is_utf16(e: &'static Encoding) -> bool {
    e == UTF_16LE || e == UTF_16BE
}

impl Recode {
    /// Из кодировки документа `from` в `to`; `eol` — новые концы строк (`Mixed` не
    /// бывает: тогда как есть).
    pub fn new(from: Enc, to: &'static Encoding, eol: Option<Eol>) -> Recode {
        let same = from.enc == to;
        let enc = (!same && to != UTF_8 && !is_utf16(to)).then(|| to.new_encoder());
        Recode {
            from,
            to,
            eol: eol.filter(|e| *e != Eol::Mixed),
            dec: (!same).then(|| from.enc.new_decoder_without_bom_handling()),
            enc,
            cr: false,
            carry: None,
            unmappable: 0,
            text: String::new(),
            out: Vec::new(),
        }
    }

    fn eol_bytes(&self) -> &'static str {
        match self.eol {
            Some(Eol::Lf) => "\n",
            Some(Eol::Cr) => "\r",
            _ => "\r\n",
        }
    }

    /// Следующий кусок байт документа; `last` — конец. Выход — в `sink`.
    pub fn feed(
        &mut self,
        input: &[u8],
        last: bool,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.out.clear();
        if self.dec.is_none() {
            self.same(input, last);
        } else {
            self.recode(input, last);
        }
        if !self.out.is_empty() {
            sink(&self.out)?;
        }
        Ok(())
    }

    /// Та же кодировка: байты как есть, концы строк — по единицам.
    fn same(&mut self, input: &[u8], last: bool) {
        if self.eol.is_none() {
            self.out.extend_from_slice(input);
            return;
        }
        let u = usize::from(self.from.unit());
        let be = self.from.is_utf16be();
        let joined;
        let data = match self.carry.take() {
            Some(c) => {
                joined = [&[c][..], input].concat();
                &joined[..]
            }
            None => input,
        };
        let whole = data.len() / u * u;
        if whole < data.len() && !last {
            self.carry = Some(data[whole]);
        }
        let eol = self.unit_eol(u, be);
        for unit in data[..whole].chunks_exact(u) {
            let k = match (u, be) {
                (1, _) => unit[0],
                (_, false) if unit[1] == 0 => unit[0],
                (_, true) if unit[0] == 0 => unit[1],
                _ => 0xFF,
            };
            let kind = match k {
                b'\r' => Unit::Cr,
                b'\n' => Unit::Lf,
                _ => Unit::Other,
            };
            if self.cr {
                self.cr = false;
                self.out.extend_from_slice(&eol);
                if kind == Unit::Lf {
                    continue;
                }
            }
            match kind {
                Unit::Cr => self.cr = true,
                Unit::Lf => self.out.extend_from_slice(&eol),
                Unit::Other => self.out.extend_from_slice(unit),
            }
        }
        if last {
            if self.cr {
                self.cr = false;
                self.out.extend_from_slice(&eol);
            }
            // Нечётный хвост UTF-16 (lossy) — как был.
            self.out.extend_from_slice(&data[whole..]);
        }
    }

    /// Новый конец строки в единицах кода документа.
    fn unit_eol(&self, u: usize, be: bool) -> Vec<u8> {
        let s = self.eol_bytes();
        if u == 1 {
            return s.as_bytes().to_vec();
        }
        s.bytes()
            .flat_map(|b| if be { [0, b] } else { [b, 0] })
            .collect()
    }

    /// Другая кодировка: декодирование в UTF-8, концы строк по символам, кодирование.
    fn recode(&mut self, mut input: &[u8], last: bool) {
        let mut dec = self.dec.take().expect("decoder for another encoding");
        loop {
            let cap = dec
                .max_utf8_buffer_length(input.len())
                .unwrap_or(input.len() * 3 + 16);
            self.text.clear();
            self.text.reserve(cap);
            let (r, read, _) = dec.decode_to_string(input, &mut self.text, last);
            input = &input[read..];
            let text = std::mem::take(&mut self.text);
            let end = last && r == CoderResult::InputEmpty;
            self.emit(&text, end);
            self.text = text;
            if r == CoderResult::InputEmpty {
                break;
            }
        }
        self.dec = Some(dec);
    }

    /// Текст UTF-8 → концы строк → целевая кодировка.
    fn emit(&mut self, s: &str, last: bool) {
        if self.eol.is_none() {
            self.encode(s, last);
            return;
        }
        let eol = self.eol_bytes();
        let mut mapped = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            if self.cr {
                self.cr = false;
                mapped.push_str(eol);
                if c == '\n' {
                    continue;
                }
            }
            match c {
                '\r' => self.cr = true,
                '\n' => mapped.push_str(eol),
                c => mapped.push(c),
            }
        }
        if last && self.cr {
            self.cr = false;
            mapped.push_str(eol);
        }
        self.encode(&mapped, last);
    }

    fn encode(&mut self, mut s: &str, last: bool) {
        if self.to == UTF_8 {
            self.out.extend_from_slice(s.as_bytes());
            return;
        }
        if is_utf16(self.to) {
            let be = self.to == UTF_16BE;
            for u in s.encode_utf16() {
                self.out
                    .extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
            return;
        }
        let e = self.enc.as_mut().expect("encoder for a legacy encoding");
        loop {
            let need = e
                .max_buffer_length_from_utf8_without_replacement(s.len())
                .unwrap_or(s.len() * 4 + 16);
            self.out.reserve(need);
            let (r, read) = e.encode_from_utf8_to_vec_without_replacement(s, &mut self.out, last);
            s = &s[read..];
            match r {
                EncoderResult::InputEmpty => return,
                EncoderResult::OutputFull => {}
                EncoderResult::Unmappable(_) => {
                    self.unmappable += 1;
                    self.out.push(b'?');
                }
            }
        }
    }
}

/// Итог перезаписи снимка (`Rewrite`): источник, куски над ним (источник —
/// `Src::Rewrite(0)`, номер ставит документ), новые места отметок.
pub struct Rewritten {
    pub src: Arc<dyn ByteSource>,
    pub pieces: Vec<Piece>,
    /// Смещения `marks` (курсор, выделение) в новом тексте.
    pub marks: Vec<u64>,
    pub unmappable: usize,
}

impl std::fmt::Debug for Rewritten {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rewritten")
            .field("len", &self.src.len())
            .field("pieces", &self.pieces.len())
            .field("marks", &self.marks)
            .finish_non_exhaustive()
    }
}

/// Куда пишется результат: память до [`MEM_MAX`], иначе временный файл (результат
/// перерос порог — память выгружается в файл); попутно — переводы строк по кускам
/// [`PIECE_MAX`].
struct Out {
    mem: Vec<u8>,
    file: Option<File>,
    len: u64,
    sep: Sep,
    lf: Vec<u32>,
    /// Каталог и имя временного файла.
    dir: std::path::PathBuf,
    name: String,
}

impl Out {
    fn new(sep: Sep, len_hint: u64, dir: &Path, name: &str) -> io::Result<Out> {
        let mut o = Out {
            mem: Vec::new(),
            file: None,
            len: 0,
            sep,
            lf: Vec::new(),
            dir: dir.to_path_buf(),
            name: name.to_owned(),
        };
        if len_hint > MEM_MAX {
            o.file = Some(create_temp(dir, name, "rw")?);
        }
        Ok(o)
    }

    /// Источник результата и куски над ним (`Src::Rewrite(0)`).
    fn finish(mut self) -> (Arc<dyn ByteSource>, Vec<Piece>) {
        let total = self.len;
        let src: Arc<dyn ByteSource> = match self.file.take() {
            Some(f) => Arc::new(super::piece::SpillSource::sealed(f, total)),
            None => Arc::new(OwnedSource::new(std::mem::take(&mut self.mem))),
        };
        let mut pieces = Vec::new();
        let mut at = 0u64;
        let mut i = 0usize;
        while at < total {
            let end = total.min(at + PIECE_MAX);
            pieces.push(Piece {
                src: Src::Rewrite(0),
                start: at,
                len: end - at,
                lf: Lf::Known(self.lf.get(i).copied().unwrap_or(0)),
            });
            at = end;
            i += 1;
        }
        (src, pieces)
    }

    fn write(&mut self, b: &[u8]) -> io::Result<()> {
        if self.file.is_none() && self.len + b.len() as u64 > MEM_MAX {
            let f = create_temp(&self.dir, &self.name, "rw")?;
            crate::sys::write_all_at(&f, &self.mem, 0)?;
            self.mem = Vec::new();
            self.file = Some(f);
        }
        let sep = self.sep;
        let lf = &mut self.lf;
        scan(b, self.len, sep, |at| {
            let i = ((at - 1) / PIECE_MAX) as usize;
            if lf.len() <= i {
                lf.resize(i + 1, 0);
            }
            lf[i] += 1;
        });
        match &self.file {
            Some(f) => crate::sys::write_all_at(f, b, self.len)?,
            None => self.mem.extend_from_slice(b),
        }
        self.len += b.len() as u64;
        Ok(())
    }
}

/// Что перезаписать.
#[derive(Clone, Debug)]
pub struct RewriteReq<'a> {
    /// Кодировка документа.
    pub from: Enc,
    pub to: Enc,
    /// Новые концы строк; `None` — как есть.
    pub eol: Option<Eol>,
    /// Смещения документа на границах символов (курсор, выделение).
    pub marks: &'a [u64],
    /// Каталог и имя временного файла.
    pub dir: &'a Path,
    pub name: &'a str,
}

/// Перезапись снимка в кодировку `to` с концами строк `eol` (вне UI-потока): перевод в
/// UTF-8, `editor.eol` (ADR-0023 §3, §5). Новые места отметок — в
/// [`Rewritten::marks`].
pub fn rewrite(
    snap: &TextSnap,
    req: &RewriteReq<'_>,
    check: &dyn Fn() -> Result<(), Cancelled>,
) -> Result<Rewritten, SaveError> {
    let _span = tracing::debug_span!("edit.rewrite", len = snap.len()).entered();
    let RewriteReq {
        from,
        to,
        eol,
        marks,
        dir,
        name,
    } = *req;
    let new_eol = eol.unwrap_or(Eol::Lf);
    let mut out = Out::new(Sep::of(&to, new_eol), snap.len(), dir, name)?;
    let mut rc = Recode::new(from, to.enc, eol);
    let mut order: Vec<(u64, usize)> = marks.iter().copied().zip(0..).collect();
    order.sort_unstable();
    let mut got = vec![0u64; marks.len()];
    let mut next = 0usize;
    let len = snap.len();
    blocks(snap, check, &mut |base, data| {
        let mut at = 0usize;
        loop {
            // Отметка на входе — позиция выхода (вход до неё уже выведен).
            while next < order.len() && order[next].0 <= base + at as u64 {
                got[order[next].1] = out.len;
                next += 1;
            }
            let cut = order
                .get(next)
                .map_or(data.len(), |m| ((m.0 - base) as usize).min(data.len()));
            let end = base + cut as u64 == len;
            rc.feed(&data[at..cut], end, &mut |b| out.write(b))?;
            at = cut;
            if at >= data.len() {
                return Ok(());
            }
        }
    })?;
    if len == 0 {
        rc.feed(&[], true, &mut |b| out.write(b))?;
    }
    for &(_, i) in &order[next..] {
        got[i] = out.len;
    }
    let unmappable = rc.unmappable;
    let (src, pieces) = out.finish();
    Ok(Rewritten {
        src,
        pieces,
        marks: got,
        unmappable,
    })
}

/// Результат массовой правки потоком («Заменить все» с большим числом совпадений,
/// S7-30): байты по порядку → источник `Rewrite` (память до [`MEM_MAX`], иначе
/// временный файл), переводы строк по кускам.
pub(crate) struct RewriteOut(Out);

impl RewriteOut {
    pub(crate) fn new(sep: Sep, len_hint: u64, dir: &Path, name: &str) -> io::Result<RewriteOut> {
        Out::new(sep, len_hint, dir, name).map(RewriteOut)
    }

    pub(crate) fn write(&mut self, b: &[u8]) -> io::Result<()> {
        self.0.write(b)
    }

    pub(crate) fn len(&self) -> u64 {
        self.0.len
    }

    /// Итог: `marks` — новые места отметок (курсор).
    pub(crate) fn finish(self, marks: Vec<u64>) -> Rewritten {
        let (src, pieces) = self.0.finish();
        Rewritten {
            src,
            pieces,
            marks,
            unmappable: 0,
        }
    }
}
