//! Подключение к агенту (F03 §2.5): `auto` — сначала `\\.\pipe\openssh-ssh-agent`,
//! затем Pageant (функция `russh`).

use russh::keys::agent::client::{AgentClient, AgentStream};

use crate::config::AgentKind;

pub(super) type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

#[cfg(windows)]
pub(super) async fn connect(kind: AgentKind) -> Option<Agent> {
    const OPENSSH: &str = r"\\.\pipe\openssh-ssh-agent";
    if kind != AgentKind::Pageant {
        match AgentClient::connect_named_pipe(OPENSSH).await {
            Ok(c) => return Some(c.dynamic()),
            Err(e) => tracing::debug!(target: "net", error = %e, "openssh agent"),
        }
    }
    if kind != AgentKind::Openssh {
        match AgentClient::connect_pageant().await {
            Ok(c) => return Some(c.dynamic()),
            Err(e) => tracing::debug!(target: "net", error = %e, "pageant"),
        }
    }
    None
}

/// Не Windows (тесты): агент из `SSH_AUTH_SOCK`.
#[cfg(not(windows))]
pub(super) async fn connect(_kind: AgentKind) -> Option<Agent> {
    AgentClient::connect_env()
        .await
        .ok()
        .map(AgentClient::dynamic)
}
