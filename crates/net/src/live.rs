//! Подключения для UI (F03-05, F03 §2.4, §2.5, §2.8): сеансы профилей, вопросы
//! подключения (ключ сервера, пароль, код), строка «Новое подключение», чтение профилей и
//! истории. Данные для UI — [`NetModel`] из core в `AppState.ext` (F03-05a); каналы
//! сеансов и ответов — в [`crate::NetState`]. Ответы — командами `ssh.*`. Сеть и диск —
//! только задачами (рантайм крейта и Runtime), UI-поток лишь читает каналы.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use toml::value::Datetime;
use urfin_core::net::{Login, NetModel, sftp_profile};
use urfin_core::{AppState, Command, Cx, Lane, Level, MsgSink, Side, TabId};

pub use urfin_core::net::{
    Ask, AskKind, Form, HintSource, HostHint, Live, LiveState, auth_id, auth_key, datetime,
    parse_auth, unix, user_host,
};

use crate::config::ConnectionsFile;
use crate::conn::{self, Answer, ConnCfg, Control, Event, HostKeyChoice, Pool};
use crate::history::{self, History};
use crate::hostkey::KnownHosts;
use crate::profile::{self, Auth, Profile, ProfileSource};
use crate::sftp::Session;
use crate::uri::SftpUri;
use crate::{NetState, SftpFs};

/// Канал ответа на вопрос подключения ([`Ask::id`]); закрыт без ответа — «Отмена».
#[derive(Debug)]
pub(crate) enum Reply {
    HostKey(oneshot::Sender<HostKeyChoice>),
    Changed(oneshot::Sender<bool>),
    Secret(oneshot::Sender<Option<Answer>>),
}

fn model<'a>(cx: &'a mut Cx<'_>) -> Option<&'a mut NetModel> {
    cx.state.ext.get_mut::<NetModel>()
}

fn state<'a>(cx: &'a mut Cx<'_>) -> Option<&'a mut NetState> {
    cx.state.ext.get_mut::<NetState>()
}

/// Ключ канала управления сеансом: имя профиля без регистра.
fn key(profile: &str) -> String {
    profile.to_lowercase()
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
            let mut profiles = profile::load(&src);
            let history = history::load(&files.history).unwrap_or_else(|e| {
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
            let list = std::mem::take(&mut profiles.list);
            let file = profiles.file;
            let _ = t.sink.send(
                urfin_core::msg("net.profiles", move |cx| {
                    if let Some(st) = state(cx) {
                        st.file = Some(file);
                    }
                    if let Some(m) = model(cx) {
                        m.profiles = Some(list);
                        m.history = Some(history);
                        m.known = known;
                        m.host_keys = keys;
                        m.rev += 1;
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
    let (Some(m), Some(st)) = (
        cx.state.ext.get::<NetModel>(),
        cx.state.ext.get::<NetState>(),
    ) else {
        return Err("no ssh state".into());
    };
    let p = if let Some(p) = m.profile(name) {
        p.clone()
    } else {
        // `user@host:port` — разовый профиль (F03 §2.6).
        let input = format!("sftp://{name}/");
        let (t, _) = crate::uri::parse_input(&input).map_err(|e| e.to_string())?;
        crate::uri::resolve(m.list(), &t).0
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
    if m.live(&p.name)
        .is_some_and(|l| l.state == LiveState::Connecting)
    {
        return Ok(());
    }
    let chain = conn::chain(&p, |n| m.profile(n).cloned()).map_err(|e| e.to_string())?;
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
    // Память секретов — общая с терминалами профиля (F03 §0: пароль вводится один раз).
    let share = conn::Share::new(&cfg);
    let task = drive(
        profile.clone(),
        chain,
        cfg,
        pool,
        ctl_rx,
        sink,
        (sftp, Arc::clone(&share)),
    );
    let m = model(cx).expect("checked above");
    if p.source == ProfileSource::History && m.profile(&p.name).is_none() {
        m.adhoc.push(p.clone());
    }
    m.live.retain(|l| !l.profile.eq_ignore_ascii_case(&profile));
    m.live.push(Live {
        path,
        ..Live::new(&profile, &user_host(&p), LiveState::Connecting, side)
    });
    m.rev += 1;
    let rt = cx.rt;
    let st = state(cx).expect("checked above");
    st.ctl.insert(key(&profile), ctl_tx);
    st.shares.insert(key(&profile), share);
    if !st.net_rt().spawn(rt, task) {
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
    (sftp, share): (Arc<SftpFs>, Arc<conn::Share>),
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let forward = async {
        while let Some(ev) = rx.recv().await {
            let note = match ev {
                Event::Connected(c) => {
                    let (algo, login) = (c.algo.clone(), c.login.clone());
                    match Session::open(c).await {
                        Ok(s) => {
                            sftp.attach(s);
                            Note::Ready { algo, login }
                        }
                        Err(e) => Note::Failed(e.to_string()),
                    }
                }
                Event::Disconnected => {
                    sftp.detach(&profile);
                    Note::Event(Event::Disconnected)
                }
                Event::Lost {
                    attempt,
                    tries,
                    delay,
                } => {
                    // Обрыв с переподключением: операции ждут связи (F03-07).
                    sftp.lost(&profile);
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
    let _ = tokio::join!(
        conn::session_with(chain, cfg, pool, tx, ctl, share),
        forward
    );
}

/// Что пришло от сеанса.
#[derive(Debug)]
enum Note {
    Event(Event),
    /// Соединение поднято, канал SFTP открыт: ключ сервера и чем вошли.
    Ready {
        algo: String,
        login: Vec<Login>,
    },
    Failed(String),
}

/// Событие моста терминала профиля (вопрос помощника `--askpass`) — как у сеанса.
pub(crate) fn apply_event(cx: &mut Cx<'_>, profile: &str, ev: Event) {
    apply(cx, profile, Note::Event(ev));
}

fn apply(cx: &mut Cx<'_>, profile: &str, note: Note) {
    let Some(m) = model(cx) else {
        return;
    };
    m.rev += 1;
    match note {
        Note::Ready { algo, login } => {
            let Some(l) = m.live_mut(profile) else {
                return;
            };
            l.state = LiveState::Connected;
            l.phase = None;
            l.algo = algo;
            l.login = login;
            let (path, side) = (l.path.clone(), l.side);
            touch(cx, profile, now());
            resume(cx, profile, &path, side);
            save_history(cx);
            crate::term::on_ready(cx, profile);
        }
        Note::Failed(e) => fail(cx, profile, &e),
        Note::Event(ev) => match ev {
            Event::Phase { phase, .. } => {
                if let Some(l) = m.live_mut(profile) {
                    l.phase = Some(phase);
                }
            }
            Event::HostKeyUnknown {
                host,
                algo,
                fingerprint,
                reply,
            } => push_ask(
                cx,
                profile,
                &host,
                AskKind::HostKey { algo, fingerprint },
                Reply::HostKey(reply),
            ),
            Event::HostKeyChanged {
                host,
                algo,
                file,
                line,
                old_fingerprint,
                fingerprint,
                reply,
            } => push_ask(
                cx,
                profile,
                &host,
                AskKind::HostChanged {
                    algo,
                    file,
                    line,
                    old: old_fingerprint,
                    fingerprint,
                },
                Reply::Changed(reply),
            ),
            Event::Prompt {
                host,
                kind,
                text,
                echo,
                reply,
            } => push_ask(
                cx,
                profile,
                &host,
                AskKind::Secret { kind, text, echo },
                Reply::Secret(reply),
            ),
            Event::Connected(_) => {}
            Event::Lost {
                attempt,
                tries,
                delay,
            } => {
                if let Some(l) = m.live_mut(profile) {
                    l.state = LiveState::Lost {
                        attempt,
                        tries,
                        retry_at: Some(Instant::now() + delay),
                    };
                }
            }
            Event::Failed(e) => fail(cx, profile, &e.to_string()),
            Event::Disconnected => {
                drop_asks(cx, profile);
                if let Some(l) = model(cx).and_then(|m| m.live_mut(profile)) {
                    l.state = LiveState::Disconnected;
                }
            }
        },
    }
}

/// Табы с папками профиля (обе панели, все табы).
fn tabs_of(s: &AppState, profile: &str) -> Vec<TabId> {
    s.panels
        .iter()
        .flat_map(|p| &p.tabs)
        .filter(|t| sftp_profile(&t.loc).is_some_and(|p| p.eq_ignore_ascii_case(profile)))
        .map(|t| t.id)
        .collect()
}

/// Подключились или переподключились: табы профиля перечитываются на месте — курсор и
/// отметка остаются (F03-06, обрыв не стирает листинг); панель `side` не на профиле —
/// переход в `path`.
fn resume(cx: &mut Cx<'_>, profile: &str, path: &str, side: Side) {
    let tabs = tabs_of(cx.state, profile);
    for id in &tabs {
        cx.dispatch(Command::new("panel.refresh").arg("tab", i64::from(id.0)));
    }
    if !tabs.contains(&cx.state.panel(side).tab().id) {
        go(cx, profile, path, side);
    }
}

/// Вопрос в очередь модели, канал ответа — в [`NetState`].
fn push_ask(cx: &mut Cx<'_>, profile: &str, host: &str, kind: AskKind, reply: Reply) {
    let Some(m) = model(cx) else {
        return;
    };
    let user_host = m
        .live(profile)
        .map(|l| l.user_host.clone())
        .unwrap_or_default();
    let a = Ask::new(profile, host, &user_host, kind);
    let id = a.id;
    m.asks.push_back(a);
    if let Some(st) = state(cx) {
        st.replies.insert(id, reply);
    }
}

fn fail(cx: &mut Cx<'_>, profile: &str, e: &str) {
    drop_asks(cx, profile);
    if let Some(m) = model(cx) {
        m.live.retain(|l| !l.profile.eq_ignore_ascii_case(profile));
    }
    if let Some(st) = state(cx) {
        st.ctl.remove(&key(profile));
    }
    let mut a = urfin_i18n::FluentArgs::new();
    a.set("profile", profile.to_owned());
    a.set("error", e.to_owned());
    tracing::info!(target: "net", profile, error = %e, "connect failed");
    cx.toast(
        Level::Warn,
        urfin_i18n::current().fmt("net-connect-failed", &a),
    );
}

/// Вопросы профиля закрываются «Отменой»: каналы ответов закрываются.
fn drop_asks(cx: &mut Cx<'_>, profile: &str) {
    let Some(m) = model(cx) else {
        return;
    };
    let (gone, keep): (VecDeque<Ask>, VecDeque<Ask>) = std::mem::take(&mut m.asks)
        .into_iter()
        .partition(|a| a.profile.eq_ignore_ascii_case(profile));
    m.asks = keep;
    if let Some(st) = state(cx) {
        for a in gone {
            st.replies.remove(&a.id);
        }
    }
}

/// Подключились: история хостов и «последнее» профиля.
fn touch(cx: &mut Cx<'_>, profile: &str, now: Datetime) {
    let Some(m) = model(cx) else {
        return;
    };
    let Some(p) = m.profile(profile).cloned() else {
        return;
    };
    m.history
        .get_or_insert_with(History::default)
        .touch(&p.host, &p.user, now);
    let Some(q) = m.profiles.as_mut().and_then(|l| {
        let q = l
            .iter_mut()
            .find(|q| q.name.eq_ignore_ascii_case(profile))?;
        q.last_used = Some(now);
        Some(q.clone())
    }) else {
        return;
    };
    if let Some(st) = state(cx) {
        set_last_used(st.file.get_or_insert_with(Default::default), &q, now);
    }
}

/// «Последнее» — в `connections.toml`: свой профиль — поле, импортированный — правка.
fn set_last_used(file: &mut ConnectionsFile, p: &Profile, now: Datetime) {
    match p.source {
        ProfileSource::Urfin => {
            if let Some(f) = file
                .profiles
                .iter_mut()
                .find(|f| f.name.eq_ignore_ascii_case(&p.name))
            {
                f.last_used = Some(now);
            }
        }
        ProfileSource::Config | ProfileSource::Putty => {
            match file
                .overrides
                .iter_mut()
                .find(|o| o.name.eq_ignore_ascii_case(&p.name))
            {
                Some(o) => o.last_used = Some(now),
                None => file.overrides.push(crate::ProfileOverride {
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
    let (Some(m), Some(st)) = (
        cx.state.ext.get::<NetModel>(),
        cx.state.ext.get::<NetState>(),
    ) else {
        return;
    };
    let Some(files) = st.files.clone() else {
        return;
    };
    let hist = m.history.clone().unwrap_or_default();
    let file = st.file.clone();
    cx.rt
        .spawn(Lane::Bulk, "net.history", cx.rt.token(), move |_| {
            if let Err(e) = history::save(&hist, &files.history) {
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
    let Some(m) = model(cx) else {
        return Err("no ssh state".into());
    };
    let Some(ask) = m.asks.pop_front() else {
        return Err("no question".into());
    };
    m.rev += 1;
    if matches!(ask.kind, AskKind::HostChanged { .. })
        && choice == "replace"
        && !text.is_some_and(|t| t.trim().eq_ignore_ascii_case(ask.plain_host()))
    {
        // Замена — только с введённым именем хоста (F03 §2.4).
        m.asks.push_front(ask);
        return Err("host name does not match".into());
    }
    let owner = m.profile(&ask.profile).cloned();
    let Some(reply) = state(cx).and_then(|st| st.replies.remove(&ask.id)) else {
        // Канала нет (фикстура сцен) — вопрос просто закрыт.
        return Ok(());
    };
    match reply {
        Reply::HostKey(r) => {
            let c = match choice {
                "trust" => HostKeyChoice::Trust,
                "once" => HostKeyChoice::Once,
                _ => HostKeyChoice::Cancel,
            };
            let _ = r.send(c);
        }
        Reply::Changed(r) => {
            let _ = r.send(choice == "replace");
        }
        Reply::Secret(r) if choice == "ok" => {
            let _ = r.send(Some(Answer {
                text: text.unwrap_or_default().to_owned(),
                remember,
            }));
        }
        Reply::Secret(r) if choice == "cmd" => {
            // «Из менеджера паролей»: команда — в профиль, пароль — её ответом.
            let template = text.unwrap_or_default().to_owned();
            if let Some(mut p) = owner.clone() {
                template.clone_into(&mut p.password_cmd);
                if matches!(
                    p.source,
                    ProfileSource::Urfin | ProfileSource::Config | ProfileSource::Putty
                ) {
                    save_cmd(cx, &p);
                }
            }
            let (host, user, id) =
                owner.map_or_else(Default::default, |p| (p.host, p.user, p.password_id));
            let sink = cx.rt.sink();
            let profile = ask.profile.clone();
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
        // Прочее — «Отмена»: канал закрывается.
        Reply::Secret(_) => {}
    }
    Ok(())
}

/// Команда менеджера паролей — в профиль (`password_cmd`), запись — задачей.
fn save_cmd(cx: &mut Cx<'_>, p: &Profile) {
    let Some(list) = model(cx).and_then(|m| m.profiles.as_mut()) else {
        return;
    };
    if let Some(q) = list
        .iter_mut()
        .find(|q| q.name.eq_ignore_ascii_case(&p.name))
    {
        q.password_cmd.clone_from(&p.password_cmd);
    }
    let Some(st) = state(cx) else {
        return;
    };
    let file = st.file.get_or_insert_with(Default::default);
    match p.source {
        ProfileSource::Urfin => {
            if let Some(f) = file
                .profiles
                .iter_mut()
                .find(|f| f.name.eq_ignore_ascii_case(&p.name))
            {
                f.password_cmd.clone_from(&p.password_cmd);
            }
        }
        _ => match file
            .overrides
            .iter_mut()
            .find(|o| o.name.eq_ignore_ascii_case(&p.name))
        {
            Some(o) => o.password_cmd = Some(p.password_cmd.clone()),
            None => file.overrides.push(crate::ProfileOverride {
                name: p.name.clone(),
                password_cmd: Some(p.password_cmd.clone()),
                ..crate::ProfileOverride::default()
            }),
        },
    }
}

/// `ssh.disconnect`: закрыть сеанс; панель профиля — «нет связи».
pub fn disconnect(cx: &mut Cx<'_>, profile: &str) {
    drop_asks(cx, profile);
    let Some(m) = model(cx) else {
        return;
    };
    let had = if let Some(l) = m.live_mut(profile) {
        l.state = LiveState::Disconnected;
        true
    } else {
        false
    };
    m.rev += 1;
    if let Some(st) = state(cx) {
        if had && let Some(ctl) = st.ctl.get(&key(profile)) {
            let _ = ctl.send(Control::Disconnect);
        }
        st.sftp.detach(profile);
    }
}

/// `ssh.reconnect`: сеанс жив (обрыв, попытки исчерпаны) — попытка сейчас; сеанса нет
/// (отключились сами, не подключились) — новое подключение к папке таба профиля.
pub fn reconnect(cx: &mut Cx<'_>, profile: &str) -> Result<(), String> {
    let live = cx
        .state
        .ext
        .get::<NetModel>()
        .is_some_and(|m| m.live(profile).is_some());
    if live
        && let Some(ctl) = state(cx).and_then(|st| st.ctl.get(&key(profile)))
        && ctl.send(Control::ReconnectNow).is_ok()
    {
        return Ok(());
    }
    if let Some(st) = state(cx) {
        st.ctl.remove(&key(profile));
    }
    // Таб профиля: активная панель, затем другая.
    let s = &*cx.state;
    let shows = |side: Side| {
        let t = s.panel(side).tab();
        sftp_profile(&t.loc)
            .filter(|p| p.eq_ignore_ascii_case(profile))
            .map(|_| (side, crate::uri::SftpUri::from_location(&t.loc)))
    };
    let (side, path) = match shows(s.active).or_else(|| shows(s.active.other())) {
        Some((side, u)) => (side, u.map(|u| u.path)),
        None => (s.active, None),
    };
    open(cx, profile, path.as_deref(), side)
}

/// Поля строки «Новое подключение» в профиль. Имя — хост (или `user@host`, если хост уже
/// занят другим профилем).
pub fn form_profile(
    m: &NetModel,
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
    let mut p = if let Some(old) = edit.and_then(|e| m.profile(e)) {
        old.clone()
    } else {
        let name = if m.profile(host).is_some() && !user.trim().is_empty() {
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
    if let Some(st) = state(cx) {
        let file = st.file.get_or_insert_with(Default::default);
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
                file.overrides
                    .retain(|q| !q.name.eq_ignore_ascii_case(&p.name));
                file.overrides.push(o);
            }
            ProfileSource::Urfin | ProfileSource::History => {
                let mut own = p.clone();
                own.source = ProfileSource::Urfin;
                own.notes.clear();
                own.keepalive_s = None;
                file.profiles
                    .retain(|q| !q.name.eq_ignore_ascii_case(&p.name));
                file.profiles.push(own);
            }
        }
    }
    let Some(m) = model(cx) else {
        return;
    };
    let src = if p.source == ProfileSource::History {
        ProfileSource::Urfin
    } else {
        p.source
    };
    let list = m.profiles.get_or_insert_with(Vec::new);
    list.retain(|q| !q.name.eq_ignore_ascii_case(&p.name));
    list.push(Profile { source: src, ..p });
    m.adhoc.retain(|q| !list.iter().any(|l| l.name == q.name));
    m.rev += 1;
    save_history(cx);
}

/// `ssh.remove`: удалить свой профиль (импортированные не трогаются — их файлы только
/// читаются).
pub fn remove_profile(cx: &mut Cx<'_>, name: &str) -> Result<(), String> {
    let Some(m) = model(cx) else {
        return Err("no ssh state".into());
    };
    let list = m.profiles.as_mut().ok_or("profiles not read")?;
    let own = list
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(name) && p.source == ProfileSource::Urfin);
    if !own {
        return Err("only own profiles can be removed".into());
    }
    list.retain(|p| !p.name.eq_ignore_ascii_case(name));
    m.rev += 1;
    if let Some(file) = state(cx).and_then(|st| st.file.as_mut()) {
        file.profiles.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> NetModel {
        let mut m = NetModel::default();
        let mut cfg = Profile::new("srv1", "srv1.corp.local");
        cfg.source = ProfileSource::Config;
        let own = Profile::new("web", "srv1-web.corp.local");
        m.profiles = Some(vec![cfg, own]);
        m
    }

    #[test]
    fn form_makes_profiles() {
        let m = model();
        let p = form_profile(
            &m,
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
        let p = form_profile(&m, "srv1", "admin", "", "", Auth::Agent, None).unwrap();
        assert_eq!((p.name.as_str(), p.port), ("admin@srv1", 22));
        // Правка — то же имя.
        let p = form_profile(&m, "10.0.0.5", "u", "22", "", Auth::Key, Some("web")).unwrap();
        assert_eq!((p.name.as_str(), p.host.as_str()), ("web", "10.0.0.5"));
        assert!(form_profile(&m, " ", "", "", "", Auth::Agent, None).is_err());
        assert!(form_profile(&m, "h", "", "0", "", Auth::Agent, None).is_err());
        assert!(form_profile(&m, "h", "", "x", "", Auth::Agent, None).is_err());
    }

    #[test]
    fn closed_reply_is_cancel() {
        let (tx, mut rx) = oneshot::channel::<bool>();
        drop(Reply::Changed(tx));
        assert!(rx.try_recv().is_err(), "без ответа — «Отмена»");
    }

    #[test]
    fn last_used_goes_to_file_or_override() {
        let mut file = ConnectionsFile::default();
        let mut own = Profile::new("web", "h");
        file.profiles.push(own.clone());
        own.source = ProfileSource::Urfin;
        let t = datetime(1_761_210_840);
        set_last_used(&mut file, &own, t);
        assert_eq!(file.profiles[0].last_used, Some(t));
        let mut cfg = Profile::new("srv1", "h");
        cfg.source = ProfileSource::Config;
        set_last_used(&mut file, &cfg, t);
        set_last_used(&mut file, &cfg, t);
        assert_eq!(file.overrides.len(), 1);
        assert_eq!(file.overrides[0].last_used, Some(t));
    }
}
