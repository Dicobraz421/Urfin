//! `--askpass` (F03 §2.7): `ssh.exe` запускает `urfin.exe` как `SSH_ASKPASS`; помощник
//! передаёт текст запроса в pipe `URFIN_ASKPASS` и печатает ответ. Урфин отвечает из
//! памяти сеанса или строкой ввода над терминалом. Отказ — пустой ответ и код 1.
//!
//! Кадр: длина `u32` BE и байты UTF-8, не длиннее [`MAX_FRAME`].

use std::io::{self, Read, Write};

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::conn::{Event, Events, PromptKind};
use crate::secrets::{Secret, Shared};

/// Переменная окружения с именем pipe.
pub const ENV: &str = "URFIN_ASKPASS";
pub const MAX_FRAME: usize = 64 * 1024;

fn too_long() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "askpass frame too long")
}

pub fn write_frame(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    let len = u32::try_from(b.len())
        .ok()
        .filter(|&n| n as usize <= MAX_FRAME)
        .ok_or_else(too_long)?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(b)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(too_long());
    }
    let mut b = Zeroizing::new(vec![0u8; len]);
    r.read_exact(&mut b)?;
    Ok(b)
}

/// Помощник: запрос по `pipe`, ответ — в `out`. Код выхода: 0 — ответ есть, 1 — отказ.
pub fn exchange(pipe: &mut (impl Read + Write), prompt: &str, out: &mut impl Write) -> i32 {
    let r = write_frame(pipe, prompt.as_bytes()).and_then(|()| read_frame(pipe));
    let printed = match r {
        Ok(a) if !a.is_empty() => out
            .write_all(&a)
            .and_then(|()| out.write_all(b"\n"))
            .and_then(|()| out.flush())
            .is_ok(),
        _ => false,
    };
    i32::from(!printed)
}

/// `urfin.exe --askpass <запрос>` (или `SSH_ASKPASS` с `URFIN_ASKPASS` в окружении):
/// без окна и без GPUI.
pub fn run(prompt: &str) -> i32 {
    let Some(name) = std::env::var_os(ENV).filter(|n| !n.is_empty()) else {
        return 1;
    };
    let Ok(mut pipe) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
    else {
        return 1;
    };
    exchange(&mut pipe, prompt, &mut io::stdout().lock())
}

/// Запрос помощника для сеанса.
#[derive(Debug)]
pub struct AskRequest {
    pub prompt: String,
    pub reply: oneshot::Sender<Option<Secret>>,
}

/// Одно подключение помощника.
pub async fn handle<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    tx: &mpsc::UnboundedSender<AskRequest>,
) -> io::Result<()> {
    let len = s.read_u32().await? as usize;
    if len > MAX_FRAME {
        return Err(too_long());
    }
    let mut b = vec![0u8; len];
    s.read_exact(&mut b).await?;
    let (reply, rx) = oneshot::channel();
    let req = AskRequest {
        prompt: String::from_utf8_lossy(&b).into_owned(),
        reply,
    };
    let answer = if tx.send(req).is_ok() {
        rx.await.ok().flatten()
    } else {
        None
    };
    let bytes = answer.as_ref().map_or(&[][..], |a| a.expose().as_bytes());
    s.write_u32(u32::try_from(bytes.len()).map_err(|_| too_long())?)
        .await?;
    s.write_all(bytes).await?;
    s.flush().await
}

/// Ответы сеанса `host`: пароль — из памяти сеанса, иначе строка ввода (событие
/// [`Event::Prompt`]); введённый пароль запоминается.
pub async fn respond(
    mut rx: mpsc::UnboundedReceiver<AskRequest>,
    secrets: Shared,
    events: Events,
    host: String,
) {
    let key = format!("pw:{host}");
    while let Some(req) = rx.recv().await {
        let is_password = req.prompt.to_ascii_lowercase().contains("password");
        let cached = if is_password { secrets.get(&key) } else { None };
        if let Some(s) = cached {
            let _ = req.reply.send(Some(s));
            continue;
        }
        let kind = if is_password {
            PromptKind::Password
        } else {
            PromptKind::Interactive {
                name: String::new(),
                instructions: String::new(),
            }
        };
        let (reply, answer) = oneshot::channel();
        let _ = events.send(Event::Prompt {
            host: host.clone(),
            kind,
            text: req.prompt,
            echo: false,
            reply,
        });
        let a = answer.await.ok().flatten().map(|a| Secret::new(a.text));
        if is_password && let Some(s) = &a {
            secrets.put(key.clone(), s.clone());
        }
        let _ = req.reply.send(a);
    }
}

/// Сервер pipe `name`: каждое подключение помощника — задача.
#[cfg(windows)]
pub async fn serve(name: String, tx: mpsc::UnboundedSender<AskRequest>) -> io::Result<()> {
    bind(name, tx)?.await
}

/// Pipe `name` создан сразу (помощник уже может подключиться); подключения обслуживает
/// возвращённая задача. Нужен контекст tokio.
#[cfg(windows)]
pub fn bind(
    name: String,
    tx: mpsc::UnboundedSender<AskRequest>,
) -> io::Result<impl Future<Output = io::Result<()>> + Send> {
    let first = crate::sys::pipe_server(&name, true)?;
    Ok(accept(name, first, tx))
}

#[cfg(windows)]
async fn accept(
    name: String,
    first: tokio::net::windows::named_pipe::NamedPipeServer,
    tx: mpsc::UnboundedSender<AskRequest>,
) -> io::Result<()> {
    let mut server = first;
    loop {
        server.connect().await?;
        let s = std::mem::replace(&mut server, crate::sys::pipe_server(&name, false)?);
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(s, &tx).await {
                tracing::debug!(target: "net", error = %e, "askpass");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Pipe в памяти: читает заготовленный ответ, пишет запрос.
    struct Pipe {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            Read::read(&mut self.input, buf)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Write::write(&mut self.output, buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn pipe(answer: &[u8]) -> Pipe {
        let mut input = Vec::new();
        write_frame(&mut input, answer).unwrap();
        Pipe {
            input: Cursor::new(input),
            output: Vec::new(),
        }
    }

    #[test]
    fn helper_prints_answer() {
        let mut p = pipe(b"s3cret");
        let mut out = Vec::new();
        assert_eq!(exchange(&mut p, "admin@srv1's password: ", &mut out), 0);
        assert_eq!(out, b"s3cret\n");
        let sent = read_frame(&mut Cursor::new(p.output)).unwrap();
        assert_eq!(&sent[..], b"admin@srv1's password: ");
    }

    #[test]
    fn helper_refuses_on_empty_or_broken() {
        let mut out = Vec::new();
        assert_eq!(exchange(&mut pipe(b""), "x", &mut out), 1);
        let mut broken = Pipe {
            input: Cursor::new(vec![0, 0]),
            output: Vec::new(),
        };
        assert_eq!(exchange(&mut broken, "x", &mut out), 1);
        assert_eq!(out.len(), 0);
        let mut huge = Vec::new();
        assert!(write_frame(&mut huge, &vec![0; MAX_FRAME + 1]).is_err());
    }

    #[test]
    fn run_without_pipe_refuses() {
        // Переменная только читается; в тестах её нет.
        if std::env::var_os(ENV).is_none() {
            assert_eq!(run("x"), 1);
        }
    }

    #[tokio::test]
    async fn server_answers_from_session_memory_then_prompt() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (etx, mut erx) = mpsc::unbounded_channel();
        let secrets = Shared::new(0);
        tokio::spawn(respond(rx, secrets.clone(), etx, "srv1".into()));

        // Нет в памяти — строка ввода; ответ запоминается.
        let (a, b) = tokio::io::duplex(1024);
        let server = tokio::spawn({
            let tx = tx.clone();
            async move { handle(b, &tx).await }
        });
        let ui = tokio::spawn(async move {
            let Some(Event::Prompt { text, reply, .. }) = erx.recv().await else {
                panic!("no prompt");
            };
            let _ = reply.send(Some("pw".into()));
            (text, erx)
        });
        let answer = tokio::task::spawn_blocking(move || {
            let mut a = SyncIo(a);
            let mut out = Vec::new();
            let code = exchange(&mut a, "Password: ", &mut out);
            (code, out)
        })
        .await
        .unwrap();
        assert_eq!(answer, (0, b"pw\n".to_vec()));
        server.await.unwrap().unwrap();
        let (text, _erx) = ui.await.unwrap();
        assert_eq!(text, "Password: ");

        // Второй раз — из памяти, без вопроса.
        let (a, b) = tokio::io::duplex(1024);
        let server = tokio::spawn(async move { handle(b, &tx).await });
        let answer = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            (exchange(&mut SyncIo(a), "Password: ", &mut out), out)
        })
        .await
        .unwrap();
        assert_eq!(answer, (0, b"pw\n".to_vec()));
        server.await.unwrap().unwrap();
    }

    /// Синхронная обёртка `DuplexStream` для помощника в тесте.
    struct SyncIo(tokio::io::DuplexStream);

    impl Read for SyncIo {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let h = tokio::runtime::Handle::current();
            h.block_on(self.0.read(buf))
        }
    }

    impl Write for SyncIo {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let h = tokio::runtime::Handle::current();
            h.block_on(self.0.write(buf))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
