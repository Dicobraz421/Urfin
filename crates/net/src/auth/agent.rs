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
