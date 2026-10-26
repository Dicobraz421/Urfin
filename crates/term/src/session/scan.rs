//! Пре-сканер OSC (S6 §1.3): байтовый автомат поверх вывода PTY до `advance`.
//!
//! Ловит `ESC ] (7|9;9|133|633|1337;CurrentDir=) … (BEL | ESC \)`. Состояние
//! переживает границы чанков. Наши OSC в эмулятор не идут, остальные байты — как
//! есть и в том же порядке. Тело длиннее 4 КБ отбрасывается целиком.
//!
//! Доверие: `633;E` и `633;P;FnCwd` — только с nonce сеанса; остальное решает
//! сеанс по [`CwdSource`].

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// Предел тела OSC (S6 §1.3).
pub const MAX_OSC: usize = 4096;

/// Откуда пришёл cwd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CwdSource {
    /// `633;P;FnCwd=<nonce>;<path>` — наша интеграция.
    FnCwd,
    /// `OSC 7 file://host/path`.
    Osc7,
    /// `OSC 9;9;path` (`ConEmu`, Windows Terminal).
    Osc9_9,
    /// `OSC 1337;CurrentDir=path` (iTerm2).
    Iterm,
}

/// Семантическое событие shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscEvent {
    /// `A`: начало промпта.
    PromptStart,
    /// `B`: конец промпта, дальше ввод.
    CommandStart,
    /// `C`: команда запущена.
    Executed,
    /// `D[;код]`: команда завершилась.
    Finished(Option<i32>),
    /// `633;E;строка;nonce` (nonce проверен).
    CommandLine(String),
    Cwd {
        path: String,
        source: CwdSource,
    },
    /// `633;P;ключ=значение` (кроме `FnCwd`).
    Prop(String, String),
}

/// Кусок потока: байты для эмулятора или событие на этом месте.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece<'a> {
    Bytes(&'a [u8]),
    Event(OscEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Osc7,
    Osc9_9,
    Osc133,
    Osc633,
    Iterm,
}

const PREFIXES: [(&[u8], Kind); 5] = [
    (b"7;", Kind::Osc7),
    (b"9;9;", Kind::Osc9_9),
    (b"133;", Kind::Osc133),
    (b"633;", Kind::Osc633),
    (b"1337;CurrentDir=", Kind::Iterm),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ground,
    /// Был `ESC`.
    Esc,
    /// `ESC ]` и часть префикса в `held`.
    Prefix,
    Payload,
    PayloadEsc,
    /// Переполнение: тело пропускается до терминатора.
    Discard,
    DiscardEsc,
}

/// Автомат пре-сканера одного сеанса.
#[derive(Debug)]
pub struct Scanner {
    nonce: Option<String>,
    st: St,
    /// `ESC ] префикс…`, пока не ясно, наш ли это OSC.
    held: Vec<u8>,
    kind: Kind,
    payload: Vec<u8>,
}

impl Scanner {
    /// `nonce` — секрет сеанса с интеграцией; `None` — без неё.
    pub fn new(nonce: Option<String>) -> Scanner {
        Scanner {
            nonce,
            st: St::Ground,
            held: Vec::with_capacity(20),
            kind: Kind::Osc7,
            payload: Vec::new(),
        }
    }

    /// Разобрать чанк. Байты выходят в исходном порядке; задержаны могут быть
    /// только байты начала `ESC ]`, пока префикс не решён.
    pub fn feed(&mut self, chunk: &[u8], out: &mut dyn FnMut(Piece<'_>)) {
        let mut run = 0;
        let mut i = 0;
        while i < chunk.len() {
            let b = chunk[i];
            match self.st {
                St::Ground => {
                    if b == ESC {
                        if run < i {
                            out(Piece::Bytes(&chunk[run..i]));
                        }
                        self.held.clear();
                        self.held.push(ESC);
                        self.st = St::Esc;
                    }
                    i += 1;
                }
                St::Esc => {
                    if b == b']' {
                        self.held.push(b);
                        self.st = St::Prefix;
                        i += 1;
                    } else {
                        // Не OSC: отдать `ESC`, байт — заново с Ground.
                        out(Piece::Bytes(&self.held));
                        self.st = St::Ground;
                        run = i;
                    }
                }
                St::Prefix => {
                    self.held.push(b);
                    let body = &self.held[2..];
                    match prefix(body) {
                        Prefix::Full(kind) => {
                            self.kind = kind;
                            self.payload.clear();
                            self.st = St::Payload;
                            i += 1;
                        }
                        Prefix::Partial => i += 1,
                        Prefix::Foreign => {
                            // Чужой OSC: отдать префикс без этого байта, байт — заново.
                            self.held.pop();
                            out(Piece::Bytes(&self.held));
                            self.st = St::Ground;
                            run = i;
                        }
                    }
                }
                St::Payload | St::Discard => {
                    let keep = self.st == St::Payload;
                    match b {
                        BEL => {
                            if keep {
                                self.finish(out);
                            }
                            self.st = St::Ground;
                            run = i + 1;
                        }
                        ESC => {
                            self.st = if keep { St::PayloadEsc } else { St::DiscardEsc };
                        }
                        CAN | SUB => {
                            self.st = St::Ground;
                            run = i + 1;
                        }
                        _ if keep => {
                            if self.payload.len() < MAX_OSC {
                                self.payload.push(b);
                            } else {
                                self.payload.clear();
                                self.st = St::Discard;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                St::PayloadEsc | St::DiscardEsc => {
                    if self.st == St::PayloadEsc {
                        self.finish(out);
                    }
                    if b == b'\\' {
                        self.st = St::Ground;
                        i += 1;
                        run = i;
                    } else {
                        // `ESC` закрыл OSC и начал новую последовательность.
                        self.held.clear();
                        self.held.push(ESC);
                        self.st = St::Esc;
                    }
                }
            }
        }
        if self.st == St::Ground && run < chunk.len() {
            out(Piece::Bytes(&chunk[run..]));
        }
    }

    fn finish(&mut self, out: &mut dyn FnMut(Piece<'_>)) {
        let body = String::from_utf8_lossy(&self.payload).into_owned();
        self.payload.clear();
        if let Some(ev) = self.parse(self.kind, &body) {
            out(Piece::Event(ev));
        }
    }

    fn parse(&self, kind: Kind, body: &str) -> Option<OscEvent> {
        match kind {
            Kind::Osc7 => osc7_path(body).map(|path| OscEvent::Cwd {
                path,
                source: CwdSource::Osc7,
            }),
            Kind::Osc9_9 => {
                let p = body.trim_matches('"');
                (!p.is_empty()).then(|| OscEvent::Cwd {
                    path: p.to_owned(),
                    source: CwdSource::Osc9_9,
                })
            }
            Kind::Iterm => (!body.is_empty()).then(|| OscEvent::Cwd {
                path: body.to_owned(),
                source: CwdSource::Iterm,
            }),
            Kind::Osc133 | Kind::Osc633 => {
                let mut f = body.split(';');
                let ev = match f.next()? {
                    "A" => OscEvent::PromptStart,
                    "B" => OscEvent::CommandStart,
                    "C" => OscEvent::Executed,
                    "D" => OscEvent::Finished(f.next().and_then(|c| c.parse().ok())),
                    "E" if kind == Kind::Osc633 => {
                        let line = f.next()?;
                        self.trusted(f.next())?;
                        OscEvent::CommandLine(unescape(line))
                    }
                    "P" if kind == Kind::Osc633 => {
                        let (k, v) = f.next()?.split_once('=')?;
                        if k == "FnCwd" {
                            self.trusted(Some(v))?;
                            OscEvent::Cwd {
                                path: unescape(f.next()?),
                                source: CwdSource::FnCwd,
                            }
                        } else {
                            OscEvent::Prop(k.to_owned(), unescape(v))
                        }
                    }
                    _ => return None,
                };
                Some(ev)
            }
        }
    }

    /// `Some(())`, если nonce совпал с секретом сеанса.
    fn trusted(&self, got: Option<&str>) -> Option<()> {
        let want = self.nonce.as_deref()?;
        let got = got?;
        // Сравнение без раннего выхода: время не выдаёт совпавший префикс.
        let eq = want.len() == got.len()
            && want
                .bytes()
                .zip(got.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        eq.then_some(())
    }
}

/// Слежение за `CSI ? … 9001 h|l` (win32-input-mode, S6-20): `ConPTY` просит его в
/// выводе, эмулятор такой режим не знает. Последовательность только читается — в
/// эмулятор идёт как есть (alacritty пропускает неизвестный режим). Состояние
/// переживает границы чанков.
#[derive(Debug, Default)]
pub struct ModeWatch {
    st: ModeSt,
    params: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ModeSt {
    #[default]
    Ground,
    Esc,
    /// `ESC [`.
    Csi,
    /// `ESC [ ?` и параметры.
    Private,
}

/// Длиннее — не наш режим.
const MAX_PARAMS: usize = 32;

impl ModeWatch {
    /// Разобрать чанк; `Some(on)` — режим 9001 включён или выключен в этом чанке
    /// (последнее из переключений).
    pub fn feed(&mut self, chunk: &[u8]) -> Option<bool> {
        let mut last = None;
        for &b in chunk {
            self.st = match (self.st, b) {
                (_, ESC) => ModeSt::Esc,
                (ModeSt::Esc, b'[') => ModeSt::Csi,
                (ModeSt::Csi, b'?') => {
                    self.params.clear();
                    ModeSt::Private
                }
                (ModeSt::Private, b'0'..=b'9' | b';') if self.params.len() < MAX_PARAMS => {
                    self.params.push(b);
                    ModeSt::Private
                }
                (ModeSt::Private, b'h' | b'l') => {
                    if self.params.split(|&c| c == b';').any(|p| p == b"9001") {
                        last = Some(b == b'h');
                    }
                    ModeSt::Ground
                }
                _ => ModeSt::Ground,
            };
        }
        last
    }
}

/// Совпадение начала OSC с нашими префиксами.
enum Prefix {
    Full(Kind),
    Partial,
    Foreign,
}

fn prefix(body: &[u8]) -> Prefix {
    let mut partial = false;
    for (p, kind) in PREFIXES {
        if body == p {
            return Prefix::Full(kind);
        }
        partial |= p.starts_with(body);
    }
    if partial {
        Prefix::Partial
    } else {
        Prefix::Foreign
    }
}

/// `file://host/path` → путь (с декодированием `%XX`); `/C:/x` → `C:/x`.
fn osc7_path(body: &str) -> Option<String> {
    let rest = body.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let path = percent_decode(path);
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(path[1..].to_owned());
    }
    Some(path)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    char::from(c).to_digit(16).map(|d| d as u8)
}

/// Экранирование VS Code в 633: `\\` → `\`, `\xHH` → байт.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            if b.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if b.get(i + 1) == Some(&b'x')
                && let (Some(h), Some(l)) = (
                    b.get(i + 2).copied().and_then(hex),
                    b.get(i + 3).copied().and_then(hex),
                )
            {
                out.push(h << 4 | l);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
