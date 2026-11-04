//! Расписание проверки обновлений (F02 §4): задача Планировщика заданий Windows
//! `\Urfin\PkgCheck`, ежедневно `urfin.exe --pkg-check`. Задачу пишет XML и `schtasks`
//! от текущего пользователя, без прав администратора и без COM (`unsafe` не нужен).
//! Сам Урфин в сеть не ходит: сеть трогают менеджеры, и только по включённому
//! пользователем расписанию; на батарее и без сети задача не стартует.
//!
//! Сверка пути: `schtasks /Query /XML` и сравнение `<Command>` с текущим `urfin.exe` —
//! portable-копию могли перенести (раздел настроек «Пакеты», F02-12).

use std::path::{Path, PathBuf};

use compact_str::CompactString;
pub use urfin_core::pkg::Task;

use crate::api::PkgError;

/// Имя задачи в Планировщике: папка `Urfin`.
pub const TASK_NAME: &str = r"\Urfin\PkgCheck";
/// Флаг безоконной проверки.
pub const CHECK_FLAG: &str = "--pkg-check";
/// Временный XML задачи в `state_dir/pkg` (удаляется после `schtasks /Create`).
const XML_FILE: &str = "PkgCheck.xml";

/// Время `ЧЧ:ММ` → часы и минуты.
pub fn parse_at(at: &str) -> Option<(u8, u8)> {
    let (h, m) = at.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u8 = h.parse().ok()?;
    let m: u8 = m.parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// XML задачи (схема Task Scheduler 1.2, F02 §4): ежедневно в `at` (местное время),
/// пропущенный запуск — при первой возможности (`StartWhenAvailable`), не на батарее и
/// только при сети, не дольше 30 мин, второй экземпляр не запускается. `user` —
/// `ДОМЕН\имя` текущего пользователя (`None` — тот, кто регистрирует задачу).
pub fn task_xml(exe: &Path, at: &str, user: Option<&str>) -> Option<String> {
    let (h, m) = parse_at(at)?;
    let user = user
        .map(|u| format!("      <UserId>{}</UserId>\n", escape(u)))
        .unwrap_or_default();
    Some(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>Urfin</Author>
    <Description>Urfin: check for package updates (winget, scoop, choco)</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>2026-01-01T{h:02}:{m:02}:00</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay>
        <DaysInterval>1</DaysInterval>
      </ScheduleByDay>
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
{user}      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>true</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>true</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT30M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>{CHECK_FLAG}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        exe = escape(&exe.display().to_string()),
    ))
}

/// Путь программы задачи из её XML (`<Command>`, без кавычек).
pub fn command_of(xml: &str) -> Option<String> {
    let start = xml.find("<Command>")? + "<Command>".len();
    let end = start + xml[start..].find("</Command>")?;
    let cmd = unescape(xml[start..end].trim());
    Some(cmd.trim_matches('"').to_owned())
}

/// Тот же файл: без учёта регистра и вида разделителей.
pub fn same_exe(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.replace('/', "\\").to_lowercase();
    norm(a) == norm(&b.display().to_string())
}

/// Состояние задачи по ответу `schtasks /Query /XML` (`None` — задачи нет).
pub fn status(xml: Option<&str>, exe: &Path) -> Task {
    let Some(xml) = xml else {
        return Task::Absent;
    };
    match command_of(xml) {
        Some(cmd) if same_exe(&cmd, exe) => Task::Ready,
        Some(cmd) => Task::OtherPath(cmd.into()),
        None => Task::Failed("no <Command> in task XML".into()),
    }
}

/// Планировщик: `schtasks` или подстановка в тестах.
pub trait Tasks: Send + Sync + 'static {
    /// Создать или заменить задачу по XML.
    fn create(&self, xml: &str) -> Result<(), PkgError>;
    /// Удалить задачу; её нет — не ошибка.
    fn delete(&self) -> Result<(), PkgError>;
    /// XML задачи; `None` — задачи нет.
    fn query(&self) -> Result<Option<String>, PkgError>;
}

/// Без Планировщика: `--smoke`, `--screens` (без каталога состояния).
#[derive(Debug, Default)]
pub struct NoTasks;

impl Tasks for NoTasks {
    fn create(&self, _xml: &str) -> Result<(), PkgError> {
        Err(PkgError::NotFound)
    }
    fn delete(&self) -> Result<(), PkgError> {
        Err(PkgError::NotFound)
    }
    fn query(&self) -> Result<Option<String>, PkgError> {
        Err(PkgError::NotFound)
    }
}

/// `ДОМЕН\имя` текущего пользователя из окружения сеанса.
pub fn current_user() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|s| !s.trim().is_empty())?;
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.trim().is_empty() => Some(format!("{d}\\{user}")),
        _ => Some(user),
    }
}

/// Задача Планировщика через `%SystemRoot%\System32\schtasks.exe`: пайпы,
/// `CREATE_NO_WINDOW`, таймаут; XML задачи — во временном файле `dir`.
#[cfg(windows)]
#[derive(Debug)]
pub struct Schtasks {
    argv: Vec<std::ffi::OsString>,
    /// Добавлять аргументы `schtasks` (в тестах — подставной процесс с фикстурой).
    append: bool,
    dir: PathBuf,
}

#[cfg(windows)]
impl Schtasks {
    /// Запуск `schtasks` одной команды не дольше.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    pub fn new(dir: PathBuf) -> Schtasks {
        let root = std::env::var_os("SystemRoot").map_or_else(
            || PathBuf::from(r"C:\Windows"),
            PathBuf::from,
        );
        Schtasks {
            argv: vec![root.join(r"System32\schtasks.exe").into_os_string()],
            append: true,
            dir,
        }
    }

    /// Подставной процесс вместо `schtasks` (свои аргументы, без аргументов `schtasks`).
    #[cfg(test)]
    pub(crate) fn fake(argv: Vec<std::ffi::OsString>, dir: PathBuf) -> Schtasks {
        Schtasks {
            argv,
            append: false,
            dir,
        }
    }

    fn run(&self, args: &[&std::ffi::OsStr]) -> Result<crate::proc::Output, PkgError> {
        let mut cmd = self.argv.clone();
        if self.append {
            cmd.extend(args.iter().map(|a| a.to_os_string()));
        }
        let cx = crate::api::RunCx {
            cancel: urfin_core::CancelToken::new(),
            timeout: Some(Self::TIMEOUT),
            lines: None,
        };
        let out = crate::proc::run_query(&cmd, &cx)?;
        tracing::debug!(target: "pkg", code = out.code, ?args, "pkg: schtasks");
        Ok(out)
    }
}

/// XML-файл для `schtasks /XML`: UTF-16LE с BOM (как экспорт Планировщика).
fn utf16(xml: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Вывод `schtasks /Query /XML` построчно → текст; UTF-16 (нули между байтами, BOM)
/// сводится к тексту.
fn joined(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.retain(|c| c != '\0' && c != '\u{FEFF}');
    s
}

#[cfg(windows)]
impl Tasks for Schtasks {
    fn create(&self, xml: &str) -> Result<(), PkgError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| PkgError::Spawn(e.to_string().into()))?;
        let file = self.dir.join(XML_FILE);
        std::fs::write(&file, utf16(xml)).map_err(|e| PkgError::Spawn(e.to_string().into()))?;
        let out = self.run(&[
            "/Create".as_ref(),
            "/TN".as_ref(),
            TASK_NAME.as_ref(),
            "/XML".as_ref(),
            file.as_os_str(),
            "/F".as_ref(),
        ]);
        let _ = std::fs::remove_file(&file);
        out?.ok().map(drop)
    }

    fn delete(&self) -> Result<(), PkgError> {
        let out = self.run(&[
            "/Delete".as_ref(),
            "/TN".as_ref(),
            TASK_NAME.as_ref(),
            "/F".as_ref(),
        ])?;
        if out.code == 0 || self.query()?.is_none() {
            return Ok(());
        }
        out.ok().map(drop)
    }

    fn query(&self) -> Result<Option<String>, PkgError> {
        let out = self.run(&[
            "/Query".as_ref(),
            "/XML".as_ref(),
            "/TN".as_ref(),
            TASK_NAME.as_ref(),
        ])?;
        // Нет задачи — «ERROR: The system cannot find the file specified.», код 1.
        Ok((out.code == 0).then(|| joined(&out.lines)))
    }
}

/// Ошибка Планировщика для тоста: хвост вывода `schtasks` или причина.
pub fn reason(e: &PkgError) -> CompactString {
    match e {
        PkgError::Exit { tail, .. } if !tail.is_empty() => tail.clone(),
        e => e.to_string().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_parses() {
        assert_eq!(parse_at("12:00"), Some((12, 0)));
        assert_eq!(parse_at("9:05"), Some((9, 5)));
        assert_eq!(parse_at(" 23:59 "), Some((23, 59)));
        for bad in ["24:00", "12:60", "12", "12:5", ":30", "aa:bb", "123:00"] {
            assert_eq!(parse_at(bad), None, "{bad}");
        }
    }

    /// XML задачи — снапшот: поля §4 видны в ревью; путь с `&` экранирован.
    #[test]
    fn xml_snapshot() {
        let xml = task_xml(
            Path::new(r"D:\Tools\R&D\urfin.exe"),
            "09:30",
            Some(r"WS01\anton"),
        )
        .unwrap();
        insta::assert_snapshot!(xml);
        assert!(task_xml(Path::new("urfin.exe"), "25:00", None).is_none());
        let bare = task_xml(Path::new("urfin.exe"), "09:30", None).unwrap();
        assert!(!bare.contains("<UserId>"));
    }

    #[test]
    fn command_roundtrip() {
        let exe = Path::new(r"D:\Tools\R&D\urfin.exe");
        let xml = task_xml(exe, "12:00", None).unwrap();
        assert_eq!(command_of(&xml).as_deref(), Some(r"D:\Tools\R&D\urfin.exe"));
        assert_eq!(status(Some(&xml), exe), Task::Ready);
        assert_eq!(
            status(Some(&xml), Path::new(r"d:/tools/r&d/URFIN.EXE")),
            Task::Ready
        );
        assert_eq!(
            status(Some(&xml), Path::new(r"E:\urfin\urfin.exe")),
            Task::OtherPath(r"D:\Tools\R&D\urfin.exe".into())
        );
        assert_eq!(status(None, exe), Task::Absent);
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/schtasks")
                .join(name),
        )
        .unwrap()
    }

    /// Строки вывода как у `schtasks` в пайп: однобайтовый текст, концы `\r\r\n`.
    fn piped(text: &str) -> Vec<String> {
        let raw = text.replace('\n', "\r\r\n");
        let d = crate::decode::Decoder::new(866);
        let mut sp = crate::decode::LineSplitter::default();
        let mut out = Vec::new();
        sp.push(raw.as_bytes(), &mut |l| out.push(d.line(l)));
        sp.finish(&mut |l| out.push(d.line(l)));
        out
    }

    /// Записанный вывод `schtasks /Query /XML` (Windows 11, системная задача
    /// `\Microsoft\Windows\Defrag\ScheduledDefrag`): путь без кавычек.
    #[test]
    fn recorded_query() {
        let xml = joined(&piped(&fixture("query-defrag.xml")));
        assert!(xml.starts_with("<?xml"), "{xml}");
        assert_eq!(
            command_of(&xml).as_deref(),
            Some(r"%windir%\system32\defrag.exe")
        );
    }

    /// Задача перенесённой portable-копии (в формате вывода `schtasks`): путь — другой.
    #[test]
    fn moved_copy() {
        let lines = piped(&fixture("query.xml"));
        let xml = joined(&lines);
        assert_eq!(
            status(Some(&xml), Path::new(r"C:\Tools\Urfin\urfin.exe")),
            Task::OtherPath(r"C:\Users\anton\Downloads\urfin-0.4.2\urfin.exe".into())
        );
        // UTF-16 без перекодировки: нули между байтами снимаются.
        let wide: Vec<String> = lines
            .iter()
            .map(|l| l.chars().flat_map(|c| [c, '\0']).collect())
            .collect();
        assert_eq!(command_of(&joined(&wide)), command_of(&xml));
    }

    #[test]
    fn utf16_has_bom() {
        let b = utf16("<a/>");
        assert_eq!(&b[..4], &[0xFF, 0xFE, b'<', 0]);
    }

    /// Процесс вместо `schtasks`: подставной менеджер повторяет записанный вывод.
    #[cfg(windows)]
    #[test]
    fn query_through_process() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/schtasks/query.xml");
        let t = Schtasks::fake(
            vec![
                crate::testutil::fake().into_os_string(),
                "--fixture".into(),
                fixture.into_os_string(),
            ],
            dir.path().to_path_buf(),
        );
        let xml = t.query().unwrap().unwrap();
        assert_eq!(
            command_of(&xml).as_deref(),
            Some(r"C:\Users\anton\Downloads\urfin-0.4.2\urfin.exe")
        );
        // Код 1 — задачи нет.
        let none = Schtasks::fake(
            vec![
                crate::testutil::fake().into_os_string(),
                "--line".into(),
                "ERROR: The system cannot find the file specified.".into(),
                "--exit".into(),
                "1".into(),
            ],
            dir.path().to_path_buf(),
        );
        assert_eq!(none.query().unwrap(), None);
        // Создание: XML во временном файле, после — удалён.
        let ok = Schtasks::fake(
            vec![
                crate::testutil::fake().into_os_string(),
                "--line".into(),
                "SUCCESS".into(),
            ],
            dir.path().to_path_buf(),
        );
        ok.create("<Task/>").unwrap();
        assert!(!dir.path().join(XML_FILE).exists());
    }
}
