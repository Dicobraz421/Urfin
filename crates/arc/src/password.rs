//! Вопрос пароля архива (S8 §3.7, S8-35): вход в архив с зашифрованными именами (7z
//! `-mhe`, rar `-hp`), F3 и F5 по зашифрованной записи, `arc.test`. Тем же [`Ask`] —
//! вопрос спилла сверх лимита (S8-31, [`SpillQuestion`]).
//!
//! Спрашивает [`Ask`] из [`ArcCache`]: handler `arc` ставит [`UiAsk`] — inline-вопрос
//! `PromptSpec { input: Some(""), secret: true }` из потока VFS (листинг, job ops, вьювер;
//! из UI-потока не спрашивает). Задача самого `arc` (`arc.test`) спрашивает через свой
//! `JobHandle::ask` ([`JobAsk`], [`scoped`]). Три попытки ([`TRIES`]): пароль данных
//! проверяется чтением самой маленькой зашифрованной записи ([`fits`], как у Shift+F2),
//! пароль имён — повторным разбором оглавления. Отказ (Esc, попытки кончились) архив
//! помнит [`DECLINE_FOR`]: F5 по сотне файлов не спрашивает сто раз.
//!
//! Пароль — только в `Zeroizing` и [`crate::OpenCtx`] индекса: в логи (`tracing`), историю и
//! сессию не попадает.

use std::cell::RefCell;
use std::io::Read;
use std::sync::Arc;
use std::thread::{self, ThreadId};
use std::time::Duration;

use urfin_core::{
    Anchor, Attrs, CancelToken, Choice, JobHandle, Level, MsgSink, PromptSpec, Reply, msg,
};
use urfin_vfs::{VfsError, VfsRead};
use zeroize::Zeroizing;

use crate::cache::ArcCache;
use crate::index::{ArchiveIndex, NodeId, ROOT};
use crate::spill::{GO, SpillQuestion};

/// Попыток пароля (S8 §3.7).
pub(crate) const TRIES: u8 = 3;

/// Отказ от пароля архива действует столько (с последнего обращения).
pub(crate) const DECLINE_FOR: Duration = Duration::from_secs(3);

/// Вопросов подряд без попытки (пустой ввод) — не больше.
const MAX_ASKS: u8 = 3 * TRIES;

/// Как часто ожидание ответа перепроверяет отмену.
const POLL: Duration = Duration::from_millis(20);

/// Байт зашифрованной записи, которые читает проверка пароля: запись не больше —
/// целиком (CRC и подпись AES), больше — начало (распаковщик ломается на чужом ключе).
const PROBE_FULL: u64 = 4 << 20;
const PROBE_HEAD: u64 = 256 << 10;

/// Вопрос пароля.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// Имя файла архива.
    pub name: String,
    /// Осталось попыток.
    pub left: u8,
    /// Предыдущий пароль не подошёл.
    pub wrong: bool,
    /// Зашифрованы имена: без пароля архив не листается.
    pub names: bool,
}

fn tr(key: &str) -> String {
    urfin_i18n::current().get(key)
}

impl Question {
    /// Inline-вопрос: заголовок «Пароль для <архив>», пояснение — «неверный пароль —
    /// осталось N» или «архив зашифрован»; поле ввода секретное.
    pub fn spec(&self, anchor: Anchor) -> PromptSpec {
        let i18n = urfin_i18n::current();
        let mut a = urfin_i18n::FluentArgs::new();
        a.set("name", self.name.clone());
        let title = i18n.fmt("arc-pw-ask", &a);
        let body = if self.wrong {
            let mut a = urfin_i18n::FluentArgs::new();
            a.set("left", i64::from(self.left));
            i18n.fmt("arc-pw-wrong", &a)
        } else if self.names {
            i18n.get("arc-error-no-password")
        } else {
            String::new()
        };
        let choice = |id: &str, key: &str| Choice {
            id: id.into(),
            label: i18n.get(key).into(),
            key: '\0',
        };
        PromptSpec {
            anchor,
            title,
            body,
            choices: vec![
                choice("ok", "ui-button-ok"),
                choice("cancel", "ui-button-cancel"),
            ]
            .into(),
            default: 0,
            input: Some(String::new()),
            allow_all: false,
            detail: None,
            secret: true,
        }
    }
}

/// Кто спрашивает пароль. `None` — отказ (Esc), отмена или спросить нельзя; пустая
/// строка — ввод пуст (не попытка).
pub trait Ask: Send + Sync {
    /// Можно ли спросить из этого потока (без блокировки).
    fn available(&self) -> bool {
        true
    }

    fn ask(&self, q: &Question, cancel: &CancelToken) -> Option<Zeroizing<String>>;

    /// Спилл сверх лимита (S8-31): `true` — «Распаковать». По умолчанию — отказ.
    fn spill(&self, q: &SpillQuestion, cancel: &CancelToken) -> bool {
        let _ = (q, cancel);
        false
    }

    /// Попытки кончились.
    fn exhausted(&self, name: &str) {
        let _ = name;
    }
}

/// Пароль из ответа: «Отмена» — `None`; сам ответ затирается.
fn from_reply(r: Reply) -> Option<Zeroizing<String>> {
    let input = Zeroizing::new(r.input.unwrap_or_default());
    (r.choice != "cancel").then_some(input)
}

/// Вопрос в UI из потока VFS: `Prompts::open` сообщением в UI-поток, ответ — каналом.
pub(crate) struct UiAsk {
    sink: MsgSink,
    ui: ThreadId,
}

impl UiAsk {
    /// Создаётся в UI-потоке (`ArcHandler::on_pump`): из него не спрашивает.
    pub(crate) fn here(sink: MsgSink) -> UiAsk {
        UiAsk {
            sink,
            ui: thread::current().id(),
        }
    }
}

impl Ask for UiAsk {
    fn available(&self) -> bool {
        thread::current().id() != self.ui
    }

    fn ask(&self, q: &Question, cancel: &CancelToken) -> Option<Zeroizing<String>> {
        self.prompt(q.spec(Anchor::Cmdline), cancel)
            .and_then(from_reply)
    }

    fn spill(&self, q: &SpillQuestion, cancel: &CancelToken) -> bool {
        self.prompt(q.spec(Anchor::Cmdline), cancel)
            .is_some_and(|r| r.choice == GO)
    }

    fn exhausted(&self, _name: &str) {
        let _ = self.sink.try_send(msg("arc.password.exhausted", |cx| {
            cx.toast(Level::Warn, tr("arc-pw-exhausted"));
        }));
    }
}

impl UiAsk {
    /// Вопрос в UI и ответ; `None` — отмена или спросить нельзя.
    fn prompt(&self, spec: PromptSpec, cancel: &CancelToken) -> Option<Reply> {
        if !self.available() {
            return None;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let open = msg("arc.password", move |cx| {
            let (id, reply) = cx.state.prompts.open(spec, None);
            // Ждущий ушёл (отмена) — вопрос не нужен.
            if tx.send((id, reply)).is_err() {
                cx.state.prompts.cancel(id);
            }
        });
        self.sink.send(open, cancel).ok()?;
        let (id, reply) = loop {
            match rx.recv_timeout(POLL) {
                Ok(v) => break v,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) if !cancel.is_cancelled() => {}
                Err(_) => return None,
            }
        };
        loop {
            match reply.recv_timeout(POLL) {
                Ok(r) => return Some(r),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return None,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            if cancel.is_cancelled() {
                let _ = self.sink.try_send(msg("arc.password.cancel", move |cx| {
                    cx.state.prompts.cancel(id);
                }));
                return None;
            }
        }
    }
}

/// Вопрос задачи `arc` (`arc.test`): `JobHandle::ask`, якорь — job-бар. Esc отменяет задачу.
pub(crate) struct JobAsk(pub(crate) Arc<JobHandle>);

impl Ask for JobAsk {
    fn ask(&self, q: &Question, _cancel: &CancelToken) -> Option<Zeroizing<String>> {
        self.0.ask(q.spec(Anchor::JobBar)).ok().and_then(from_reply)
    }

    fn spill(&self, q: &SpillQuestion, _cancel: &CancelToken) -> bool {
        self.0
            .ask(q.spec(Anchor::JobBar))
            .is_ok_and(|r| r.choice == GO)
    }
}

thread_local! {
    static SCOPED: RefCell<Option<Arc<dyn Ask>>> = const { RefCell::new(None) };
}

/// Вопросы пароля в этом потоке на время `f` задаёт `ask` (задача `arc` — своим
/// `JobHandle::ask`), а не общий из [`ArcCache`].
pub(crate) fn scoped<R>(ask: Arc<dyn Ask>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<dyn Ask>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            SCOPED.with(|s| *s.borrow_mut() = prev);
        }
    }
    let prev = SCOPED.with(|s| s.borrow_mut().replace(ask));
    let _restore = Restore(prev);
    f()
}

/// Спрашивающий этого потока: [`scoped`], иначе `fallback`.
pub(crate) fn current(fallback: Option<Arc<dyn Ask>>) -> Option<Arc<dyn Ask>> {
    SCOPED
        .with(|s| s.borrow().clone())
        .or(fallback)
        .filter(|a| a.available())
}

/// Три попытки: `check` — подошёл ли пароль. `Ok(false)` — отказ или попытки кончились.
pub(crate) fn obtain(
    ask: &dyn Ask,
    mut q: Question,
    cancel: &CancelToken,
    mut check: impl FnMut(&str) -> Result<bool, VfsError>,
) -> Result<bool, VfsError> {
    let mut asked = 0;
    while q.left > 0 && asked < MAX_ASKS {
        asked += 1;
        let Some(pw) = ask.ask(&q, cancel) else {
            return Ok(false);
        };
        if pw.is_empty() {
            continue;
        }
        if check(&pw)? {
            return Ok(true);
        }
        q.left -= 1;
        q.wrong = true;
        tracing::info!(target: "arc", left = q.left, "wrong archive password");
    }
    if q.left == 0 {
        ask.exhausted(&q.name);
    }
    Ok(false)
}

/// Нет пароля или он не подошёл (`arc-error-no-password`), а не отказ ОС в доступе.
pub(crate) fn denied(e: &VfsError) -> bool {
    matches!(e, VfsError::Io { source, .. }
        if source.kind() == std::io::ErrorKind::PermissionDenied && source.raw_os_error().is_none())
}

/// Пароль подходит к записи размера `size`, открытой `open`: читает её (до
/// [`PROBE_FULL`] — целиком, иначе [`PROBE_HEAD`]); `PermissionDenied`/`InvalidData` —
/// не подходит.
pub(crate) fn fits(
    open: Result<Box<dyn VfsRead>, VfsError>,
    size: u64,
    path: &std::path::Path,
) -> Result<bool, VfsError> {
    // Целиком — с запасом в байт: проверка CRC и подписи AES — на конце потока.
    let take = if size <= PROBE_FULL {
        size + 1
    } else {
        PROBE_HEAD
    };
    let fit = |e: &std::io::Error| {
        !matches!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidData
        )
    };
    let mut r = match open {
        Ok(r) => r,
        Err(VfsError::Io { source, .. }) if !fit(&source) => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut sink = std::io::sink();
    match std::io::copy(&mut r.by_ref().take(take), &mut sink) {
        Ok(_) => Ok(true),
        Err(e) if !fit(&e) => Ok(false),
        Err(e) => Err(VfsError::from_io(path.to_path_buf(), e)),
    }
}

/// Самая маленькая зашифрованная запись архива.
pub(crate) fn probe_of(idx: &ArchiveIndex) -> Option<NodeId> {
    let mut best: Option<(u64, NodeId)> = None;
    idx.subtree(ROOT, |f| {
        let n = idx.node(f);
        if n.attrs.has(Attrs::ENCRYPTED) && best.is_none_or(|(s, _)| n.size < s) {
            best = Some((n.size, f));
        }
    });
    best.map(|(_, id)| id)
}

/// Имя файла архива.
pub(crate) fn name_of(idx: &ArchiveIndex) -> String {
    idx.outer_key
        .loc
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Пароль данных `idx`: есть — `true`; нет — спросить (если есть кому) и проверить
/// записью [`probe_of`], открытой `open`. Отказ запоминается на [`DECLINE_FOR`].
pub(crate) fn ensure_data(
    cache: &ArcCache,
    idx: &ArchiveIndex,
    cancel: &CancelToken,
    open: &dyn Fn(NodeId) -> Result<Box<dyn VfsRead>, VfsError>,
) -> bool {
    if idx.ctx().has_password() {
        return true;
    }
    let Some(ask) = current(cache.asker()) else {
        return false;
    };
    let Some(probe) = probe_of(idx) else {
        return true;
    };
    let outer = &idx.outer_key.loc;
    let _turn = cache.ask_turn();
    if idx.ctx().has_password() {
        return true;
    }
    if cache.declined(outer) {
        return false;
    }
    let q = Question {
        name: name_of(idx),
        left: TRIES,
        wrong: false,
        names: false,
    };
    let size = idx.node(probe).size;
    let path = std::path::PathBuf::from(idx.path_of(probe));
    let ok = obtain(&*ask, q, cancel, |pw| {
        idx.ctx().set_password(Some(pw));
        let ok = fits(open(probe), size, &path);
        if !matches!(ok, Ok(true)) {
            idx.ctx().set_password(None);
        }
        ok
    });
    match ok {
        Ok(true) => true,
        Ok(false) if cancel.is_cancelled() => false,
        Ok(false) => {
            cache.decline(outer);
            false
        }
        Err(e) => {
            tracing::debug!(target: "arc", error = %e, "password probe failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Отвечает по списку и запоминает вопросы.
    pub(crate) struct Script {
        pub answers: Mutex<Vec<Option<&'static str>>>,
        pub asked: Mutex<Vec<Question>>,
        pub exhausted: Mutex<u32>,
    }

    impl Script {
        pub fn new(answers: &[Option<&'static str>]) -> Script {
            Script {
                answers: Mutex::new(answers.iter().rev().copied().collect()),
                asked: Mutex::default(),
                exhausted: Mutex::default(),
            }
        }
    }

    impl Ask for Script {
        fn ask(&self, q: &Question, _cancel: &CancelToken) -> Option<Zeroizing<String>> {
            self.asked.lock().unwrap().push(q.clone());
            let a = self.answers.lock().unwrap().pop().flatten()?;
            Some(Zeroizing::new(a.to_owned()))
        }

        fn exhausted(&self, _name: &str) {
            *self.exhausted.lock().unwrap() += 1;
        }
    }

    fn q() -> Question {
        Question {
            name: "a.zip".into(),
            left: TRIES,
            wrong: false,
            names: false,
        }
    }

    #[test]
    fn wrong_password_asks_again_with_tries_left() {
        let s = Script::new(&[Some("bad"), Some(""), Some("good")]);
        let ok = obtain(&s, q(), &CancelToken::new(), |pw| Ok(pw == "good")).unwrap();
        assert!(ok);
        let asked = s.asked.lock().unwrap();
        let seen: Vec<(u8, bool)> = asked.iter().map(|q| (q.left, q.wrong)).collect();
        assert_eq!(
            seen,
            [(3, false), (2, true), (2, true)],
            "пустой — не попытка"
        );
    }

    #[test]
    fn three_wrong_passwords_and_esc() {
        let s = Script::new(&[Some("a"), Some("b"), Some("c"), Some("never")]);
        assert!(!obtain(&s, q(), &CancelToken::new(), |_| Ok(false)).unwrap());
        assert_eq!(s.asked.lock().unwrap().len(), 3);
        assert_eq!(*s.exhausted.lock().unwrap(), 1);

        let esc = Script::new(&[None]);
        assert!(!obtain(&esc, q(), &CancelToken::new(), |_| Ok(true)).unwrap());
        assert_eq!(*esc.exhausted.lock().unwrap(), 0, "Esc — не исчерпано");
    }

    #[test]
    fn prompt_is_secret_with_an_empty_field() {
        urfin_i18n::set_language("ru");
        let spec = Question {
            wrong: true,
            left: 2,
            ..q()
        }
        .spec(Anchor::Cmdline);
        assert!(spec.secret);
        assert_eq!(spec.input.as_deref(), Some(""));
        assert_eq!(spec.title, "Пароль для a.zip");
        assert_eq!(spec.body, "Неверный пароль — осталось 2 попытки");
        let ids: Vec<&str> = spec.choices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["ok", "cancel"]);
        let names = Question { names: true, ..q() }.spec(Anchor::Cmdline);
        assert_eq!(names.body, tr("arc-error-no-password"));
    }

    #[test]
    fn scoped_asker_wins_in_its_thread_only() {
        let fallback: Arc<dyn Ask> = Arc::new(Script::new(&[]));
        let job: Arc<dyn Ask> = Arc::new(Script::new(&[]));
        let pick = |want: &Arc<dyn Ask>| {
            let got = current(Some(Arc::clone(&fallback))).unwrap();
            Arc::ptr_eq(&got, want)
        };
        assert!(pick(&fallback));
        scoped(Arc::clone(&job), || {
            assert!(pick(&job));
            let other = thread::spawn({
                let fallback = Arc::clone(&fallback);
                move || {
                    let got = current(Some(Arc::clone(&fallback))).unwrap();
                    Arc::ptr_eq(&got, &fallback)
                }
            });
            assert!(other.join().unwrap(), "другой поток — общий");
        });
        assert!(pick(&fallback), "после — снова общий");
    }
}
