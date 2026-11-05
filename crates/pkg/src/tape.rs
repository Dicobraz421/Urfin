//! Лента процессов пакетов (F02 §3.1) — ленточный сеанс терминала: заголовок
//! `▸ winget upgrade Mozilla.Firefox`, строки, `код выхода`.

use urfin_term::feed::FeedSink;

use crate::api::{PkgError, Tape};

pub(crate) struct FeedTape(pub FeedSink);

impl Tape for FeedTape {
    fn head(&self, text: &str) {
        self.0.head(&format!("▸ {text}"));
    }

    fn line(&self, text: &str) {
        self.0.line(text);
    }

    fn exit(&self, res: Result<u32, &PkgError>) {
        let i18n = urfin_i18n::current();
        let mut a = urfin_i18n::FluentArgs::new();
        let key = match res {
            Ok(code) => {
                a.set("code", code.to_string());
                "pkg-feed-exit"
            }
            Err(e) => {
                a.set("error", e.to_string());
                "pkg-feed-stopped"
            }
        };
        self.0.note(&i18n.fmt(key, &a));
    }
}
