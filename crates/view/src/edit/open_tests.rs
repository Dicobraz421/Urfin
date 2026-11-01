//! Приёмка S7-29 (открытие): открытие → запись снимка без правок = те же байты
//! (13 кодировок V5 × 4 EOL, BOM, lossy); большой файл — под запретом записи; ошибка
//! чтения — документ только для чтения.

use std::path::Path;
use std::sync::Arc;

use encoding_rs::{
    Encoding, IBM866, KOI8_R, KOI8_U, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1251,
    WINDOWS_1252,
};
use urfin_core::CancelToken;

use super::*;
use crate::config::EolCfg;
use crate::edit::input::{self, Act, Engine, Env};
use crate::edit::piece::Src;
use crate::source::SourceKind;

pub(crate) const RU: &str = "Вечером над рекой поднимался туман, и старый паромщик зажигал фонарь.";
pub(crate) const FR: &str = "Le marché du samedi s'installe très tôt sur la place de l'église.";
pub(crate) const JA: &str = "今日は朝から雨が降っていたので、私は図書館へ行って本を読みました。";

pub(crate) fn opts(enc: Option<&'static Encoding>) -> LoadOpts {
    LoadOpts {
        enc,
        tld: Some(b"ru"),
        eol: EolCfg::File,
        name: "a.txt".into(),
        new: None,
    }
}

/// Текст в кодировке (UTF-16 — вручную, `encoding_rs` его не кодирует).
pub(crate) fn bytes_of(text: &str, e: &'static Encoding, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if e == UTF_16LE || e == UTF_16BE {
        if bom {
            out.extend_from_slice(if e == UTF_16BE {
                &[0xFE, 0xFF]
            } else {
                &[0xFF, 0xFE]
            });
        }
        for u in text.encode_utf16() {
            out.extend_from_slice(&if e == UTF_16BE {
                u.to_be_bytes()
            } else {
                u.to_le_bytes()
            });
        }
        return out;
    }
    if bom {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    let (b, _, bad) = e.encode(text);
    assert!(!bad, "{}", e.name());
    out.extend_from_slice(&b);
    out
}

pub(crate) fn lines(body: &str, n: usize, eol: &str) -> String {
    let mixed = ["\r\n", "\n", "\r"];
    let mut s = String::new();
    for i in 0..n {
        s.push_str(body);
        s.push_str(if eol == "mixed" { mixed[i % 3] } else { eol });
    }
    s
}

/// Файл как его запишет потоковый писатель без правок: BOM и куски снимка.
fn written(o: &Opened) -> Vec<u8> {
    let mut out = Vec::new();
    if o.bom {
        let len = u64::from(o.enc.bom_len);
        let mut b = vec![0u8; len as usize];
        crate::source::read_full(&**o.text.orig(), 0, &mut b).unwrap();
        out.extend_from_slice(&b);
    }
    super::super::write::blocks(&o.text.snap(), &|| Ok(()), &mut |_, b| {
        out.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    out
}

pub(crate) fn open_bytes(
    dir: &Path,
    name: &str,
    b: &[u8],
    enc: Option<&'static Encoding>,
) -> Opened {
    let path = dir.join(name);
    std::fs::write(&path, b).unwrap();
    open_path(&path, 1, &opts(enc), &CancelToken::new()).unwrap()
}

/// Приёмка: открытие → запись без правок = те же байты (кодировки V5, BOM, все виды EOL,
/// по выбору вручную и по детекту).
#[test]
fn open_then_write_gives_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, &'static Encoding, bool); 13] = [
        (RU, WINDOWS_1251, false),
        (RU, IBM866, false),
        (RU, KOI8_R, false),
        (
            "Їжак ішов стежкою через ґанок до саду, де під яблунею росла трава.",
            KOI8_U,
            false,
        ),
        (FR, WINDOWS_1252, false),
        (JA, SHIFT_JIS, false),
        (RU, UTF_8, false),
        (RU, UTF_8, true),
        (RU, UTF_16LE, true),
        (RU, UTF_16BE, true),
        (FR, UTF_16LE, false),
        (FR, UTF_16BE, false),
        ("plain ascii", UTF_8, false),
    ];
    let mut checked = 0;
    for (body, e, bom) in cases {
        for eol in ["\n", "\r\n", "\r", "mixed"] {
            let text = lines(body, 40, eol) + body;
            let b = bytes_of(&text, e, bom);
            let o = open_bytes(dir.path(), "a.txt", &b, Some(e));
            assert_eq!(o.guard, Guard::Memory);
            assert!(!o.lossy, "{} {eol:?}", e.name());
            assert_eq!(o.bom, bom, "{} {eol:?}", e.name());
            assert!(written(&o) == b, "{} {eol:?}: байты другие", e.name());
            let auto = open_bytes(dir.path(), "a.txt", &b, None);
            assert!(written(&auto) == b, "{} {eol:?} по детекту", e.name());
            checked += 1;
        }
    }
    assert_eq!(checked, 52);
}

/// Недекодируемые байты — `lossy`, но сохраняются как были (ADR-0023 §3).
#[test]
fn lossy_bytes_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let b = b"ok \xFF\xFE bad\r\nnext \xC3 line\r\n".to_vec();
    let o = open_bytes(dir.path(), "l.txt", &b, Some(UTF_8));
    assert!(o.lossy);
    assert_eq!(o.eol, Eol::CrLf);
    assert_eq!(written(&o), b);
    // Нечётный хвост UTF-16.
    let mut b = bytes_of("абв\r\n", UTF_16LE, true);
    b.push(0x41);
    let o = open_bytes(dir.path(), "u.txt", &b, Some(UTF_16LE));
    assert!(o.lossy);
    assert_eq!(written(&o), b);
}

/// Концы строк и разделитель: по всем байтам файла в памяти; UTF-16 — единицами.
#[test]
fn eols_by_units() {
    let enc = Enc::utf8();
    let c = count_eols(b"a\r\nb\nc\rd\r", &enc);
    assert_eq!((c.crlf, c.lf, c.cr), (1, 1, 2));
    let u = Enc::new(UTF_16LE);
    let b = bytes_of("a\r\nb\n", UTF_16LE, false);
    let c = count_eols(&b, &u);
    assert_eq!((c.crlf, c.lf, c.cr), (1, 1, 0));
    // Байт 0x0A внутри единицы UTF-16 (`U+0A0D`) — не конец строки.
    let c = count_eols(&[0x0D, 0x0A], &u);
    assert_eq!((c.crlf, c.lf, c.cr), (0, 0, 0));
    let dir = tempfile::tempdir().unwrap();
    let o = open_bytes(dir.path(), "cr.txt", b"one\rtwo\rthree", None);
    assert_eq!(o.eol, Eol::Cr);
    assert_eq!(o.text.line_of(8), crate::index::LineGuess::Exact(2));
}

/// Файл больше 16 МБ — источник `File` по своему handle (не mmap), запрет записи.
#[test]
fn big_file_is_locked_file_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    std::fs::write(&path, "line one\r\nline two\r\n").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(MEM_MAX + 1).unwrap();
    drop(f);
    let o = open_path(&path, 1, &opts(None), &CancelToken::new()).unwrap();
    assert_eq!(o.guard, Guard::Locked);
    assert_eq!(o.text.orig().kind(), SourceKind::File);
    assert_eq!(o.text.len(), MEM_MAX + 1);
    assert_eq!(o.eol, Eol::CrLf);
    assert!(!o.text.index().done, "индекс большого — фоном");
}

/// Приёмка (Windows): другой не может открыть файл > 16 МБ на запись, читать и
/// переименовывать — может; файл, который уже пишут, открывается без запрета.
#[cfg(windows)]
#[test]
fn big_file_denies_writers_allows_readers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(MEM_MAX + 1).unwrap();
    drop(f);
    let o = open_path(&path, 1, &opts(None), &CancelToken::new()).unwrap();
    assert_eq!(o.guard, Guard::Locked);
    let w = std::fs::OpenOptions::new().write(true).open(&path);
    assert!(
        w.as_ref().is_err_and(crate::sys::sharing_violation),
        "{w:?}"
    );
    assert!(std::fs::File::open(&path).is_ok(), "чтение разрешено");
    let moved = dir.path().join("moved.log");
    std::fs::rename(&path, &moved).unwrap();
    // Удалённый или переименованный файл живёт, пока открыт handle документа.
    let mut b = [0u8; 4];
    assert_eq!(o.text.snap().read_at(0, &mut b).unwrap(), 4);
    drop(o);

    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&moved)
        .unwrap();
    let o = open_path(&moved, 2, &opts(None), &CancelToken::new()).unwrap();
    assert_eq!(o.guard, Guard::Unlocked);
    drop(writer);
}

/// Источник, у которого чтение всегда падает (носитель пропал).
#[derive(Debug)]
struct Gone(u64);

impl crate::source::ByteSource for Gone {
    fn len(&self) -> u64 {
        self.0
    }
    fn is_final(&self) -> bool {
        true
    }
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("device is gone"))
    }
    fn kind(&self) -> SourceKind {
        SourceKind::File
    }
    fn refresh_len(&self) -> std::io::Result<u64> {
        Ok(self.0)
    }
}

/// Ошибка `read_at` (USB, сеть пропали): строки не прочитаны, документ — только для
/// чтения, правка не принимается.
#[test]
fn read_error_makes_document_read_only() {
    let src = Arc::new(Gone(1 << 20));
    let sep = crate::index::Sep::Byte(b'\n');
    let add = AddBuf::with_limit(
        std::env::temp_dir().join("urfin-test"),
        format!("gone-{}", std::process::id()),
        16 << 20,
    );
    let text = TextDoc::new(src, 0, sep, add, LineIndex::default());
    let mut e = Engine::new(text, Enc::utf8(), Eol::Lf, Guard::Locked, 0..1 << 20);
    let env = Env {
        eol_new: Eol::Lf,
        readonly: false,
        now: std::time::Instant::now(),
    };
    assert!(
        e.act(Act::Move(input::Move::Right, input::Extend::No), &env)
            .is_err()
    );
    e.want_visible();
    let reqs = e.text.take_requests();
    assert!(!reqs.is_empty());
    for (src, loader, req) in reqs {
        assert_eq!(src, Src::Original);
        let loaded = loader.load(&req, &CancelToken::new());
        assert!(loaded.error.is_some());
        e.on_pages(src, loaded);
    }
    assert!(e.read_failed && !e.editable());
    let r = e.act(
        Act::Input {
            text: "x".into(),
            replace: false,
        },
        &env,
    );
    assert_eq!(r, Ok(input::Done::Same));
    assert_eq!(e.len(), 1 << 20);
}
