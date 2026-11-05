//! Аутентификация (F03 §2.5, F03-02): агент OpenSSH или Pageant, ключи OpenSSH, пароль,
//! keyboard-interactive (2FA), команда менеджера паролей (F03-03). Порядок — по `auth`
//! профиля; второй фактор (`partial_success`) — по методам, которые назвал сервер.
//! Пароли и фразы-пароли живут в памяти сеанса ([`crate::secrets`]) для переподключений;
//! ответы keyboard-interactive (коды 2FA) не запоминаются.

mod agent;
pub(crate) use agent::seen as agent_seen;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::MethodKind;
use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg};
use tokio::sync::oneshot;

use crate::conn::{Answer, Client, ConnError, Cx, Event, PromptKind, Waited, expand_home};
use crate::profile::{Auth, Profile};
use crate::pwcmd::{self, PwOutput};
use crate::secrets::Secret;
pub use urfin_core::net::Login;

/// Попыток ввода пароля и фразы-пароля, как у OpenSSH.
const TRIES: usize = 3;
/// Раундов второго фактора.
const ROUNDS: usize = 4;

/// Узел для вопросов и ошибок.
struct Node<'a, 'b> {
    h: &'a mut Handle<Client>,
    user: String,
    /// `host` или `[host]:port`.
    name: &'a str,
    /// Цель кэша паролей `Urfin/ssh/<user>@<host>:<port>`.
    target: String,
    waited: &'a Waited,
    cx: &'a mut Cx<'b>,
    /// Чем вошли, по факторам — статус панели (№92).
    login: Vec<Login>,
}

/// Аутентифицирует `h` по профилю `p`; ответ — чем вошли, по факторам.
pub(crate) async fn authenticate(
    handle: &mut Handle<Client>,
    p: &Profile,
    name: &str,
    waited: &Waited,
    cx: &mut Cx<'_>,
) -> Result<Vec<Login>, ConnError> {
    let user = if p.user.is_empty() {
        default_user()
    } else {
        p.user.clone()
    };
    let target = crate::vault::target(&user, &p.host, p.port);
    let mut n = Node {
        h: handle,
        user,
        name,
        target,
        waited,
        cx,
        login: Vec::new(),
    };
    // `none` — узнать методы сервера.
    let mut r = n.h.authenticate_none(n.user.clone()).await?;
    let AuthResult::Failure {
        remaining_methods: methods,
        ..
    } = &r
    else {
        return Ok(vec![Login::Open]);
    };
    let methods = methods.to_vec();
    r = match p.auth {
        Auth::Agent => match n.agent().await? {
            Some(r) if r.success() => r,
            _ => n.keys(&n.cx.cfg.default_keys.clone()).await?,
        },
        Auth::Key => {
            let keys = match expand_home(&p.key, dirs::home_dir().as_deref()) {
                Some(k) if !p.key.is_empty() => vec![k],
                _ => n.cx.cfg.default_keys.clone(),
            };
            n.keys(&keys).await?
        }
        Auth::Command => n.command(p).await?,
        Auth::Password => {
            if methods.contains(&MethodKind::Password) {
                n.password().await?
            } else {
                n.interactive().await?
            }
        }
    };
    for _ in 0..ROUNDS {
        let AuthResult::Failure {
            remaining_methods,
            partial_success,
        } = r
        else {
            return Ok(n.login);
        };
        if !partial_success {
            return Err(ConnError::Auth);
        }
        r = if remaining_methods.contains(&MethodKind::KeyboardInteractive) {
            n.interactive().await?
        } else if remaining_methods.contains(&MethodKind::Password) {
            n.password().await?
        } else if remaining_methods.contains(&MethodKind::PublicKey) {
            n.keys(&n.cx.cfg.default_keys.clone()).await?
        } else {
            return Err(ConnError::Auth);
        };
    }
    Err(ConnError::Auth)
}

fn default_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

impl Node<'_, '_> {
    async fn ask(&mut self, kind: PromptKind, text: String, echo: bool) -> Option<String> {
        self.answer(kind, text, echo).await.map(|a| a.text)
    }

    /// Как [`Node::ask`], с флажком «Запомнить».
    async fn answer(&mut self, kind: PromptKind, text: String, echo: bool) -> Option<Answer> {
        let (reply, rx) = oneshot::channel();
        let _ = self.cx.events.send(Event::Prompt {
            host: self.name.to_owned(),
            kind,
            text,
            echo,
            reply,
        });
        self.waited.user(rx).await.flatten()
    }

    async fn rsa_hash(&self) -> Option<HashAlg> {
        self.h
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten()
    }

    /// Ключи агента; `None` — агента нет или ключей в нём нет.
    async fn agent(&mut self) -> Result<Option<AuthResult>, ConnError> {
        let Some((mut agent, pageant)) = agent::connect(self.cx.cfg.agent).await else {
            return Ok(None);
        };
        if let Some(b) = self.h.agent_session_binding() {
            let _ = agent.bind_session(b, false).await;
        }
        let ids = match agent.request_identities().await {
            Ok(ids) if !ids.is_empty() => ids,
            _ => return Ok(None),
        };
        let hash = self.rsa_hash().await;
        let mut last = None;
        for id in ids {
            let keys::agent::AgentIdentity::PublicKey { key, .. } = id else {
                continue;
            };
            let hash = matches!(key.algorithm(), keys::Algorithm::Rsa { .. })
                .then_some(hash)
                .flatten();
            match self
                .h
                .authenticate_publickey_with(self.user.clone(), key, hash, &mut agent)
                .await
            {
                Ok(r) if r.success() => {
                    self.login.push(Login::Agent { pageant });
                    return Ok(Some(r));
                }
                Ok(r) => last = Some(r),
                Err(e) => tracing::debug!(target: "net", error = %e, "agent sign"),
            }
        }
        Ok(last)
    }

    /// Ключи OpenSSH по порядку; `.ppk` и отсутствующие пропускаются.
    async fn keys(&mut self, paths: &[PathBuf]) -> Result<AuthResult, ConnError> {
        let mut last = AuthResult::Failure {
            remaining_methods: russh::MethodSet::empty(),
            partial_success: false,
        };
        for path in paths {
            let Some(key) = self.load_key(path).await? else {
                continue;
            };
            let hash = if matches!(key.algorithm(), keys::Algorithm::Rsa { .. }) {
                self.rsa_hash().await
            } else {
                None
            };
            let r = self
                .h
                .authenticate_publickey(
                    self.user.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await?;
            if accepted(&r) {
                self.login.push(Login::Key(path.clone()));
                return Ok(r);
            }
            last = r;
        }
        Ok(last)
    }

    async fn load_key(&mut self, path: &Path) -> Result<Option<keys::PrivateKey>, ConnError> {
        // Чтение ключа — мелкий файл на потоке net, не на UI.
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(None);
        };
        if text.trim_start().starts_with("PuTTY-User-Key-File-") {
            tracing::info!(target: "net", key = %path.display(), ".ppk is not read: load it into Pageant");
            return Ok(None);
        }
        let slot = format!("pp:{}", path.display());
        let mut pass = self.cx.mem.get(&slot);
        for _ in 0..TRIES {
            match keys::decode_secret_key(&text, pass.as_ref().map(Secret::expose)) {
                Ok(k) => {
                    if let Some(p) = pass {
                        self.cx.mem.put(slot, p);
                    }
                    return Ok(Some(k));
                }
                // Нет фразы или она неверна.
                Err(_) if encrypted(&text) => {
                    self.cx.mem.remove(&slot);
                    let prompt = format!("Enter passphrase for key '{}':", path.display());
                    let kind = PromptKind::Passphrase(path.to_path_buf());
                    let p = self
                        .ask(kind, prompt, false)
                        .await
                        .ok_or(ConnError::Cancelled)?;
                    pass = Some(Secret::new(p));
                }
                Err(e) => {
                    tracing::info!(target: "net", key = %path.display(), error = %e, "key");
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    fn password_slot(&self) -> String {
        format!("pw:{}", self.name)
    }

    /// Пароль: память сеанса, затем кэш (Диспетчер учётных данных, если разрешён), затем
    /// строка ввода. «Запомнить» кладёт принятый пароль в кэш; неверный из кэша — удаляется.
    async fn password(&mut self) -> Result<AuthResult, ConnError> {
        let slot = self.password_slot();
        let vault = self.cx.cfg.vault.clone();
        let mut cached = vault.as_ref().and_then(|v| v.get(&self.target, true));
        let mut last = None;
        for _ in 0..TRIES {
            let mut remember = false;
            let from_vault = cached.is_some();
            let pw = if let Some(p) = self.cx.mem.get(&slot) {
                p
            } else if let Some(p) = cached.take() {
                p
            } else {
                let text = format!("{}@{}'s password:", self.user, self.name);
                let a = self.answer(PromptKind::Password, text, false).await;
                let a = a.ok_or(ConnError::Cancelled)?;
                remember = a.remember;
                Secret::new(a.text)
            };
            let r = self.try_password(&pw).await?;
            if accepted(&r) {
                if remember && let Some(v) = &vault {
                    let _ = v
                        .remember(&self.target, &pw)
                        .inspect_err(|e| tracing::warn!(target: "net", error = %e, "vault"));
                }
                self.cx.mem.put(slot, pw);
                self.login.push(Login::Password);
                return Ok(r);
            }
            self.cx.mem.remove(&slot);
            if from_vault && let Some(v) = &vault {
                let _ = v.forget(&self.target);
            }
            last = Some(r);
        }
        last.ok_or(ConnError::Auth)
    }

    async fn try_password(&mut self, pw: &Secret) -> Result<AuthResult, ConnError> {
        // russh берёт `String`: копия живёт только в пакете запроса.
        Ok(self
            .h
            .authenticate_password(self.user.clone(), pw.expose().to_owned())
            .await?)
    }

    /// Команда менеджера паролей (F03 §2.5): пароль или PEM-ключ. Ответ живёт в памяти
    /// сеанса: при переподключении команда не запускается снова.
    async fn command(&mut self, p: &Profile) -> Result<AuthResult, ConnError> {
        let slot = self.password_slot();
        if let Some(pw) = self.cx.mem.get(&slot) {
            let r = self.try_password(&pw).await?;
            if accepted(&r) {
                self.login.push(Login::Command);
                return Ok(r);
            }
            self.cx.mem.remove(&slot);
        }
        let cfg = self.cx.cfg;
        let template = if !p.password_cmd.is_empty() {
            p.password_cmd.clone()
        } else if !cfg.password_cmd.is_empty() {
            cfg.password_cmd.clone()
        } else {
            pwcmd::template(cfg.password_manager)
                .ok_or_else(|| ConnError::PasswordCommand("no password command".into()))?
                .to_owned()
        };
        let vars = pwcmd::Vars {
            host: &p.host,
            user: &self.user,
            id: &p.password_id,
        };
        let argv =
            pwcmd::argv(&template, vars).map_err(|e| ConnError::PasswordCommand(e.to_string()))?;
        // Ожидание команды — как ожидание пользователя: в таймаут не входит.
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(pwcmd::run(&argv, pwcmd::TIMEOUT));
        });
        let out = self
            .waited
            .user(rx)
            .await
            .ok_or(ConnError::Cancelled)?
            .map_err(|e| ConnError::PasswordCommand(e.to_string()))?;
        let r = match out {
            PwOutput::Password(pw) => {
                let r = self.try_password(&pw).await?;
                if accepted(&r) {
                    self.cx.mem.put(slot, pw);
                }
                r
            }
            PwOutput::Key(pem) => {
                let text = std::str::from_utf8(pem.expose())
                    .map_err(|_| ConnError::PasswordCommand("key is not UTF-8".into()))?;
                let key = keys::decode_secret_key(text, None)
                    .map_err(|e| ConnError::PasswordCommand(e.to_string()))?;
                let hash = if matches!(key.algorithm(), keys::Algorithm::Rsa { .. }) {
                    self.rsa_hash().await
                } else {
                    None
                };
                let r = self
                    .h
                    .authenticate_publickey(
                        self.user.clone(),
                        PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                    )
                    .await?;
                // Ключ — в память сеанса: его отдаёт `ssh.exe` агент Урфина (F03 §2.7).
                if accepted(&r) {
                    self.cx
                        .mem
                        .put(key_slot(self.name), Secret::new(text.to_owned()));
                }
                r
            }
        };
        if accepted(&r) {
            self.login.push(Login::Command);
        }
        Ok(r)
    }

    /// keyboard-interactive: каждый запрос сервера — строка ввода.
    async fn interactive(&mut self) -> Result<AuthResult, ConnError> {
        let mut r = self
            .h
            .authenticate_keyboard_interactive_start(self.user.clone(), None)
            .await?;
        loop {
            match r {
                KeyboardInteractiveAuthResponse::Success => {
                    self.login.push(Login::Interactive);
                    return Ok(AuthResult::Success);
                }
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods,
                    partial_success,
                } => {
                    if partial_success {
                        self.login.push(Login::Interactive);
                    }
                    return Ok(AuthResult::Failure {
                        remaining_methods,
                        partial_success,
                    });
                }
                KeyboardInteractiveAuthResponse::InfoRequest {
                    name,
                    instructions,
                    prompts,
                } => {
                    let mut answers = Vec::with_capacity(prompts.len());
                    for pr in prompts {
                        let kind = PromptKind::Interactive {
                            name: name.clone(),
                            instructions: instructions.clone(),
                        };
                        answers.push(
                            self.ask(kind, pr.prompt, pr.echo)
                                .await
                                .ok_or(ConnError::Cancelled)?,
                        );
                    }
                    r = self
                        .h
                        .authenticate_keyboard_interactive_respond(answers)
                        .await?;
                }
            }
        }
    }
}

/// Слот памяти сеанса для PEM-ключа из менеджера паролей узла `name`.
pub(crate) fn key_slot(name: &str) -> String {
    format!("key:{name}")
}

fn encrypted(text: &str) -> bool {
    matches!(
        keys::decode_secret_key(text, None),
        Err(keys::Error::KeyIsEncrypted)
    )
}

/// Принят (или принят первым фактором).
fn accepted(r: &AuthResult) -> bool {
    matches!(
        r,
        AuthResult::Success
            | AuthResult::Failure {
                partial_success: true,
                ..
            }
    )
}
