//! Операции над ФС. Одиночные быстрые (mkdir, rename) выполняются задачей
//! `Lane::Interactive` без job; копирование, перенос и удаление — через движок (S5-04).

pub mod compare;
pub(crate) mod copy;
pub mod delete;
pub(crate) mod link;
pub mod mkdir;
pub mod multi_rename;
pub mod rename;
pub(crate) mod trash;
pub(crate) mod vfs_copy;
pub(crate) mod vfs_delete;
pub(crate) mod vfs_extract;
