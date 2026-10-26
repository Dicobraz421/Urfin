//! Элемент листинга. Производит S4.

use compact_str::CompactString;

/// Элемент листинга, не больше 64 байт (48 + даты создания и доступа, решение 2026-10-26).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Имя для отображения. При `Attrs::ESCAPED_NAME` содержит обратимо
    /// экранированные не-UTF-8 юниты; имя для ОС даёт `vfs::name::to_os`.
    pub name: CompactString,
    pub size: u64,
    /// Наносекунды от Unix epoch; `i64::MIN` — неизвестно.
    pub mtime: i64,
    /// Создан и последний доступ — как `mtime` (колонки «Создан», «Доступ», S2-19). ОС
    /// отдаёт их тем же чтением каталога; у схем без этих дат — `i64::MIN`.
    pub ctime: i64,
    pub atime: i64,
    pub attrs: Attrs,
    pub kind: EntryKind,
}

/// Вид элемента.
#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EntryKind {
    File,
    Dir,
    LinkFile,
    LinkDir,
    LinkBroken,
    Other,
}

impl EntryKind {
    /// В элемент можно войти как в каталог: `Dir` или `LinkDir`.
    pub fn is_dir_like(self) -> bool {
        matches!(self, EntryKind::Dir | EntryKind::LinkDir)
    }
}

/// Битовые атрибуты элемента.
#[derive(Copy, Clone, Eq, PartialEq, Default, Debug)]
pub struct Attrs(pub u32);

impl Attrs {
    pub const HIDDEN: u32 = 1;
    pub const SYSTEM: u32 = 2;
    pub const READONLY: u32 = 4;
    pub const ARCHIVE: u32 = 8;
    pub const REPARSE: u32 = 16;
    pub const EXEC: u32 = 32;
    pub const COMPRESSED: u32 = 64;
    pub const ENCRYPTED: u32 = 128;
    pub const OFFLINE: u32 = 256;
    pub const DOTFILE: u32 = 512;
    /// Имя содержит обратимо экранированные не-UTF-8 юниты (PUA U+10F700…U+10FFFF, S4 §0).
    pub const ESCAPED_NAME: u32 = 1 << 10;
    /// У каталога поле `size` валидно (посчитано).
    pub const SIZE_KNOWN: u32 = 1 << 11;

    /// Установлены все биты `f`.
    pub fn has(self, f: u32) -> bool {
        self.0 & f == f
    }
}

#[cfg(test)]
mod tests {
    use super::{Attrs, Entry, EntryKind};

    #[test]
    fn entry_fits_64_bytes() {
        assert!(
            size_of::<Entry>() <= 64,
            "Entry = {} байт",
            size_of::<Entry>()
        );
    }

    #[test]
    fn attrs_bits_are_distinct() {
        let all = [
            Attrs::HIDDEN,
            Attrs::SYSTEM,
            Attrs::READONLY,
            Attrs::ARCHIVE,
            Attrs::REPARSE,
            Attrs::EXEC,
            Attrs::COMPRESSED,
            Attrs::ENCRYPTED,
            Attrs::OFFLINE,
            Attrs::DOTFILE,
            Attrs::ESCAPED_NAME,
            Attrs::SIZE_KNOWN,
        ];
        let mut seen = 0u32;
        for b in all {
            assert_eq!(b.count_ones(), 1);
            assert_eq!(seen & b, 0);
            seen |= b;
        }
        let a = Attrs(Attrs::HIDDEN | Attrs::SIZE_KNOWN);
        assert!(a.has(Attrs::HIDDEN));
        assert!(a.has(Attrs::HIDDEN | Attrs::SIZE_KNOWN));
        assert!(!a.has(Attrs::HIDDEN | Attrs::SYSTEM));
    }

    #[test]
    fn dir_like() {
        assert!(EntryKind::Dir.is_dir_like());
        assert!(EntryKind::LinkDir.is_dir_like());
        for k in [
            EntryKind::File,
            EntryKind::LinkFile,
            EntryKind::LinkBroken,
            EntryKind::Other,
        ] {
            assert!(!k.is_dir_like());
        }
    }
}
