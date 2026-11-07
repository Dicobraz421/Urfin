//! Назначение копирования и переноса (S5 §2.1, семантика Far): текст из
//! черновика → `Location` → внутрь каталога или под новым именем.

use std::path::Path;

use urfin_core::{EntryKind, Location};

use crate::api::{DestMode, OpRequest};

/// Назначение не подходит для запроса.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DestError {
    #[error("empty destination")]
    Empty,
    /// Несколько элементов в существующий файл.
    #[error("destination is a file: {0}")]
    NotADirectory(String),
}

/// Текст назначения → адрес: абсолютный путь как есть, относительный — от `base`;
/// адрес схемы `sftp://профиль/путь` (F03-07). Прочие uri (`zip:…`) — S8.
pub fn parse_dest(base: &Location, text: &str) -> Result<Location, DestError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(DestError::Empty);
    }
    // Адрес схемы: `sftp://srv1/var/www` (№87); хвостовой `/` — признак «внутрь»
    // ([`trailing_separator`]), в адрес не входит.
    if let Some((scheme, _)) = text.split_once("://")
        && scheme.len() >= 2
        && scheme
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        let t = text.trim_end_matches(['/', '\\']);
        let t = if t
            .get(scheme.len() + 3..)
            .is_none_or(|rest| !rest.contains('/'))
        {
            text
        } else {
            t
        };
        if let Ok(l) = Location::parse_uri(t) {
            return Ok(l);
        }
    }
    // Адрес схемы без сервера: `search:/3`, `du:/1/src` — поле черновика с пассивной
    // панелью такой схемы (S8-12; запись туда — отказ в handler). `C:` — буква диска.
    if let Some((scheme, rest)) = text.split_once(':')
        && scheme.len() >= 2
        && rest.starts_with('/')
        && scheme
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && let Ok(l) = Location::parse_uri(text)
    {
        return Ok(l);
    }
    let p = Path::new(text);
    Ok(if p.is_absolute() {
        Location::local(p)
    } else if base.is_local() {
        Location::local(base.path.join(p))
    } else {
        // Относительный путь от папки сервера — через `/`.
        let b = base.path.to_string_lossy();
        let rel = text.replace('\\', "/");
        Location {
            path: format!(
                "{}/{}",
                b.trim_end_matches(['/', '\\']),
                rel.trim_end_matches('/')
            )
            .into(),
            ..base.clone()
        }
    })
}

/// Завершающий разделитель пути: «создать каталог и положить внутрь».
pub fn trailing_separator(text: &str) -> bool {
    text.trim_end_matches(' ').ends_with(['/', '\\'])
}

/// Куда кладём (Far):
/// - назначение — существующий каталог → внутрь;
/// - завершающий разделитель → создать каталог и внутрь;
/// - несколько элементов → внутрь (каталог создаётся), в существующий файл — ошибка;
/// - один элемент → новое имя (существующий файл — конфликт «существует» позже).
pub fn resolve_dest(
    req: &OpRequest,
    dest: &Location,
    raw: &str,
    dest_exists: Option<EntryKind>,
) -> Result<DestMode, DestError> {
    let is_dir = matches!(dest_exists, Some(EntryKind::Dir | EntryKind::LinkDir));
    let is_file = dest_exists.is_some() && !is_dir;
    if is_dir || trailing_separator(raw) {
        return Ok(DestMode::Into(dest.clone()));
    }
    if req.items.len() > 1 {
        if is_file {
            return Err(DestError::NotADirectory(dest.path.display().to_string()));
        }
        return Ok(DestMode::Into(dest.clone()));
    }
    Ok(DestMode::RenameTo(dest.clone()))
}

#[cfg(test)]
mod tests {
    use compact_str::CompactString;
    use urfin_core::{EntryKind, Location};

    use super::*;
    use crate::api::{OpKind, OpOptions};

    fn req(n: usize) -> OpRequest {
        OpRequest {
            kind: OpKind::Copy,
            base: Location::local(base()),
            items: (0..n)
                .map(|i| CompactString::from(format!("f{i}")))
                .collect(),
            dest: None,
            opts: OpOptions::default(),
        }
    }

    fn base() -> &'static str {
        if cfg!(windows) { r"C:\src" } else { "/src" }
    }

    fn target() -> (&'static str, Location) {
        let t = if cfg!(windows) { r"D:\dst" } else { "/dst" };
        (t, Location::local(t))
    }

    #[test]
    fn existing_dir_is_into() {
        let (raw, d) = target();
        let m = resolve_dest(&req(1), &d, raw, Some(EntryKind::Dir)).unwrap();
        assert_eq!(m, DestMode::Into(d.clone()));
        let m = resolve_dest(&req(3), &d, raw, Some(EntryKind::LinkDir)).unwrap();
        assert_eq!(m, DestMode::Into(d));
    }

    #[test]
    fn trailing_separator_creates_dir() {
        let (raw, d) = target();
        let raw = format!("{raw}{}", std::path::MAIN_SEPARATOR);
        let m = resolve_dest(&req(1), &d, &raw, None).unwrap();
        assert_eq!(m, DestMode::Into(d));
    }

    #[test]
    fn single_item_new_name() {
        let (raw, d) = target();
        assert_eq!(
            resolve_dest(&req(1), &d, raw, None).unwrap(),
            DestMode::RenameTo(d)
        );
    }

    #[test]
    fn single_item_over_existing_file() {
        let (raw, d) = target();
        assert_eq!(
            resolve_dest(&req(1), &d, raw, Some(EntryKind::File)).unwrap(),
            DestMode::RenameTo(d)
        );
    }

    #[test]
    fn many_items_create_dir() {
        let (raw, d) = target();
        assert_eq!(
            resolve_dest(&req(2), &d, raw, None).unwrap(),
            DestMode::Into(d)
        );
    }

    #[test]
    fn many_items_into_file_is_error() {
        let (raw, d) = target();
        assert!(matches!(
            resolve_dest(&req(2), &d, raw, Some(EntryKind::File)),
            Err(DestError::NotADirectory(_))
        ));
    }

    /// Адрес сервера в поле F5 (F03-07): `sftp://профиль/путь`, `/` в конце — «внутрь».
    #[test]
    fn sftp_dest() {
        let b = Location::local(base());
        let d = parse_dest(&b, " sftp://srv1/var/www/43/ ").unwrap();
        assert_eq!(d.backend, "sftp");
        assert_eq!(d.authority.as_deref(), Some("srv1"));
        assert_eq!(d.to_uri().replace('\\', "/"), "sftp://srv1/var/www/43");
        assert!(trailing_separator("sftp://srv1/var/www/43/"));
        let r = parse_dest(&d, "sub").unwrap();
        assert_eq!(r.to_uri().replace('\\', "/"), "sftp://srv1/var/www/43/sub");
        let root = parse_dest(&b, "sftp://srv1/").unwrap();
        assert_eq!(root.authority.as_deref(), Some("srv1"));
    }

    #[test]
    fn relative_dest_is_from_base() {
        let b = Location::local(base());
        let d = parse_dest(&b, "sub").unwrap();
        assert_eq!(
            d.path,
            Location::local(std::path::Path::new(base()).join("sub")).path
        );
        assert_eq!(parse_dest(&b, "  "), Err(DestError::Empty));
    }

    /// Пассивная панель результатов поиска в поле F5 (S8-12): адрес схемы, а не путь от
    /// `base`; буква диска — путь.
    #[test]
    fn scheme_dest_without_server() {
        let b = Location::local(base());
        let d = parse_dest(&b, "search:/3").unwrap();
        assert_eq!(
            (d.backend.as_str(), d.authority.as_deref()),
            ("search", None)
        );
        if cfg!(windows) {
            assert!(parse_dest(&b, r"D:\x").unwrap().is_local());
            assert!(parse_dest(&b, "D:/x").unwrap().is_local());
        }
    }
}
