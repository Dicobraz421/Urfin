//! `[pkg]` на лету (F02-12): раздел настроек «Пакеты» и `pkg.important.toggle`. Новый
//! конфиг (`app.reload_config`) и своя правка идут одним путём: книга, модель, вкладки.

use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use urfin_core::pkg::{BadPkgId, Filter, PkgId, PkgModel, filter_of};
use urfin_core::{CmdError, Command, Cx, Lane, Row, msg, settings};

use super::{CACHE_LOADING, CACHE_NONE, CACHE_READY, Env, cached_managers, refresh_tabs};
use crate::book::{CACHE_FILE, PkgCache};
use crate::config::PkgConfig;

/// Действующий `[pkg]`: книга (порядок, «важное»), модель для UI, состояние крейта;
/// открытые вкладки перестраиваются.
pub(super) fn apply(cx: &mut Cx<'_>, env: &Env, cfg: PkgConfig) {
    env.book.configure(&cfg);
    if let Some(m) = cx.state.ext.get_mut::<PkgModel>() {
        m.config.clone_from(&cfg);
    }
    if let Some(p) = cx.state.ext.get_mut::<crate::PkgState>() {
        p.config.clone_from(&cfg);
    }
    *env.config.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(cfg);
    refresh_tabs(cx);
}

/// Новый конфиг: `[pkg]` на лету (без изменений — ничего).
pub(super) fn follow(cx: &mut Cx<'_>, env: &Env, seen: &mut Option<Arc<urfin_core::Config>>) {
    let fresh = Arc::clone(&cx.state.config);
    match seen.replace(Arc::clone(&fresh)) {
        None => return,
        Some(old) if Arc::ptr_eq(&old, &fresh) => return,
        Some(_) => {}
    }
    let (cfg, _) = fresh.section::<PkgConfig>("pkg");
    if cfg != *env.config() {
        apply(cx, env, cfg);
    }
}

/// Сверка при входе в раздел настроек: кэш (подписи важных, поиск по установленным) и
/// найденные менеджеры — в модель, без запросов к менеджерам (F02 §2.5).
pub(super) fn survey(cx: &mut Cx<'_>, env: &Arc<Env>) {
    let read = env
        .cache
        .compare_exchange(
            CACHE_NONE,
            CACHE_LOADING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    let env = Arc::clone(env);
    cx.rt
        .spawn(Lane::Interactive, "pkg.survey", cx.rt.token(), move |tcx| {
            if read {
                let dir = env.dir.as_ref();
                env.book
                    .load(dir.and_then(|d| PkgCache::load(&d.join(CACHE_FILE))));
            }
            env.book.known(env.source.detect(&cached_managers(&env)));
            let m = msg("pkg.survey", move |cx| {
                if read {
                    env.cache.store(CACHE_READY, Ordering::Release);
                }
                refresh_tabs(cx);
            });
            let _ = tcx.sink.send(m, &tcx.cancel);
        });
}

/// Пакет команды: аргумент `id` (чипы настроек) или строка под курсором вкладки `pkg:`
/// (контекстное меню); в поиске группа — её рекомендуемый вариант.
fn target(cmd: &Command, cx: &Cx<'_>, env: &Env) -> Result<Option<PkgId>, CmdError> {
    if let Some(text) = cmd.args.str("id") {
        return text.parse().map(Some).map_err(|e: BadPkgId| CmdError::Arg {
            name: "id".into(),
            msg: e.to_string().into(),
        });
    }
    let t = cx.state.active_tab();
    let (Some(f), Some(Row::Entry { entry, .. })) = (filter_of(&t.loc), t.current()) else {
        return Ok(None);
    };
    let view = env.book.view();
    Ok(match f {
        Filter::Search => view.search.row(&entry.name).map(|r| r.target().clone()),
        _ => view.row(f, &entry.name).map(|r| r.pkg.id.clone()),
    })
}

/// `pkg.important.toggle id?`: в «важные» или обратно (со всеми двойниками строки);
/// применяется сразу, в конфиг пишется списком `[pkg].important`.
pub(super) fn important_cmd(cmd: &Command, cx: &mut Cx<'_>, env: &Env) -> Result<(), CmdError> {
    let Some(id) = target(cmd, cx, env)? else {
        cx.keep_rev();
        return Ok(());
    };
    let family = env.book.family(&id);
    let mut cfg = PkgConfig::clone(&env.config());
    if cfg.important.iter().any(|i| family.contains(i)) {
        cfg.important.retain(|i| !family.contains(i));
    } else {
        cfg.important.push(id);
    }
    let list: settings::Value = cfg.important.iter().map(ToString::to_string).collect();
    settings::set(&["pkg", "important"], list).apply(cx);
    apply(cx, env, cfg);
    Ok(())
}
