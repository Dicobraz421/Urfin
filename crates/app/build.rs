//! Ресурсы exe на Windows (R-01): VERSIONINFO с именем продукта и издателем
//! (S0-decisions №45, №49). Версии файла и продукта winresource берёт из Cargo.
//! Иконка приложения — ресурс с id 1 (`set_icon`): её показывают Проводник,
//! ярлыки и панель задач; GPUI берёт её же для класса окна.
//!
//! Манифест (S9-17) здесь не встраивается: ресурс манифеста #1 кладёт gpui (feature
//! `windows-manifest`, без форка не выключается), второй ресурс #1 линкер не принимает
//! (`CVT1100: duplicate resource`), а `/MANIFESTINPUT` он с ресурсами не сливает. Свой
//! `urfin.manifest` (ключи gpui + `longPathAware`, `activeCodePage`) заменяет ресурс после
//! сборки: `mt.exe -manifest crates/app/urfin.manifest -outputresource:urfin.exe;#1`.
//!
//! Сборка (S9-18a): короткий sha коммита и дата — в `URFIN_BUILD_SHA` и `URFIN_BUILD_DATE`
//! для `urfin --version`.

const ICON: &str = "../../assets/icon/urfin.ico";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");
    build_info();
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ICON);
        res.set("ProductName", "Urfin")
            .set("CompanyName", "Aahz")
            .set("FileDescription", "Urfin — files and settings")
            .set("InternalName", "urfin")
            .set("OriginalFilename", "urfin.exe");
        if let Err(e) = res.compile() {
            panic!("exe resources (rc.exe from Windows SDK): {e}");
        }
    }
}

/// `URFIN_BUILD_SHA`: `GITHUB_SHA` (CI) или `git rev-parse --short HEAD`; `URFIN_BUILD_DATE`:
/// `SOURCE_DATE_EPOCH` или дата коммита (UTC, `YYYY-MM-DD`); без git — `unknown`. Скрипт не
/// следит за `.git`: локальная сборка не пересобирается на каждый коммит.
fn build_info() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let sha = std::env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.trim().chars().take(7).collect::<String>())
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or_else(|| git(&["log", "-1", "--format=%ct", "HEAD"]).and_then(|s| s.parse().ok()))
        .map_or_else(|| "unknown".into(), ymd);
    println!("cargo:rustc-env=URFIN_BUILD_SHA={sha}");
    println!("cargo:rustc-env=URFIN_BUILD_DATE={date}");
}

fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").ok()?)
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// Секунды Unix → `YYYY-MM-DD` (UTC; алгоритм `civil_from_days` Г. Хиннанта).
fn ymd(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
