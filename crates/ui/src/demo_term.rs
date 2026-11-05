//! Экраны терминала для сцен `--screens` (S6-06): поток ANSI разбирает тот же эмулятор,
//! что и живой сеанс (`parse_inline`), сетка показывается вместо активного сеанса
//! (`TermState.preview`). `frames` — рамки mc/Far, `unicode` — кириллица, CJK, эмодзи,
//! блоки, рамки и Powerline, `colors` — 16/256 цветов, truecolor и начертания, `termhl` —
//! вывод под подсветку (F03-10);
//! `dense` — для `--bench`: 200×60 плотного цветного текста, каждый вызов — другой.

use std::fmt::Write as _;

use urfin_term::session::{SessionEvent, SessionOpts, SessionSink, parse_inline};
use urfin_term::sys::conpty::PtySize;

/// Размер экрана сцен: во всю область панелей окна 1200×760.
pub(crate) const COLS: usize = 146;
pub(crate) const ROWS: usize = 32;

struct Quiet;

impl SessionSink for Quiet {
    fn poke(&self) {}
    fn event(&self, _ev: SessionEvent) {}
}

/// Сетка экрана `kind` (`seed` меняет текст `dense`); `None` — неизвестный вид.
pub(crate) fn screen(
    kind: &str,
    seed: usize,
) -> Option<std::sync::Arc<urfin_term::vt::shared::TermShared>> {
    let (bytes, cols, rows) = match kind {
        "frames" => (frames(), COLS, ROWS),
        "unicode" => (unicode(), COLS, ROWS),
        "colors" => (colors(), COLS, ROWS),
        "dense" => (dense(seed), 200, 60),
        "termhl" => (termhl(), COLS, ROWS),
        "sshrec" => (sshrec(), COLS, ROWS),
        _ => return None,
    };
    let opts = SessionOpts {
        size: PtySize {
            cols: u16::try_from(cols).unwrap_or(u16::MAX),
            rows: u16::try_from(rows).unwrap_or(u16::MAX),
        },
        scrollback: 100,
        osc52: "off".into(),
        nonce: None,
    };
    let shared = parse_inline(bytes.as_bytes(), &opts, &Quiet);
    shared.mark_dirty();
    Some(shared)
}

/// Перейти в строку `r` (с 1), колонку 1.
fn at(out: &mut String, r: usize) {
    let _ = write!(out, "\x1b[{r};1H");
}

/// Текст ровно в `w` колонок (узкие символы): обрезать или дополнить пробелами.
fn fit(s: &str, w: usize) -> String {
    let mut t: String = s.chars().take(w).collect();
    let n = t.chars().count();
    t.extend(std::iter::repeat_n(' ', w - n));
    t
}

/// Текст по центру `w` колонок, остальное — `fill`.
fn center(s: &str, w: usize, fill: char) -> String {
    let n = s.chars().count().min(w);
    let left = (w - n) / 2;
    let mut t: String = std::iter::repeat_n(fill, left).collect();
    t.extend(s.chars().take(n));
    t.extend(std::iter::repeat_n(fill, w - n - left));
    t
}

const BORDER: &str = "\x1b[0;44;96m";
const HEAD: &str = "\x1b[0;44;93m";
const FILE: &str = "\x1b[0;44;96m";
const DIR: &str = "\x1b[0;44;97;1m";
const EXE: &str = "\x1b[0;44;92;1m";
const CURSOR: &str = "\x1b[0;46;30m";

/// Панель в стиле mc/Far: 28 строк по `w` колонок.
fn panel(
    title: &str,
    files: &[(&str, &str, &str)],
    cursor: Option<usize>,
    w: usize,
) -> Vec<String> {
    let (nw, sw) = (w - 2 - 1 - 11 - 1 - 19, 11);
    let dw = 19;
    let mut rows = Vec::with_capacity(28);
    rows.push(format!(
        "{BORDER}╔{}╤{}╤{}╗",
        center(&format!(" {title} "), nw, '═'),
        "═".repeat(sw),
        "═".repeat(dw)
    ));
    rows.push(format!(
        "{BORDER}║{HEAD}{}{BORDER}│{HEAD}{}{BORDER}│{HEAD}{}{BORDER}║",
        center("Имя", nw, ' '),
        center("Размер", sw, ' '),
        center("Изменён", dw, ' ')
    ));
    for i in 0..22 {
        let (name, size, date) = files.get(i).copied().unwrap_or(("", "", ""));
        let style = if cursor == Some(i) {
            CURSOR
        } else if name.starts_with('/') || name == ".." {
            DIR
        } else if std::path::Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        {
            EXE
        } else {
            FILE
        };
        rows.push(format!(
            "{BORDER}║{style}{}{BORDER}│{style}{:>sw$}{BORDER}│{style}{}{BORDER}║",
            fit(name, nw),
            size,
            fit(date, dw)
        ));
    }
    rows.push(format!(
        "{BORDER}╟{}┴{}┴{}╢",
        "─".repeat(nw),
        "─".repeat(sw),
        "─".repeat(dw)
    ));
    let info = cursor.and_then(|c| files.get(c)).map_or("", |f| f.0);
    rows.push(format!("{BORDER}║{FILE}{}{BORDER}║", fit(info, w - 2)));
    rows.push(format!(
        "{BORDER}║{FILE}{}{BORDER}║",
        center("Свободно 412 ГБ из 953 ГБ (43 %)", w - 2, ' ')
    ));
    rows.push(format!(
        "{BORDER}╚{}╝",
        center(" 14 файлов, 1 428 МБ ", w - 2, '═')
    ));
    rows
}

/// Рамки mc/Far: меню, две панели двойной рамкой со стыками ╤ ┴ ╟ ╢, строка команды и
/// keybar.
fn frames() -> String {
    let left: &[(&str, &str, &str)] = &[
        ("..", "<ВВЕРХ>", "22.10.2026 09:14"),
        ("/Документы", "<ПАПКА>", "21.10.2026 18:02"),
        ("/Проекты", "<ПАПКА>", "20.10.2026 11:47"),
        ("/Фото 2026", "<ПАПКА>", "18.10.2026 08:30"),
        ("/src", "<ПАПКА>", "22.10.2026 10:05"),
        ("Cargo.lock", "184 233", "22.10.2026 10:05"),
        ("Cargo.toml", "3 412", "21.10.2026 16:40"),
        ("README.md", "7 905", "19.10.2026 13:22"),
        ("отчёт_за_3_квартал.xlsx", "1 288 490", "15.10.2026 17:55"),
        ("презентация.pptx", "24 117 802", "14.10.2026 12:10"),
        ("setup.exe", "58 720 256", "10.10.2026 09:00"),
        ("urfin.exe", "25 690 112", "22.10.2026 10:06"),
        ("заметки.txt", "1 024", "09.10.2026 21:31"),
        ("archive.7z", "734 003 200", "01.10.2026 07:45"),
    ];
    let right: &[(&str, &str, &str)] = &[
        ("..", "<ВВЕРХ>", "22.10.2026 09:14"),
        ("/bin", "<ПАПКА>", "22.10.2026 10:06"),
        ("/config", "<ПАПКА>", "20.10.2026 15:12"),
        ("/logs", "<ПАПКА>", "22.10.2026 10:07"),
        ("build.log", "48 211", "22.10.2026 10:07"),
        ("deploy.ps1", "2 307", "17.10.2026 11:11"),
        ("far.exe", "6 553 600", "05.10.2026 10:00"),
        ("mc.ini", "4 096", "05.10.2026 10:01"),
    ];
    let half = COLS / 2;
    let l = panel("C:\\Users\\demo", left, Some(8), half);
    let r = panel("D:\\tools", right, None, COLS - half);
    let mut out = String::from("\x1b[2J");
    at(&mut out, 1);
    let menu = "  Левая     Файл     Команда     Настройки     Правая";
    let _ = write!(out, "\x1b[0;46;30m{}", fit(menu, COLS));
    for (i, (a, b)) in l.iter().zip(&r).enumerate() {
        at(&mut out, i + 2);
        out.push_str(a);
        out.push_str(b);
    }
    at(&mut out, 30);
    let _ = write!(out, "\x1b[0m{}", fit("C:\\Users\\demo>", COLS));
    at(&mut out, 31);
    let keys = [
        "Помощь",
        "Меню",
        "Просм",
        "Правка",
        "Копия",
        "Перен",
        "Папка",
        "Удал",
        "МенюП",
        "Выход",
    ];
    let cell = COLS / keys.len();
    for (i, k) in keys.iter().enumerate() {
        let n = i + 1;
        let w = if n == keys.len() {
            COLS - cell * (keys.len() - 1)
        } else {
            cell
        };
        let num = n.to_string();
        let _ = write!(
            out,
            "\x1b[0;97;40m{num}\x1b[0;46;30m{}",
            fit(k, w - num.len())
        );
    }
    out.push_str("\x1b[0m\x1b[30;16H");
    out
}

/// Юникод: кириллица, CJK и эмодзи (две ячейки, сетка не плывёт), блоки, рамки,
/// Powerline-приглашение; курсор — черта.
fn unicode() -> String {
    let mut out = String::from("\x1b[2J\x1b[H");
    let ps = "\x1b[0;97;44m demo \x1b[0;34;42m\u{E0B0}\x1b[0;30;42m ~\\Projects \x1b[0;32m\u{E0B0}\x1b[0m ";
    let lines = [
        format!("{ps}Get-Content languages.txt"),
        "Русский     Съешь же ещё этих мягких французских булок, да выпей чаю.".into(),
        "Українська  Чуєш їх, доцю, га? Кумедна ж ти, прощайся без ґольфів!".into(),
        "Ελληνικά    Ξεσκεπάζω την ψυχοφθόρα βδελυγμία.".into(),
        "中文        我能吞下玻璃而不伤身体。".into(),
        "日本語      私はガラスを食べられます。それは私を傷つけません。".into(),
        "한국어      나는 유리를 먹을 수 있어요. 그래도 아프지 않아요.".into(),
        "Эмодзи      👍 🚀 🎉 ✅ 📁 🔥 ⭐".into(),
        String::new(),
        "Сетка       |漢字|abcd|Юник|".into(),
        "            |abcd|漢字|abcd|".into(),
        "            |Юник|abcd|漢字|".into(),
        String::new(),
        "Блоки       ▁▂▃▄▅▆▇█  ░▒▓  ▀▄▌▐".into(),
        String::new(),
        "Рамки       ┌──┬──┐  ╔══╦══╗  ┏━━┳━━┓  ╒══╤══╕  ╭──╮".into(),
        "            │  │  │  ║  ║  ║  ┃  ┃  ┃  │  │  │  │  │".into(),
        "            ├──┼──┤  ╠══╬══╣  ┣━━╋━━┫  ╞══╪══╡  │  │".into(),
        "            │  │  │  ║  ║  ║  ┃  ┃  ┃  │  │  │  │  │".into(),
        "            └──┴──┘  ╚══╩══╝  ┗━━┻━━┛  ╘══╧══╛  ╰──╯".into(),
        String::new(),
        "Powerline   \x1b[0;30;43m main \x1b[0;33;46m\u{E0B0}\x1b[0;30;46m ✓ 3 \x1b[0;36;45m\u{E0B0}\x1b[0;97;45m 12:04 \x1b[0;35m\u{E0B0}\x1b[0m  \x1b[0;35m\u{E0B2}\x1b[0;97;45m rust 1.91 \x1b[0m".into(),
        String::new(),
        ps.to_owned(),
    ];
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(l);
    }
    out.push_str("\x1b[5 q");
    out
}

/// Цвета: 16 ANSI, куб 6×6×6 и серые палитры 256, градиенты truecolor, начертания.
#[allow(
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "каналы цвета градиента"
)]
fn colors() -> String {
    let mut out = String::from("\x1b[2J\x1b[H");
    out.push_str("\x1b[0;1m16 цветов\x1b[0m\r\n");
    for i in 0..16u8 {
        let fg = if i == 0 || i == 8 { 97 } else { 30 };
        let _ = write!(out, "\x1b[{fg};48;5;{i}m {i:>2}  ");
    }
    out.push_str("\x1b[0m\r\n");
    for (i, n) in [
        30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96, 97,
    ]
    .iter()
    .enumerate()
    {
        let _ = write!(out, "\x1b[{n}m Aa{i:<2} ");
    }
    out.push_str("\x1b[0m\r\n\r\n\x1b[1m256 цветов\x1b[0m\r\n");
    for g in 0..6u8 {
        for r in 0..6u8 {
            for b in 0..6u8 {
                let c = 16 + 36 * r + 6 * g + b;
                let _ = write!(out, "\x1b[48;5;{c}m   ");
            }
            out.push_str("\x1b[0m ");
        }
        out.push_str("\x1b[0m\r\n");
    }
    for c in 232..=255u8 {
        let _ = write!(out, "\x1b[48;5;{c}m      ");
    }
    out.push_str("\x1b[0m\r\n\r\n\x1b[1mTruecolor\x1b[0m\r\n");
    let w = COLS - 2;
    for row in 0..3 {
        for x in 0..w {
            #[allow(clippy::cast_precision_loss, reason = "градиент")]
            let t = x as f32 / (w - 1) as f32;
            let [r, g, b] = match row {
                0 => hue(t),
                1 => {
                    let v = (t * 255.0) as u8;
                    [v, v, v]
                }
                _ => [
                    (255.0 * (1.0 - t)) as u8,
                    (120.0 * t) as u8,
                    (255.0 * t) as u8,
                ],
            };
            let _ = write!(out, "\x1b[48;2;{r};{g};{b}m ");
        }
        out.push_str("\x1b[0m\r\n");
    }
    out.push_str("\x1b[0m\r\n\x1b[1mНачертания\x1b[0m\r\n");
    out.push_str(
        "\x1b[1mжирный\x1b[0m  \x1b[2mтусклый\x1b[0m  \x1b[3mкурсив\x1b[0m  \x1b[4mподчёркнутый\x1b[0m  \
         \x1b[9mзачёркнутый\x1b[0m  \x1b[7mинверсия\x1b[0m  \x1b[1;3;4;31mвсё сразу\x1b[0m  \
         \x1b[38;2;255;140;0mоранжевый 24 бит\x1b[0m  \x1b[38;5;45mголубой 256\x1b[0m\r\n",
    );
    out.push_str("\x1b[4 q");
    out
}

/// Плотный экран 200×60 (худший случай рендера): слова по 3–7 символов, у каждого свой
/// цвет из 256, каждое пятое — жирное.
fn dense(seed: usize) -> String {
    const WORDS: [&str; 8] = [
        "cargo",
        "файл",
        "build",
        "журнал",
        "src",
        "проект",
        "target",
        "release",
    ];
    let mut out = String::from("\x1b[2J\x1b[H");
    for r in 0..60 {
        let _ = write!(out, "\x1b[{};1H", r + 1);
        let mut col = 0;
        let mut i = seed.wrapping_mul(7).wrapping_add(r * 13);
        while col < 200 {
            let w = WORDS[i % WORDS.len()];
            let n = w.chars().count().min(200 - col);
            let bold = if i.is_multiple_of(5) { "1;" } else { "" };
            let _ = write!(out, "\x1b[0;{bold}38;5;{}m", 16 + i % 216);
            out.extend(w.chars().take(n));
            col += n;
            if col < 200 {
                out.push(' ');
                col += 1;
            }
            i = i.wrapping_add(3);
        }
    }
    out.push_str("\x1b[0m");
    out
}

/// Цвет радуги по `t` ∈ [0, 1] (HSV, S = V = 1).
#[allow(
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "каналы цвета"
)]
fn hue(t: f32) -> [u8; 3] {
    let h = t * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u8 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let q = |v: f32| (v * 255.0).round() as u8;
    [q(r), q(g), q(b)]
}

/// Подсветка вывода (`screen-termhl`, F03-10): pwsh, `cargo test` и `ping` — как в
/// макете. Цвета cargo и промпта — программы (SGR); остальное красит подсветка Урфина.
fn termhl() -> String {
    const N: &str = "\x1b[0m";
    let prompt = |cmd: &str| format!("\x1b[90mPS \x1b[34m{}\x1b[90m> {N}{cmd}", r"C:\Dev\urfin");
    let cargo = |verb: &str, rest: &str| format!("\x1b[1;32m{verb:>12}{N} {rest}");
    let ok = "\x1b[32mok\x1b[0m";
    let failed = "\x1b[31mFAILED\x1b[0m";
    let ip = "10.20.4.30";
    let lines = [
        prompt("cargo test -p urfin-ops"),
        cargo("Compiling", r"urfin-ops v0.4.2 (C:\Dev\urfin\crates\ops)"),
        cargo(
            "Finished",
            "`test` profile [unoptimized + debuginfo] target(s) in 4.12s",
        ),
        cargo(
            "Running",
            r"unittests src\lib.rs (target\debug\deps\urfin_ops-3f2a9c1d7b5e8a40.exe)",
        ),
        String::new(),
        "running 4 tests".to_owned(),
        format!("test copy::tests::cancel_before_start ... {ok}"),
        format!("test copy::tests::cancel_mid_file_removes_partial ... {failed}"),
        format!("test job::tests::cancel_token_shared ... {ok}"),
        format!("test delete::tests::cancel_keeps_rest ... {ok}"),
        String::new(),
        "---- copy::tests::cancel_mid_file_removes_partial stdout ----".to_owned(),
        r"2026-10-23T11:02:12.418Z WARN urfin_ops::copy: partial left: C:\Users\a.antropov\AppData\Local\Temp\.urfin-part-7f3a".to_owned(),
        r"thread 'copy::tests::cancel_mid_file_removes_partial' panicked at crates\ops\src\copy.rs:212:9:".to_owned(),
        "assertion failed: !partial.exists()".to_owned(),
        String::new(),
        format!("test result: {failed}. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s"),
        String::new(),
        format!("\x1b[1;31merror{N}\x1b[1m: test failed, to rerun pass `-p urfin-ops --lib`{N}"),
        prompt(&format!("ping -n 4 {ip}")),
        String::new(),
        format!("Обмен пакетами с {ip} по с 32 байтами данных:"),
        format!("Ответ от {ip}: число байт=32 время=2мс TTL=63"),
        format!("Ответ от {ip}: число байт=32 время=3мс TTL=63"),
        "Превышен интервал ожидания для запроса.".to_owned(),
        format!("Ответ от {ip}: число байт=32 время=2мс TTL=63"),
        String::new(),
        format!("Статистика Ping для {ip}:"),
        "    Пакетов: отправлено = 4, получено = 3, потеряно = 1".to_owned(),
        "    (25% потерь)".to_owned(),
        prompt(""),
    ];
    lines.join("\r\n")
}

/// Файл записи сцены `screen-sshrecview` (F03-13): 132×30, 12:47, 1,2 МБ, шесть
/// совпадений `failed` — метки шкалы как на макете.
pub(crate) fn cast_file() -> String {
    const N: &str = "\x1b[0m";
    /// Начало: 23.10.26 14:02 UTC+3.
    const START: u64 = 1_792_753_320;
    const SIZE: usize = 1_258_291;
    let prompt = |cmd: &str| format!("\x1b[1;32madmin@srv1{N}:\x1b[1;34m/var/www/app{N}$ {cmd}");
    let green = |s: &str| format!("\x1b[32m{s}{N}");
    let plus = |n: usize| green(&"+".repeat(n));
    let journal = |src: &str, rest: &str| format!("Oct 23 14:05:58 srv1 {src}: {rest}");
    let sessions = plus(21);
    let tokens = plus(12);
    let events: Vec<(f64, String)> = vec![
        (2.0, prompt("git pull --ff-only")),
        (9.0, "Updating 9b04e7d..a71d04b".to_owned()),
        (9.4, "Fast-forward".to_owned()),
        (9.5, format!(" db/migrations/0007_sessions.sql | 21 {sessions}")),
        (9.6, format!(" db/migrations/0008_tokens.sql   | 12 {tokens}")),
        (9.7, " 2 files changed, 33 insertions(+)".to_owned()),
        (31.0, prompt("git log --oneline -3")),
        (38.0, "\x1b[33ma71d04b\x1b[0m (HEAD -> main, origin/main) http: graceful shutdown".to_owned()),
        (38.1, "\x1b[33m3f9c2e1\x1b[0m config: cache.ttl".to_owned()),
        (38.2, "\x1b[33m9b04e7d\x1b[0m db: sessions table".to_owned()),
        (112.0, prompt("sudo -u app /var/www/app/bin/app migrate --dry-run")),
        (130.0, "2026-10-23T14:05:51Z INFO migrate: 2 pending: 0007_sessions, 0008_tokens".to_owned()),
        (130.1, "2026-10-23T14:05:51Z WARN migrate: 0007_sessions touches existing table \"sessions\"".to_owned()),
        (211.0, prompt("sudo systemctl restart app")),
        (244.0, "Job for app.service failed because the control process exited with error code.".to_owned()),
        (244.1, "See \"systemctl status app.service\" and \"journalctl -xeu app.service\" for details.".to_owned()),
        (246.0, prompt("systemctl status app --no-pager")),
        (252.0, "\x1b[31m×\x1b[0m app.service - Web app".to_owned()),
        (252.0, "     Loaded: loaded (/etc/systemd/system/app.service; enabled; preset: enabled)".to_owned()),
        (252.0, "     Active: failed (Result: exit-code) since Fri 2026-10-23 14:05:58 MSK; 4s ago".to_owned()),
        (252.0, "    Process: 48377 ExecStartPre=/var/www/app/bin/app migrate (code=exited, status=1/FAILURE)".to_owned()),
        (252.0, "   Main PID: 48211 (code=exited, status=0/SUCCESS)".to_owned()),
        (252.0, "        CPU: 412ms".to_owned()),
        (252.0, String::new()),
        (252.0, journal("app[48377]", "ERROR migrate: relation \"sessions\" already exists")),
        (252.0, journal("systemd[1]", "app.service: Control process exited, code=exited, status=1/FAILURE")),
        (252.0, journal("systemd[1]", "app.service: Failed with result 'exit-code'.")),
        (252.0, journal("systemd[1]", "Failed to start app.service - Web app.")),
        (586.8, prompt("sudo systemctl restart app")),
        (586.9, "Job for app.service failed again.".to_owned()),
        (661.9, prompt("sudo systemctl restart app")),
        (662.0, "Job for app.service failed again.".to_owned()),
        (767.0, "logout".to_owned()),
    ];
    let mut out = format!(
        "{{\"version\":2,\"width\":132,\"height\":30,\"timestamp\":{START},\
         \"env\":{{\"TERM\":\"xterm-256color\",\"SHELL\":\"ssh.exe\"}},\"title\":\"admin@srv1\"}}\n"
    );
    let line = |out: &mut String, t: f64, text: &str| {
        let at = std::time::Duration::from_secs_f64(t);
        out.push_str(&urfin_term::rec::output_line(at, text.as_bytes()));
    };
    // Первая строка экрана макета пуста, приглашение — без перевода строки в конце.
    line(&mut out, 0.5, "\r\n");
    for (t, text) in &events {
        line(&mut out, *t, text);
        line(&mut out, *t, "\r\n");
    }
    line(&mut out, 252.1, "");
    // Размер файла как на макете: пустые события в конце.
    while out.len() < SIZE {
        line(&mut out, 767.0, "");
    }
    out
}

/// Запись SSH-сеанса (`screen-sshrec`, F03-12): bash на srv1 — `uptime`, `df`, `free`,
/// `journalctl` под sudo (пароль не виден: эхо выключено), `tail` журнала nginx.
/// Цвета промпта — bash; остальное красит подсветка вывода Урфина.
fn sshrec() -> String {
    const N: &str = "\x1b[0m";
    let prompt = |cmd: &str| format!("\x1b[1;32madmin@srv1{N}:\x1b[1;34m/var/www/app{N}$ {cmd}");
    let lines = [
        prompt("uptime"),
        " 13:58:02 up 41 days,  3:05,  2 users,  load average: 0.12, 0.09, 0.08".to_owned(),
        prompt("df -h /var /srv"),
        "Filesystem                Size  Used Avail Use% Mounted on".to_owned(),
        "/dev/mapper/vg0-var         40G   31G  7.1G  82% /var".to_owned(),
        "/dev/mapper/vg0-srv        200G   64G  127G  34% /srv".to_owned(),
        prompt("free -h"),
        "               total        used        free      shared  buff/cache   available".to_owned(),
        "Mem:           7.7Gi       2.9Gi       1.1Gi       112Mi       3.9Gi       4.6Gi".to_owned(),
        "Swap:          2.0Gi        64Mi       1.9Gi".to_owned(),
        prompt("sudo journalctl -u app --since \"2026-10-23 13:55\" --no-pager"),
        "[sudo] password for admin: ".to_owned(),
        "Oct 23 13:58:40 srv1 systemd[1]: Stopping app.service - Web app...".to_owned(),
        "Oct 23 13:58:41 srv1 app[47002]: INFO shutdown: drained 14 connections".to_owned(),
        "Oct 23 13:58:41 srv1 systemd[1]: app.service: Deactivated successfully.".to_owned(),
        "Oct 23 13:58:41 srv1 systemd[1]: Started app.service - Web app.".to_owned(),
        "Oct 23 13:58:42 srv1 app[48103]: INFO build a71d04b, config /etc/app/app.yaml".to_owned(),
        "Oct 23 13:58:42 srv1 app[48103]: INFO listening on 10.20.4.11:8080".to_owned(),
        "Oct 23 13:59:07 srv1 app[48103]: WARN slow query 1.8s: SELECT * FROM sessions WHERE user_id = $1".to_owned(),
        "Oct 23 14:00:15 srv1 app[48103]: WARN upstream http://10.20.4.21:9000/health answered 503".to_owned(),
        "Oct 23 14:01:02 srv1 app[48103]: ERROR db: dial tcp 10.20.4.30:5432: connect: connection refused".to_owned(),
        "Oct 23 14:01:02 srv1 app[48103]: INFO db: retry in 5s (attempt 1 of 10)".to_owned(),
        "Oct 23 14:01:07 srv1 app[48103]: INFO db: connected to 10.20.4.30:5432".to_owned(),
        "Oct 23 14:01:44 srv1 app[48103]: INFO upload saved to /var/www/app/static/uploads/2026/10/report_q3.pdf".to_owned(),
        "Oct 23 14:02:10 srv1 app[48103]: ERROR mail: smtp 10.20.4.40:25: 451 temporary local problem".to_owned(),
        "Oct 23 14:02:10 srv1 app[48103]: INFO mail: queued for retry, id 5e8a40c7".to_owned(),
        prompt("sudo tail -n 3 /var/log/nginx/error.log"),
        "2026/10/23 14:02:31 [error] 1123#1123: *88 upstream timed out, client: 10.20.1.57, server: app.corp.local".to_owned(),
        "2026/10/23 14:02:44 [warn] 1123#1123: *91 an upstream response is buffered to a temporary file /var/cache/nginx/proxy_temp/4/02/0000000024".to_owned(),
        "2026/10/23 14:03:02 [error] 1123#1123: *95 connect() failed (111: Connection refused) while connecting to upstream".to_owned(),
        prompt(""),
    ];
    lines.join("\r\n")
}

/// Терминал пары SSH (`screen-sshpair`, F03-09): 79×18, вывод `git pull`, `systemctl
/// status` и журнала, промпт bash. Цвета — как у самих программ (git, systemctl, bash);
/// подсветка вывода Урфина (F03-10) сюда не входит.
pub(crate) fn sshpair() -> std::sync::Arc<urfin_term::vt::shared::TermShared> {
    const G: &str = "\x1b[32m";
    const R: &str = "\x1b[31m";
    const P: &str = "\x1b[1;32m";
    const N: &str = "\x1b[0m";
    let prompt = format!("{P}admin@srv1:/var/www/app${N} ");
    let lines = [
        "   3f9c2e1..a71d04b  main       -> origin/main".to_owned(),
        "Updating 3f9c2e1..a71d04b".to_owned(),
        "Fast-forward".to_owned(),
        format!(" src/http/server.go | 18 {G}+++++++++++{R}-------{N}"),
        " 1 file changed, 11 insertions(+), 7 deletions(-)".to_owned(),
        format!("{prompt}sudo systemctl restart app && systemctl status app"),
        format!("{G}●{N} app.service - Web app"),
        "     Loaded: loaded (/etc/systemd/system/app.service; enabled)".to_owned(),
        format!("     Active: {G}active (running){N} since Fri 2026-10-23 14:02:11 MSK; 3s ago"),
        "   Main PID: 48211 (app)".to_owned(),
        "     CGroup: /system.slice/app.service".to_owned(),
        "             └─48211 /var/www/app/bin/app --config /etc/app/app.yaml".to_owned(),
        String::new(),
        "Oct 23 14:02:11 srv1 app[48211]: INFO listening on 10.20.4.11:8080".to_owned(),
        "Oct 23 14:02:12 srv1 app[48211]: WARN cache.ttl not set, using 60s".to_owned(),
        "Oct 23 14:02:12 srv1 app[48211]: error: db 10.20.4.30:5432 refused".to_owned(),
        "Oct 23 14:02:13 srv1 app[48211]: INFO db: connected, retry 1".to_owned(),
    ];
    let mut out = lines.join("\r\n");
    out.push_str("\r\n");
    out.push_str(&prompt);
    let opts = SessionOpts {
        size: PtySize { cols: 79, rows: 18 },
        scrollback: 100,
        osc52: "off".into(),
        nonce: None,
    };
    let shared = parse_inline(out.as_bytes(), &opts, &Quiet);
    shared.mark_dirty();
    shared
}

#[cfg(test)]
mod tests {
    use urfin_term::vt::grid::{CursorKind, Defaults};

    use super::*;

    const D: Defaults = Defaults {
        fg: [0xcc, 0xcc, 0xcc],
        bg: [0x0c, 0x0c, 0x0c],
        cursor: [0xff, 0xff, 0xff],
    };

    /// Рамки ложатся в сетку: стыки на своих колонках, кириллица по одной ячейке.
    #[test]
    fn frames_fill_the_grid() {
        let snap = screen("frames", 0).unwrap().snapshot(&D);
        assert_eq!(snap.cells.len(), ROWS);
        let row = |r: usize| snap.cells[r].iter().map(|c| c.ch).collect::<String>();
        let top = row(1);
        assert!(top.starts_with('╔') && top.ends_with('╗'), "{top}");
        assert_eq!(top.chars().filter(|&c| c == '╤').count(), 4);
        assert_eq!(snap.cells[1][0].bg, [0x00, 0x37, 0xda]);
        let bottom = row(28);
        assert!(bottom.starts_with('╚') && bottom.ends_with('╝'), "{bottom}");
        assert_eq!(snap.cursor, (29, 15));
    }

    /// Широкие символы — две ячейки (вторая — заполнитель), столбцы после них на месте.
    #[test]
    fn wide_chars_keep_columns() {
        let snap = screen("unicode", 0).unwrap().snapshot(&D);
        let r = &snap.cells[9];
        let bar = |row: &[urfin_term::vt::grid::Cell]| {
            row.iter()
                .enumerate()
                .filter(|(_, c)| c.ch == '|')
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        assert_eq!(bar(r), bar(&snap.cells[10]));
        assert_eq!(bar(r), bar(&snap.cells[11]));
        let k = r.iter().position(|c| c.ch == '漢').unwrap();
        assert!(r[k].wide && r[k + 1].spacer);
        assert_eq!(snap.cursor_kind, CursorKind::Beam);
    }

    #[test]
    fn colors_resolve_palette_and_truecolor() {
        let snap = screen("colors", 0).unwrap().snapshot(&D);
        // Куб: первая строка начинается с 16 (чёрный), truecolor — радуга с красного.
        assert_eq!(snap.cells[5][0].bg, [0, 0, 0]);
        let tc = snap
            .cells
            .iter()
            .position(|r| {
                r.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .starts_with("Truecolor")
            })
            .unwrap();
        assert_eq!(snap.cells[tc + 1][0].bg, [255, 0, 0]);
        assert_eq!(snap.cursor_kind, CursorKind::Underline);
        assert!(screen("nope", 0).is_none());
        let dense = screen("dense", 1).unwrap().snapshot(&D);
        assert_eq!((dense.cols, dense.rows), (200, 60));
        assert!(dense.cells[59].iter().filter(|c| c.ch != ' ').count() > 150);
    }
}
