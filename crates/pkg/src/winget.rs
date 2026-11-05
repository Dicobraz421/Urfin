//! winget (F02 §2.3, F02-04): таблицы `list`/`upgrade`/`search` (`table.rs`) с ролями
//! колонок по содержимому; точные Id и версии — из `winget export`, усечённые `…` Id и
//! имена чинятся по `export` и ARP. `--output json` — только если `winget list --help`
//! его знает (в известных версиях его нет, тогда текст).
//!
//! Флаги всегда: `--accept-source-agreements --disable-interactivity`; у install и
//! upgrade ещё `--accept-package-agreements` и `--silent` по настройке.

use std::ffi::OsString;
use std::path::Path;

use compact_str::CompactString;
use serde::Deserialize;

use crate::api::{
    ActionPlan, CheckedId, Hit, MatchKind, Mgr, Package, PkgError, PkgId, PkgOp, Update,
};
use crate::arp::{ArpEntry, ArpScope};
use crate::table;

/// Флаги каждого вызова.
pub const COMMON_FLAGS: [&str; 2] = ["--accept-source-agreements", "--disable-interactivity"];
/// Источники winget по умолчанию: по ним узнаётся колонка Source.
pub const KNOWN_SOURCES: [&str; 3] = ["winget", "msstore", "winget-font"];
/// Знак усечения в ячейках winget.
pub const ELLIPSIS: char = '…';

/// Коды выхода winget (`returnCodes.md`), которые решают итог.
pub mod code {
    /// Запрос без результатов (`search` без совпадений) — пустой список, не ошибка.
    pub const NO_APPLICATIONS_FOUND: u32 = 0x8A15_0014;
    /// «Нет применимого обновления»: уже последняя версия.
    pub const UPDATE_NOT_APPLICABLE: u32 = 0x8A15_002B;
    /// install: пакет уже установлен.
    pub const PACKAGE_ALREADY_INSTALLED: u32 = 0x8A15_0061;
    /// Установлено, для завершения нужна перезагрузка.
    pub const INSTALL_REBOOT_REQUIRED_TO_FINISH: u32 = 0x8A15_0109;
    /// Установлено, перезагрузка уже запущена.
    pub const INSTALL_REBOOT_INITIATED: u32 = 0x8A15_010B;
}

/// Таблица кодов выхода действий: какие — успех, какие — успех с перезагрузкой.
/// Прочие — ошибка с хвостом вывода (в нём текст winget на языке Windows).
pub fn exit_codes(op: PkgOp) -> (Vec<u32>, Vec<u32>) {
    let ok = match op {
        PkgOp::Install => vec![
            0,
            code::UPDATE_NOT_APPLICABLE,
            code::PACKAGE_ALREADY_INSTALLED,
        ],
        PkgOp::Upgrade => vec![0, code::UPDATE_NOT_APPLICABLE],
        PkgOp::Uninstall => vec![0],
    };
    let reboot = vec![
        code::INSTALL_REBOOT_REQUIRED_TO_FINISH,
        code::INSTALL_REBOOT_INITIATED,
    ];
    (ok, reboot)
}

/// Строка таблицы winget по ролям колонок.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: CompactString,
    pub id: CompactString,
    pub version: CompactString,
    pub available: Option<CompactString>,
    pub source: Option<CompactString>,
    /// Из таблицы после первой: `winget upgrade` выносит туда пакеты, которые
    /// обновляются только явно (пины, `RequireExplicitUpgrade`).
    pub explicit: bool,
}

/// Локальный Id без источника: `ARP\…` (реестр) или `MSIX\…` (пакет приложения).
pub fn is_local(id: &str) -> bool {
    urfin_core::pkg::winget_local(id)
}

/// Усечён ли текст ячейки.
pub fn is_truncated(s: &str) -> bool {
    s.ends_with(ELLIPSIS)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Available,
    Match,
    Source,
    Skip,
}

/// Строка данных: Id без пробелов (локальный — может с пробелами: ключ ARP) и
/// версия. Хвосты в сетке («3 upgrades available.») сюда не проходят.
fn is_row(cells: &[String]) -> bool {
    cells.len() >= 3
        && !cells[1].is_empty()
        && !cells[2].is_empty()
        && (is_local(&cells[1]) || !cells[1].contains(' '))
}

/// Роли колонок после Name, Id, Version: известные источники — Source; `:` в
/// значениях — Match поиска (`Tag: firefox`); последняя после ещё одной — Source
/// (свой источник); иначе Available. Пустая колонка не нужна.
fn roles(rows: &[&Vec<String>], n: usize) -> Vec<Role> {
    (3..n)
        .map(|j| {
            let vals: Vec<&str> = rows
                .iter()
                .map(|r| r[j].as_str())
                .filter(|v| !v.is_empty())
                .collect();
            if vals.is_empty() {
                Role::Skip
            } else if vals
                .iter()
                .all(|v| KNOWN_SOURCES.iter().any(|s| s.eq_ignore_ascii_case(v)))
            {
                Role::Source
            } else if vals.iter().any(|v| v.contains(':')) {
                Role::Match
            } else if j == n - 1 && j > 3 {
                Role::Source
            } else {
                Role::Available
            }
        })
        .collect()
}

/// Строки всех таблиц вывода `list`/`upgrade`/`search`. Name, Id и Version —
/// первые три колонки (порядок у winget постоянный), остальные — по содержимому.
pub fn rows(lines: &[String]) -> Vec<Row> {
    let mut out = Vec::new();
    for (k, t) in table::parse(lines).into_iter().enumerate() {
        let n = t.starts.len();
        if n < 3 {
            continue;
        }
        let good: Vec<&Vec<String>> = t.rows.iter().filter(|r| is_row(r)).collect();
        let roles = roles(&good, n);
        for r in good {
            let mut row = Row {
                name: r[0].as_str().into(),
                id: r[1].as_str().into(),
                version: r[2].as_str().into(),
                available: None,
                source: None,
                explicit: k > 0,
            };
            for (role, v) in roles.iter().zip(&r[3..]) {
                let v = (!v.is_empty()).then(|| CompactString::from(v.as_str()));
                match role {
                    Role::Available => row.available = v,
                    Role::Source => row.source = v,
                    Role::Match | Role::Skip => {}
                }
            }
            out.push(row);
        }
    }
    out
}

fn parse_err(e: impl std::fmt::Display) -> PkgError {
    PkgError::Parse {
        line: e.to_string().into(),
    }
}

/// Строки `--output json`, если winget его знает: массив объектов (верхний уровень
/// или первое поле-массив), ключи без учёта регистра.
pub fn json_rows(text: &str) -> Result<Vec<Row>, PkgError> {
    use serde_json::Value;
    let start = text.find(['[', '{']).unwrap_or(0);
    let v: Value = serde_json::from_str(&text[start..]).map_err(parse_err)?;
    let items = match &v {
        Value::Array(a) => a,
        Value::Object(o) => o
            .values()
            .find_map(Value::as_array)
            .ok_or_else(|| parse_err("no array"))?,
        _ => return Err(parse_err("no array")),
    };
    items
        .iter()
        .map(|it| {
            let get = |keys: &[&str]| {
                it.as_object()?
                    .iter()
                    .find(|(k, _)| keys.iter().any(|w| k.eq_ignore_ascii_case(w)))?
                    .1
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(CompactString::from)
            };
            Ok(Row {
                id: get(&["Id", "PackageIdentifier"]).ok_or_else(|| parse_err(it))?,
                name: get(&["Name", "PackageName"]).unwrap_or_default(),
                version: get(&["Version", "InstalledVersion"]).unwrap_or_default(),
                available: get(&["Available", "AvailableVersion"]),
                source: get(&["Source"]),
                explicit: false,
            })
        })
        .collect()
}

/// Пакет из `winget export`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub id: CompactString,
    /// С `--include-versions`.
    pub version: Option<CompactString>,
    pub source: CompactString,
}

/// Файл `winget export` (схема `winget-packages.schema.2.0`; BOM допускается).
pub fn parse_export(text: &str) -> Result<Vec<Exported>, PkgError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct File {
        #[serde(default)]
        sources: Vec<Source>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Source {
        #[serde(default)]
        packages: Vec<Pkg>,
        source_details: Details,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Details {
        name: CompactString,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Pkg {
        package_identifier: CompactString,
        #[serde(default)]
        version: Option<CompactString>,
    }
    let file: File =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(parse_err)?;
    Ok(file
        .sources
        .into_iter()
        .flat_map(|s| {
            let source = s.source_details.name;
            s.packages.into_iter().map(move |p| Exported {
                id: p.package_identifier,
                version: p.version,
                source: source.clone(),
            })
        })
        .collect())
}

/// Id, который winget даёт записи ARP: `ARP\Machine\X64\<ключ>`.
pub fn arp_id(e: &ArpEntry) -> String {
    let hive = match e.scope {
        ArpScope::Machine => r"Machine\X64",
        ArpScope::Machine32 => r"Machine\X86",
        ArpScope::User => r"User\X64",
    };
    format!(r"ARP\{hive}\{}", e.key)
}

/// Единственный элемент; ноль или несколько — `None`.
fn only<T>(mut it: impl Iterator<Item = T>) -> Option<T> {
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

/// Починка усечённого `…`: Id — по единственному продолжению префикса в `export`
/// (того же источника) или среди Id записей ARP; версия — из `export`; имя — из
/// `DisplayName` своей записи ARP или единственной с таким началом (при нескольких —
/// с той же версией). Неоднозначное остаётся усечённым.
pub fn repair(rows: &mut [Row], export: &[Exported], arp: &[ArpEntry]) {
    let arp_ids: Vec<String> = arp.iter().map(arp_id).collect();
    for row in rows {
        if let Some(prefix) = row.id.strip_suffix(ELLIPSIS) {
            let found = if prefix.starts_with("ARP\\") {
                only(arp_ids.iter().filter(|id| id.starts_with(prefix))).map(String::as_str)
            } else {
                only(export.iter().filter(|e| {
                    e.id.starts_with(prefix) && row.source.as_ref().is_none_or(|s| *s == e.source)
                }))
                .map(|e| e.id.as_str())
            };
            if let Some(id) = found {
                row.id = id.into();
            }
        }
        if is_truncated(&row.version)
            && let Some(v) = export
                .iter()
                .find(|e| e.id == row.id)
                .and_then(|e| e.version.clone())
        {
            row.version = v;
        }
        if let Some(prefix) = row.name.strip_suffix(ELLIPSIS) {
            let own = arp_ids.iter().position(|id| *id == row.id).map(|i| &arp[i]);
            let found = own.map(|e| e.name.clone()).or_else(|| {
                let starts: Vec<&ArpEntry> =
                    arp.iter().filter(|e| e.name.starts_with(prefix)).collect();
                match starts.as_slice() {
                    [one] => Some(one.name.clone()),
                    _ => only(
                        starts
                            .iter()
                            .filter(|e| e.version.as_deref() == Some(row.version.as_str())),
                    )
                    .map(|e| e.name.clone()),
                }
            });
            if let Some(name) = found {
                row.name = name;
            }
        }
    }
}

/// Издатель по Id источника: префикс до первой точки (`Mozilla.Firefox` → `Mozilla`).
pub fn publisher_of(id: &str) -> Option<CompactString> {
    id.split_once('.')
        .map(|(p, _)| p)
        .filter(|p| !p.is_empty())
        .map(CompactString::from)
}

/// Пакет из строки: издатель локального — из ARP, остальных — по Id.
pub fn package(row: &Row, arp: &[ArpEntry]) -> Package {
    let publisher = if is_local(&row.id) {
        arp.iter()
            .find(|e| arp_id(e) == row.id)
            .and_then(|e| e.publisher.clone())
    } else {
        publisher_of(&row.id)
    };
    Package {
        id: PkgId {
            mgr: Mgr::Winget,
            id: row.id.clone(),
        },
        name: row.name.clone(),
        version: row.version.clone(),
        source: row.source.clone(),
        publisher,
    }
}

/// Обновления из строк `winget upgrade`; без Available — не обновление.
pub fn updates(rows: &[Row], arp: &[ArpEntry]) -> Vec<Update> {
    rows.iter()
        .filter_map(|r| {
            Some(Update {
                pkg: package(r, arp),
                available: r.available.clone()?,
                pinned: r.explicit,
            })
        })
        .collect()
}

/// Результаты `winget search q`.
pub fn hits(q: &str, rows: &[Row]) -> Vec<Hit> {
    rows.iter()
        .map(|r| Hit {
            pkg: package(r, &[]),
            matched: MatchKind::of(q, &r.id, &r.name),
        })
        .collect()
}

fn argv(exe: &Path, args: &[&str]) -> Vec<OsString> {
    std::iter::once(exe.as_os_str().to_owned())
        .chain(args.iter().map(OsString::from))
        .chain(COMMON_FLAGS.iter().map(OsString::from))
        .collect()
}

/// `winget list --help`: знает ли `--output`.
pub fn help_argv(exe: &Path) -> Vec<OsString> {
    vec![exe.into(), "list".into(), "--help".into()]
}

/// Есть ли `--output` в справке `winget list --help`.
pub fn knows_output(help: &[String]) -> bool {
    help.iter()
        .any(|l| l.split_whitespace().any(|w| w == "--output"))
}

pub fn list_argv(exe: &Path) -> Vec<OsString> {
    argv(exe, &["list"])
}

pub fn upgrade_list_argv(exe: &Path, include_unknown: bool) -> Vec<OsString> {
    let mut a = argv(exe, &["upgrade"]);
    if include_unknown {
        a.push("--include-unknown".into());
    }
    a
}

/// Запрос поиска для argv: без управляющих символов и ведущих `-` (не флаг).
pub fn search_query(q: &str) -> String {
    let q: String = q.chars().filter(|c| !c.is_control()).collect();
    q.trim().trim_start_matches('-').trim_start().to_owned()
}

/// `winget search --query <q>`; `q` — после [`search_query`].
pub fn search_argv(exe: &Path, q: &str) -> Vec<OsString> {
    let mut a = argv(exe, &["search", "--query"]);
    a.insert(3, q.into());
    a
}

/// Сведения о пакете (`pkg.info`): `winget show --id <id> --exact`. Локальный Id
/// (`ARP\…`, `MSIX\…`) в источниках не найти.
pub fn info_argv(exe: &Path, id: &PkgId) -> Result<Vec<OsString>, PkgError> {
    let checked = id.checked(Mgr::Winget)?;
    if is_local(checked) {
        return Err(PkgError::BadId(id.to_string().into()));
    }
    Ok(argv(exe, &["show", "--id", checked, "--exact"]))
}

/// `winget export -o <out> --include-versions`.
pub fn export_argv(exe: &Path, out: &Path) -> Vec<OsString> {
    let mut a = argv(exe, &["export", "-o"]);
    a.insert(3, out.into());
    a.insert(4, "--include-versions".into());
    a
}

/// Действие: `winget <install|upgrade|uninstall> --id <id> --exact` и флаги.
/// Локальный Id (`ARP\…`, `MSIX\…`) — только удаление: ставить его неоткуда.
pub fn action_plan(
    exe: &Path,
    op: PkgOp,
    id: &PkgId,
    silent: bool,
) -> Result<ActionPlan, PkgError> {
    let checked = id.checked(Mgr::Winget)?;
    if op != PkgOp::Uninstall && is_local(checked) {
        return Err(PkgError::BadId(id.to_string().into()));
    }
    let verb = match op {
        PkgOp::Install => "install",
        PkgOp::Upgrade => "upgrade",
        PkgOp::Uninstall => "uninstall",
    };
    let mut argv = argv(exe, &[verb, "--id", checked, "--exact"]);
    if op != PkgOp::Uninstall {
        argv.push("--accept-package-agreements".into());
        if silent {
            argv.push("--silent".into());
        }
    }
    let (success_codes, reboot_codes) = exit_codes(op);
    Ok(ActionPlan {
        argv,
        elevate: false,
        success_codes,
        reboot_codes,
    })
}

#[cfg(windows)]
pub use adapter::WingetAdapter;

#[cfg(windows)]
mod adapter {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    use compact_str::CompactString;

    use super::{
        Exported, Row, action_plan, code, help_argv, hits, info_argv, is_truncated, json_rows,
        knows_output,
        list_argv, package, parse_export, repair, rows, search_argv, search_query, updates,
        upgrade_list_argv,
    };
    use crate::api::{
        ActionPlan, Detected, Hit, Mgr, MgrCaps, Package, PackageManager, PkgError, PkgId, PkgOp,
        RunCx, Update,
    };
    use crate::arp::Arp;
    use crate::config::PkgConfig;
    use crate::detect::query_version;
    use crate::found::Found;
    use crate::proc::{Output, run_query};

    /// winget через процессы (F02 §2.3).
    #[derive(Debug)]
    pub struct WingetAdapter {
        found: Found,
        silent: bool,
        include_unknown: bool,
    }

    /// Запрос: «ничего не найдено» — пустой вывод, иначе код 0.
    fn query(argv: &[OsString], cx: &RunCx) -> Result<Output, PkgError> {
        let out = run_query(argv, cx)?;
        if out.code == code::NO_APPLICATIONS_FOUND {
            return Ok(Output {
                code: 0,
                lines: Vec::new(),
            });
        }
        out.ok()
    }

    /// `winget export` во временный файл. Ошибка — без починки (`Vec` пуст), кроме
    /// отмены и таймаута.
    fn export(exe: &Path, cx: &RunCx) -> Result<Vec<Exported>, PkgError> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "urfin-winget-export-{}-{}.json",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let res = run_query(&super::export_argv(exe, &path), cx)
            .and_then(Output::ok)
            .and_then(|_| {
                std::fs::read_to_string(&path).map_err(|e| PkgError::Parse {
                    line: e.to_string().into(),
                })
            })
            .and_then(|text| parse_export(&text));
        let _ = std::fs::remove_file(&path);
        match res {
            Err(e @ (PkgError::Cancelled | PkgError::Timeout)) => Err(e),
            Err(e) => {
                tracing::warn!(error = %e, "pkg: winget export");
                Ok(Vec::new())
            }
            ok => ok,
        }
    }

    impl WingetAdapter {
        /// `cached` — из `managers.toml`.
        pub fn new(config: &PkgConfig, cached: Option<Detected>) -> WingetAdapter {
            WingetAdapter {
                found: Found::new(Mgr::Winget, cached),
                silent: config.winget_silent,
                include_unknown: config.winget_include_unknown,
            }
        }

        /// Таблица запроса: JSON, если winget его знает (не разобрался — текст).
        fn rows(&self, argv: &[OsString], cx: &RunCx) -> Result<Vec<Row>, PkgError> {
            // Версия заодно выясняет возможности (`--output`).
            let _ = self.version(cx);
            if self.found.caps().contains(MgrCaps::JSON_OUTPUT) {
                let mut json = argv.to_vec();
                json.extend(["--output".into(), "json".into()]);
                match query(&json, cx).and_then(|o| json_rows(&o.lines.join("\n"))) {
                    Ok(r) => return Ok(r),
                    Err(e @ (PkgError::Cancelled | PkgError::Timeout)) => return Err(e),
                    Err(e) => tracing::warn!(error = %e, "pkg: winget --output json, text instead"),
                }
            }
            Ok(rows(&query(argv, cx)?.lines))
        }

        /// Строки с починкой `…`: `export` — только если усечены Id или версии.
        fn repaired(
            &self,
            argv: &[OsString],
            cx: &RunCx,
        ) -> Result<(Vec<Row>, Vec<crate::arp::ArpEntry>), PkgError> {
            let exe = self.found.exe()?;
            let mut rows = self.rows(argv, cx)?;
            let exported = if rows
                .iter()
                .any(|r| is_truncated(&r.id) || is_truncated(&r.version))
            {
                export(&exe, cx)?
            } else {
                Vec::new()
            };
            let arp = Arp::load();
            repair(&mut rows, &exported, &arp);
            Ok((rows, arp))
        }
    }

    impl PackageManager for WingetAdapter {
        fn mgr(&self) -> Mgr {
            Mgr::Winget
        }

        fn detect(&self) -> Option<Detected> {
            self.found.get()
        }

        /// `--version` и сверка `--output` по `winget list --help`.
        fn version(&self, cx: &RunCx) -> Result<CompactString, PkgError> {
            self.found.version(|d| {
                let v = query_version(d, cx)?;
                match run_query(&help_argv(&d.exe), cx) {
                    Ok(out) if knows_output(&out.lines) => d.caps |= MgrCaps::JSON_OUTPUT,
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "pkg: winget list --help"),
                }
                Ok(v)
            })
        }

        fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError> {
            let exe = self.found.exe()?;
            let (rows, arp) = self.repaired(&list_argv(&exe), cx)?;
            Ok(rows.iter().map(|r| package(r, &arp)).collect())
        }

        fn list_updates(&self, cx: &RunCx) -> Result<Vec<Update>, PkgError> {
            let exe = self.found.exe()?;
            let (rows, arp) = self.repaired(&upgrade_list_argv(&exe, self.include_unknown), cx)?;
            Ok(updates(&rows, &arp))
        }

        fn search(&self, q: &str, cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
            let exe = self.found.exe()?;
            let q = search_query(q);
            if q.is_empty() {
                return Ok(Vec::new());
            }
            Ok(hits(&q, &self.rows(&search_argv(&exe, &q), cx)?))
        }

        fn info_argv(&self, id: &PkgId) -> Result<Vec<OsString>, PkgError> {
            info_argv(&self.found.exe()?, id)
        }

        fn action(&self, op: PkgOp, id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
            action_plan(&self.found.exe()?, op, id, self.silent)
        }
    }
}

#[cfg(test)]
#[path = "winget_tests.rs"]
mod tests;
