//! Вызовы ОС вьювера и редактора (S7 §1). `unsafe` во view — только здесь (решение
//! 2026-10-30 №3, CONTRACTS «Крейты»).

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "memmap2::Mmap::map — отображение файла (S7-02), id файла (S7-12); решение 2026-10-30 №3"
)]
mod windows;

#[cfg(windows)]
pub(crate) use windows::{file_id, map, open_seq, open_spool, read_at, write_all_at};

#[cfg(not(windows))]
#[allow(
    unsafe_code,
    reason = "memmap2::Mmap::map — сборки вне Windows только для проверки"
)]
mod other;

#[cfg(not(windows))]
pub(crate) use other::{file_id, map, open_seq, open_spool, read_at, write_all_at};
