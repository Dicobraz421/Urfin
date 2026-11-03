//! Запись в `sftp://` для движка операций (F03 §4.1, F03-07): `urfin_ops::RemoteFs`
//! поверх [`SftpFs`] — закачка с байта `n` (докачка), `mkdir`/`remove`, rename поверх
//! (`posix-rename`), права и время, связь профиля для «ждёт связи».

use std::io;
use std::sync::Arc;

use urfin_core::Location;
use urfin_ops::{Link, RemoteFs, RemoteWrite};
use urfin_vfs::{Vfs, VfsError};

use super::{SetStat, SftpFs, SftpWriter};
use crate::uri::{SCHEME, SftpUri};

/// `RemoteFs` схемы `sftp`: тот же backend, что в реестре VFS.
#[derive(Debug)]
pub struct SftpOps {
    fs: Arc<SftpFs>,
    channels: u16,
}

impl SftpOps {
    /// `channels` — `[ssh] transfer_channels`: файлов параллельно внутри задачи.
    pub fn new(fs: Arc<SftpFs>, channels: u8) -> SftpOps {
        SftpOps {
            fs,
            channels: u16::from(channels.max(1)),
        }
    }
}

struct Writer(SftpWriter);

impl io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl RemoteWrite for Writer {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        self.0.finish()
    }
}

/// нс Unix → секунды SFTP v3 (`u32`).
fn secs(ns: i64) -> u32 {
    u32::try_from(ns.div_euclid(1_000_000_000).max(0)).unwrap_or(u32::MAX)
}

impl RemoteFs for SftpOps {
    fn scheme(&self) -> &'static str {
        SCHEME
    }

    fn vfs(&self) -> &dyn Vfs {
        &*self.fs
    }

    fn open_write(
        &self,
        p: &Location,
        from: Option<u64>,
    ) -> Result<Box<dyn RemoteWrite>, VfsError> {
        Ok(Box::new(Writer(self.fs.open_write_at(p, from)?)))
    }

    fn mkdir(&self, p: &Location) -> Result<(), VfsError> {
        self.fs.mkdir(p)
    }

    fn remove(&self, p: &Location) -> Result<(), VfsError> {
        self.fs.remove(p)
    }

    fn rename(&self, from: &Location, to: &Location, replace: bool) -> Result<(), VfsError> {
        self.fs.rename_with(from, to, replace)
    }

    fn set_meta(
        &self,
        p: &Location,
        mode: Option<u32>,
        mtime: Option<i64>,
    ) -> Result<(), VfsError> {
        if mode.is_none() && mtime.is_none() {
            return Ok(());
        }
        let times = mtime.map(|t| (secs(t), secs(t)));
        self.fs.setstat(p, SetStat { mode, times })
    }

    fn link(&self, at: &Location) -> Link {
        SftpUri::from_location(at).map_or(Link::Down, |u| self.fs.link(&u.profile))
    }

    fn channels(&self) -> u16 {
        self.channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_and_down_without_session() {
        assert_eq!(secs(1_500_000_000), 1);
        assert_eq!(secs(-5), 0);
        let ops = SftpOps::new(Arc::new(SftpFs::new()), 0);
        assert_eq!(ops.channels(), 1);
        let at = SftpUri::new("srv1", "/x").to_location();
        assert_eq!(ops.link(&at), Link::Down);
        ops.fs.lost("srv1");
        assert_eq!(ops.link(&at), Link::Waiting);
        ops.fs.detach("srv1");
        assert_eq!(ops.link(&at), Link::Down);
    }
}
