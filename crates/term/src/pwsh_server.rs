//! Сервер комплита PowerShell (S6-18, S6 §6.2): фоновый `pwsh -NoProfile
//! -NonInteractive -Command -` (или Windows PowerShell — под вид сеанса) отвечает
//! `TabExpansion2` на строку и курсор. Протокол — строки: запрос — вызов
//! `__urfin_tx '<base64 JSON>'`, ответ — одна строка JSON. Запускается лениво при
//! первом Tab в сеансе PowerShell, умирает после [`IDLE`] простоя; опоздавший ответ
//! (дольше `complete.timeout_ms`) отбрасывает вызывающий. CLM или ошибка старта —
//! сервер выключен до перезапуска Урфина. Падение сервера сеанс не трогает: это
//! отдельный процесс без консоли.
//!
//! Сервер видит cmdlet-ы, параметры, модули (autoload), пути и типы, но не переменные
//! и функции интерактивного сеанса.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use crossbeam_channel::{Receiver, Sender, unbounded};
use urfin_core::Runtime;

use crate::complete::{Candidate, Source};

/// Простой, после которого процесс сервера закрывается.
pub const IDLE: Duration = Duration::from_secs(600);

/// Скрипт сервера: одна строка (так её исполняет `-Command -`).
const BOOT: &str = "$ErrorActionPreference='SilentlyContinue'; \
[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
function __urfin_tx($b){ \
$q=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b)) | ConvertFrom-Json; \
try { if ($q.cwd) { Set-Location -LiteralPath $q.cwd }; \
$r=TabExpansion2 -inputScript $q.line -cursorColumn $q.cursor; \
$o=@{id=$q.id; start=$r.ReplacementIndex; len=$r.ReplacementLength; \
items=@($r.CompletionMatches | Select-Object -First 200 | ForEach-Object { \
@{t=$_.CompletionText; l=$_.ListItemText; k=[string]$_.ResultType} })} } \
catch { $o=@{id=$q.id; err=[string]$_} }; \
[Console]::Out.WriteLine(($o | ConvertTo-Json -Compress -Depth 4)); [Console]::Out.Flush() }; \
[Console]::Out.WriteLine('{\"ready\":1}'); [Console]::Out.Flush()";

/// Запрос комплита.
#[derive(Clone, Debug)]
pub struct Req {
    pub id: u64,
    pub cwd: PathBuf,
    pub line: String,
    /// Байт курсора в `line`.
    pub cursor: usize,
}

/// Ответ сервера (уже в байтах строки).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub id: u64,
    pub cands: Vec<Candidate>,
}

/// Живой сервер: канал запросов к потоку-владельцу процесса.
#[derive(Debug)]
pub struct Server {
    tx: Sender<Req>,
    alive: Arc<AtomicBool>,
}

impl Server {
    /// Запустить поток сервера; ответы — в `on_reply` (из потока сервера).
    pub fn start(
        rt: &Runtime,
        program: &Path,
        on_reply: impl Fn(Reply) + Send + 'static,
        on_dead: impl Fn(bool) + Send + 'static,
    ) -> Server {
        let (tx, rx) = unbounded::<Req>();
        let alive = Arc::new(AtomicBool::new(true));
        let flag = alive.clone();
        let program = program.to_path_buf();
        rt.spawn_dedicated("term.pwsh-complete".into(), rt.token(), move |_| {
            let failed_start = run(&program, &rx, &on_reply).is_err();
            flag.store(false, Ordering::Release);
            on_dead(failed_start);
        });
        Server { tx, alive }
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Отправить запрос; `false` — сервер уже закрыт.
    pub fn ask(&self, req: Req) -> bool {
        self.alive() && self.tx.send(req).is_ok()
    }
}

/// Процесс без окна консоли.
fn spawn(program: &Path) -> std::io::Result<Child> {
    let mut cmd = Command::new(program);
    cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

/// Цикл сервера: `Err` — не стартовал (нет pwsh, CLM — нет строки готовности).
fn run(program: &Path, rx: &Receiver<Req>, on_reply: &dyn Fn(Reply)) -> Result<(), ()> {
    let mut child = spawn(program).map_err(|_| ())?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return Err(());
    };
    let (lines_tx, lines) = unbounded::<String>();
    std::thread::Builder::new()
        .name("term.pwsh-complete-out".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| ())?;
    let send = |stdin: &mut ChildStdin, s: &str| -> bool {
        stdin.write_all(s.as_bytes()).is_ok()
            && stdin.write_all(b"\n").is_ok()
            && stdin.flush().is_ok()
    };
    if !send(&mut stdin, BOOT) {
        let _ = child.kill();
        return Err(());
    }
    // Готовность: строка `{"ready":1}`. CLM (нет [Console]) её не даст.
    let ready = lines
        .recv_timeout(Duration::from_secs(15))
        .is_ok_and(|l| l.contains("\"ready\""));
    if !ready {
        let _ = child.kill();
        return Err(());
    }
    // Простой дольше IDLE или канал закрыт — сервер закрывается.
    while let Ok(req) = rx.recv_timeout(IDLE) {
        let wire = request_line(&req);
        if !send(&mut stdin, &wire) {
            break;
        }
        // Ответ на этот запрос; чужие (поздние) строки пропускаются.
        let reply = loop {
            match lines.recv_timeout(Duration::from_secs(30)) {
                Ok(l) => {
                    if let Some(r) = parse_reply(&l, &req) {
                        break Some(r);
                    }
                }
                Err(_) => break None,
            }
        };
        match reply {
            Some(r) => on_reply(r),
            None => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Строка запроса: курсор — в UTF-16 (индекс строки .NET).
pub fn request_line(req: &Req) -> String {
    let cursor16 = req.line[..req.cursor.min(req.line.len())]
        .encode_utf16()
        .count();
    let json = format!(
        "{{\"id\":{},\"cwd\":{},\"line\":{},\"cursor\":{}}}",
        req.id,
        json_str(&req.cwd.display().to_string()),
        json_str(&req.line),
        cursor16
    );
    format!(
        "__urfin_tx '{}'",
        base64::engine::general_purpose::STANDARD.encode(json.as_bytes())
    )
}

/// Позиция UTF-16 в строке → байт (за концом — конец строки).
fn byte_at(line: &str, units: usize) -> usize {
    let mut n = 0;
    for (i, c) in line.char_indices() {
        if n >= units {
            return i;
        }
        n += c.len_utf16();
    }
    line.len()
}

/// Ответ сервера → варианты; `None` — строка не этот ответ.
pub fn parse_reply(line: &str, req: &Req) -> Option<Reply> {
    let v: toml::Value = json_lite(line)?;
    let id = v.get("id")?.as_integer()?;
    if u64::try_from(id).ok()? != req.id {
        return None;
    }
    let start = usize::try_from(
        v.get("start")
            .and_then(toml::Value::as_integer)
            .unwrap_or(0),
    )
    .ok()?;
    let len = usize::try_from(v.get("len").and_then(toml::Value::as_integer).unwrap_or(0)).ok()?;
    let (from, to) = (byte_at(&req.line, start), byte_at(&req.line, start + len));
    let items = v
        .get("items")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cands = items
        .iter()
        .filter_map(|it| {
            let t = it.get("t")?.as_str()?.to_owned();
            let l = it
                .get("l")
                .and_then(toml::Value::as_str)
                .unwrap_or(&t)
                .to_owned();
            let k = it.get("k").and_then(toml::Value::as_str).unwrap_or("");
            let (source, dir) = match k {
                "ProviderContainer" => (Source::Path, true),
                "ProviderItem" => (Source::Path, false),
                _ => (Source::Command, false),
            };
            Some(Candidate {
                label: l,
                insert: t,
                range: from..to,
                source,
                dir,
            })
        })
        .collect();
    Some(Reply { id: req.id, cands })
}

/// Минимальный разбор JSON ответа сервера (объект со строками, числами и массивом
/// объектов) в `toml::Value` — без отдельной зависимости на JSON.
fn json_lite(s: &str) -> Option<toml::Value> {
    let mut p = Json {
        b: s.trim().as_bytes(),
        i: 0,
    };
    let v = p.value()?;
    p.ws();
    (p.i == p.b.len()).then_some(v)
}

struct Json<'a> {
    b: &'a [u8],
    i: usize,
}

impl Json<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn value(&mut self) -> Option<toml::Value> {
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut t = toml::map::Map::new();
                if self.eat(b'}').is_some() {
                    return Some(toml::Value::Table(t));
                }
                loop {
                    let k = self.string()?;
                    self.eat(b':')?;
                    if let Some(v) = self.value() {
                        t.insert(k, v);
                    }
                    if self.eat(b',').is_none() {
                        self.eat(b'}')?;
                        return Some(toml::Value::Table(t));
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                if self.eat(b']').is_some() {
                    return Some(toml::Value::Array(a));
                }
                loop {
                    if let Some(v) = self.value() {
                        a.push(v);
                    }
                    if self.eat(b',').is_none() {
                        self.eat(b']')?;
                        return Some(toml::Value::Array(a));
                    }
                }
            }
            b'"' => self.string().map(toml::Value::String),
            b'n' => {
                self.i += 4;
                None
            }
            b't' => {
                self.i += 4;
                Some(toml::Value::Boolean(true))
            }
            b'f' => {
                self.i += 5;
                Some(toml::Value::Boolean(false))
            }
            _ => {
                let st = self.i;
                while self.b.get(self.i).is_some_and(|c| {
                    c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E')
                }) {
                    self.i += 1;
                }
                let n = std::str::from_utf8(&self.b[st..self.i]).ok()?;
                n.parse::<i64>()
                    .map(toml::Value::Integer)
                    .ok()
                    .or_else(|| n.parse::<f64>().ok().map(toml::Value::Float))
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' | b'f' => {}
                        b'u' => {
                            let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            let mut cp = u32::from_str_radix(h, 16).ok()?;
                            // Суррогатная пара.
                            if (0xD800..0xDC00).contains(&cp)
                                && self.b.get(self.i..self.i + 2) == Some(b"\\u")
                            {
                                let lo = std::str::from_utf8(self.b.get(self.i + 2..self.i + 6)?)
                                    .ok()?;
                                let lo = u32::from_str_radix(lo, 16).ok()?;
                                self.i += 6;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        other => out.push(char::from(other)),
                    }
                }
                _ => {
                    // UTF-8 — байтами до следующего ASCII.
                    let st = self.i - 1;
                    while self.b.get(self.i).is_some_and(|c| *c >= 0x80) {
                        self.i += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.b[st..self.i]).ok()?);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(line: &str) -> Req {
        Req {
            id: 7,
            cwd: PathBuf::from(r"C:\x"),
            line: line.to_owned(),
            cursor: line.len(),
        }
    }

    #[test]
    fn wire_format() {
        let r = req("ls Мои\\д");
        let w = request_line(&r);
        let b64 = w
            .strip_prefix("__urfin_tx '")
            .unwrap()
            .strip_suffix('\'')
            .unwrap();
        let json = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
        .unwrap();
        assert!(json.contains("\"cursor\":8"), "{json}");
        assert!(json.contains("\"cwd\":\"C:\\\\x\""), "{json}");
    }

    #[test]
    fn replies_map_to_byte_ranges() {
        let r = req("Get-Ch Мои");
        let line = r#"{"items":[{"t":"Get-ChildItem","l":"Get-ChildItem","k":"Command"},{"t":"'C:\\x\\Мои документы'","l":"Мои документы","k":"ProviderContainer"}],"len":6,"start":0,"id":7}"#;
        let rep = parse_reply(line, &r).unwrap();
        assert_eq!(rep.cands.len(), 2);
        assert_eq!(rep.cands[0].range, 0..6);
        assert_eq!(rep.cands[1].insert, "'C:\\x\\Мои документы'");
        assert!(rep.cands[1].dir);
        assert!(parse_reply(r#"{"id":8,"items":[]}"#, &r).is_none());
        assert!(parse_reply("PS>", &r).is_none());
        // Кириллица до курсора: UTF-16 → байты.
        let r2 = req("cd Мо");
        let rep2 = parse_reply(
            r#"{"id":7,"start":3,"len":2,"items":[{"t":"Мои","k":"ProviderContainer"}]}"#,
            &r2,
        )
        .unwrap();
        assert_eq!(&r2.line[rep2.cands[0].range.clone()], "Мо");
    }
}
