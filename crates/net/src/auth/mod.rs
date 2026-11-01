//! Аутентификация (F03 §2.5, F03-02): агент OpenSSH или Pageant, ключи OpenSSH, пароль,
//! keyboard-interactive (2FA). Порядок — по `auth` профиля; второй фактор
//! (`partial_success`) — по методам, которые назвал сервер.

mod agent;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::MethodKind;
use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg};
use tokio::sync::oneshot;

use crate::conn::{Client, ConnError, Cx, Event, PromptKind, Waited, expand_home};
use crate::profile::{Auth, Profile};

/// Попыток ввода пароля и фразы-пароля, как у OpenSSH.
const TRIES: usize = 3;
/// Раундов второго фактора.
const ROUNDS: usize = 4;

/// Секреты сеанса для переподключений: пароли узлов и фразы-пароли ключей.
/// Ответы keyboard-interactive (коды 2FA) не запоминаются.
#[derive(Default)]
pub(crate) struct Remembered {
    passwords: HashMap<String, String>,
    passphrases: HashMap<PathBuf, String>,
}

impl std::fmt::Debug for Remembered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Remembered(..)")
    }
}

/// Узел для вопросов и ошибок.
struct Node<'a, 'b> {
    h: &'a mut Handle<Client>,
    user: String,
    /// `host` или `[host]:port`.
    name: &'a str,
    waited: &'a Waited,
    cx: &'a mut Cx<'b>,
}

/// Аутентифицирует `h` по профилю `p`.
pub(crate) async fn authenticate(
    handle: &mut Handle<Client>,
    p: &Profile,
    name: &str,
    waited: &Waited,
    cx: &mut Cx<'_>,
) -> Result<(), ConnError> {
    let user = if p.user.is_empty() {
        default_user()
    } else {
        p.user.clone()
    };
    let mut n = Node {
        h: handle,
        user,
        name,
        waited,
        cx,
    };
    // `none` — узнать методы сервера.
    let mut r = n.h.authenticate_none(n.user.clone()).await?;
    let AuthResult::Failure {
        remaining_methods: methods,
        ..
    } = &r
    else {
        return Ok(());
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
        // Пароль командой — F03-03; пока — строкой ввода.
        Auth::Password | Auth::Command => {
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
            return Ok(());
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
        let Some(mut agent) = agent::connect(self.cx.cfg.agent).await else {
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
                Ok(r) if r.success() => return Ok(Some(r)),
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
            if r.success()
                || matches!(
                    r,
                    AuthResult::Failure {
                        partial_success: true,
                        ..
                    }
                )
            {
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
        let mut pass = self.cx.mem.passphrases.get(path).cloned();
        for _ in 0..TRIES {
            match keys::decode_secret_key(&text, pass.as_deref()) {
                Ok(k) => {
                    if let Some(p) = pass {
                        self.cx.mem.passphrases.insert(path.to_path_buf(), p);
                    }
                    return Ok(Some(k));
                }
                // Нет фразы или она неверна.
                Err(_) if encrypted(&text) => {
                    self.cx.mem.passphrases.remove(path);
                    let prompt = format!("Enter passphrase for key '{}':", path.display());
                    let kind = PromptKind::Passphrase(path.to_path_buf());
                    pass = Some(
                        self.ask(kind, prompt, false)
                            .await
                            .ok_or(ConnError::Cancelled)?,
                    );
                }
                Err(e) => {
                    tracing::info!(target: "net", key = %path.display(), error = %e, "key");
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    async fn password(&mut self) -> Result<AuthResult, ConnError> {
        let mut last = None;
        for _ in 0..TRIES {
            let pw = if let Some(p) = self.cx.mem.passwords.get(self.name) {
                p.clone()
            } else {
                let text = format!("{}@{}'s password:", self.user, self.name);
                self.ask(PromptKind::Password, text, false)
                    .await
                    .ok_or(ConnError::Cancelled)?
            };
            let r = self
                .h
                .authenticate_password(self.user.clone(), pw.clone())
                .await?;
            if r.success()
                || matches!(
                    r,
                    AuthResult::Failure {
                        partial_success: true,
                        ..
                    }
                )
            {
                self.cx.mem.passwords.insert(self.name.to_owned(), pw);
                return Ok(r);
            }
            self.cx.mem.passwords.remove(self.name);
            last = Some(r);
        }
        last.ok_or(ConnError::Auth)
    }

    /// keyboard-interactive: каждый запрос сервера — строка ввода.
    async fn interactive(&mut self) -> Result<AuthResult, ConnError> {
        let mut r = self
            .h
            .authenticate_keyboard_interactive_start(self.user.clone(), None)
            .await?;
        loop {
            match r {
                KeyboardInteractiveAuthResponse::Success => return Ok(AuthResult::Success),
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods,
                    partial_success,
                } => {
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

fn encrypted(text: &str) -> bool {
    matches!(
        keys::decode_secret_key(text, None),
        Err(keys::Error::KeyIsEncrypted)
    )
}
