//! Агент Урфина (F03 §2.7): ключи из менеджера паролей (PEM из команды, §2.5) для
//! `ssh.exe` через `-o IdentityAgent=<pipe>`. Протокол ssh-agent — сервер
//! `russh::keys::agent`; только эти ключи и только список и подпись: добавить, удалить
//! или запереть ключи клиенты не могут.
//!
//! Ключи кладёт сам Урфин первым подключением (в памяти, до pipe), затем агент
//! запечатывается.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::{Stream, StreamExt as _};
use russh::keys::PrivateKey;
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::server::{self, Agent, MessageType};
use tokio::io::{AsyncRead, AsyncWrite};

/// Поток подключения к агенту.
pub trait Io: AsyncRead + AsyncWrite + Send + Sync + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Sync + Unpin> Io for T {}

#[derive(Clone, Debug, Default)]
struct OnlyThese {
    sealed: Arc<AtomicBool>,
}

impl Agent for OnlyThese {
    fn confirm_request(&self, msg: MessageType) -> impl Future<Output = bool> + Send {
        let ok = match msg {
            MessageType::RequestKeys | MessageType::Sign => true,
            MessageType::AddKeys => !self.sealed.load(Ordering::SeqCst),
            MessageType::RemoveKeys
            | MessageType::RemoveAllKeys
            | MessageType::Lock
            | MessageType::Unlock => false,
        };
        std::future::ready(ok)
    }
}

/// Агент с ключами `keys` на подключениях `listener`; работает, пока идёт `listener`.
pub async fn serve<L>(listener: L, keys: Vec<PrivateKey>) -> io::Result<()>
where
    L: Stream<Item = io::Result<Box<dyn Io>>> + Unpin + Send + 'static,
{
    let agent = OnlyThese::default();
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let first = futures::stream::once(std::future::ready(Ok(Box::new(theirs) as Box<dyn Io>)));
    let all = first.chain(listener);
    let task = tokio::spawn(server::serve(all, agent.clone()));
    let mut client = AgentClient::connect(ours);
    for k in &keys {
        client
            .add_identity(k, &[])
            .await
            .map_err(io::Error::other)?;
    }
    agent.sealed.store(true, Ordering::SeqCst);
    drop(client);
    match task.await {
        Ok(r) => r.map_err(io::Error::other),
        Err(e) => Err(io::Error::other(e)),
    }
}

/// Pipe `name` (DACL — текущий пользователь): подключения по одному экземпляру.
#[cfg(windows)]
pub fn pipe_listener(
    name: String,
) -> io::Result<impl Stream<Item = io::Result<Box<dyn Io>>> + Unpin + Send + 'static> {
    let first = crate::sys::pipe_server(&name, true)?;
    Ok(Box::pin(futures::stream::unfold(
        Some(first),
        move |server| {
            let name = name.clone();
            async move {
                let server = server?;
                if let Err(e) = server.connect().await {
                    return Some((Err(e), None));
                }
                let next = crate::sys::pipe_server(&name, false).ok();
                Some((Ok(Box::new(server) as Box<dyn Io>), next))
            }
        },
    )))
}

#[cfg(test)]
mod tests {
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use tokio::sync::mpsc;

    use super::*;

    fn key(seed: u8) -> PrivateKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
    }

    type Conns = mpsc::UnboundedSender<Box<dyn Io>>;

    /// Подключения из канала — как экземпляры pipe.
    fn listener() -> (
        Conns,
        impl Stream<Item = io::Result<Box<dyn Io>>> + Unpin + Send,
    ) {
        let (tx, rx) = mpsc::unbounded_channel::<Box<dyn Io>>();
        let s =
            futures::stream::unfold(
                rx,
                |mut rx| async move { rx.recv().await.map(|c| (Ok(c), rx)) },
            );
        (tx, Box::pin(s))
    }

    fn connect(tx: &Conns) -> AgentClient<tokio::io::DuplexStream> {
        let (a, b) = tokio::io::duplex(64 * 1024);
        tx.send(Box::new(b)).unwrap();
        AgentClient::connect(a)
    }

    #[tokio::test]
    async fn pem_key_is_the_only_identity_and_signs() {
        // PEM — как из команды менеджера паролей.
        let pem = key(7)
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap();
        let k = russh::keys::decode_secret_key(&pem, None).unwrap();
        let (tx, l) = listener();
        tokio::spawn(serve(l, vec![k]));
        let mut c = connect(&tx);
        let ids = c.request_identities().await.unwrap();
        assert_eq!(ids.len(), 1);
        let russh::keys::agent::AgentIdentity::PublicKey { key: pubkey, .. } = &ids[0] else {
            panic!("certificate");
        };
        assert_eq!(pubkey.key_data(), key(7).public_key().key_data());
        let sig = c.sign_request(&ids[0], None, b"data".to_vec()).await;
        assert!(sig.is_ok());
        // Чужой ключ не добавить, свой не удалить.
        assert!(c.add_identity(&key(8), &[]).await.is_err());
        let mut c = connect(&tx);
        assert!(c.remove_all_identities().await.is_err());
        let mut c = connect(&tx);
        assert_eq!(c.request_identities().await.unwrap().len(), 1);
    }
}
