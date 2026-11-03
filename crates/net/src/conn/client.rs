//! Обработчик клиента russh: проверка ключа сервера (F03 §2.4) и признак обрыва.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use russh::client::{DisconnectReason, Handler};
use russh::keys::PublicKeyOrCertificate;
use tokio::sync::{oneshot, watch};

use super::{ConnError, Cx, Event, Events, HostKeyChoice, Phase, Waited};
use crate::hostkey::{self, KnownHosts, Verdict};

/// Обработчик соединения одного узла.
pub struct Client {
    /// `host` или `[host]:port`.
    name: String,
    known: Vec<PathBuf>,
    trust_file: Option<PathBuf>,
    session_file: Option<PathBuf>,
    events: Events,
    waited: Arc<Waited>,
    /// Почему отказали в ключе — для ошибки подключения.
    outcome: Arc<Mutex<Option<ConnError>>>,
    lost: Arc<watch::Sender<bool>>,
    /// Алгоритм принятого ключа сервера — статус панели (№92).
    algo: Arc<Mutex<String>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub(super) fn new(
        name: String,
        cx: &Cx<'_>,
        waited: Arc<Waited>,
        outcome: Arc<Mutex<Option<ConnError>>>,
        lost: Arc<watch::Sender<bool>>,
        algo: Arc<Mutex<String>>,
    ) -> Client {
        let session = cx.session_hosts.map(Path::to_path_buf);
        Client {
            name,
            known: cx
                .cfg
                .known_hosts
                .iter()
                .cloned()
                .chain(session.clone())
                .collect(),
            trust_file: cx.cfg.trust_file.clone(),
            session_file: session,
            events: cx.events.clone(),
            waited,
            outcome,
            lost,
            algo,
        }
    }

    fn reject(&self, e: ConnError) -> bool {
        *self.outcome.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
        false
    }

    async fn verify(&self, key: &PublicKeyOrCertificate) -> bool {
        let _ = self.events.send(Event::Phase {
            host: self.name.clone(),
            phase: Phase::HostKey,
        });
        let known = KnownHosts::load(&self.known);
        let algo = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.algorithm().to_string(),
            PublicKeyOrCertificate::Certificate(cert) => cert.algorithm().to_string(),
        };
        algo.clone_into(&mut self.algo.lock().unwrap_or_else(PoisonError::into_inner));
        let key = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(cert) => {
                return match known.check_cert(&self.name, cert) {
                    Verdict::Known => true,
                    Verdict::Revoked => self.reject(ConnError::HostKeyRevoked),
                    // Сертификат без CA в known_hosts — не доверяем молча.
                    _ => self.reject(ConnError::HostKeyRejected),
                };
            }
        };
        let fingerprint = hostkey::fingerprint(key);
        match known.check(&self.name, key) {
            Verdict::Known => true,
            Verdict::Revoked => self.reject(ConnError::HostKeyRevoked),
            Verdict::Unknown => {
                let (reply, rx) = oneshot::channel();
                let _ = self.events.send(Event::HostKeyUnknown {
                    host: self.name.clone(),
                    algo,
                    fingerprint,
                    reply,
                });
                let file = match self.waited.user(rx).await {
                    Some(HostKeyChoice::Trust) => self.trust_file.as_ref(),
                    Some(HostKeyChoice::Once) => self.session_file.as_ref(),
                    Some(HostKeyChoice::Cancel) | None => {
                        return self.reject(ConnError::HostKeyRejected);
                    }
                };
                if let Some(f) = file
                    && let Err(e) = hostkey::append(f, &self.name, key)
                {
                    tracing::warn!(target: "net", file = %f.display(), error = %e, "known_hosts append");
                }
                true
            }
            Verdict::Changed { file, line, old } => {
                let (reply, rx) = oneshot::channel();
                let _ = self.events.send(Event::HostKeyChanged {
                    host: self.name.clone(),
                    algo,
                    file: file.clone(),
                    line,
                    old_fingerprint: hostkey::fingerprint(&old),
                    fingerprint,
                    reply,
                });
                if self.waited.user(rx).await != Some(true) {
                    return self.reject(ConnError::HostKeyChanged { file, line });
                }
                match hostkey::replace(&file, line, &self.name, key) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(target: "net", file = %file.display(), error = %e, "known_hosts replace");
                        self.reject(ConnError::HostKeyChanged { file, line })
                    }
                }
            }
        }
    }
}

impl Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(self.verify(key).await)
    }

    fn disconnected(
        &mut self,
        reason: DisconnectReason<Self::Error>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.lost.send_replace(true);
        let r = match reason {
            DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            DisconnectReason::Error(e) => Err(e),
        };
        async { r }
    }
}
