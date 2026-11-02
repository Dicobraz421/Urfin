//! Фильтры анализа места (F01 §3.5): таблица типов, порог «Старые», вид вкладки в
//! адресе (`du:/<id>?f=…`), правила «Временные».
//!
//! Всё здесь — без I/O: фильтры строятся по снимку, переменные окружения раскрываются
//! один раз (функцией окружения — в тестах своя).

use std::collections::HashMap;
use std::sync::OnceLock;

use super::model::{DuFilter, DuType};
use crate::walk::RawName;

// ---------------------------------------------------------------------------
// Типы

/// Таблица типов (F01 §3.5): расширение — ровно один тип (тест покрытия). Порядок
/// расширений — как в подсказке строки статуса («Архивы: .zip .7z .rar …»). `ts`
/// здесь — TypeScript («Код»): в каталогах разработки он чаще MPEG-TS.
pub const TYPE_TABLE: [(DuType, &[&str]); 9] = [
    (
        DuType::Video,
        &[
            "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "mts", "m2ts", "flv", "vob",
        ],
    ),
    (
        DuType::Audio,
        &[
            "mp3", "flac", "wav", "aac", "ogg", "opus", "m4a", "wma", "ape", "alac",
        ],
    ),
    (
        DuType::Image,
        &[
            "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "avif", "raw",
            "cr2", "cr3", "nef", "arw", "dng", "psd", "svg", "ico",
        ],
    ),
    (
        DuType::Doc,
        &[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "txt",
            "md", "epub", "djvu", "vsdx",
        ],
    ),
    (
        DuType::Archive,
        &[
            "zip", "7z", "rar", "tar", "gz", "cab", "tgz", "bz2", "xz", "zst", "lz4",
        ],
    ),
    (
        DuType::DiskImage,
        &[
            "iso", "img", "vhd", "vhdx", "vmdk", "vdi", "qcow2", "wim", "esd",
        ],
    ),
    (
        DuType::Exec,
        &["exe", "msi", "msp", "msix", "appx", "dll", "sys"],
    ),
    (
        DuType::Code,
        &[
            "rs", "c", "cpp", "h", "cs", "js", "ts", "py", "go", "java", "o", "obj", "pdb", "lib",
            "a", "rlib", "rmeta", "class", "jar", "node", "pyc",
        ],
    ),
    (
        DuType::Temp,
        &["tmp", "temp", "bak", "old", "dmp", "mdmp", "etl", "swp"],
    ),
];

fn type_map() -> &'static HashMap<&'static str, DuType> {
    static MAP: OnceLock<HashMap<&'static str, DuType>> = OnceLock::new();
    MAP.get_or_init(|| {
        TYPE_TABLE
            .iter()
            .flat_map(|(t, exts)| exts.iter().map(move |e| (*e, *t)))
            .collect()
    })
}

/// Тип по расширению в нижнем регистре: таблица, затем классы иконок (S4 §5.7);
/// без расширения и неизвестное — «Прочее».
pub fn type_by_ext(ext: &str) -> DuType {
    use crate::icons::IconClass as C;
    if let Some(t) = type_map().get(ext) {
        return *t;
    }
    match crate::icons::class_by_ext(ext) {
        Some(C::Video) => DuType::Video,
        Some(C::Audio) => DuType::Audio,
        Some(C::Image) => DuType::Image,
        Some(C::Doc | C::Text) => DuType::Doc,
        Some(C::Archive) => DuType::Archive,
        Some(C::Disk) => DuType::DiskImage,
        Some(C::Exec) => DuType::Exec,
        Some(C::Code) => DuType::Code,
        _ => DuType::Other,
    }
}

/// Тип файла по имени: `~*` (временные Office и т. п.) — «Временные», иначе по
/// расширению.
pub fn type_of(name: &RawName<'_>) -> DuType {
    let tilde = match *name {
        RawName::Wide(w) => w.first() == Some(&u16::from(b'~')),
        RawName::Str { name, .. } => name.starts_with('~'),
    };
    if tilde {
        return DuType::Temp;
    }
    let mut buf = [0u8; 16];
    name.ext_lower(&mut buf).map_or(DuType::Other, type_by_ext)
}

impl DuType {
    /// Имя в адресе (`open=archive`) и в командах (`group=archive`).
    pub fn id(self) -> &'static str {
        match self {
            DuType::Video => "video",
            DuType::Audio => "audio",
            DuType::Image => "image",
            DuType::Doc => "doc",
            DuType::Archive => "archive",
            DuType::DiskImage => "disk",
            DuType::Exec => "exec",
            DuType::Code => "code",
            DuType::Temp => "temp",
            DuType::Other => "other",
        }
    }

    pub fn from_id(id: &str) -> Option<DuType> {
        DuType::ALL.into_iter().find(|t| t.id() == id)
    }

    /// Ключ Fluent названия группы («Архивы»).
    pub fn label_key(self) -> &'static str {
        match self {
            DuType::Video => "du-type-video",
            DuType::Audio => "du-type-audio",
            DuType::Image => "du-type-image",
            DuType::Doc => "du-type-doc",
            DuType::Archive => "du-type-archive",
            DuType::DiskImage => "du-type-disk",
            DuType::Exec => "du-type-exec",
            DuType::Code => "du-type-code",
            DuType::Temp => "du-type-temp",
            DuType::Other => "du-type-other",
        }
    }

    /// Название группы на текущем языке.
    pub fn label(self) -> String {
        urfin_i18n::current().get(self.label_key())
    }

    /// Группа по названию строки фильтра «Типы».
    pub fn from_label(label: &str) -> Option<DuType> {
        DuType::ALL.into_iter().find(|t| t.label() == label)
    }

    /// Расширения типа из таблицы (подсказка в строке статуса).
    pub fn exts(self) -> &'static [&'static str] {
        TYPE_TABLE
            .iter()
            .find(|(t, _)| *t == self)
            .map_or(&[], |(_, e)| e)
    }
}

/// Группа фильтра «Типы» (`screen-dutypes`): «Сборки и кэши» — всё внутри сборок и кэшей
/// проекта (правила по имени каталога), остальные — типы файлов вне сборок (решение
/// 2026-11-02 «du-filters»).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TypeGroup {
    Build,
    Type(DuType),
}

impl TypeGroup {
    /// Все группы: «Сборки и кэши», затем типы по порядку `DuType`.
    pub const ALL: [TypeGroup; DuType::COUNT + 1] = {
        let mut out = [TypeGroup::Build; DuType::COUNT + 1];
        let mut i = 0;
        while i < DuType::COUNT {
            out[i + 1] = TypeGroup::Type(DuType::ALL[i]);
            i += 1;
        }
        out
    };

    /// Имя в команде `du.filter group=…`.
    pub fn id(self) -> &'static str {
        match self {
            TypeGroup::Build => "build",
            TypeGroup::Type(t) => t.id(),
        }
    }

    pub fn from_id(id: &str) -> Option<TypeGroup> {
        TypeGroup::ALL.into_iter().find(|g| g.id() == id)
    }

    /// Ключ Fluent названия.
    pub fn label_key(self) -> &'static str {
        match self {
            TypeGroup::Build => "du-type-build",
            TypeGroup::Type(t) => t.label_key(),
        }
    }

    /// Название на текущем языке.
    pub fn label(self) -> String {
        urfin_i18n::current().get(self.label_key())
    }

    /// Группа по названию строки «Типов».
    pub fn from_label(label: &str) -> Option<TypeGroup> {
        TypeGroup::ALL.into_iter().find(|g| g.label() == label)
    }

    /// Бит раскрытой группы: тип — `1 << DuType`, сборки — старший.
    pub fn bit(self) -> u16 {
        match self {
            TypeGroup::Build => 1 << 15,
            TypeGroup::Type(t) => 1 << t as u8,
        }
    }

    /// Группа крупного файла.
    pub fn of_file(f: &super::model::FileRec) -> TypeGroup {
        if f.attrs & super::model::FileRec::IN_BUILD != 0 {
            TypeGroup::Build
        } else {
            TypeGroup::Type(f.ty)
        }
    }

    /// Что в группе для подсказки в строке статуса: расширения типа, у сборок — имена
    /// каталогов правил.
    pub fn exts(self) -> Vec<String> {
        match self {
            TypeGroup::Build => NAME_RULES.iter().map(|r| r.name.to_owned()).collect(),
            TypeGroup::Type(t) => t.exts().iter().map(|e| format!(".{e}")).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Вид вкладки: фильтр и его параметры в адресе

impl DuFilter {
    pub const ALL: [DuFilter; 6] = [
        DuFilter::Dirs,
        DuFilter::Large,
        DuFilter::Types,
        DuFilter::Old,
        DuFilter::Dups,
        DuFilter::Temp,
    ];

    /// `kind` команды `du.filter` и `f=` адреса.
    pub fn id(self) -> &'static str {
        match self {
            DuFilter::Dirs => "dirs",
            DuFilter::Large => "large",
            DuFilter::Types => "types",
            DuFilter::Old => "old",
            DuFilter::Dups => "dups",
            DuFilter::Temp => "temp",
        }
    }

    pub fn from_id(id: &str) -> Option<DuFilter> {
        DuFilter::ALL.into_iter().find(|f| f.id() == id)
    }
}

/// Порог «Старые»: чипы 6m / 1y / 2y / 5y (`[du] old_age`).
pub const AGES: [&str; 4] = ["6m", "1y", "2y", "5y"];

/// Возраст в днях: `6m`, `1y`, `30d`, `2w` (единицы d/w/m/y, месяц — 30 дней,
/// год — 365); иначе `None`.
pub fn age_days(s: &str) -> Option<u32> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u32 = num.parse().ok()?;
    let per = match unit.trim() {
        "d" => 1,
        "w" => 7,
        "m" => 30,
        "y" => 365,
        _ => return None,
    };
    n.checked_mul(per).filter(|d| *d > 0)
}

/// Вид вкладки `du:`: фильтр, порог «Старые» (`age`, как в конфиге).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub filter: DuFilter,
    pub age: compact_str::CompactString,
}

impl Default for View {
    fn default() -> Self {
        View {
            filter: DuFilter::Dirs,
            age: "1y".into(),
        }
    }
}

impl View {
    pub fn new(filter: DuFilter) -> View {
        View {
            filter,
            ..View::default()
        }
    }

    /// Хвост первого сегмента адреса: `""` у «По папкам», иначе `?f=old&age=2y`.
    pub fn query(&self) -> String {
        match self.filter {
            DuFilter::Dirs => String::new(),
            DuFilter::Old => format!("?f=old&age={}", self.age),
            f => format!("?f={}", f.id()),
        }
    }

    /// Разбор хвоста `f=…&age=…` (без `?`); неизвестное — `None`.
    pub fn parse_query(q: &str) -> Option<View> {
        let mut v = View::default();
        for kv in q.split('&').filter(|s| !s.is_empty()) {
            let (k, val) = kv.split_once('=')?;
            match k {
                "f" => v.filter = DuFilter::from_id(val)?,
                "age" if age_days(val).is_some() => v.age = val.into(),
                _ => return None,
            }
        }
        Some(v)
    }

    /// Порог «Старые» в наносекундах.
    pub fn age_nanos(&self) -> i64 {
        i64::from(age_days(&self.age).unwrap_or(365)) * 86_400 * 1_000_000_000
    }
}

// ---------------------------------------------------------------------------
// «Временные»: правила

/// Где показывать совпадение: в папке анализа (сборки, пакеты) или «Система и
/// профиль».
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TempGroup {
    Project,
    System,
}

/// Правило по пути (F01 §3.5): `%VAR%`, компоненты с `*` и `{a,b}`; `what` — ключ
/// Fluent «что это».
#[derive(Copy, Clone, Debug)]
pub struct PathRule {
    pub path: &'static str,
    pub what: &'static str,
}

/// Правила по пути — «Система и профиль».
pub const PATH_RULES: [PathRule; 18] = [
    PathRule {
        path: r"%TEMP%",
        what: "du-temp-what-temp",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\Temp",
        what: "du-temp-what-temp",
    },
    PathRule {
        path: r"%WINDIR%\Temp",
        what: "du-temp-what-temp",
    },
    PathRule {
        path: r"%WINDIR%\SoftwareDistribution\Download",
        what: "du-temp-what-wu",
    },
    PathRule {
        path: r"%WINDIR%\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
        what: "du-temp-what-do",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\CrashDumps",
        what: "du-temp-what-dumps",
    },
    PathRule {
        path: r"%WINDIR%\Minidump",
        what: "du-temp-what-dumps",
    },
    PathRule {
        path: r"%WINDIR%\MEMORY.DMP",
        what: "du-temp-what-dumps",
    },
    PathRule {
        path: r"%PROGRAMDATA%\Microsoft\Windows\WER\Report*",
        what: "du-temp-what-dumps",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db",
        what: "du-temp-what-thumbs",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\{Google\Chrome,Microsoft\Edge,BraveSoftware\Brave-Browser}\User Data\*\{Cache,Code Cache,GPUCache}",
        what: "du-temp-what-browser",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\Mozilla\Firefox\Profiles\*\cache2",
        what: "du-temp-what-browser",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\npm-cache",
        what: "du-temp-what-devcache",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\pip\Cache",
        what: "du-temp-what-devcache",
    },
    PathRule {
        path: r"%USERPROFILE%\.nuget\packages",
        what: "du-temp-what-devcache",
    },
    PathRule {
        path: r"%USERPROFILE%\.cargo\registry\cache",
        what: "du-temp-what-devcache",
    },
    PathRule {
        path: r"%LOCALAPPDATA%\Yarn\Cache",
        what: "du-temp-what-devcache",
    },
    PathRule {
        path: r"?:\$Recycle.Bin",
        what: "du-temp-what-recycle",
    },
];

/// Правило по имени каталога в любом месте анализа — сборки и кэши проекта
/// (`screen-dutemp`): `target` Cargo (с `debug` или `release` внутри), `node_modules`,
/// `.cache`. Совпавший каталог — целиком, внутри уже не ищем.
#[derive(Copy, Clone, Debug)]
pub struct NameRule {
    pub name: &'static str,
    /// Внутри должен быть один из подкаталогов (пусто — не нужно).
    pub needs: &'static [&'static str],
    /// Ключ «что это»; `None` — подсказка из имён крупнейших подкаталогов.
    pub what: Option<&'static str>,
}

pub const NAME_RULES: [NameRule; 3] = [
    NameRule {
        name: "target",
        needs: &["debug", "release"],
        what: Some("du-temp-what-cargo"),
    },
    NameRule {
        name: "node_modules",
        needs: &[],
        what: Some("du-temp-what-npm"),
    },
    NameRule {
        name: ".cache",
        needs: &[],
        what: None,
    },
];

/// Правило сборок по имени каталога (`target`, `node_modules`, `.cache`), без учёта
/// регистра; нужные подкаталоги проверяет вызывающий.
pub fn build_rule(name: &str) -> Option<&'static NameRule> {
    NAME_RULES
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(name))
}

/// Одиночные временные файлы вне правил: `*.tmp`, `~$*`, `*.dmp`.
pub fn temp_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("~$")
        || std::path::Path::new(&lower)
            .extension()
            .is_some_and(|e| e == "tmp" || e == "dmp")
}

/// Правило по пути после раскрытия переменных: компоненты-шаблоны в нижнем регистре.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expanded {
    pub comps: Vec<String>,
    pub what: &'static str,
}

/// Раскрыть `%VAR%` функцией окружения; неизвестная переменная — `None`.
pub fn expand_env(s: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        let tail = &rest[a + 1..];
        let b = tail.find('%')?;
        let val = env(&tail[..b])?;
        out.push_str(val.trim_end_matches(['\\', '/']));
        rest = &tail[b + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// `{a,b}` → варианты (вложенных скобок нет).
pub fn expand_braces(s: &str) -> Vec<String> {
    let (Some(a), Some(b)) = (s.find('{'), s.find('}')) else {
        return vec![s.to_owned()];
    };
    if b < a {
        return vec![s.to_owned()];
    }
    let (head, alts, tail) = (&s[..a], &s[a + 1..b], &s[b + 1..]);
    alts.split(',')
        .flat_map(|alt| expand_braces(&format!("{head}{alt}{tail}")))
        .collect()
}

/// Правила по пути, раскрытые для окружения `env`: компоненты в нижнем регистре;
/// `?:` — любой диск.
pub fn expand_rules(env: &dyn Fn(&str) -> Option<String>) -> Vec<Expanded> {
    let mut out: Vec<Expanded> = Vec::new();
    for r in PATH_RULES {
        let Some(p) = expand_env(r.path, env) else {
            continue;
        };
        for v in expand_braces(&p) {
            let comps: Vec<String> = v
                .split(['\\', '/'])
                .filter(|c| !c.is_empty())
                .map(str::to_lowercase)
                .collect();
            if comps.len() < 2 || out.iter().any(|e| e.comps == comps) {
                continue;
            }
            out.push(Expanded {
                comps,
                what: r.what,
            });
        }
    }
    out
}

/// Правила для окружения процесса (раскрываются один раз).
pub fn rules() -> &'static [Expanded] {
    static RULES: OnceLock<Vec<Expanded>> = OnceLock::new();
    RULES.get_or_init(|| expand_rules(&|k| std::env::var(k).ok()))
}

/// Компонент пути по шаблону (`*` — любые знаки, `?:` — любой диск), без учёта
/// регистра; `pat` уже в нижнем регистре.
pub fn comp_match(pat: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    if pat == "?:" {
        return name.len() == 2 && name.ends_with(':');
    }
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut at = first.len();
    for mid in &parts[1..parts.len() - 1] {
        match name[at..].find(mid) {
            Some(i) => at += i + mid.len(),
            None => return false,
        }
    }
    name.len() - last.len() >= at && name.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str) -> RawName<'_> {
        RawName::Str {
            name,
            escaped: false,
        }
    }

    /// F01-08: каждое расширение таблицы — ровно один тип, и `type_of` с ним согласен.
    #[test]
    fn type_table_covers_each_ext_once() {
        let mut seen: HashMap<&str, DuType> = HashMap::new();
        for (t, exts) in TYPE_TABLE {
            for e in exts {
                assert_eq!(*e, e.to_ascii_lowercase(), "{e}: нижний регистр");
                if let Some(was) = seen.insert(e, t) {
                    panic!("{e}: и {was:?}, и {t:?}");
                }
                assert_eq!(type_by_ext(e), t, "{e}");
                assert_eq!(type_of(&s(&format!("file.{}", e.to_uppercase()))), t);
            }
        }
        // У каждого типа, кроме «Прочее», есть строка таблицы.
        for t in DuType::ALL {
            assert_eq!(t.exts().is_empty(), t == DuType::Other, "{t:?}");
            assert_eq!(DuType::from_id(t.id()), Some(t));
        }
        assert_eq!(type_of(&s("~WRL0001.tmp")), DuType::Temp);
        assert_eq!(type_of(&s("~$report.docx")), DuType::Temp);
        assert_eq!(type_of(&s("noext")), DuType::Other);
        assert_eq!(type_of(&s(".bashrc")), DuType::Other);
        // Не из таблицы — по классу иконки.
        assert_eq!(type_of(&s("App.tsx")), type_by_ext("tsx"));
        let w: Vec<u16> = "~x.bin".encode_utf16().collect();
        assert_eq!(type_of(&RawName::Wide(&w)), DuType::Temp);
    }

    #[test]
    fn views_roundtrip() {
        for f in DuFilter::ALL {
            let v = View::new(f);
            let q = v.query();
            if f == DuFilter::Dirs {
                assert_eq!(q, "");
                continue;
            }
            assert_eq!(View::parse_query(&q[1..]), Some(v), "{q}");
        }
        let old = View::parse_query("f=old&age=2y").unwrap();
        assert_eq!(old.filter, DuFilter::Old);
        assert_eq!(old.age, "2y");
        assert_eq!(old.age_nanos(), 730 * 86_400 * 1_000_000_000);
        assert!(View::parse_query("f=nope").is_none());
        assert!(View::parse_query("x=1").is_none());
        // Плохой возраст — адрес не разобран.
        assert_eq!(View::parse_query("f=old&age=zz").map(|v| v.age), None);
        assert_eq!(age_days("6m"), Some(180));
        assert_eq!(age_days("5y"), Some(1825));
        assert_eq!(age_days("0d"), None);
        assert_eq!(age_days("1 y"), Some(365));
        assert_eq!(age_days("y"), None);
    }

    /// F01-08: переменные окружения раскрываются; неизвестная — правило пропущено.
    #[test]
    fn env_expanded() {
        let env = |k: &str| match k {
            "TEMP" => Some(r"C:\Users\me\AppData\Local\Temp\".to_owned()),
            "LOCALAPPDATA" => Some(r"C:\Users\me\AppData\Local".to_owned()),
            "WINDIR" => Some(r"C:\Windows".to_owned()),
            _ => None,
        };
        assert_eq!(
            expand_env(r"%WINDIR%\Temp", &env).as_deref(),
            Some(r"C:\Windows\Temp")
        );
        assert_eq!(expand_env(r"%NOPE%\x", &env), None);
        assert_eq!(expand_env(r"%TEMP", &env), None);
        let rules = expand_rules(&env);
        let has = |p: &str| {
            let c: Vec<String> = p.split('\\').map(str::to_lowercase).collect();
            rules.iter().any(|r| r.comps == c)
        };
        assert!(has(r"C:\Users\me\AppData\Local\Temp"));
        assert!(has(r"C:\Windows\SoftwareDistribution\Download"));
        assert!(has(
            r"C:\Users\me\AppData\Local\Microsoft\Edge\User Data\*\Code Cache"
        ));
        assert!(has(r"?:\$Recycle.Bin"));
        // %TEMP% и %LOCALAPPDATA%\Temp совпали — одно правило.
        let temps = rules
            .iter()
            .filter(|r| r.comps.last().is_some_and(|c| c == "temp") && r.comps.len() == 6)
            .count();
        assert_eq!(temps, 1);
        // Без %PROGRAMDATA% и %USERPROFILE% — пропущены.
        assert!(!rules.iter().any(|r| r.comps.iter().any(|c| c == ".nuget")));
    }

    #[test]
    fn braces_and_globs() {
        assert_eq!(expand_braces("a{b,c}d"), ["abd", "acd"]);
        assert_eq!(expand_braces("x"), ["x"]);
        assert!(comp_match("report*", "ReportArchive"));
        assert!(comp_match("thumbcache_*.db", "thumbcache_256.db"));
        assert!(!comp_match("thumbcache_*.db", "thumbcache_256.dbx"));
        assert!(comp_match("*", "anything"));
        assert!(comp_match("a*b*c", "aXXbYYc"));
        assert!(!comp_match("a*b*c", "ac"));
        assert!(comp_match("?:", "D:"));
        assert!(comp_match("cache", "Cache"));
        assert!(temp_file("~$Отчёт.docx"));
        assert!(temp_file("x.TMP"));
        assert!(temp_file("crash.dmp"));
        assert!(!temp_file("x.tmpl"));
    }
}
