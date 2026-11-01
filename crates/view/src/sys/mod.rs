//! Вызовы ОС вьювера и редактора (S7 §1). `unsafe` во view — только здесь (решение
//! 2026-10-30 №3, CONTRACTS «Крейты»).

#[cfg(windows)]
#[cfg_attr(test, allow(dead_code, reason = "тесты — с буфером обмена процесса"))]
#[allow(
    unsafe_code,
    reason = "memmap2::Mmap::map — отображение файла (S7-02), id файла (S7-12), замена файла при сохранении (S7-17, S7-31), пакеты грамматик (S7-21a), буфер обмена редактора (S7-29), свободное место (S7-31); решение 2026-10-30 №3"
)]
mod windows;

#[cfg(windows)]
pub(crate) use windows::{
    attrs, file_id, file_info, free_space, load_grammar, map, move_replace, open_locked, open_seq,
    open_spool, read_at, replace_file, replace_file_backup, set_attrs, sharing_violation,
    write_all_at,
};

#[cfg(not(windows))]
#[allow(
    unsafe_code,
    reason = "memmap2::Mmap::map — сборки вне Windows только для проверки"
)]
mod other;

#[cfg(not(windows))]
pub(crate) use other::{
    attrs, file_id, file_info, free_space, load_grammar, map, move_replace, open_locked, open_seq,
    open_spool, read_at, replace_file, replace_file_backup, set_attrs, sharing_violation,
    write_all_at,
};

/// Пакет грамматики (S7-21a): функция языка tree-sitter и текст запросов подсветки.
pub(crate) struct GrammarPack {
    pub lang: tree_sitter_language::LanguageFn,
    pub query: String,
}

#[cfg(all(windows, not(test)))]
pub(crate) use windows::{clip_read, clip_write};

/// Буфер обмена процесса: тесты (системный буфер общий с владельцем стенда и другими
/// процессами — тест его не трогает) и сборки вне Windows.
#[cfg(any(test, not(windows)))]
mod clip_fake {
    use std::io;
    use std::sync::{Mutex, PoisonError};

    static CLIP: Mutex<String> = Mutex::new(String::new());

    #[allow(clippy::unnecessary_wraps, reason = "сигнатура как в windows.rs")]
    pub(crate) fn clip_read() -> io::Result<String> {
        Ok(CLIP.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    #[allow(clippy::unnecessary_wraps, reason = "сигнатура как в windows.rs")]
    pub(crate) fn clip_write(text: &str) -> io::Result<()> {
        text.clone_into(&mut CLIP.lock().unwrap_or_else(PoisonError::into_inner));
        Ok(())
    }
}

#[cfg(any(test, not(windows)))]
pub(crate) use clip_fake::{clip_read, clip_write};

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
