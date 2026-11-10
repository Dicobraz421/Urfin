//! Определение архива по имени и сигнатуре (S8 §3.1).
//!
//! Имя решает, перехватывает ли Enter файл (`[arc] enter_exts`), сигнатура — что внутри
//! на самом деле (`arc.enter`, Ctrl+Shift+PgDn: `.docx` с `PK\x03\x04` — это zip).

/// Вид архива. Схема адреса — [`ArcKind::scheme`]: сжатые tar и одиночные `.gz`, `.xz`,
/// `.zst`, `.bz2` читает одна схема `tar` (S8-30), одиночный файл — псевдо-архив из одной
/// записи.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArcKind {
    Zip,
    SevenZ,
    Rar,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
    Gz,
    Bz2,
    Xz,
    Zst,
}

impl ArcKind {
    /// Схема адреса внутри архива (`Location.backend`).
    pub fn scheme(self) -> &'static str {
        match self {
            ArcKind::Zip => "zip",
            ArcKind::SevenZ => "7z",
            ArcKind::Rar => "rar",
            _ => "tar",
        }
    }

    /// Метка формата у пути (`screen-archive`) и в `ArcSummary::format`.
    pub fn label(self) -> &'static str {
        match self {
            ArcKind::Zip => "zip",
            ArcKind::SevenZ => "7z",
            ArcKind::Rar => "rar",
            ArcKind::Tar => "tar",
            ArcKind::TarGz => "tar.gz",
            ArcKind::TarBz2 => "tar.bz2",
            ArcKind::TarXz => "tar.xz",
            ArcKind::TarZst => "tar.zst",
            ArcKind::Gz => "gz",
            ArcKind::Bz2 => "bz2",
            ArcKind::Xz => "xz",
            ArcKind::Zst => "zst",
        }
    }

    /// Сжатый поток без своего оглавления: tar внутри или один файл.
    fn compressor(self) -> bool {
        matches!(
            self,
            ArcKind::Gz | ArcKind::Bz2 | ArcKind::Xz | ArcKind::Zst
        )
    }

    /// Сжатый tar того же компрессора.
    fn as_tar(self) -> ArcKind {
        match self {
            ArcKind::Gz => ArcKind::TarGz,
            ArcKind::Bz2 => ArcKind::TarBz2,
            ArcKind::Xz => ArcKind::TarXz,
            ArcKind::Zst => ArcKind::TarZst,
            k => k,
        }
    }
}

/// Расширения архивов: длинные раньше коротких (`tar.gz` раньше `gz`).
const EXTS: [(&str, ArcKind); 16] = [
    ("tar.gz", ArcKind::TarGz),
    ("tar.bz2", ArcKind::TarBz2),
    ("tar.xz", ArcKind::TarXz),
    ("tar.zst", ArcKind::TarZst),
    ("tgz", ArcKind::TarGz),
    ("tbz2", ArcKind::TarBz2),
    ("txz", ArcKind::TarXz),
    ("tzst", ArcKind::TarZst),
    ("zip", ArcKind::Zip),
    ("7z", ArcKind::SevenZ),
    ("rar", ArcKind::Rar),
    ("tar", ArcKind::Tar),
    ("gz", ArcKind::Gz),
    ("bz2", ArcKind::Bz2),
    ("xz", ArcKind::Xz),
    ("zst", ArcKind::Zst),
];

/// Все архивные расширения — `[arc] enter_exts` по умолчанию (без `docx`, `jar` и других
/// документов-архивов: они открываются в приложении ОС, №39).
pub fn archive_exts() -> impl Iterator<Item = &'static str> {
    EXTS.iter().map(|(e, _)| *e)
}

/// Расширение имени из списка `exts` (без точки, любой регистр): самое длинное
/// совпадение, `a.tar.gz` → `tar.gz`. Имя из одного расширения (`.zip`) — не архив.
pub fn ext_in<'a, I>(name: &str, exts: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let lower = name.to_lowercase();
    exts.into_iter()
        .filter(|e| {
            let e = e.trim_start_matches('.');
            !e.is_empty()
                && lower.len() > e.len() + 1
                && lower.ends_with(&e.to_lowercase())
                && lower.as_bytes()[lower.len() - e.len() - 1] == b'.'
        })
        .max_by_key(|e| e.len())
}

/// Вид архива по имени файла.
pub fn by_name(name: &str) -> Option<ArcKind> {
    let ext = ext_in(name, archive_exts())?;
    EXTS.iter().find(|(e, _)| *e == ext).map(|(_, k)| *k)
}

/// Сколько первых байт нужно [`by_head`]: `ustar` лежит со смещения 257.
pub const HEAD_LEN: usize = 512;

/// Вид архива по первым байтам файла (до [`HEAD_LEN`]). Сжатый поток — компрессор
/// ([`ArcKind::Gz`]): есть ли внутри tar, видно только после распаковки.
pub fn by_head(head: &[u8]) -> Option<ArcKind> {
    const SIGS: [(&[u8], ArcKind); 9] = [
        (b"PK\x03\x04", ArcKind::Zip),
        (b"PK\x05\x06", ArcKind::Zip),
        (b"7z\xBC\xAF\x27\x1C", ArcKind::SevenZ),
        (b"Rar!\x1A\x07\x00", ArcKind::Rar),
        (b"Rar!\x1A\x07\x01\x00", ArcKind::Rar),
        (b"\x1F\x8B", ArcKind::Gz),
        (b"\xFD7zXZ\x00", ArcKind::Xz),
        (b"\x28\xB5\x2F\xFD", ArcKind::Zst),
        (b"BZh", ArcKind::Bz2),
    ];
    if let Some((_, k)) = SIGS.iter().find(|(s, _)| head.starts_with(s)) {
        return Some(*k);
    }
    (head.get(257..262) == Some(b"ustar")).then_some(ArcKind::Tar)
}

/// Имя и сигнатура вместе: формат решает сигнатура, имя уточняет, tar ли внутри сжатого
/// потока (`.tgz`, `.tar.gz`). Без сигнатуры архива — `None`, даже если имя `.zip`.
pub fn detect(name: &str, head: &[u8]) -> Option<ArcKind> {
    let sig = by_head(head)?;
    if !sig.compressor() {
        return Some(sig);
    }
    match by_name(name) {
        Some(n) if n == sig.as_tar() => Some(n),
        _ => Some(sig),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_head() -> Vec<u8> {
        let mut h = vec![0u8; HEAD_LEN];
        h[..5].copy_from_slice(b"a.txt");
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }

    #[test]
    fn names_pick_the_longest_extension() {
        assert_eq!(by_name("src.tar.gz"), Some(ArcKind::TarGz));
        assert_eq!(by_name("SRC.TGZ"), Some(ArcKind::TarGz));
        assert_eq!(by_name("dump.sql.gz"), Some(ArcKind::Gz));
        assert_eq!(by_name("a.7z"), Some(ArcKind::SevenZ));
        assert_eq!(by_name("Отчёт.ZIP"), Some(ArcKind::Zip));
        assert_eq!(by_name("a.tar.zst"), Some(ArcKind::TarZst));
        assert_eq!(by_name(".zip"), None, "одно расширение — не имя архива");
        assert_eq!(by_name("report.docx"), None, "документы — приложению ОС");
        assert_eq!(by_name("zip"), None);
        assert_eq!(by_name("a.gzip"), None);
        assert_eq!(ext_in("a.tar.gz", ["gz", "tar.gz"]), Some("tar.gz"));
        assert_eq!(ext_in("a.DOCX", [".docx"]), Some(".docx"));
        assert_eq!(ext_in("a.tar.gz", ["zip"]), None);
    }

    #[test]
    fn signatures_decide_the_format() {
        assert_eq!(by_head(b"PK\x03\x04rest"), Some(ArcKind::Zip));
        assert_eq!(by_head(b"PK\x05\x06"), Some(ArcKind::Zip), "пустой zip");
        assert_eq!(
            by_head(b"7z\xBC\xAF\x27\x1C\x00\x04"),
            Some(ArcKind::SevenZ)
        );
        assert_eq!(by_head(b"Rar!\x1A\x07\x00"), Some(ArcKind::Rar));
        assert_eq!(by_head(b"Rar!\x1A\x07\x01\x00"), Some(ArcKind::Rar));
        assert_eq!(by_head(&tar_head()), Some(ArcKind::Tar));
        assert_eq!(by_head(b"\x1F\x8B\x08"), Some(ArcKind::Gz));
        assert_eq!(by_head(b"\xFD7zXZ\x00\x00"), Some(ArcKind::Xz));
        assert_eq!(by_head(b"\x28\xB5\x2F\xFD"), Some(ArcKind::Zst));
        assert_eq!(by_head(b"BZh91AY"), Some(ArcKind::Bz2));
        assert_eq!(by_head(b"plain text"), None);
        assert_eq!(by_head(b""), None);
        assert_eq!(by_head(&tar_head()[..200]), None, "короткая голова");
    }

    #[test]
    fn detect_combines_name_and_signature() {
        assert_eq!(detect("report.docx", b"PK\x03\x04"), Some(ArcKind::Zip));
        assert_eq!(detect("x.tgz", b"\x1F\x8B\x08"), Some(ArcKind::TarGz));
        assert_eq!(detect("x.tar.xz", b"\xFD7zXZ\x00"), Some(ArcKind::TarXz));
        assert_eq!(detect("x.log.gz", b"\x1F\x8B\x08"), Some(ArcKind::Gz));
        assert_eq!(detect("x.tgz", b"\x28\xB5\x2F\xFD"), Some(ArcKind::Zst));
        assert_eq!(detect("fake.zip", b"hello"), None);
        assert_eq!(
            detect("a.zip", b"7z\xBC\xAF\x27\x1C"),
            Some(ArcKind::SevenZ)
        );
    }

    #[test]
    fn schemes_and_labels() {
        assert_eq!(ArcKind::TarGz.scheme(), "tar");
        assert_eq!(ArcKind::Gz.scheme(), "tar");
        assert_eq!(ArcKind::SevenZ.scheme(), "7z");
        assert_eq!(ArcKind::TarZst.label(), "tar.zst");
        assert_eq!(archive_exts().count(), 16);
        for e in archive_exts() {
            let k = by_name(&format!("a.{e}")).unwrap();
            assert_ne!(k.label(), "");
        }
    }
}
