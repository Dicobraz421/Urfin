//! Подключение к агенту (F03 §2.5): `auto` — сначала `\\.\pipe\openssh-ssh-agent`,
//! затем Pageant (функция `russh`). Ответ — агент и `true`, если это Pageant.

use russh::keys::agent::client::{AgentClient, AgentStream};

use urfin_core::net::AgentSeen;

use crate::config::AgentKind;

/// Pipe агента OpenSSH для Windows.
const OPENSSH_PIPE: &str = "openssh-ssh-agent";

/// Какой агент виден среди имён pipe (`pageant.<user>.<hash>` — Pageant ≥0.75): порядок
/// и выбор — как у [`connect`]. Только имена, к агенту не подключается.
pub(crate) fn seen_in<'a>(
    kind: AgentKind,
    mut pipes: impl Iterator<Item = &'a str> + Clone,
) -> AgentSeen {
    if kind != AgentKind::Pageant && pipes.clone().any(|p| p == OPENSSH_PIPE) {
        AgentSeen::OpenSsh
    } else if kind != AgentKind::Openssh && pipes.any(|p| p.starts_with("pageant.")) {
        AgentSeen::Pageant
    } else {
        AgentSeen::Missing
    }
}

/// Агент на машине для пояснения настроек: перечень `\\.\pipe\`. Зовётся задачей Runtime.
pub(crate) fn seen(kind: AgentKind) -> AgentSeen {
    let pipes: Vec<String> = std::fs::read_dir(r"\\.\pipe\")
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    seen_in(kind, pipes.iter().map(String::as_str))
}

pub(super) type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

/// Ключей в агенте — подсказка строки «Новое подключение» (F03-15 для F03-18); `None` —
/// агента нет или он не ответил. Только перечень, без подписи.
pub(crate) async fn key_count(kind: AgentKind) -> Option<usize> {
    let (agent, _) = connect(kind).await?;
    count(agent).await
}

async fn count<S: AgentStream + Unpin>(mut agent: AgentClient<S>) -> Option<usize> {
    match agent.request_identities().await {
        Ok(ids) => Some(ids.len()),
        Err(e) => {
            tracing::debug!(target: "net", error = %e, "agent identities");
            None
        }
    }
}

#[cfg(windows)]
pub(super) async fn connect(kind: AgentKind) -> Option<(Agent, bool)> {
    if kind != AgentKind::Pageant {
        let pipe = format!(r"\\.\pipe\{OPENSSH_PIPE}");
        match AgentClient::connect_named_pipe(pipe).await {
            Ok(c) => return Some((c.dynamic(), false)),
            Err(e) => tracing::debug!(target: "net", error = %e, "openssh agent"),
        }
    }
    if kind != AgentKind::Openssh {
        match AgentClient::connect_pageant().await {
            Ok(c) => return Some((c.dynamic(), true)),
            Err(e) => tracing::debug!(target: "net", error = %e, "pageant"),
        }
    }
    None
}

/// Не Windows (тесты): агент из `SSH_AUTH_SOCK`.
#[cfg(not(windows))]
pub(super) async fn connect(_kind: AgentKind) -> Option<(Agent, bool)> {
    AgentClient::connect_env()
        .await
        .ok()
        .map(|c| (c.dynamic(), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    use russh::keys::PrivateKey;
    use russh::keys::agent::server;
    use russh::keys::ssh_key::private::Ed25519Keypair;

    #[derive(Clone)]
    struct Yes;
    impl server::Agent for Yes {}

    /// Агент russh на паре `duplex`: ключи, добавленные клиентом, видны в числе.
    #[tokio::test]
    async fn counts_agent_keys() {
        let agent = || {
            let (c, s) = tokio::io::duplex(64 * 1024);
            let listener = futures::stream::iter([Ok::<_, std::io::Error>(s)]);
            tokio::spawn(server::serve(listener, Yes));
            AgentClient::connect(c)
        };
        assert_eq!(count(agent()).await, Some(0));
        let mut a = agent();
        for seed in [31, 32] {
            let k = PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]));
            a.add_identity(&k, &[]).await.unwrap();
        }
        assert_eq!(count(a).await, Some(2));
    }

    #[test]
    fn agent_by_pipe_names() {
        let both = ["x", "openssh-ssh-agent", "pageant.me.abc"];
        let only = ["pageant.me.abc"];
        let none = ["x"];
        let s = |k, p: &[&'static str]| seen_in(k, p.iter().copied());
        assert_eq!(s(AgentKind::Auto, &both), AgentSeen::OpenSsh);
        assert_eq!(s(AgentKind::Pageant, &both), AgentSeen::Pageant);
        assert_eq!(s(AgentKind::Auto, &only), AgentSeen::Pageant);
        assert_eq!(s(AgentKind::Openssh, &only), AgentSeen::Missing);
        assert_eq!(s(AgentKind::Auto, &none), AgentSeen::Missing);
    }
}
