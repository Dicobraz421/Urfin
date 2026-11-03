//! SSH-терминал и пара «панель + терминал» (F03 §3.1, §3.2, F03-09): `ssh.exe` в сеансе
//! `urfin-term` (`SessionKind::External`), мост `SSH_ASKPASS` и агент Урфина (§2.7) из
//! общей памяти сеанса профиля, папка терминала по OSC 7 → панель, `ssh.cd_here`.
//!
//! Терминал стартует, когда соединение SFTP профиля поднято: ключ сервера уже проверен
//! (`ssh.exe` вопроса не задаёт), пароль — в памяти сеанса (вводится один раз).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use urfin_core::net::{NetModel, Pair, PairCwd, TermLink, TermLogin, posix_normalize, same_host};
use urfin_core::{Command, Cx, Focus, Level, Side, UiEvent};

use crate::bridge::{self, askpass, ssh_exe};
use crate::conn::{self, ConnCfg, Share, expand_home};
use crate::profile::{Auth, Profile, ProfileSource};
use crate::uri::SftpUri;
use crate::{NetState, live};

/// Без OSC 7 столько после старта — «папка: сервер не сообщает».
pub const OSC7_WAIT: Duration = Duration::from_secs(5);

/// Аргументы `ssh.exe` (F03 §3.1): профиль из `ssh_config` — `ssh.exe <Host>`; свой —
/// `-p <port> [-i <key>] [-J <цепочка>] <user>@<host>`. Опции `-o` (§2.4, §2.7) — до хоста.
pub fn argv(
    ssh: &Path,
    p: &Profile,
    jumps: &[Profile],
    key: Option<&Path>,
    options: &[String],
) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec![ssh.as_os_str().to_owned()];
    out.extend(options.iter().map(OsString::from));
    if p.source == ProfileSource::Config {
        out.push(p.name.clone().into());
        return out;
    }
    out.push("-p".into());
    out.push(p.port.to_string().into());
    if let Some(k) = key {
        out.push("-i".into());
        out.push(k.as_os_str().to_owned());
    }
    if !jumps.is_empty() {
        let via: Vec<String> = jumps
            .iter()
            .map(|j| format!("{}:{}", at(&j.user, &j.host), j.port))
            .collect();
        out.push("-J".into());
        out.push(via.join(",").into());
    }
    out.push(at(&p.user, &p.host).into());
    out
}

fn at(user: &str, host: &str) -> String {
    if user.is_empty() {
        host.to_owned()
    } else {
        format!("{user}@{host}")
    }
}

/// `ssh.cd_here` (F03 §3.2): очистить строку (Ctrl+U) и `cd -- '<путь>'`, Enter.
pub fn cd_bytes(path: &str) -> Vec<u8> {
    format!("\x15cd -- '{}'\r", path.replace('\'', r"'\''")).into_bytes()
}

/// Чем войдёт `ssh.exe` — подпись статуса пары.
fn login_of(p: &Profile, bridge: bool, agent_algo: Option<String>) -> TermLogin {
    if let Some(algo) = agent_algo {
        return TermLogin::Agent { algo: Some(algo) };
    }
    if !bridge && matches!(p.auth, Auth::Password | Auth::Command) {
        return TermLogin::InTerminal;
    }
    match p.auth {
        Auth::Agent => TermLogin::Agent { algo: None },
        Auth::Key if !p.key.is_empty() => TermLogin::Key(PathBuf::from(&p.key)),
        Auth::Key => TermLogin::Agent { algo: None },
        Auth::Password | Auth::Command => TermLogin::Password,
    }
}

/// `ssh-ed25519` → `ed25519`.
fn short_algo(a: &str) -> String {
    a.strip_prefix("ssh-").unwrap_or(a).to_owned()
}

fn model<'a>(cx: &'a mut Cx<'_>) -> Option<&'a mut NetModel> {
    cx.state.ext.get_mut::<NetModel>()
}

fn state<'a>(cx: &'a mut Cx<'_>) -> Option<&'a mut NetState> {
    cx.state.ext.get_mut::<NetState>()
}

fn side_name(side: Side) -> &'static str {
    if side == Side::Left { "left" } else { "right" }
}

/// `ssh.terminal {profile?, side?, session?, new?}`: пара профиля — панель `sftp://`
/// сверху, терминал под ней; `session` — показать в паре другой сеанс профиля, `new` —
/// ещё один сеанс («Новый сеанс» в меню шапки).
pub fn terminal(
    cx: &mut Cx<'_>,
    profile: &str,
    side: Option<Side>,
    session: Option<u32>,
    new: bool,
) -> Result<(), String> {
    let s = &*cx.state;
    let on_profile = |side: Side| {
        urfin_core::net::sftp_profile(&s.panel(side).tab().loc)
            .is_some_and(|p| p.eq_ignore_ascii_case(profile))
    };
    let side = side.unwrap_or_else(|| {
        if on_profile(s.active) {
            s.active
        } else if on_profile(s.active.other()) {
            s.active.other()
        } else {
            s.active.other()
        }
    });
    let m = model(cx).ok_or("no ssh state")?;
    if let Some(id) = session {
        let p = m.pair.get_or_insert_with(|| Pair::new(profile, side));
        p.term = Some(id);
        p.cwd = PairCwd::Wait;
        p.host = None;
        m.rev += 1;
        return focus_term(cx);
    }
    let keep = m
        .pair
        .as_ref()
        .is_some_and(|p| p.profile.eq_ignore_ascii_case(profile) && p.term.is_some());
    if keep && !new {
        return focus_term(cx);
    }
    let mut pair = Pair::new(profile, side);
    if let Some(old) = m.pair.take() {
        pair.back = old.back;
    }
    m.pair = Some(pair);
    m.rev += 1;
    let up = cx
        .state
        .ext
        .get::<NetState>()
        .is_some_and(|st| st.sftp.session(profile).is_some());
    if up {
        if !on_panel(cx, profile, side) {
            live::open(cx, profile, None, side)?;
        }
        start(cx, profile);
    } else {
        // Терминал — после подключения панели (`live::apply`, `on_ready`).
        if let Some(st) = state(cx) {
            st.pending_term = Some(profile.to_lowercase());
        }
        live::open(cx, profile, None, side)?;
    }
    Ok(())
}

fn on_panel(cx: &Cx<'_>, profile: &str, side: Side) -> bool {
    urfin_core::net::sftp_profile(&cx.state.panel(side).tab().loc)
        .is_some_and(|p| p.eq_ignore_ascii_case(profile))
}

/// Соединение профиля поднято (`live::apply`): ждущий терминал стартует.
pub(crate) fn on_ready(cx: &mut Cx<'_>, profile: &str) {
    let wanted = state(cx).is_some_and(|st| {
        st.pending_term
            .take_if(|p| p.eq_ignore_ascii_case(profile))
            .is_some()
    });
    if wanted {
        start(cx, profile);
    }
}

/// Что нужно задаче моста из UI-потока.
struct Plan {
    profile: Profile,
    jumps: Vec<Profile>,
    host: String,
    key: Option<PathBuf>,
    known_hosts: Vec<PathBuf>,
    cache: PathBuf,
    urfin: PathBuf,
    share: Arc<Share>,
}

/// Запуск `ssh.exe`: поиск и версия — задачей, pipe моста — на потоке крейта, сеанс
/// терминала — сообщением в UI, когда pipe уже слушают.
fn start(cx: &mut Cx<'_>, profile: &str) {
    let plan = (|| {
        let m = cx.state.ext.get::<NetModel>()?;
        let st = cx.state.ext.get::<NetState>()?;
        let p = m.profile(profile)?.clone();
        let chain = conn::chain(&p, |n| m.profile(n).cloned()).ok()?;
        let jumps = chain[..chain.len() - 1].to_vec();
        let files = st.files.as_ref()?;
        let home = dirs::home_dir();
        let cfg = ConnCfg::new(&st.cfg, home.as_deref(), Some(&files.state_dir));
        let share = st
            .shares
            .get(&profile.to_lowercase())
            .cloned()
            .unwrap_or_else(|| Share::new(&cfg));
        let mut known_hosts = cfg.known_hosts.clone();
        known_hosts.extend(share.known_hosts());
        let key = (p.auth == Auth::Key && !p.key.is_empty())
            .then(|| expand_home(&p.key, home.as_deref()))
            .flatten();
        Some(Plan {
            host: crate::hostkey::host_name(&p.host, p.port),
            profile: p,
            jumps,
            key,
            known_hosts,
            cache: files.state_dir.join("ssh").join("ssh_exe.toml"),
            urfin: std::env::current_exe().ok()?,
            share,
        })
    })();
    let Some(plan) = plan else {
        tracing::info!(target: "net", profile, "terminal: no profile or files");
        return;
    };
    let (stop_tx, stop_rx) = oneshot::channel();
    let nonce = bridge::nonce();
    let sink = cx.rt.sink();
    let rt = cx.rt;
    let Some(st) = state(cx) else { return };
    st.stops.insert(nonce.clone(), stop_tx);
    let task = run_bridge(plan, nonce, sink, stop_rx);
    if !st.net_rt().spawn(rt, task) {
        tracing::warn!(target: "net", "terminal: net runtime is stopped");
    }
}

/// Готово к запуску `ssh.exe`.
struct Ready {
    nonce: String,
    profile: String,
    argv: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    login: TermLogin,
    /// Мост недоступен: OpenSSH старше 8.9.
    old: bool,
}

/// Задача моста сеанса: `ssh.exe` и его версия, pipe `--askpass` и агента, затем
/// ответы помощнику до остановки (сеанс терминала закрыт).
#[cfg(windows)]
async fn run_bridge(
    plan: Plan,
    nonce: String,
    sink: urfin_core::MsgSink,
    stop: oneshot::Receiver<()>,
) {
    let span = tracing::info_span!(target: "net", "ssh.term.spawn", profile = %plan.profile.name);
    let _g = span.enter();
    let cache = plan.cache.clone();
    let exe = tokio::task::spawn_blocking(move || ssh_exe::detect(&cache))
        .await
        .ok()
        .flatten();
    let profile = plan.profile.name.clone();
    let Some(exe) = exe else {
        let _ = sink.try_send(urfin_core::msg("net.term", move |cx| {
            cx.toast(Level::Error, urfin_i18n::current().get("net-term-no-ssh"));
            if let Some(m) = model(cx) {
                m.pair.take_if(|p| p.term.is_none());
                m.rev += 1;
            }
        }));
        return;
    };
    let bridge_ok = exe.bridge_ok();
    // Ключи менеджера паролей (PEM в памяти сеанса) — агенту Урфина.
    let keys: Vec<russh::keys::PrivateKey> = std::iter::once(plan.host.as_str())
        .chain(plan.jumps.iter().map(|j| j.host.as_str()))
        .filter_map(|h| plan.share.secrets.get(&crate::auth::key_slot(h)))
        .filter_map(|pem| russh::keys::decode_secret_key(pem.expose(), None).ok())
        .collect();
    let mut env = Vec::new();
    let mut agent = None;
    let mut ask_fut = None;
    let mut agent_fut = None;
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    if bridge_ok {
        let pipe = bridge::askpass_pipe(&nonce);
        let (tx, rx) = mpsc::unbounded_channel();
        match askpass::bind(pipe.clone(), tx) {
            Ok(f) => {
                ask_fut = Some(f);
                env = bridge::askpass_env(&plan.urfin, &pipe);
                tokio::spawn(askpass::respond(
                    rx,
                    plan.share.secrets.clone(),
                    ev_tx.clone(),
                    plan.host.clone(),
                ));
            }
            Err(e) => tracing::warn!(target: "net", error = %e, "askpass pipe"),
        }
        if !keys.is_empty() {
            let name = bridge::agent_pipe(&nonce);
            match bridge::agent::pipe_listener(name.clone()) {
                Ok(l) => {
                    agent_fut = Some(bridge::agent::serve(l, keys.clone()));
                    agent = Some(name);
                }
                Err(e) => tracing::warn!(target: "net", error = %e, "agent pipe"),
            }
        }
    }
    let algo = keys
        .first()
        .filter(|_| agent.is_some())
        .map(|k| short_algo(k.algorithm().as_str()));
    let options = bridge::ssh_options(&plan.known_hosts, agent.as_deref());
    let ready = Ready {
        nonce: nonce.clone(),
        profile: profile.clone(),
        argv: argv(
            &exe.path,
            &plan.profile,
            &plan.jumps,
            plan.key.as_deref(),
            &options,
        ),
        env,
        login: login_of(&plan.profile, bridge_ok, algo),
        old: !bridge_ok,
    };
    let _ = sink.try_send(urfin_core::msg("net.term", move |cx| opened(cx, ready)));
    // Вопросы помощника (код 2FA, фраза-пароль) — строкой ввода, как у SFTP.
    let forward = {
        let sink = sink.clone();
        let profile = profile.clone();
        async move {
            while let Some(ev) = ev_rx.recv().await {
                let p = profile.clone();
                let _ = sink.try_send(urfin_core::msg("net.term", move |cx| {
                    live::apply_event(cx, &p, ev);
                }));
            }
        }
    };
    let share = plan.share;
    let serve = async move {
        let ask = async {
            match ask_fut {
                Some(f) => f.await,
                None => std::future::pending().await,
            }
        };
        let agent = async {
            match agent_fut {
                Some(f) => f.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = ask => tracing::debug!(target: "net", ?r, "askpass stopped"),
            r = agent => tracing::debug!(target: "net", ?r, "agent stopped"),
            () = forward => {}
        }
    };
    tokio::select! {
        _ = stop => {}
        () = serve => {}
    }
    drop(ev_tx);
    // Папка «Только в этот раз» живёт, пока жив терминал.
    drop(share);
    tracing::debug!(target: "net", profile, "terminal bridge closed");
}

#[cfg(not(windows))]
async fn run_bridge(
    _plan: Plan,
    _nonce: String,
    _sink: urfin_core::MsgSink,
    _stop: oneshot::Receiver<()>,
) {
}

/// Pipe моста слушают: сеанс терминала с `ssh.exe`.
fn opened(cx: &mut Cx<'_>, r: Ready) {
    if r.old {
        cx.toast(Level::Warn, urfin_i18n::current().get("net-term-old-ssh"));
    }
    let ext = urfin_term::manager::External {
        argv: r.argv,
        env: r.env,
        title: format!("ssh {}", r.profile),
    };
    let back = active_origin(cx.state);
    let id = match urfin_term::open_external(cx, ext, true) {
        Ok(id) => id,
        Err(e) => {
            if let Some(st) = state(cx) {
                st.stops.remove(&r.nonce);
            }
            cx.toast(Level::Error, e.to_string());
            return;
        }
    };
    if let Some(st) = state(cx) {
        if let Some(stop) = st.stops.remove(&r.nonce) {
            st.bridges.insert(id, stop);
        }
        st.term_started.insert(id, Instant::now());
    }
    cx.rt.waker().wake_after(OSC7_WAIT);
    let Some(m) = model(cx) else { return };
    let n = m
        .terms
        .iter()
        .filter(|t| t.profile.eq_ignore_ascii_case(&r.profile))
        .map(|t| t.n)
        .max()
        .unwrap_or(0)
        + 1;
    m.terms.push(TermLink {
        id,
        profile: r.profile.clone(),
        n,
        login: r.login,
    });
    if let Some(p) = m
        .pair
        .as_mut()
        .filter(|p| p.profile.eq_ignore_ascii_case(&r.profile))
    {
        p.term = Some(id);
        p.focus_term = true;
        p.cwd = PairCwd::Wait;
        p.host = None;
        if p.back.is_none() {
            p.back = back.filter(|b| *b != id);
        }
    }
    m.rev += 1;
    cx.state.focus = Focus::Terminal;
    cx.ui(UiEvent::FocusChanged(Focus::Terminal));
}

/// Первый id цепочки активного сеанса терминала.
fn active_origin(s: &urfin_core::AppState) -> Option<u32> {
    s.ext
        .get::<urfin_term::TermState>()?
        .sessions
        .active()
        .map(|a| a.origin)
}

/// Фокус — терминалу пары: его сеанс активен (ввод — в него), прежний запомнен.
fn focus_term(cx: &mut Cx<'_>) -> Result<(), String> {
    let back = active_origin(cx.state);
    let Some(p) = model(cx).and_then(|m| m.pair.as_mut()) else {
        return Err("no pair".into());
    };
    let Some(id) = p.term else {
        return Ok(()); // ещё запускается: фокус придёт с сеансом
    };
    if back != Some(id) {
        p.back = back;
    }
    p.focus_term = true;
    let side = p.side;
    if let Some(m) = model(cx) {
        m.rev += 1;
    }
    urfin_term::activate(cx, id).map_err(|e| e.to_string())?;
    cx.state.active = side;
    cx.state.focus = Focus::Terminal;
    cx.ui(UiEvent::FocusChanged(Focus::Terminal));
    Ok(())
}

/// Фокус — панели пары; активным снова становится прежний сеанс терминала.
fn focus_panel(cx: &mut Cx<'_>) {
    let Some(p) = model(cx).and_then(|m| m.pair.as_mut()) else {
        return;
    };
    p.focus_term = false;
    let (side, back) = (p.side, p.back.take());
    if let Some(m) = model(cx) {
        m.rev += 1;
    }
    if let Some(b) = back {
        let _ = urfin_term::activate(cx, b);
    }
    cx.state.focus = Focus::Panel;
    cx.ui(UiEvent::FocusChanged(Focus::Panel));
    cx.dispatch(Command::new("panel.focus").arg("side", side_name(side)));
}

/// `ssh.pair_focus` (Ctrl+Tab, `in_sshpair`): панель ↔ терминал.
pub fn pair_focus(cx: &mut Cx<'_>) -> Result<(), String> {
    let p = cx
        .state
        .ext
        .get::<NetModel>()
        .and_then(|m| m.pair.as_ref())
        .ok_or("no pair")?;
    if p.focus_term && cx.state.focus == Focus::Terminal {
        focus_panel(cx);
        Ok(())
    } else {
        focus_term(cx)
    }
}

/// `ssh.cd_here` («Перейти в терминале сюда»): папка панели пары — в терминал.
pub fn cd_here(cx: &mut Cx<'_>) -> Result<(), String> {
    let p = cx
        .state
        .ext
        .get::<NetModel>()
        .and_then(|m| m.pair.as_ref())
        .ok_or("no pair")?;
    let id = p.term.ok_or("terminal is starting")?;
    let uri = SftpUri::from_location(&cx.state.panel(p.side).tab().loc)
        .filter(|u| u.profile.eq_ignore_ascii_case(&p.profile))
        .ok_or("panel is not on the server")?;
    urfin_term::write_to(cx, id, cd_bytes(&uri.path)).map_err(|e| e.to_string())
}

/// Каждый pump: мосты закрытых сеансов, пара без терминала, OSC 7 → панель, фокус.
pub(crate) fn pump(cx: &mut Cx<'_>) {
    let alive = |s: &urfin_core::AppState, id: u32| urfin_term::session_by_origin(s, id).is_some();
    let gone: Vec<u32> = cx
        .state
        .ext
        .get::<NetState>()
        .map(|st| {
            st.bridges
                .keys()
                .copied()
                .filter(|id| !alive(cx.state, *id))
                .collect()
        })
        .unwrap_or_default();
    if let Some(st) = cx.state.ext.get_mut::<NetState>() {
        for id in &gone {
            st.bridges.remove(id);
            st.term_started.remove(id);
        }
    }
    let Some(m) = cx.state.ext.get::<NetModel>() else {
        return;
    };
    let dead: Vec<u32> = m
        .terms
        .iter()
        .map(|t| t.id)
        .filter(|id| !alive(cx.state, *id))
        .collect();
    let pair = m.pair.clone();
    if !dead.is_empty()
        && let Some(m) = model(cx)
    {
        m.terms.retain(|t| !dead.contains(&t.id));
        m.rev += 1;
    }
    let Some(pair) = pair else { return };
    let Some(id) = pair.term else { return };
    // Сеанс пары закрыт (`term.close`): пары нет, фокус — панели.
    if !alive(cx.state, id) {
        if let Some(m) = model(cx) {
            m.pair = None;
            m.rev += 1;
        }
        if cx.state.focus == Focus::Terminal && pair.focus_term {
            cx.state.focus = Focus::Panel;
            cx.ui(UiEvent::FocusChanged(Focus::Panel));
        }
        return;
    }
    // Фокус ушёл из терминала пары (Tab, клик в панель): он больше не у пары.
    if pair.focus_term && matches!(cx.state.focus, Focus::Panel | Focus::Cmdline) {
        if let Some(p) = model(cx).and_then(|m| m.pair.as_mut()) {
            p.focus_term = false;
            if let Some(b) = p.back.take() {
                let _ = urfin_term::activate(cx, b);
            }
        }
        if let Some(m) = model(cx) {
            m.rev += 1;
        }
    }
    sync_cwd(cx, &pair, id);
}

/// OSC 7 терминала пары → папка панели (F03 §3.2): хост сверяется с профилем, путь
/// нормализуется; панель не на профиле — не трогается.
fn sync_cwd(cx: &mut Cx<'_>, pair: &Pair, id: u32) {
    let remote = urfin_term::session_by_origin(cx.state, id).and_then(|s| s.remote.clone());
    let seen = cx
        .state
        .ext
        .get::<NetState>()
        .and_then(|st| st.osc_seen.get(&id).copied())
        .unwrap_or(0);
    let Some(r) = remote.filter(|r| r.seq > seen) else {
        // Сервер молчит дольше `OSC7_WAIT` — подпись в статусе.
        let silent = pair.cwd == PairCwd::Wait
            && cx
                .state
                .ext
                .get::<NetState>()
                .and_then(|st| st.term_started.get(&id))
                .is_some_and(|t| t.elapsed() >= OSC7_WAIT);
        if silent && let Some(p) = model(cx).and_then(|m| m.pair.as_mut()) {
            p.cwd = PairCwd::Silent;
            if let Some(m) = model(cx) {
                m.rev += 1;
            }
        }
        return;
    };
    if let Some(st) = state(cx) {
        st.osc_seen.insert(id, r.seq);
    }
    let Some(m) = cx.state.ext.get::<NetModel>() else {
        return;
    };
    let host = m
        .profile(&pair.profile)
        .map(|p| p.host.clone())
        .unwrap_or_default();
    // Первый OSC 7 — от сервера профиля (на нём оболочка), его хост запоминается:
    // имя хоста сервера часто не совпадает с адресом профиля (IP). Дальше другой хост —
    // вложенный `ssh`, панель не двигается.
    let trusted = match &pair.host {
        None => true,
        Some(first) => {
            r.host.eq_ignore_ascii_case(first) || same_host(&r.host, &host, &pair.profile)
        }
    };
    if !trusted {
        tracing::debug!(target: "net", host = %r.host, "osc 7 from another host");
        return;
    }
    let path = posix_normalize(&r.path);
    if let Some(p) = model(cx).and_then(|m| m.pair.as_mut()) {
        p.host.get_or_insert_with(|| r.host.clone());
        p.cwd = PairCwd::At(path.clone());
    }
    if let Some(m) = model(cx) {
        m.rev += 1;
    }
    let on = SftpUri::from_location(&cx.state.panel(pair.side).tab().loc)
        .filter(|u| u.profile.eq_ignore_ascii_case(&pair.profile));
    if on.is_some_and(|u| posix_normalize(&u.path) != path) {
        let loc = SftpUri::new(&pair.profile, &path).to_location();
        cx.dispatch(
            Command::new("panel.go")
                .arg("side", side_name(pair.side))
                .arg("path", loc),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, host: &str, user: &str, port: u16) -> Profile {
        let mut p = Profile::new(name, host);
        user.clone_into(&mut p.user);
        p.port = port;
        p
    }

    fn strs(v: &[OsString]) -> Vec<String> {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn argv_own_and_config() {
        let ssh = Path::new(r"C:\Windows\System32\OpenSSH\ssh.exe");
        let o = bridge::ssh_options(&[PathBuf::from(r"C:\h\known_hosts")], None);
        let own = p("web", "10.0.0.5", "admin", 2222);
        let jumps = [
            p("bastion", "bastion.corp", "jump", 22),
            p("j2", "10.1.1.1", "", 2200),
        ];
        let key = PathBuf::from(r"C:\h\.ssh\id_ed25519");
        assert_eq!(
            strs(&argv(ssh, &own, &jumps, Some(&key), &o)),
            [
                r"C:\Windows\System32\OpenSSH\ssh.exe",
                "-o",
                r#"UserKnownHostsFile="C:/h/known_hosts""#,
                "-o",
                "StrictHostKeyChecking=yes",
                "-p",
                "2222",
                "-i",
                r"C:\h\.ssh\id_ed25519",
                "-J",
                "jump@bastion.corp:22,10.1.1.1:2200",
                "admin@10.0.0.5",
            ]
        );
        let mut cfg = p("srv1", "srv1.corp.local", "admin", 22);
        cfg.source = ProfileSource::Config;
        let v = strs(&argv(ssh, &cfg, &[], None, &o));
        assert_eq!(v.last().map(String::as_str), Some("srv1"));
        assert!(!v.iter().any(|a| a == "-p"));
        assert_eq!(v.len(), 6);
    }

    #[test]
    fn cd_here_quotes() {
        assert_eq!(cd_bytes("/var/www/app"), b"\x15cd -- '/var/www/app'\r");
        assert_eq!(cd_bytes("/tmp/it's"), b"\x15cd -- '/tmp/it'\\''s'\r");
    }

    #[test]
    fn login_labels() {
        let mut pr = p("srv1", "h", "u", 22);
        assert_eq!(
            login_of(&pr, true, Some("ed25519".into())),
            TermLogin::Agent {
                algo: Some("ed25519".into())
            }
        );
        assert_eq!(login_of(&pr, true, None), TermLogin::Agent { algo: None });
        pr.auth = Auth::Password;
        assert_eq!(login_of(&pr, true, None), TermLogin::Password);
        assert_eq!(login_of(&pr, false, None), TermLogin::InTerminal);
        pr.auth = Auth::Key;
        pr.key = "~/.ssh/id_rsa".into();
        assert_eq!(
            login_of(&pr, true, None),
            TermLogin::Key(PathBuf::from("~/.ssh/id_rsa"))
        );
        assert_eq!(short_algo("ssh-ed25519"), "ed25519");
    }
}
