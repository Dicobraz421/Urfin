//! Подключения для UI (F03-05, F03 §2.4, §2.5, §2.8): сеансы профилей, вопросы
//! подключения (ключ сервера, пароль, код), строка «Новое подключение», чтение профилей и
//! истории. Состояние — в [`crate::NetState`] (`AppState.ext`), его рисует UI; ответы —
//! командами `ssh.*`. Сеть и диск — только задачами (рантайм крейта и Runtime), UI-поток
//! лишь читает каналы.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use toml::value::Datetime;
use urfin_core::{Command, Cx, Lane, Level, MsgSink, Side};

use crate::conn::{self, Answer, ConnCfg, Control, Event, HostKeyChoice, Phase, Pool, PromptKind};
use crate::history::History;
use crate::hostkey::KnownHosts;
use crate::profile::{self, Auth, Profile, ProfileSource, Profiles};
use crate::sftp::Session;
use crate::uri::SftpUri;
use crate::{NetState, SftpFs};

/// Что с сеансом профиля.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveState {
    /// Идут фазы TCP → ключ сервера → вход.
    Connecting,
    Connected,
    /// Обрыв: попытка `attempt` из `tries`.
    Lost {
        attempt: u32,
        tries: u32,
    },
    Disconnected,
    /// Не подключились: текст для строки ошибки.
    Failed(String),
}

/// Сеанс профиля для UI.
#[derive(Debug)]
pub struct Live {
    pub profile: String,
    /// `admin@srv1.corp.local` — подсказка пути и статус (№87).
    pub user_host: String,
    pub state: LiveState,
    /// Фаза последнего узла цепочки — «k из 3» в статусе.
    pub phase: Option<Phase>,
    /// Где открыть папку после подключения.
    pub side: Side,
    pub path: String,
    ctl: mpsc::UnboundedSender<Control>,
}

impl Live {
    /// Сеанс без задачи подключения: фикстуры сцен `--screens` и тесты UI.
    pub fn shown(profile: &str, user_host: &str, state: LiveState, side: Side) -> Live {
        let (ctl, _) = mpsc::unbounded_channel();
        Live {
            profile: profile.to_owned(),
            user_host: user_host.to_owned(),
            state,
            phase: None,
            side,
            path: "/".into(),
            ctl,
        }
    }
}

/// Номер фазы для «k из 3».
pub fn phase_no(p: Phase) -> u8 {
    match p {
        Phase::Tcp => 1,
        Phase::HostKey => 2,
        Phase::Auth => 3,
    }
}

/// Вопрос подключения: строка внизу панели.
#[derive(Debug)]
pub enum AskKind {
    /// Неизвестный ключ (`screen-sshhostkey`).
    HostKey {
        algo: String,
        fingerprint: String,
        reply: Option<oneshot::Sender<HostKeyChoice>>,
    },
    /// Ключ сменился (`screen-sshhostchanged`): было (`file:line`) и сейчас.
    HostChanged {
        algo: String,
        file: PathBuf,
        line: usize,
        old: String,
        fingerprint: String,
        reply: Option<oneshot::Sender<bool>>,
    },
    /// Пароль, фраза-пароль, код (`screen-sshpass`).
    Secret {
        kind: PromptKind,
        text: String,
        echo: bool,
        reply: Option<oneshot::Sender<Option<Answer>>>,
    },
}

#[derive(Debug)]
pub struct Ask {
    /// Номер вопроса: новый — новые поля строки.
    pub id: u64,
    pub profile: String,
    /// Узел цепочки: `host` или `[host]:port`.
    pub host: String,
    /// `admin@srv1.corp.local`.
    pub user_host: String,
    pub kind: AskKind,
}

impl Ask {
    /// Новый вопрос (номер — следующий).
    pub fn new(profile: &str, host: &str, user_host: &str, kind: AskKind) -> Ask {
        Ask {
            id: ask_id(),
            profile: profile.to_owned(),
            host: host.to_owned(),
            user_host: user_host.to_owned(),
            kind,
        }
    }

    /// Имя хоста для подтверждения замены ключа: без `[]:port`.
    pub fn plain_host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .map_or(self.host.as_str(), |(h, _)| h)
    }
}

/// Строка «Новое подключение» (`screen-sshnew`): начальные значения полей.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    /// Номер открытия: новый — новые поля.
    pub seq: u64,
    pub host: String,
    pub user: String,
    pub port: String,
    pub via: String,
    pub auth: Auth,
    pub save: bool,
    /// Правка профиля с этим именем (F4), иначе новый.
    pub edit: Option<String>,
    pub side: Side,
}

/// Подпись способа входа.
pub fn auth_key(a: Auth) -> &'static str {
    match a {
        Auth::Agent => "net-auth-agent",
        Auth::Key => "net-auth-key",
        Auth::Password => "net-auth-password",
        Auth::Command => "net-auth-command",
    }
}

pub fn auth_id(a: Auth) -> &'static str {
    match a {
        Auth::Agent => "agent",
        Auth::Key => "key",
        Auth::Password => "password",
        Auth::Command => "command",
    }
}

pub fn parse_auth(s: &str) -> Option<Auth> {
    [Auth::Agent, Auth::Key, Auth::Password, Auth::Command]
        .into_iter()
        .find(|a| auth_id(*a).eq_ignore_ascii_case(s))
}

/// Вариант автодополнения «Хоста»: значение и источник справа.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostHint {
    pub host: String,
    pub source: HintSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintSource {
    /// `~/.ssh/config` · `Host <имя>`.
    Config(String),
    /// История · когда (секунды Unix).
    History(Option<i64>),
    KnownHosts,
    /// Свой профиль.
    Profile(String),
}

/// Варианты для набранного `text`: начинаются с него (без регистра), без повторов;
/// порядок — config, история, `known_hosts`, свои профили (`screen-sshnew`).
pub fn host_hints(st: &NetState, text: &str) -> Vec<HostHint> {
    let t = text.trim().to_lowercase();
    let mut out: Vec<HostHint> = Vec::new();
    let mut push = |host: &str, source: HintSource| {
        if host.to_lowercase().starts_with(&t)
            && !out.iter().any(|h| h.host.eq_ignore_ascii_case(host))
        {
            out.push(HostHint {
                host: host.to_owned(),
                source,
            });
        }
    };
    let list = st.profiles.as_ref().map_or(&[][..], |p| &p.list[..]);
    for p in list.iter().filter(|p| p.source == ProfileSource::Config) {
        push(&p.host, HintSource::Config(p.name.clone()));
    }
    for e in st.history.iter().flat_map(|h| &h.entries) {
        push(&e.host, HintSource::History(unix(&e.time)));
    }
    for h in &st.known {
        push(h, HintSource::KnownHosts);
    }
    for p in list.iter().filter(|p| p.source != ProfileSource::Config) {
        push(&p.host, HintSource::Profile(p.name.clone()));
    }
    out
}

/// `user@host` профиля (пользователь по умолчанию — из ОС).
pub fn user_host(p: &Profile) -> String {
    if p.user.is_empty() {
        p.host.clone()
    } else {
        format!("{}@{}", p.user, p.host)
    }
}

impl NetState {
    /// Профиль по имени: свои, импортированные, разовые этого запуска.
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles
            .as_ref()
            .and_then(|p| p.get(name))
            .or_else(|| {
                self.adhoc
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
            })
    }

    /// Сеанс профиля.
    pub fn live(&self, profile: &str) -> Option<&Live> {
        self.live
            .iter()
            .find(|l| l.profile.eq_ignore_ascii_case(profile))
    }

    fn live_mut(&mut self, profile: &str) -> Option<&mut Live> {
        self.live
            .iter_mut()
            .find(|l| l.profile.eq_ignore_ascii_case(profile))
    }

    /// Вопрос, который показан (первый в очереди).
    pub fn ask(&self) -> Option<&Ask> {
        self.asks.front()
    }

    /// Подключённых профилей (статус «SSH: N подключений»).
    pub fn connected(&self) -> usize {
        self.live
            .iter()
            .filter(|l| l.state == LiveState::Connected)
            .count()
    }

    /// Пинг профиля: RTT последнего листинга или keep-alive.
    pub fn rtt(&self, profile: &str) -> Option<std::time::Duration> {
        self.sftp.rtt(profile)
    }
}

/// `ssh.reload`: перечитать профили, историю и `known_hosts` задачей Runtime. Без диска
/// (`--smoke`, `--screens`) — ничего: там профили — фикстура.
pub fn reload(cx: &mut Cx<'_>) {
    let Some(st) = cx.state.ext.get::<NetState>() else {
        return;
    };
    let Some(files) = st.files.clone() else {
        return;
    };
    let src = st.sources();
    let known = known_files();
    cx.rt
        .spawn(Lane::Interactive, "net.profiles", cx.rt.token(), move |t| {
            let profiles = profile::load(&src);
            let history = History::load(&files.history).unwrap_or_else(|e| {
                tracing::warn!(target: "net", error = %e, "history");
                History::default()
            });
            let kh = KnownHosts::load(&known);
            let keys = profiles
                .list
                .iter()
                .filter_map(|p| {
                    let k = kh.key_of(&crate::hostkey::host_name(&p.host, p.port))?;
                    Some((p.name.to_lowercase(), k))
                })
                .collect();
            let known = kh.hosts();
            let _ = t.sink.send(
                urfin_core::msg("net.profiles", move |cx| {
                    if let Some(st) = cx.state.ext.get_mut::<NetState>() {
                        st.profiles = Some(profiles);
                        st.history = Some(history);
                        st.known = known;
                        st.host_keys = keys;
                        st.rev += 1;
                    }
                }),
                &t.cancel,
            );
        });
}

/// `~/.ssh/known_hosts` и `known_hosts2`.
fn known_files() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|h| {
            let d = h.join(".ssh");
            vec![d.join("known_hosts"), d.join("known_hosts2")]
        })
        .unwrap_or_default()
}

/// `sftp.open {profile, path?, side?}`: папка профиля в панели. Есть канал — сразу
/// `panel.go`; нет — подключение (сеть — только здесь, по явному действию).
pub fn open(cx: &mut Cx<'_>, name: &str, path: Option<&str>, side: Side) -> Result<(), String> {
    let Some(st) = cx.state.ext.get::<NetState>() else {
        return Err("no ssh state".into());
    };
    let p = if let Some(p) = st.profile(name) {
        p.clone()
    } else {
        // `user@host:port` — разовый профиль (F03 §2.6).
        let input = format!("sftp://{name}/");
        let (t, _) = crate::uri::parse_input(&input).map_err(|e| e.to_string())?;
        let list = st.profiles.as_ref().map_or(&[][..], |p| &p.list[..]);
        crate::uri::resolve(list, &t).0
    };
    let path = path
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .or_else(|| (!p.start_dir.is_empty()).then(|| p.start_dir.clone()))
        .unwrap_or_else(|| "/".into());
    if st.sftp.session(&p.name).is_some() {
        go(cx, &p.name, &path, side);
        return Ok(());
    }
    if st
        .live(&p.name)
        .is_some_and(|l| l.state == LiveState::Connecting)
    {
        return Ok(());
    }
    let chain = conn::chain(&p, |n| st.profile(n).cloned()).map_err(|e| e.to_string())?;
    let mut cfg = ConnCfg::new(
        &st.cfg,
        dirs::home_dir().as_deref(),
        st.files.as_ref().map(|f| f.state_dir.as_path()),
    );
    cfg.vault.clone_from(&st.vault);
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
    let sink = cx.rt.sink();
    let sftp = Arc::clone(&st.sftp);
    let pool = Arc::clone(&st.pool);
    let profile = p.name.clone();
    let task = drive(profile.clone(), chain, cfg, pool, ctl_rx, sink, sftp);
    let st = cx.state.ext.get_mut::<NetState>().expect("checked above");
    if p.source == ProfileSource::History && st.profile(&p.name).is_none() {
        st.adhoc.push(p.clone());
    }
    st.live
        .retain(|l| !l.profile.eq_ignore_ascii_case(&profile));
    st.live.push(Live {
        profile,
        user_host: user_host(&p),
        state: LiveState::Connecting,
        phase: None,
        side,
        path,
        ctl: ctl_tx,
    });
    st.rev += 1;
    if !st.net_rt().spawn(cx.rt, task) {
        return Err("net runtime is stopped".into());
    }
    Ok(())
}

fn go(cx: &mut Cx<'_>, profile: &str, path: &str, side: Side) {
    let loc = SftpUri::new(profile, path).to_location();
    let side = if side == Side::Left { "left" } else { "right" };
    cx.dispatch(Command::new("panel.focus").arg("side", side));
    cx.dispatch(Command::new("panel.go").arg("side", side).arg("path", loc));
}

/// Сеанс на потоке крейта: события подключения — в UI сообщениями; поднятое соединение —
/// канал SFTP в backend.
async fn drive(
    profile: String,
    chain: Vec<Profile>,
    cfg: ConnCfg,
    pool: Arc<Pool>,
    ctl: mpsc::UnboundedReceiver<Control>,
    sink: MsgSink,
    sftp: Arc<SftpFs>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let forward = async {
        while let Some(ev) = rx.recv().await {
            let note = match ev {
                Event::Connected(c) => match Session::open(c).await {
                    Ok(s) => {
                        sftp.attach(s);
                        Note::Ready
                    }
                    Err(e) => Note::Failed(e.to_string()),
                },
                Event::Disconnected => {
                    sftp.detach(&profile);
                    Note::Event(Event::Disconnected)
                }
                Event::Lost {
                    attempt,
                    tries,
                    delay,
                } => {
                    sftp.detach(&profile);
                    Note::Event(Event::Lost {
                        attempt,
                        tries,
                        delay,
                    })
                }
                ev => Note::Event(ev),
            };
            let p = profile.clone();
            let _ = sink.try_send(urfin_core::msg("net.session", move |cx| {
                apply(cx, &p, note);
            }));
        }
    };
    let _ = tokio::join!(conn::session(chain, cfg, pool, tx, ctl), forward);
}

/// Что пришло от сеанса.
#[derive(Debug)]
enum Note {
    Event(Event),
    /// Соединение поднято, канал SFTP открыт.
    Ready,
    Failed(String),
}

static ASK_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn ask_id() -> u64 {
    ASK_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn apply(cx: &mut Cx<'_>, profile: &str, note: Note) {
    let Some(st) = cx.state.ext.get_mut::<NetState>() else {
        return;
    };
    st.rev += 1;
    let user_host = st
        .live(profile)
        .map(|l| l.user_host.clone())
        .unwrap_or_default();
    let ask = |host: String, kind: AskKind| Ask {
        id: ask_id(),
        profile: profile.to_owned(),
        host,
        user_host: user_host.clone(),
        kind,
    };
    match note {
        Note::Ready => {
            let Some(l) = st.live_mut(profile) else {
                return;
            };
            l.state = LiveState::Connected;
            l.phase = None;
            let (path, side) = (l.path.clone(), l.side);
            let now = now();
            touch(st, profile, now);
            go(cx, profile, &path, side);
            save_history(cx);
        }
        Note::Failed(e) => fail(cx, profile, &e),
        Note::Event(ev) => match ev {
            Event::Phase { phase, .. } => {
                if let Some(l) = st.live_mut(profile) {
                    l.phase = Some(phase);
                }
            }
            Event::HostKeyUnknown {
                host,
                algo,
                fingerprint,
                reply,
            } => st.asks.push_back(ask(
                host,
                AskKind::HostKey {
                    algo,
                    fingerprint,
                    reply: Some(reply),
                },
            )),
            Event::HostKeyChanged {
                host,
                algo,
                file,
                line,
                old_fingerprint,
                fingerprint,
                reply,
            } => st.asks.push_back(ask(
                host,
                AskKind::HostChanged {
                    algo,
                    file,
                    line,
                    old: old_fingerprint,
                    fingerprint,
                    reply: Some(reply),
                },
            )),
            Event::Prompt {
                host,
                kind,
                text,
                echo,
                reply,
            } => st.asks.push_back(ask(
                host,
                AskKind::Secret {
                    kind,
                    text,
                    echo,
                    reply: Some(reply),
                },
            )),
            Event::Connected(_) => {}
            Event::Lost { attempt, tries, .. } => {
                if let Some(l) = st.live_mut(profile) {
                    l.state = LiveState::Lost { attempt, tries };
                }
            }
            Event::Failed(e) => fail(cx, profile, &e.to_string()),
            Event::Disconnected => {
                drop_asks(st, profile);
                if let Some(l) = st.live_mut(profile) {
                    l.state = LiveState::Disconnected;
                }
            }
        },
    }
}

fn fail(cx: &mut Cx<'_>, profile: &str, e: &str) {
    let Some(st) = cx.state.ext.get_mut::<NetState>() else {
        return;
    };
    drop_asks(st, profile);
    st.live.retain(|l| !l.profile.eq_ignore_ascii_case(profile));
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("profile", profile.to_owned());
    a.set("error", e.to_owned());
    tracing::info!(target: "net", profile, error = %e, "connect failed");
    cx.toast(
        Level::Warn,
        urfin_i18n::current().fmt("net-connect-failed", &a),
    );
}

/// Вопросы профиля закрываются «Отменой».
fn drop_asks(st: &mut NetState, profile: &str) {
    let (gone, keep): (VecDeque<Ask>, VecDeque<Ask>) = std::mem::take(&mut st.asks)
        .into_iter()
        .partition(|a| a.profile.eq_ignore_ascii_case(profile));
    st.asks = keep;
    // Ответ «Отмена» — закрытый канал.
    drop(gone);
}

/// Подключились: история хостов и «последнее» профиля.
fn touch(st: &mut NetState, profile: &str, now: Datetime) {
    let Some(p) = st.profile(profile).cloned() else {
        return;
    };
    st.history
        .get_or_insert_with(History::default)
        .touch(&p.host, &p.user, now);
    if let Some(ps) = st.profiles.as_mut()
        && let Some(q) = ps
            .list
            .iter_mut()
            .find(|q| q.name.eq_ignore_ascii_case(profile))
    {
        q.last_used = Some(now);
        let q = q.clone();
        set_last_used(ps, &q, now);
    }
}

/// «Последнее» — в `connections.toml`: свой профиль — поле, импортированный — правка.
fn set_last_used(ps: &mut Profiles, p: &Profile, now: Datetime) {
    match p.source {
        ProfileSource::Urfin => {
            if let Some(f) = ps
                .file
                .profiles
                .iter_mut()
                .find(|f| f.name.eq_ignore_ascii_case(&p.name))
            {
                f.last_used = Some(now);
            }
        }
        ProfileSource::Config | ProfileSource::Putty => {
            match ps
                .file
                .overrides
                .iter_mut()
                .find(|o| o.name.eq_ignore_ascii_case(&p.name))
            {
                Some(o) => o.last_used = Some(now),
                None => ps.file.overrides.push(crate::ProfileOverride {
                    name: p.name.clone(),
                    last_used: Some(now),
                    ..crate::ProfileOverride::default()
                }),
            }
        }
        ProfileSource::History => {}
    }
}

/// Запись истории и `connections.toml` задачей Runtime.
fn save_history(cx: &mut Cx<'_>) {
    let Some(st) = cx.state.ext.get::<NetState>() else {
        return;
    };
    let Some(files) = st.files.clone() else {
        return;
    };
    let history = st.history.clone().unwrap_or_default();
    let file = st.profiles.as_ref().map(|p| p.file.clone());
    cx.rt
        .spawn(Lane::Bulk, "net.history", cx.rt.token(), move |_| {
            if let Err(e) = history.save(&files.history) {
                tracing::warn!(target: "net", error = %e, "history save");
            }
            if let Some(f) = file
                && let Err(e) = f.save(&files.connections)
            {
                tracing::warn!(target: "net", error = %e, "connections save");
            }
        });
}

/// Ответ на показанный вопрос (`ssh.answer`).
pub fn answer(
    cx: &mut Cx<'_>,
    choice: &str,
    text: Option<&str>,
    remember: bool,
) -> Result<(), String> {
    let Some(st) = cx.state.ext.get_mut::<NetState>() else {
        return Err("no ssh state".into());
    };
    let Some(mut a) = st.asks.pop_front() else {
        return Err("no question".into());
    };
    st.rev += 1;
    let plain = a.plain_host().to_owned();
    match &mut a.kind {
        AskKind::HostKey { reply, .. } => {
            let c = match choice {
                "trust" => HostKeyChoice::Trust,
                "once" => HostKeyChoice::Once,
                _ => HostKeyChoice::Cancel,
            };
            if let Some(r) = reply.take() {
                let _ = r.send(c);
            }
        }
        AskKind::HostChanged { reply, .. } => {
            // Замена — только с введённым именем хоста (F03 §2.4).
            let ok =
                choice == "replace" && text.is_some_and(|t| t.trim().eq_ignore_ascii_case(&plain));
            if choice == "replace" && !ok {
                st.asks.push_front(a);
                return Err("host name does not match".into());
            }
            if let Some(r) = reply.take() {
                let _ = r.send(ok);
            }
        }
        AskKind::Secret { reply, .. } => {
            if choice == "ok"
                && let Some(r) = reply.take()
            {
                let _ = r.send(Some(Answer {
                    text: text.unwrap_or_default().to_owned(),
                    remember,
                }));
            } else if choice == "cmd"
                && let Some(r) = reply.take()
            {
                // «Из менеджера паролей»: команда — в профиль, пароль — её ответом.
                let template = text.unwrap_or_default().to_owned();
                let p = st.profile(&a.profile).cloned();
                if let Some(mut p) = p.clone() {
                    template.clone_into(&mut p.password_cmd);
                    if matches!(
                        p.source,
                        ProfileSource::Urfin | ProfileSource::Config | ProfileSource::Putty
                    ) {
                        save_cmd(st, &p);
                    }
                }
                let (host, user, id) =
                    p.map_or_else(Default::default, |p| (p.host, p.user, p.password_id));
                let sink = cx.rt.sink();
                let profile = a.profile.clone();
                let task = async move {
                    let out = tokio::task::spawn_blocking(move || {
                        let vars = crate::pwcmd::Vars {
                            host: &host,
                            user: &user,
                            id: &id,
                        };
                        crate::pwcmd::argv(&template, vars)
                            .and_then(|argv| crate::pwcmd::run(&argv, crate::pwcmd::TIMEOUT))
                    })
                    .await;
                    match out {
                        Ok(Ok(crate::pwcmd::PwOutput::Password(pw))) => {
                            let _ = r.send(Some(Answer::from(pw.expose())));
                        }
                        Ok(Ok(crate::pwcmd::PwOutput::Key(_))) => {
                            let _ = r.send(None);
                        }
                        Ok(Err(e)) => {
                            let e = e.to_string();
                            let _ = sink.try_send(urfin_core::msg("net.pwcmd", move |cx| {
                                fail(cx, &profile, &e);
                            }));
                        }
                        Err(_) => {}
                    }
                };
                let st = cx.state.ext.get::<NetState>().expect("checked above");
                st.net_rt().spawn(cx.rt, task);
            }
        }
    }
    Ok(())
}

/// Команда менеджера паролей — в профиль (`password_cmd`), запись — задачей.
fn save_cmd(st: &mut NetState, p: &Profile) {
    let Some(ps) = st.profiles.as_mut() else {
        return;
    };
    if let Some(q) = ps
        .list
        .iter_mut()
        .find(|q| q.name.eq_ignore_ascii_case(&p.name))
    {
        q.password_cmd.clone_from(&p.password_cmd);
    }
    match p.source {
        ProfileSource::Urfin => {
            if let Some(f) = ps
                .file
                .profiles
                .iter_mut()
                .find(|f| f.name.eq_ignore_ascii_case(&p.name))
            {
                f.password_cmd.clone_from(&p.password_cmd);
            }
        }
        _ => match ps
            .file
            .overrides
            .iter_mut()
            .find(|o| o.name.eq_ignore_ascii_case(&p.name))
        {
            Some(o) => o.password_cmd = Some(p.password_cmd.clone()),
            None => ps.file.overrides.push(crate::ProfileOverride {
                name: p.name.clone(),
                password_cmd: Some(p.password_cmd.clone()),
                ..crate::ProfileOverride::default()
            }),
        },
    }
}

/// `ssh.disconnect`: закрыть сеанс; панель профиля — «нет связи».
pub fn disconnect(cx: &mut Cx<'_>, profile: &str) {
    let Some(st) = cx.state.ext.get_mut::<NetState>() else {
        return;
    };
    drop_asks(st, profile);
    if let Some(l) = st.live_mut(profile) {
        let _ = l.ctl.send(Control::Disconnect);
        l.state = LiveState::Disconnected;
    }
    st.sftp.detach(profile);
    st.rev += 1;
}

/// `ssh.reconnect`.
pub fn reconnect(cx: &mut Cx<'_>, profile: &str) {
    if let Some(l) = cx
        .state
        .ext
        .get_mut::<NetState>()
        .and_then(|s| s.live_mut(profile))
    {
        let _ = l.ctl.send(Control::ReconnectNow);
    }
}

/// Поля строки «Новое подключение» в профиль. Имя — хост (или `user@host`, если хост уже
/// занят другим профилем).
pub fn form_profile(
    st: &NetState,
    host: &str,
    user: &str,
    port: &str,
    via: &str,
    auth: Auth,
    edit: Option<&str>,
) -> Result<Profile, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("no host".into());
    }
    let port: u16 = match port.trim() {
        "" => profile::DEFAULT_PORT,
        p => p
            .parse()
            .ok()
            .filter(|&n| n != 0)
            .ok_or_else(|| format!("bad port: {p}"))?,
    };
    let mut p = if let Some(old) = edit.and_then(|e| st.profile(e)) {
        old.clone()
    } else {
        let name = if st.profile(host).is_some() && !user.trim().is_empty() {
            format!("{}@{host}", user.trim())
        } else {
            host.to_owned()
        };
        Profile::new(name, host)
    };
    host.clone_into(&mut p.host);
    p.port = port;
    user.trim().clone_into(&mut p.user);
    via.trim().clone_into(&mut p.via);
    p.auth = auth;
    Ok(p)
}

/// Сохранить профиль: свой — в `[[profile]]`, импортированный — правкой `[[override]]`.
pub fn save_profile(cx: &mut Cx<'_>, p: Profile) {
    let Some(st) = cx.state.ext.get_mut::<NetState>() else {
        return;
    };
    let ps = st.profiles.get_or_insert_with(Profiles::default);
    match p.source {
        ProfileSource::Config | ProfileSource::Putty => {
            let o = crate::ProfileOverride {
                name: p.name.clone(),
                host: Some(p.host.clone()),
                port: Some(p.port),
                user: Some(p.user.clone()),
                auth: Some(p.auth),
                via: Some(p.via.clone()),
                last_used: p.last_used,
                ..crate::ProfileOverride::default()
            };
            ps.file
                .overrides
                .retain(|q| !q.name.eq_ignore_ascii_case(&p.name));
            ps.file.overrides.push(o);
        }
        ProfileSource::Urfin | ProfileSource::History => {
            let mut own = p.clone();
            own.source = ProfileSource::Urfin;
            own.notes.clear();
            own.keepalive_s = None;
            ps.file
                .profiles
                .retain(|q| !q.name.eq_ignore_ascii_case(&p.name));
            ps.file.profiles.push(own);
        }
    }
    let src = if p.source == ProfileSource::History {
        ProfileSource::Urfin
    } else {
        p.source
    };
    ps.list.retain(|q| !q.name.eq_ignore_ascii_case(&p.name));
    ps.list.push(Profile { source: src, ..p });
    st.adhoc
        .retain(|q| !ps.list.iter().any(|l| l.name == q.name));
    st.rev += 1;
    save_history(cx);
}

/// `ssh.remove`: удалить свой профиль (импортированные не трогаются — их файлы только
/// читаются).
pub fn remove_profile(cx: &mut Cx<'_>, name: &str) -> Result<(), String> {
    let Some(st) = cx.state.ext.get_mut::<NetState>() else {
        return Err("no ssh state".into());
    };
    let ps = st.profiles.as_mut().ok_or("profiles not read")?;
    let own = ps
        .list
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(name) && p.source == ProfileSource::Urfin);
    if !own {
        return Err("only own profiles can be removed".into());
    }
    ps.list.retain(|p| !p.name.eq_ignore_ascii_case(name));
    ps.file
        .profiles
        .retain(|p| !p.name.eq_ignore_ascii_case(name));
    st.rev += 1;
    save_history(cx);
    Ok(())
}

/// Сейчас, UTC.
pub fn now() -> Datetime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    datetime(i64::try_from(secs).unwrap_or(0))
}

/// Секунды Unix → `Datetime` UTC.
pub fn datetime(secs: i64) -> Datetime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
    .parse()
    .expect("valid RFC 3339")
}

/// `Datetime` → секунды Unix (без даты — `None`; без зоны — как UTC).
pub fn unix(t: &Datetime) -> Option<i64> {
    let d = t.date?;
    let days = days_from_civil(i64::from(d.year), u32::from(d.month), u32::from(d.day));
    let (h, mi, s) = t.time.map_or((0, 0, 0), |t| {
        (
            i64::from(t.hour),
            i64::from(t.minute),
            i64::from(t.second.unwrap_or(0)),
        )
    });
    let off = match t.offset {
        Some(toml::value::Offset::Custom { minutes }) => i64::from(minutes) * 60,
        _ => 0,
    };
    Some(days * 86_400 + h * 3600 + mi * 60 + s - off)
}

/// Дни от 1970-01-01 → (год, месяц, день) (H. Hinnant, `civil_from_days`).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_roundtrip() {
        for secs in [0, 1_761_210_840, 951_782_400, 4_102_444_799] {
            assert_eq!(unix(&datetime(secs)), Some(secs), "{secs}");
        }
        assert_eq!(datetime(1_761_210_840).to_string(), "2025-10-23T09:14:00Z");
        let t: Datetime = "2026-10-23T12:14:00+03:00".parse().unwrap();
        assert_eq!(unix(&t), unix(&"2026-10-23T09:14:00Z".parse().unwrap()));
    }

    #[test]
    fn auth_ids_roundtrip() {
        for a in [Auth::Agent, Auth::Key, Auth::Password, Auth::Command] {
            assert_eq!(parse_auth(auth_id(a)), Some(a));
        }
        assert_eq!(parse_auth("nope"), None);
    }

    fn state() -> NetState {
        let mut st = NetState::new(crate::SshCfg::default(), None);
        let mut cfg = Profile::new("srv1", "srv1.corp.local");
        cfg.source = ProfileSource::Config;
        let own = Profile::new("web", "srv1-web.corp.local");
        let mut ps = Profiles::default();
        ps.list = vec![cfg, own];
        st.profiles = Some(ps);
        let mut h = History::default();
        h.touch("srv1-stage.corp.local", "admin", now());
        h.touch("srv1.corp.local", "admin", now());
        st.history = Some(h);
        st.known = vec![
            "srv1-db.corp.local".into(),
            "srv10.corp.local".into(),
            "db".into(),
        ];
        st
    }

    #[test]
    fn hints_by_source_without_repeats() {
        let st = state();
        let h = host_hints(&st, "SRV1");
        let hosts: Vec<&str> = h.iter().map(|h| h.host.as_str()).collect();
        assert_eq!(
            hosts,
            [
                "srv1.corp.local",
                "srv1-stage.corp.local",
                "srv1-db.corp.local",
                "srv10.corp.local",
                "srv1-web.corp.local"
            ]
        );
        assert_eq!(h[0].source, HintSource::Config("srv1".into()));
        assert!(matches!(h[1].source, HintSource::History(Some(_))));
        assert_eq!(h[2].source, HintSource::KnownHosts);
        assert_eq!(h[4].source, HintSource::Profile("web".into()));
        assert_eq!(host_hints(&st, "").len(), 6);
    }

    #[test]
    fn form_makes_profiles() {
        let st = state();
        let p = form_profile(
            &st,
            " db.corp ",
            "root",
            "2222",
            "srv1",
            Auth::Password,
            None,
        )
        .unwrap();
        assert_eq!(
            (
                p.name.as_str(),
                p.host.as_str(),
                p.port,
                p.user.as_str(),
                p.via.as_str()
            ),
            ("db.corp", "db.corp", 2222, "root", "srv1")
        );
        assert_eq!(p.auth, Auth::Password);
        // Имя хоста уже занято профилем — `user@host`.
        let p = form_profile(&st, "srv1", "admin", "", "", Auth::Agent, None).unwrap();
        assert_eq!((p.name.as_str(), p.port), ("admin@srv1", 22));
        // Правка — то же имя.
        let p = form_profile(&st, "10.0.0.5", "u", "22", "", Auth::Key, Some("web")).unwrap();
        assert_eq!((p.name.as_str(), p.host.as_str()), ("web", "10.0.0.5"));
        assert!(form_profile(&st, " ", "", "", "", Auth::Agent, None).is_err());
        assert!(form_profile(&st, "h", "", "0", "", Auth::Agent, None).is_err());
        assert!(form_profile(&st, "h", "", "x", "", Auth::Agent, None).is_err());
    }

    #[test]
    fn changed_key_needs_host_name() {
        let (tx, mut rx) = oneshot::channel();
        let a = Ask {
            id: 1,
            profile: "srv1".into(),
            host: "[srv1.corp.local]:2222".into(),
            user_host: String::new(),
            kind: AskKind::HostChanged {
                algo: "ssh-ed25519".into(),
                file: "known_hosts".into(),
                line: 14,
                old: "SHA256:a".into(),
                fingerprint: "SHA256:b".into(),
                reply: Some(tx),
            },
        };
        assert_eq!(a.plain_host(), "srv1.corp.local");
        drop(a);
        assert!(rx.try_recv().is_err(), "без ответа — «Отмена»");
    }

    #[test]
    fn last_used_goes_to_file_or_override() {
        let mut ps = Profiles::default();
        let mut own = Profile::new("web", "h");
        ps.file.profiles.push(own.clone());
        own.source = ProfileSource::Urfin;
        let t = datetime(1_761_210_840);
        set_last_used(&mut ps, &own, t);
        assert_eq!(ps.file.profiles[0].last_used, Some(t));
        let mut cfg = Profile::new("srv1", "h");
        cfg.source = ProfileSource::Config;
        set_last_used(&mut ps, &cfg, t);
        set_last_used(&mut ps, &cfg, t);
        assert_eq!(ps.file.overrides.len(), 1);
        assert_eq!(ps.file.overrides[0].last_used, Some(t));
    }
}
