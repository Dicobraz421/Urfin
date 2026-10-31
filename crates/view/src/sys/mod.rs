//! Вызовы ОС вьювера и редактора (S7 §1). `unsafe` во view — только здесь (решение
//! 2026-10-30 №3, CONTRACTS «Крейты»).

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "memmap2::Mmap::map — отображение файла (S7-02), id файла (S7-12), замена файла при сохранении (S7-17); решение 2026-10-30 №3"
)]
mod windows;

#[cfg(windows)]
pub(crate) use windows::{
    attrs, file_id, file_info, map, move_replace, open_seq, open_spool, read_at, replace_file,
    set_attrs, write_all_at,
};

#[cfg(not(windows))]
#[allow(
    unsafe_code,
    reason = "memmap2::Mmap::map — сборки вне Windows только для проверки"
)]
mod other;

#[cfg(not(windows))]
pub(crate) use other::{
    attrs, file_id, file_info, map, move_replace, open_seq, open_spool, read_at, replace_file,
    set_attrs, write_all_at,
};

/// Сведения о файле по handle: id (том, индекс), жёсткие ссылки, атрибуты.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileInfo {
    pub id: (u64, u128),
    pub nlink: u32,
    pub attrs: u32,
}

/// `FILE_ATTRIBUTE_READONLY`.
pub(crate) const ATTR_READONLY: u32 = 0x1;
/// `FILE_ATTRIBUTE_HIDDEN`.
#[cfg(test)]
pub(crate) const ATTR_HIDDEN: u32 = 0x2;
/// Атрибуты, которые сохранение возвращает файлу: только чтение, скрытый, системный,
/// архивный, временный, не индексировать.
pub(crate) const ATTR_KEEP: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x2000;
