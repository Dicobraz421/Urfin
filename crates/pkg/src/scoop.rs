//! scoop (F02 §2.3, F02-05): скрипт PowerShell в `-EncodedCommand` (`ExecutionPolicy`
//! не мешает, `scoop.ps1` идёт с `-ExecutionPolicy Bypass`, как в шиме самого scoop).
//! Скрипт выставляет вывод в UTF-8, глушит `Write-Host` (`6>$null`), печатает маркер
//! `<<URFIN>> <секция>` и за ним — данные. Всё до первого маркера — мусор (вывод
//! `scoop update`, строки в конвейере), разбор его не видит.
//!
//! - установленные: `scoop export` (`apps[]: Name, Version, Source, Updated, Info`;
//!   до 2022 — текст `7zip (v:19.00) [main]`);
//! - обновления: `scoop update` (git pull бакетов, по `scoop_update_buckets`), затем
//!   `scoop export` и `scoop status --local` JSON;
//! - поиск: `scoop search <q>` JSON (`Name, Version, Source`).
//!
//! `ConvertTo-Json` по конвейеру отдаёт один объект вместо массива из одного; скрипт
//! передаёт массив в `-InputObject`, а разбор принимает и объект.

use std::ffi::OsString;
use std::path::Path;

use base64::Engine as _;
use compact_str::CompactString;
use serde_json::{Map, Value};

use crate::api::{
    ActionPlan, CheckedId, Hit, MatchKind, Mgr, Package, PkgError, PkgId, PkgOp, Update, valid_id,
};

/// Маркер начала секции данных в выводе скрипта.
pub const MARKER: &str = "<<URFIN>>";

/// Кавычки, которые PowerShell считает одинарными (и типографские).
const QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// Строка PowerShell в одинарных кавычках: кавычки удваиваются, управляющие символы
/// снимаются. Внутри `'…'` PowerShell ничего не раскрывает.
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars().filter(|c| !c.is_control()) {
        if QUOTES.contains(&c) {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Скрипт для `-EncodedCommand`: UTF-16LE в base64.
pub fn encode(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand …`.
pub fn powershell_argv(ps: &Path, script: &str) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![ps.into()];
    a.extend(
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
        ]
        .map(OsString::from),
    );
    a.push(encode(script).into());
    a
}

/// Начало каждого скрипта: UTF-8 на выходе (без консоли setter бросает — молча),
/// без полос прогресса, путь к `scoop.ps1`.
fn script(scoop: &Path, body: &str) -> String {
    format!(
        "$ProgressPreference = 'SilentlyContinue'\n\
         try {{ [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false }} catch {{}}\n\
         $OutputEncoding = [Console]::OutputEncoding\n\
         $scoop = {}\n\
         {body}",
        ps_quote(&scoop.to_string_lossy())
    )
}

/// Секция `name`: объекты конвейера `expr` JSON-массивом (и при одном элементе).
fn json_section(name: &str, expr: &str) -> String {
    format!(
        "$r = @({expr})\n'{MARKER} {name}'\nConvertTo-Json -InputObject $r -Depth 4 -Compress\n"
    )
}

/// Секция `export`: `scoop export` печатает JSON сам (старый — текст); если вернул
/// объект — сериализуем.
const EXPORT: &str = "$e = @(& $scoop export 6>$null)\n\
     '<<URFIN>> export'\n\
     if ($e.Count -gt 0 -and $e[0] -isnot [string]) { ConvertTo-Json -InputObject $e[0] -Depth 5 -Compress } else { $e -join \"`n\" }\n";

/// Версия: `scoop --version` пишет через `Write-Host` — в вывод.
pub fn version_script(scoop: &Path) -> String {
    script(
        scoop,
        &format!(
            "'{MARKER} version'\n& $scoop --version *>&1 | ForEach-Object {{ \"$_\" }}\nexit 0\n"
        ),
    )
}

/// Установленные: `scoop export`.
pub fn export_script(scoop: &Path) -> String {
    script(scoop, &format!("{EXPORT}exit 0\n"))
}

/// Обновления: `scoop update` (по настройке, вывод — до маркера), `export` и
/// `status --local`.
pub fn status_script(scoop: &Path, update_buckets: bool) -> String {
    let mut body = String::new();
    if update_buckets {
        body.push_str("& $scoop update *>&1 | ForEach-Object { \"$_\" }\n");
    }
    body.push_str(EXPORT);
    body.push_str(&json_section("status", "& $scoop status --local 6>$null"));
    body.push_str("exit 0\n");
    script(scoop, &body)
}

/// Поиск: `scoop search <q>`; `q` — после [`crate::winget::search_query`].
pub fn search_script(scoop: &Path, q: &str) -> String {
    let expr = format!("& $scoop search {} 6>$null", ps_quote(q));
    script(scoop, &format!("{}exit 0\n", json_section("search", &expr)))
}

/// Секции вывода по маркерам: имя и текст до следующего маркера.
pub fn sections(lines: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in lines {
        if let Some(name) = line.trim().strip_prefix(MARKER) {
            out.push((name.trim().to_owned(), String::new()));
        } else if let Some((_, text)) = out.last_mut() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line);
        }
    }
    out
}

/// Текст секции `name` (последней с этим именем).
pub fn section<'a>(secs: &'a [(String, String)], name: &str) -> Result<&'a str, PkgError> {
    secs.iter()
        .rev()
        .find(|(n, _)| n == name)
        .map(|(_, t)| t.as_str())
        .ok_or_else(|| PkgError::Parse {
            line: format!("no {MARKER} {name}").into(),
        })
}

fn parse_err(e: impl std::fmt::Display) -> PkgError {
    PkgError::Parse {
        line: e.to_string().into(),
    }
}

/// Обёртка PowerShell 5.1 над массивом: `{"value": […], "Count": N}`.
fn unwrap_value(v: Value) -> Value {
    match v {
        Value::Object(mut o)
            if o.len() == 2 && o.contains_key("value") && o.contains_key("Count") =>
        {
            o.remove("value").unwrap_or(Value::Null)
        }
        v => v,
    }
}

/// Объекты JSON: массив, один объект (`ConvertTo-Json` по конвейеру), пусто.
/// Не-объекты (строки, попавшие в конвейер) пропускаются.
pub fn json_items(text: &str) -> Result<Vec<Map<String, Value>>, PkgError> {
    let text = text.trim().trim_start_matches('\u{feff}');
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(text).map_err(parse_err)?;
    Ok(match unwrap_value(v) {
        Value::Array(a) => a
            .into_iter()
            .filter_map(|v| match v {
                Value::Object(o) => Some(o),
                _ => None,
            })
            .collect(),
        Value::Object(o) => vec![o],
        _ => Vec::new(),
    })
}

/// Поле без учёта регистра (первое из `names`): строка, число; массив строк —
/// через «, ». Пустое — `None`.
fn field(o: &Map<String, Value>, names: &[&str]) -> Option<CompactString> {
    let text = |v: &Value| -> Option<String> {
        match v {
            Value::String(s) => Some(s.trim().to_owned()),
            Value::Number(n) => Some(n.to_string()),
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            _ => None,
        }
    };
    names.iter().find_map(|name| {
        o.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| text(v))
            .filter(|s| !s.is_empty())
            .map(CompactString::from)
    })
}

/// Id scoop: `bucket/name`, если бакет — имя (а не URL или путь манифеста).
pub fn scoop_id(bucket: Option<&str>, name: &str) -> CompactString {
    match bucket {
        Some(b) if valid_id(Mgr::Scoop, &format!("{b}/{name}")) => format!("{b}/{name}").into(),
        _ => name.into(),
    }
}

/// Имя без бакета: `main/7zip` → `7zip`.
pub fn app_name(id: &str) -> &str {
    id.rsplit_once('/').map_or(id, |(_, n)| n)
}

fn package(name: &str, version: &str, bucket: Option<&str>) -> Package {
    Package {
        id: PkgId {
            mgr: Mgr::Scoop,
            id: scoop_id(bucket, name),
        },
        name: name.into(),
        version: version.into(),
        source: bucket.map(CompactString::from),
        publisher: None,
    }
}

/// Строка старого `scoop export`: `7zip (v:19.00) *global* [main]`.
fn legacy_line(line: &str) -> Option<Package> {
    let (name, rest) = line.trim().split_once(" (v:")?;
    let (version, rest) = rest.split_once(')')?;
    let bucket = rest
        .rsplit_once('[')
        .and_then(|(_, b)| b.strip_suffix(']'))
        .map(str::trim)
        .filter(|b| !b.is_empty());
    let name = name.trim();
    (!name.is_empty() && !name.contains(' ')).then(|| package(name, version.trim(), bucket))
}

/// `scoop export`: JSON (`apps[]`, или массив приложений), иначе старый текст.
pub fn parse_export(text: &str) -> Result<Vec<Package>, PkgError> {
    let trimmed = text.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with(['{', '[']) {
        let v: Value = serde_json::from_str(trimmed).map_err(parse_err)?;
        let apps = match unwrap_value(v) {
            Value::Object(mut o) => {
                let key = o.keys().find(|k| k.eq_ignore_ascii_case("apps")).cloned();
                match key.and_then(|k| o.remove(&k)) {
                    Some(apps) => json_items(&apps.to_string())?,
                    None => vec![o],
                }
            }
            v => json_items(&v.to_string())?,
        };
        return Ok(apps
            .iter()
            .filter_map(|a| {
                let name = field(a, &["Name"])?;
                let version = field(a, &["Version"]).unwrap_or_default();
                let bucket = field(a, &["Source", "Bucket"]);
                Some(package(&name, &version, bucket.as_deref()))
            })
            .collect());
    }
    let pkgs: Vec<Package> = trimmed.lines().filter_map(legacy_line).collect();
    if pkgs.is_empty() {
        return Err(PkgError::Parse {
            line: trimmed.lines().next().unwrap_or_default().into(),
        });
    }
    Ok(pkgs)
}

/// `scoop status --local`: строки с более новой версией; пакет — из `installed`
/// (бакет), иначе только имя. Удержанный (`scoop hold`, Info «Held package») — пин.
pub fn parse_status(text: &str, installed: &[Package]) -> Result<Vec<Update>, PkgError> {
    let mut out = Vec::new();
    for o in json_items(text)? {
        let Some(name) = field(&o, &["Name"]) else {
            continue;
        };
        let current = field(&o, &["Installed Version", "Installed", "Version"]).unwrap_or_default();
        let Some(latest) = field(&o, &["Latest Version", "Latest"]) else {
            continue;
        };
        if latest == current {
            continue;
        }
        let info = field(&o, &["Info"]).unwrap_or_default();
        let mut pkg = installed
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&name) || app_name(&p.id.id) == name)
            .cloned()
            .unwrap_or_else(|| package(&name, &current, None));
        if !current.is_empty() {
            pkg.version = current;
        }
        out.push(Update {
            pkg,
            available: latest,
            pinned: info.to_ascii_lowercase().contains("held"),
        });
    }
    Ok(out)
}

/// `scoop search`: `Name, Version, Source`.
pub fn parse_search(q: &str, text: &str) -> Result<Vec<Hit>, PkgError> {
    Ok(json_items(text)?
        .iter()
        .filter_map(|o| {
            let name = field(o, &["Name"])?;
            let version = field(o, &["Version"]).unwrap_or_default();
            let bucket = field(o, &["Source", "Bucket"]);
            let pkg = package(&name, &version, bucket.as_deref());
            let matched = MatchKind::of(q, &pkg.id.id, &pkg.name);
            Some(Hit { pkg, matched })
        })
        .collect())
}

/// Версия scoop: первое слово вида `v0.5.2` / `0.5.2` (иначе — `None`: сборка из git).
pub fn parse_version(text: &str) -> Option<CompactString> {
    text.split_whitespace()
        .map(|w| w.strip_prefix(['v', 'V']).unwrap_or(w))
        .find(|w| {
            let mut parts = w.split('.');
            parts
                .next()
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                && parts
                    .next()
                    .is_some_and(|p| p.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(CompactString::from)
}

/// Действие: `scoop install <bucket/name>` / `update <name>` / `uninstall <name>`;
/// вывод `Write-Host` — в поток (ленточный сеанс), код — `$LASTEXITCODE` scoop.
/// UAC не нужен: scoop ставит в профиль пользователя (глобальных в F-02 нет).
pub fn action_plan(ps: &Path, scoop: &Path, op: PkgOp, id: &PkgId) -> Result<ActionPlan, PkgError> {
    let checked = id.checked(Mgr::Scoop)?;
    let (verb, target) = match op {
        PkgOp::Install => ("install", checked),
        PkgOp::Upgrade => ("update", app_name(checked)),
        PkgOp::Uninstall => ("uninstall", app_name(checked)),
    };
    let body = format!(
        "& $scoop {verb} {} *>&1 | ForEach-Object {{ \"$_\" }}\nexit $LASTEXITCODE\n",
        ps_quote(target)
    );
    Ok(ActionPlan {
        argv: powershell_argv(ps, &script(scoop, &body)),
        elevate: false,
        success_codes: vec![0],
        reboot_codes: Vec::new(),
    })
}

/// Сведения о пакете (`pkg.info`): `scoop info <app>`.
pub fn info_argv(ps: &Path, scoop: &Path, id: &PkgId) -> Result<Vec<OsString>, PkgError> {
    let checked = id.checked(Mgr::Scoop)?;
    let body = format!(
        "& $scoop info {} *>&1 | ForEach-Object {{ \"$_\" }}\nexit $LASTEXITCODE\n",
        ps_quote(checked)
    );
    Ok(powershell_argv(ps, &script(scoop, &body)))
}

#[cfg(windows)]
pub use adapter::ScoopAdapter;

#[cfg(windows)]
mod adapter {
    use std::path::PathBuf;

    use compact_str::CompactString;

    use super::{
        action_plan, export_script, info_argv, parse_export, parse_search, parse_status,
        parse_version, powershell_argv, search_script, section, sections, status_script,
        version_script,
    };
    use crate::api::{
        ActionPlan, Detected, Hit, Mgr, Package, PackageManager, PkgError, PkgId, PkgOp, RunCx,
        Update,
    };
    use crate::config::PkgConfig;
    use crate::detect::{SystemProbe, find_powershell};
    use crate::found::Found;
    use crate::proc::{Output, run_query};

    /// scoop через скрипты PowerShell (F02 §2.3).
    #[derive(Debug)]
    pub struct ScoopAdapter {
        found: Found,
        powershell: PathBuf,
        update_buckets: bool,
    }

    impl ScoopAdapter {
        /// `cached` — из `managers.toml`; `powershell` — из кэша S6
        /// (`term/shells.toml`: pwsh, иначе 5.1), `None` — поиск самим.
        pub fn new(
            config: &PkgConfig,
            cached: Option<Detected>,
            powershell: Option<PathBuf>,
        ) -> ScoopAdapter {
            ScoopAdapter {
                found: Found::new(Mgr::Scoop, cached),
                powershell: powershell.unwrap_or_else(|| find_powershell(&SystemProbe)),
                update_buckets: config.scoop_update_buckets,
            }
        }

        /// Скрипт и его секции.
        fn run(&self, script: &str, cx: &RunCx) -> Result<Vec<(String, String)>, PkgError> {
            let out =
                run_query(&powershell_argv(&self.powershell, script), cx).and_then(Output::ok)?;
            Ok(sections(&out.lines))
        }
    }

    impl PackageManager for ScoopAdapter {
        fn mgr(&self) -> Mgr {
            Mgr::Scoop
        }

        fn detect(&self) -> Option<Detected> {
            self.found.get()
        }

        fn version(&self, cx: &RunCx) -> Result<CompactString, PkgError> {
            self.found.version(|d| {
                let secs = self.run(&version_script(&d.exe), cx)?;
                let text = section(&secs, "version")?;
                let v = parse_version(text).ok_or_else(|| PkgError::Parse {
                    line: text.lines().next().unwrap_or_default().into(),
                })?;
                d.version = Some(v.clone());
                Ok(v)
            })
        }

        fn list_installed(&self, cx: &RunCx) -> Result<Vec<Package>, PkgError> {
            let secs = self.run(&export_script(&self.found.exe()?), cx)?;
            parse_export(section(&secs, "export")?)
        }

        fn list_updates(&self, cx: &RunCx) -> Result<Vec<Update>, PkgError> {
            let script = status_script(&self.found.exe()?, self.update_buckets);
            let secs = self.run(&script, cx)?;
            let installed = parse_export(section(&secs, "export")?)?;
            parse_status(section(&secs, "status")?, &installed)
        }

        fn search(&self, q: &str, cx: &RunCx) -> Result<Vec<Hit>, PkgError> {
            let exe = self.found.exe()?;
            let q = crate::winget::search_query(q);
            if q.is_empty() {
                return Ok(Vec::new());
            }
            let secs = self.run(&search_script(&exe, &q), cx)?;
            parse_search(&q, section(&secs, "search")?)
        }

        fn info_argv(&self, id: &PkgId) -> Result<Vec<std::ffi::OsString>, PkgError> {
            info_argv(&self.powershell, &self.found.exe()?, id)
        }

        fn action(&self, op: PkgOp, id: &PkgId, _cx: &RunCx) -> Result<ActionPlan, PkgError> {
            action_plan(&self.powershell, &self.found.exe()?, op, id)
        }
    }
}

#[cfg(test)]
#[path = "scoop_tests.rs"]
mod tests;
