//! Классы иконок по имени и виду записи (S4 §5.7, решение владельца №7). Глифы
//! классов — S2 §1.6. Скрытость, облако (`OFFLINE`) и `..` — оверлеи и тон в UI,
//! не классы; раскраска «временных» (№32) — S2-05.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

use crate::api::{Attrs, EntryKind};
use crate::name::ext;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconClass {
    Dir,
    DirLink,
    File,
    Text,
    Doc,
    Image,
    Video,
    Audio,
    Archive,
    Exec,
    Code,
    /// Образы дисков и корни томов.
    Disk,
    /// Ссылки-файлы и ярлыки (`.lnk`, `.url`).
    Link,
}

/// Длиннее — не расширение из таблицы.
const MAX_EXT: usize = 16;

/// Класс иконки. Без аллокаций: расширение сворачивается в нижний регистр в
/// стековый буфер. Dotfile без второй точки — `File`.
pub fn icon_class(name: &str, kind: EntryKind, attrs: Attrs) -> IconClass {
    match kind {
        EntryKind::Dir => return IconClass::Dir,
        EntryKind::LinkDir => return IconClass::DirLink,
        EntryKind::LinkFile | EntryKind::LinkBroken => return IconClass::Link,
        EntryKind::File | EntryKind::Other => {}
    }
    let e = ext(name);
    if !e.is_empty()
        && e.len() <= MAX_EXT
        && e.is_ascii()
        && let Some(c) = by_ext(e)
    {
        return c;
    }
    if attrs.has(Attrs::EXEC) {
        IconClass::Exec
    } else {
        IconClass::File
    }
}

/// Класс по расширению без точки (анализ места: типы файлов, F01 §2.1).
pub(crate) fn class_by_ext(e: &str) -> Option<IconClass> {
    (!e.is_empty() && e.len() <= MAX_EXT && e.is_ascii())
        .then(|| by_ext(e))
        .flatten()
}

fn by_ext(e: &str) -> Option<IconClass> {
    let mut buf = [0u8; MAX_EXT];
    let lower = &mut buf[..e.len()];
    lower.copy_from_slice(e.as_bytes());
    lower.make_ascii_lowercase();
    let key = std::str::from_utf8(lower).ok()?;
    table().get(key).copied()
}

fn table() -> &'static FxHashMap<&'static str, IconClass> {
    static TABLE: OnceLock<FxHashMap<&'static str, IconClass>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = FxHashMap::default();
        for (class, exts) in EXTS {
            for e in exts.split_ascii_whitespace() {
                let prev = m.insert(e, *class);
                debug_assert!(prev.is_none(), "duplicate extension {e}");
            }
        }
        m
    })
}

/// Расширения по классам (нижний регистр). `ts` — TypeScript, не MPEG-TS.
const EXTS: &[(IconClass, &str)] = &[
    (
        IconClass::Image,
        "png jpg jpeg jfif gif bmp webp tif tiff ico icns svg heic heif avif jxl raw cr2 cr3 nef arw dng orf rw2 psd xcf tga dds exr hdr",
    ),
    (
        IconClass::Video,
        "mp4 m4v mkv avi mov wmv webm mpg mpeg flv 3gp 3g2 mts m2ts vob ogv divx",
    ),
    (
        IconClass::Audio,
        "mp3 flac wav ogg oga opus m4a aac wma aif aiff alac ape mid midi mka wv",
    ),
    (
        IconClass::Archive,
        "zip 7z rar tar gz tgz bz2 tbz tbz2 xz txz zst tzst lz lzma lz4 cab arj lzh z cpio deb rpm apk jar war ear nupkg whl gem",
    ),
    (
        IconClass::Exec,
        "exe msi msix appx bat cmd ps1 psm1 com scr vbs wsf sh bash zsh fish run bin appimage",
    ),
    (
        IconClass::Doc,
        "pdf doc docx docm odt rtf xls xlsx xlsm ods ppt pptx odp epub djvu xps oxps pages numbers key one fb2",
    ),
    (
        IconClass::Text,
        "txt md markdown log ini cfg conf csv tsv rst nfo diz text properties env",
    ),
    (
        IconClass::Code,
        "rs c h cpp cc cxx hpp hh hxx cs fs java kt kts go py pyw pyi rb php js mjs cjs ts tsx jsx swift m mm scala sc clj cljs lua pl pm r dart zig nim hs ml mli ex exs erl vb sql html htm xhtml css scss sass less xml xsd xsl json jsonc yaml yml toml gradle cmake mk asm s vue svelte ipynb proto graphql tf lock",
    ),
    (
        IconClass::Disk,
        "iso img vhd vhdx vmdk vdi dmg qcow2 wim esd",
    ),
    (IconClass::Link, "lnk url desktop webloc"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> IconClass {
        icon_class(name, EntryKind::File, Attrs::default())
    }

    #[test]
    fn by_kind_and_extension() {
        assert_eq!(
            icon_class("src", EntryKind::Dir, Attrs::default()),
            IconClass::Dir
        );
        assert_eq!(
            icon_class("j", EntryKind::LinkDir, Attrs::default()),
            IconClass::DirLink
        );
        assert_eq!(
            icon_class("x.txt", EntryKind::LinkFile, Attrs::default()),
            IconClass::Link
        );
        assert_eq!(file("photo.JPG"), IconClass::Image);
        assert_eq!(file("Archive.Tar.GZ"), IconClass::Archive);
        assert_eq!(file("setup.exe"), IconClass::Exec);
        assert_eq!(file("main.rs"), IconClass::Code);
        assert_eq!(file("app.ts"), IconClass::Code);
        assert_eq!(file("readme.md"), IconClass::Text);
        assert_eq!(file("report.PDF"), IconClass::Doc);
        assert_eq!(file("win.iso"), IconClass::Disk);
        assert_eq!(file("Shortcut.lnk"), IconClass::Link);
        assert_eq!(file("unknown.qwerty"), IconClass::File);
        assert_eq!(file("noext"), IconClass::File);
        assert_eq!(file("файл.ТХТ"), IconClass::File, "не-ASCII расширение");
        assert_eq!(file(&format!("a.{}", "x".repeat(40))), IconClass::File);
    }

    #[test]
    fn dotfiles_without_extension_are_files() {
        assert_eq!(file(".bashrc"), IconClass::File);
        assert_eq!(file(".gitignore"), IconClass::File);
        assert_eq!(file(".config.toml"), IconClass::Code);
    }

    #[test]
    fn exec_attribute_without_known_extension() {
        let exec = Attrs(Attrs::EXEC);
        assert_eq!(icon_class("tool", EntryKind::File, exec), IconClass::Exec);
        assert_eq!(
            icon_class("notes.txt", EntryKind::File, exec),
            IconClass::Text
        );
    }

    #[test]
    fn table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (_, exts) in EXTS {
            for e in exts.split_ascii_whitespace() {
                assert!(seen.insert(e), "duplicate {e}");
                assert!(e.len() <= MAX_EXT && e == e.to_ascii_lowercase(), "{e}");
            }
        }
        assert!(seen.len() >= 150, "{}", seen.len());
    }
}
