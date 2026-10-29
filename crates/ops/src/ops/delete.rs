//! Удаление (S0-decisions №11): F8/Del — в корзину, Shift — насовсем.
//!
//! Насовсем — `std::fs::remove_dir_all`: на Windows удаление с POSIX-семантикой
//! (`FILE_DISPOSITION_FLAG_POSIX_SEMANTICS`, read-only снимается флагом), ссылки и
//! junction — как ссылки, цель не трогается. Крейт `remove_dir_all` был в 50 раз
//! медленнее `rd /s /q` (бенч #9, S5-25: 2000 файлов — 20 с против 0,4 с); корзина —
//! [`super::trash`] (S5-13).

use std::io;
use std::path::Path;

use compact_str::CompactString;

/// Не удалось удалить элемент.
#[derive(Debug)]
pub struct DeleteError {
    pub name: CompactString,
    pub error: String,
}

/// Удаляет `items` из `dir` насовсем. Вызывает `done` после каждого элемента,
/// `stop` — перед каждым (отмена). Ошибки собираются, удаление идёт дальше.
pub fn delete_permanent(
    dir: &Path,
    items: &[CompactString],
    stop: &dyn Fn() -> bool,
    done: &dyn Fn(&str),
) -> Vec<DeleteError> {
    let mut errors = Vec::new();
    for name in items {
        if stop() {
            break;
        }
        let path = dir.join(name.as_str());
        if let Err(e) = remove(&path) {
            errors.push(DeleteError {
                name: name.clone(),
                error: e.to_string(),
            });
        }
        done(name);
    }
    errors
}

/// Один элемент: ссылка (в том числе junction) — сама ссылка, каталог — деревом.
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        // Ссылка на каталог на Windows удаляется как каталог, на файл — как файл.
        return std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path));
    }
    if ft.is_dir() {
        return std::fs::remove_dir_all(path);
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // Read-only: снять атрибут и повторить.
            let mut p = meta.permissions();
            #[allow(
                clippy::permissions_set_readonly_false,
                reason = "снимаем read-only для удаления"
            )]
            p.set_readonly(false);
            std::fs::set_permissions(path, p)?;
            std::fs::remove_file(path)
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_files_dirs_readonly() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::write(p.join("f.txt"), b"1").unwrap();
        std::fs::create_dir_all(p.join("tree/a/b")).unwrap();
        std::fs::write(p.join("tree/a/b/x"), b"2").unwrap();
        // Read-only внутри дерева.
        std::fs::write(p.join("tree/a/ro"), b"4").unwrap();
        let mut perm = std::fs::metadata(p.join("tree/a/ro"))
            .unwrap()
            .permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(p.join("tree/a/ro"), perm).unwrap();
        std::fs::write(p.join("ro"), b"3").unwrap();
        let mut perm = std::fs::metadata(p.join("ro")).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(p.join("ro"), perm).unwrap();
        let items: Vec<CompactString> = ["f.txt", "tree", "ro"].map(Into::into).into();
        let errs = delete_permanent(p, &items, &|| false, &|_| {});
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(std::fs::read_dir(p).unwrap().count(), 0);
    }

    #[test]
    fn missing_item_is_reported_and_others_go_on() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("b"), b"1").unwrap();
        let items: Vec<CompactString> = ["a", "b"].map(Into::into).into();
        let errs = delete_permanent(d.path(), &items, &|| false, &|_| {});
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].name, "a");
        assert!(!d.path().join("b").exists());
    }

    /// Junction на каталог вне дерева удаляется как ссылка, цель цела (S5-09: тест обязателен).
    #[cfg(windows)]
    #[test]
    fn junction_target_is_untouched() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"1").unwrap();
        let tree = d.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let link = tree.join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "mklink /J");
        let items: Vec<CompactString> = vec!["tree".into()];
        let errs = delete_permanent(d.path(), &items, &|| false, &|_| {});
        assert!(errs.is_empty(), "{errs:?}");
        assert!(!tree.exists());
        assert!(target.join("keep").is_file());
    }
}
