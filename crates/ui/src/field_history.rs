//! История полей ввода (№70): пути, маски, имена, шаблоны, время — свои списки, свежие
//! первыми, без повторов, не больше [`CAP`]. Файл `<state_dir>/fields.toml` читает и
//! пишет Runtime (UI-поток диск не трогает). Формат — строка на запись `вид<TAB>текст`
//! (без serde: бинарь ≤ 25 МБ, №54); непонятные строки пропускаются.

/// Записей в списке не больше.
pub const CAP: usize = 50;

/// Вид поля — источник дополнений и своя история.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// Путь (F5/F6 «куда», ввод пути панели): ФС и история путей.
    Path,
    /// Маска (отметка группы, фильтр): история масок.
    Mask,
    /// Имя (F7): имена в текущей папке и история.
    Name,
    /// Шаблон группового переименования.
    Pattern,
    /// Время (`ops.touch`, атрибуты).
    Time,
    /// Запрос «Найти пакет» (F02-09).
    Package,
    /// Образец поиска в записи сеанса (F03 §3.6).
    Find,
}

impl FieldKind {
    pub const ALL: [FieldKind; 7] = [
        FieldKind::Path,
        FieldKind::Mask,
        FieldKind::Name,
        FieldKind::Pattern,
        FieldKind::Time,
        FieldKind::Package,
        FieldKind::Find,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Path => "path",
            FieldKind::Mask => "mask",
            FieldKind::Name => "name",
            FieldKind::Pattern => "pattern",
            FieldKind::Time => "time",
            FieldKind::Package => "package",
            FieldKind::Find => "find",
        }
    }

    pub fn parse(s: &str) -> Option<FieldKind> {
        FieldKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Поле черновика команды `cmd` (`ui.draft command=`).
    pub fn of_draft(cmd: &str) -> Option<FieldKind> {
        Some(match cmd {
            "ops.copy" | "ops.move" | "ops.link" | "arc.extract" => FieldKind::Path,
            "sel.add" | "sel.remove" | "panel.filter" => FieldKind::Mask,
            "ops.mkdir" => FieldKind::Name,
            "ops.multi_rename" => FieldKind::Pattern,
            "ops.attrs" | "ops.touch" => FieldKind::Time,
            "pkg.search" => FieldKind::Package,
            _ => return None,
        })
    }
}

/// Списки по видам полей.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldHistory {
    path: Vec<String>,
    mask: Vec<String>,
    name: Vec<String>,
    pattern: Vec<String>,
    time: Vec<String>,
    package: Vec<String>,
    find: Vec<String>,
}

impl FieldHistory {
    fn list_mut(&mut self, k: FieldKind) -> &mut Vec<String> {
        match k {
            FieldKind::Path => &mut self.path,
            FieldKind::Mask => &mut self.mask,
            FieldKind::Name => &mut self.name,
            FieldKind::Pattern => &mut self.pattern,
            FieldKind::Time => &mut self.time,
            FieldKind::Package => &mut self.package,
            FieldKind::Find => &mut self.find,
        }
    }

    /// Свежие первыми.
    pub fn get(&self, k: FieldKind) -> &[String] {
        match k {
            FieldKind::Path => &self.path,
            FieldKind::Mask => &self.mask,
            FieldKind::Name => &self.name,
            FieldKind::Pattern => &self.pattern,
            FieldKind::Time => &self.time,
            FieldKind::Package => &self.package,
            FieldKind::Find => &self.find,
        }
    }

    /// Запомнить введённое: в начало, прежняя такая же запись убирается. `false` —
    /// ничего не изменилось (пусто или уже первое).
    pub fn push(&mut self, k: FieldKind, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        let list = self.list_mut(k);
        if list.first().is_some_and(|f| f == text) {
            return false;
        }
        list.retain(|t| t != text);
        list.insert(0, text.to_owned());
        list.truncate(CAP);
        true
    }

    /// Текст файла: `вид<TAB>текст` на строку, свежие первыми.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for k in FieldKind::ALL {
            for t in self.get(k) {
                out.push_str(k.as_str());
                out.push('\t');
                out.push_str(t);
                out.push('\n');
            }
        }
        out
    }

    /// Разбор файла: непонятные строки пропускаются, лишнее сверх [`CAP`] — тоже.
    pub fn from_text(text: &str) -> FieldHistory {
        let mut h = FieldHistory::default();
        for line in text.lines() {
            let Some((k, t)) = line.split_once('\t') else {
                continue;
            };
            let Some(k) = FieldKind::parse(k) else {
                continue;
            };
            let list = h.list_mut(k);
            let t = t.trim();
            if !t.is_empty() && list.len() < CAP && !list.iter().any(|x| x == t) {
                list.push(t.to_owned());
            }
        }
        h
    }
}

/// Записи истории `k`, начинающиеся с `prefix` (без учёта регистра), кроме самого
/// `prefix`.
pub fn matching<'a>(list: &'a [String], prefix: &str) -> impl Iterator<Item = &'a String> + 'a {
    let p = prefix.to_lowercase();
    list.iter()
        .filter(move |t| t.to_lowercase().starts_with(&p) && t.to_lowercase() != p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedups_and_caps() {
        let mut h = FieldHistory::default();
        assert!(h.push(FieldKind::Mask, "*.rs"));
        assert!(h.push(FieldKind::Mask, " *.toml "));
        assert!(h.push(FieldKind::Mask, "*.rs"));
        assert!(!h.push(FieldKind::Mask, "*.rs"));
        assert!(!h.push(FieldKind::Mask, "  "));
        assert_eq!(h.get(FieldKind::Mask), ["*.rs", "*.toml"]);
        for i in 0..CAP + 5 {
            h.push(FieldKind::Path, &format!("C:\\d{i}"));
        }
        assert_eq!(h.get(FieldKind::Path).len(), CAP);
        assert_eq!(h.get(FieldKind::Path)[0], format!("C:\\d{}", CAP + 4));
    }

    #[test]
    fn file_round_trip_and_broken_lines() {
        let mut h = FieldHistory::default();
        h.push(FieldKind::Path, "C:\\Dev\\urfin");
        h.push(FieldKind::Name, "release-notes");
        assert_eq!(FieldHistory::from_text(&h.to_text()), h);
        assert_eq!(
            FieldHistory::from_text("path = 5\nnope\tx\n"),
            FieldHistory::default()
        );
    }

    #[test]
    fn kinds_and_matching() {
        assert_eq!(FieldKind::of_draft("ops.move"), Some(FieldKind::Path));
        assert_eq!(FieldKind::of_draft("panel.filter"), Some(FieldKind::Mask));
        assert_eq!(FieldKind::of_draft("ops.delete"), None);
        assert_eq!(FieldKind::parse("pattern"), Some(FieldKind::Pattern));
        let list = vec![
            "C:\\Dev".to_owned(),
            "c:\\data".to_owned(),
            "D:\\x".to_owned(),
        ];
        let m: Vec<_> = matching(&list, "c:\\d").collect();
        assert_eq!(m, ["C:\\Dev", "c:\\data"]);
        assert_eq!(matching(&list, "D:\\x").count(), 0);
    }
}
