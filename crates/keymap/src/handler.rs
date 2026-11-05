//! Handler `keymap`: быстрый поиск (S3 §1.3, S3-06, ADR-0016), начатый аккорд,
//! перезагрузка и выгрузка раскладки (S3-10).
//!
//! Состояние — [`KeymapView`] в `AppState.ext`; меняет его только этот handler, ui
//! рисует. Ввод поиска приходит командами из [`crate::KeyInput`], курсор ставит
//! `panel.cursor.to` (имя найдено здесь же: нет совпадения — символ не принимается и
//! тоста нет).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use compact_str::CompactString;
use smallvec::SmallVec;
use urfin_core::{
    ArgSpec, Banner, CmdError, Command, CommandMeta, CommandRegistry, Config, Core, Cx, Focus, Gen,
    Handler, Lane, Level, Location, Mask, OverlayKind, Paths, Row, Side, Tab, TabId, UiEvent,
    Value, ValueKind, msg, settings,
};
use urfin_i18n::FluentArgs;

use crate::edit::{self, BindPlan, Edit, EditError, Target};
use crate::{Chord, FlagRegistry, KeyInput, Keymap, KeymapCfg, KeymapWarning, Os, WarnKind};

/// Строка быстрого поиска на панели `side`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickSearch {
    pub side: Side,
    /// Введённое (без неявной `*` в конце).
    pub mask: CompactString,
    /// Совпадений с маской в табе.
    pub matches: u32,
    /// Последний не принятый символ: ui подсвечивает поле 300 мс (S2 §576).
    pub miss_at: Option<Instant>,
}

/// Действующая раскладка. App отдаёт её `KeyInput`, когда растёт `generation`
/// (`keymap.reload`, `keymap.preset`, `app.reload_config`).
#[derive(Debug)]
pub struct ActiveKeymap {
    pub generation: u64,
    pub keymap: Arc<Keymap>,
    pub cfg: KeymapCfg,
    /// Файл пользовательского слоя (`keymaps/user.toml`).
    pub user_file: PathBuf,
    /// Баннер о проблемах раскладки, если показан он (снимается чистой загрузкой).
    banner: Option<String>,
}

/// Состояние keymap для ui (S3 §5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeymapView {
    pub qs: Option<QuickSearch>,
    /// Начатый аккорд для строки статуса: `Ctrl+K`.
    pub pending: Option<CompactString>,
}

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(
    id: &'static str,
    title: &'static str,
    args: &'static [ArgSpec],
    repeatable: bool,
) -> CommandMeta {
    CommandMeta {
        id,
        title,
        category: "keymap",
        args,
        repeatable,
    }
}

static COMMANDS: [CommandMeta; 15] = [
    meta("keymap.reload", "Reload key bindings", &[], false),
    meta(
        "keymap.preset",
        "Key bindings preset",
        &[arg("name", ValueKind::Str, true)],
        false,
    ),
    meta("keymap.show", "Key bindings", &[], false),
    // Правка `keymaps/user.toml` (раздел «Клавиатура», решение 2026-11-05).
    meta(
        "keymap.bind",
        "Bind a key",
        &[
            arg("context", ValueKind::Str, true),
            arg("chord", ValueKind::Str, true),
            // Команды привязки (`id арг=значение`), как в файле.
            arg("target", ValueKind::List, true),
            arg("repeat", ValueKind::Bool, false),
            // Заменяемая клавиша строки и её контекст.
            arg("old", ValueKind::Str, false),
            arg("old_context", ValueKind::Str, false),
            // Конфликт подтверждён: снять клавишу с других команд.
            arg("steal", ValueKind::Bool, false),
        ],
        false,
    ),
    meta(
        "keymap.unbind",
        "Remove a key",
        &[
            arg("context", ValueKind::Str, true),
            arg("chord", ValueKind::Str, true),
        ],
        false,
    ),
    meta(
        "keymap.reset",
        "Reset key bindings to the preset",
        &[
            // Строка таблицы; без неё — весь файл.
            arg("target", ValueKind::Str, false),
            arg("place", ValueKind::Str, false),
        ],
        false,
    ),
    meta("keymap.open", "Open user.toml", &[], false),
    meta(
        "keymap.dump",
        "Export key bindings",
        &[arg("path", ValueKind::Str, false)],
        false,
    ),
    meta(
        "keymap.qs.start",
        "Quick search",
        &[arg("text", ValueKind::Str, false)],
        false,
    ),
    meta(
        "keymap.qs.input",
        "Quick search: type",
        &[arg("text", ValueKind::Str, true)],
        false,
    ),
    meta("keymap.qs.next", "Quick search: next", &[], true),
    meta("keymap.qs.prev", "Quick search: previous", &[], true),
    meta("keymap.qs.backspace", "Quick search: erase", &[], true),
    meta("keymap.qs.cancel", "Quick search: close", &[], false),
    meta(
        "keymap.pending",
        "Pending chord",
        &[arg("chord", ValueKind::Str, false)],
        false,
    ),
];

/// Регистрирует handler `keymap`, загружает раскладку (`config_dir/keymaps/`) и
/// возвращает конвейер ввода для `raw_input_hook` (S3 §5). Регистрировать последним:
/// раскладка проверяется по командам уже зарегистрированных handler'ов.
/// Предупреждения раскладки — баннером в первом pump.
pub fn register(core: &mut Core, paths: Paths, cfg: KeymapCfg, flags: FlagRegistry) -> KeyInput {
    let mut metas: Vec<&'static CommandMeta> = core.registry().iter().collect();
    metas.extend(COMMANDS.iter());
    let os = Os::current();
    let (keymap, warnings) = Keymap::load(&paths, &cfg, &registry_of(&metas), &flags, os);
    let input = KeyInput::new(Arc::clone(&keymap), &cfg);
    let write = Mutex::new(());
    core.register(Box::new(KeymapHandler {
        cache: None,
        reload: Arc::new(Reload {
            paths,
            metas,
            flags,
            os,
            write,
        }),
        init: Some((keymap, cfg, warnings)),
        config: None,
    }));
    input
}

struct KeymapHandler {
    /// Строки, подходящие под текущую маску: следующий символ проверяет только их.
    cache: Option<Cands>,
    reload: Arc<Reload>,
    /// Раскладка из `register` — в `AppState.ext` первым pump.
    init: Option<(Arc<Keymap>, KeymapCfg, Vec<KeymapWarning>)>,
    /// Последний виденный конфиг: новый (`app.reload_config`) — перечитать раскладку.
    config: Option<Arc<Config>>,
}

/// Всё для загрузки раскладки вне UI-потока.
struct Reload {
    paths: Paths,
    metas: Vec<&'static CommandMeta>,
    flags: FlagRegistry,
    os: Os,
    /// Правки файла по одной: чтение и запись не перемешиваются.
    write: Mutex<()>,
}

impl Reload {
    /// Прочитать `user.toml`, применить правки, записать атомарно (вне UI-потока).
    fn write(&self, cfg: &KeymapCfg, edits: &[Edit]) -> Result<(), EditError> {
        let _one = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let path = crate::user_path(&self.paths, cfg);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let new = edit::apply(&text, edits, self.os)?;
        if new != text {
            urfin_core::paths::write_atomic(&path, new.as_bytes())?;
        }
        Ok(())
    }
}

fn registry_of(metas: &[&'static CommandMeta]) -> CommandRegistry {
    let mut r = CommandRegistry::new();
    for m in metas {
        // Id уже проверены при первой регистрации.
        let _ = r.register(std::slice::from_ref(*m));
    }
    r
}

/// Кандидаты маски `mask` в табе `key`.
struct Cands {
    key: TabKey,
    mask: CompactString,
    rows: Vec<u32>,
}

/// Содержимое таба не менялось: тот же таб, загрузка, порядок.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TabKey {
    tab: TabId,
    generation: Gen,
    arrange_seq: u32,
    rows: u32,
}

fn tab_key(t: &Tab) -> TabKey {
    TabKey {
        tab: t.id,
        generation: t.listing.generation,
        arrange_seq: t.listing.arrange_seq,
        rows: t.rows(),
    }
}

impl Handler for KeymapHandler {
    fn namespace(&self) -> &'static str {
        "keymap"
    }

    fn commands(&self) -> &'static [CommandMeta] {
        &COMMANDS
    }

    fn handle(&mut self, cmd: &Command, cx: &mut Cx<'_>) -> Result<(), CmdError> {
        let a = &cmd.args;
        match cmd.id.as_str() {
            "keymap.qs.start" => {
                let side = cx.state.active;
                view(cx).qs = Some(QuickSearch {
                    side,
                    mask: CompactString::default(),
                    matches: 0,
                    miss_at: None,
                });
                set_focus(cx, Focus::Overlay(OverlayKind::QuickSearch));
                self.cache = None;
                if let Some(t) = a.str("text") {
                    self.input(cx, t);
                }
            }
            "keymap.qs.input" => {
                if qs(cx).is_some() {
                    self.input(cx, a.str("text").unwrap_or_default());
                } else {
                    cx.keep_rev();
                }
            }
            "keymap.qs.next" => step(cx, true),
            "keymap.qs.prev" => step(cx, false),
            "keymap.qs.backspace" => {
                let Some(q) = qs(cx) else {
                    cx.keep_rev();
                    return Ok(());
                };
                let mut mask = q.mask.clone();
                mask.pop();
                self.cache = None;
                let matches = if mask.is_empty() {
                    0
                } else {
                    self.scan(cx.state.active_tab(), &mask)
                        .map_or(0, |(_, n)| n)
                };
                if let Some(q) = &mut view(cx).qs {
                    q.mask = mask;
                    q.matches = matches;
                    q.miss_at = None;
                }
            }
            "keymap.qs.cancel" => {
                self.cache = None;
                view(cx).qs = None;
                if cx.state.focus == Focus::Overlay(OverlayKind::QuickSearch) {
                    set_focus(cx, Focus::Panel);
                }
            }
            "keymap.reload" => {
                let cfg = active(cx).map(|a| a.cfg.clone()).unwrap_or_default();
                self.load(cx, cfg, false);
                cx.keep_rev();
            }
            "keymap.preset" => {
                let mut cfg = active(cx).map(|a| a.cfg.clone()).unwrap_or_default();
                cfg.preset = a.str("name").unwrap_or_default().into();
                self.load(cx, cfg, true);
                cx.keep_rev();
            }
            "keymap.show" => show(cx),
            "keymap.bind" => {
                self.bind(cx, a);
                cx.keep_rev();
            }
            "keymap.unbind" => {
                if let Some((km, _)) = active_km(cx)
                    && let Some(context) = a.str("context")
                    && let Some(chord) = chord_arg(&km, a.str("chord"))
                {
                    self.edit(cx, edit::plan_unbind(&km, context, &chord));
                }
                cx.keep_rev();
            }
            "keymap.reset" => {
                if let Some((km, _)) = active_km(cx) {
                    let edits = match a.str("target") {
                        Some(t) => edit::plan_reset(&km, t, a.str("place").unwrap_or_default()),
                        None => vec![Edit::Clear],
                    };
                    self.edit(cx, edits);
                }
                cx.keep_rev();
            }
            "keymap.open" => {
                self.open(cx);
                cx.keep_rev();
            }
            "keymap.dump" => {
                let Some(km) = active(cx).map(|a| Arc::clone(&a.keymap)) else {
                    return Ok(());
                };
                let path = a.str("path").map_or_else(
                    || {
                        self.reload
                            .paths
                            .config_dir
                            .join("keymaps")
                            .join("dump.toml")
                    },
                    PathBuf::from,
                );
                dump(cx, &km, path);
                cx.keep_rev();
            }
            "keymap.pending" => {
                let chord = a.str("chord").map(CompactString::from);
                let v = view(cx);
                if v.pending == chord {
                    cx.keep_rev();
                } else {
                    v.pending = chord;
                }
            }
            _ => return Err(CmdError::Unknown(cmd.id.as_str().into())),
        }
        Ok(())
    }

    /// Первый pump — раскладка из `register` в `AppState.ext`; новый конфиг —
    /// перечитать раскладку; фокус ушёл из поиска (клик, меню, `ui.cancel`) — поиск
    /// закрыт.
    fn on_pump(&mut self, cx: &mut Cx<'_>) {
        if let Some((keymap, cfg, warnings)) = self.init.take() {
            let user_file = crate::user_path(&self.reload.paths, &cfg);
            cx.state.ext.insert(ActiveKeymap {
                generation: 0,
                keymap,
                cfg,
                user_file,
                banner: None,
            });
            apply_banner(cx, &warnings, false);
            cx.state.touch();
        }
        match &self.config {
            None => self.config = Some(Arc::clone(&cx.state.config)),
            Some(seen) if !Arc::ptr_eq(seen, &cx.state.config) => {
                self.config = Some(Arc::clone(&cx.state.config));
                let (cfg, warning) = cx.state.config.section::<KeymapCfg>("keymap");
                if let Some(w) = warning {
                    tracing::warn!(key = %w.key, msg = %w.msg, "[keymap] section ignored");
                }
                self.load(cx, cfg, false);
            }
            Some(_) => {}
        }
        let open = cx
            .state
            .ext
            .get::<KeymapView>()
            .is_some_and(|v| v.qs.is_some());
        if open && cx.state.focus != Focus::Overlay(OverlayKind::QuickSearch) {
            self.cache = None;
            view(cx).qs = None;
            cx.state.touch();
        }
    }
}

impl KeymapHandler {
    /// Загрузка раскладки на `Lane::Bulk`; применяет её Msg в следующем pump.
    /// `save` — принятый пресет пишется в `[keymap] preset` (`keymap.preset`, S2-22).
    fn load(&self, cx: &mut Cx<'_>, cfg: KeymapCfg, save: bool) {
        let r = Arc::clone(&self.reload);
        cx.rt
            .spawn(Lane::Bulk, "keymap.reload", cx.rt.token(), move |tcx| {
                let (keymap, warnings) =
                    Keymap::load(&r.paths, &cfg, &registry_of(&r.metas), &r.flags, r.os);
                let user_file = crate::user_path(&r.paths, &cfg);
                let apply = msg("keymap.loaded", move |cx| {
                    let preset = save.then(|| cfg.preset.clone());
                    if loaded(cx, keymap, cfg, user_file, &warnings)
                        && let Some(p) = preset
                    {
                        settings::set(&["keymap", "preset"], p.as_str()).apply(cx);
                    }
                });
                let _ = tcx.sink.send(apply, &tcx.cancel);
            });
    }

    /// `keymap.bind`: план по действующей раскладке, запись — задачей.
    fn bind(&self, cx: &mut Cx<'_>, a: &urfin_core::Args) {
        let Some((km, _)) = active_km(cx) else {
            return;
        };
        let (Some(context), Some(chord), Some(list)) = (
            a.str("context"),
            chord_arg(&km, a.str("chord")),
            a.list("target"),
        ) else {
            return;
        };
        let cmds = list.iter().filter_map(|v| match v {
            Value::Str(s) => Some(CompactString::from(s.as_str())),
            _ => None,
        });
        let plan = BindPlan {
            context,
            chord,
            target: Target {
                cmds: cmds.collect(),
                repeat: a.bool("repeat"),
            },
            old: chord_arg(&km, a.str("old"))
                .map(|c| (a.str("old_context").unwrap_or(context), c)),
            steal: a.bool("steal") == Some(true),
        };
        self.edit(cx, edit::plan_bind(&km, &plan));
    }

    /// Правки файла на `Lane::Bulk`; затем `keymap.reload`, ошибка — тост.
    fn edit(&self, cx: &mut Cx<'_>, edits: Vec<Edit>) {
        let Some((_, cfg)) = active_km(cx) else {
            return;
        };
        let r = Arc::clone(&self.reload);
        cx.rt
            .spawn(Lane::Bulk, "keymap.edit", cx.rt.token(), move |tcx| {
                let res = r.write(&cfg, &edits);
                let path = crate::user_path(&r.paths, &cfg);
                let done = msg("keymap.edited", move |cx| match res {
                    Ok(()) => {
                        // Слой был выключен: файл записан — включить.
                        if let Some(a) = cx.state.ext.get_mut::<ActiveKeymap>()
                            && a.cfg.user.is_none()
                        {
                            a.cfg.user = Some("user.toml".into());
                            settings::set(&["keymap", "user"], "user.toml").apply(cx);
                        }
                        cx.dispatch(Command::new("keymap.reload"));
                    }
                    Err(e) => {
                        let key = match e {
                            EditError::Parse(_) | EditError::Layout => "keymap-edit-broken",
                            _ => "keymap-edit-failed",
                        };
                        let mut args = FluentArgs::new();
                        args.set("path", path.display().to_string());
                        args.set("error", e.to_string());
                        cx.toast(Level::Warn, urfin_i18n::current().fmt(key, &args));
                    }
                });
                let _ = tcx.sink.send(done, &tcx.cancel);
            });
    }

    /// `keymap.open`: файла нет — создать с шапкой, открыть в редакторе. После ручной
    /// правки раскладку перечитывает открытие раздела или «Перечитать».
    fn open(&self, cx: &mut Cx<'_>) {
        let Some((_, cfg)) = active_km(cx) else {
            return;
        };
        let r = Arc::clone(&self.reload);
        cx.rt
            .spawn(Lane::Bulk, "keymap.open", cx.rt.token(), move |tcx| {
                let path = crate::user_path(&r.paths, &cfg);
                let res = if path.exists() {
                    Ok(())
                } else {
                    urfin_core::paths::write_atomic(&path, edit::USER_TEMPLATE.as_bytes())
                };
                let done = msg("keymap.opened", move |cx| match res {
                    Ok(()) => {
                        cx.dispatch(Command::new("ui.settings").arg("close", true));
                        cx.dispatch(
                            Command::new("editor.open")
                                .arg("path", Location::local(&path))
                                .arg("internal", true),
                        );
                    }
                    Err(e) => {
                        let mut args = FluentArgs::new();
                        args.set("path", path.display().to_string());
                        args.set("error", e.to_string());
                        cx.toast(
                            Level::Warn,
                            urfin_i18n::current().fmt("keymap-edit-failed", &args),
                        );
                    }
                });
                let _ = tcx.sink.send(done, &tcx.cancel);
            });
    }

    /// Строка для маски `text` — текущая, если подходит, иначе первая сверху — и число
    /// совпадений. Если `text` продолжает прошлую маску, проверяются только её
    /// кандидаты. Кэш меняется только при попадании.
    fn scan(&mut self, t: &Tab, text: &str) -> Option<(u32, u32)> {
        let m = Matcher::new(text)?;
        let key = tab_key(t);
        let names = Names::new(t);
        let rows: Vec<u32> = match &self.cache {
            Some(c) if c.key == key && text.starts_with(c.mask.as_str()) => c
                .rows
                .iter()
                .copied()
                .filter(|&r| names.get(r).is_some_and(|n| m.matches(n)))
                .collect(),
            _ => (0..key.rows)
                .filter(|&r| names.get(r).is_some_and(|n| m.matches(n)))
                .collect(),
        };
        let first = *rows.first()?;
        let cur = t.cursor.row;
        let row = if rows.binary_search(&cur).is_ok() {
            cur
        } else {
            first
        };
        let n = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        self.cache = Some(Cands {
            key,
            mask: text.into(),
            rows,
        });
        Some((row, n))
    }

    /// Символы по одному, пока есть совпадения (вставка — так же, Far).
    fn input(&mut self, cx: &mut Cx<'_>, text: &str) {
        let Some(q) = qs(cx) else {
            return;
        };
        let mut mask = q.mask.clone();
        let mut accepted = None;
        let mut miss = false;
        for c in text.chars() {
            let mut next = mask.clone();
            next.push(c);
            if let Some(hit) = self.scan(cx.state.active_tab(), &next) {
                mask = next;
                accepted = Some(hit);
            } else {
                miss = true;
                break;
            }
        }
        if let Some((row, _)) = accepted {
            go(cx, row);
        }
        if let Some(q) = &mut view(cx).qs {
            if let Some((_, n)) = accepted {
                q.mask = mask;
                q.matches = n;
                q.miss_at = None;
            }
            if miss {
                q.miss_at = Some(Instant::now());
            }
        }
    }
}

/// Битый файл или пресет: прежняя раскладка остаётся (S3-10).
fn broken(w: &KeymapWarning) -> bool {
    matches!(
        w.kind,
        WarnKind::BadFile(_)
            | WarnKind::BadFormat(_)
            | WarnKind::ExtendsMissing(_)
            | WarnKind::ExtendsCycle(_)
            | WarnKind::ExtendsTooDeep(_)
    )
}

/// Применить загруженную раскладку (или оставить прежнюю) и баннер.
/// `true` — раскладка принята (иначе остаётся прежняя).
fn loaded(
    cx: &mut Cx<'_>,
    keymap: Arc<Keymap>,
    cfg: KeymapCfg,
    user_file: PathBuf,
    warnings: &[KeymapWarning],
) -> bool {
    let kept = warnings.iter().any(broken);
    if !kept {
        if let Some(a) = cx.state.ext.get_mut::<ActiveKeymap>() {
            a.generation += 1;
            a.keymap = keymap;
            a.cfg = cfg;
            a.user_file = user_file;
        } else {
            cx.state.ext.insert(ActiveKeymap {
                generation: 1,
                keymap,
                cfg,
                user_file,
                banner: None,
            });
        }
    }
    apply_banner(cx, warnings, kept);
    !kept
}

/// Баннер о проблемах раскладки (кроме команд сессий, которых нет в сборке); чистая
/// загрузка снимает только свой баннер.
fn apply_banner(cx: &mut Cx<'_>, warnings: &[KeymapWarning], kept: bool) {
    let shown: Vec<&KeymapWarning> = warnings
        .iter()
        .filter(|w| !w.missing_provider() && !w.advisory())
        .collect();
    let ours = cx
        .state
        .ext
        .get::<ActiveKeymap>()
        .and_then(|a| a.banner.clone());
    let text = shown.first().map(|first| {
        let mut args = FluentArgs::new();
        args.set("count", shown.len());
        args.set("first", first.to_string());
        let key = if kept {
            "keymap-kept"
        } else {
            "keymap-warnings"
        };
        urfin_i18n::current().fmt(key, &args)
    });
    match &text {
        Some(t) => {
            // Кнопки баннера (v2 №15): перечитать после правки файла, скрыть.
            let i18n = urfin_i18n::current();
            let mut actions = SmallVec::new();
            actions.push((
                i18n.get("keymap-action-reload").into(),
                Command::new("keymap.reload"),
            ));
            actions.push((
                i18n.get("core-action-dismiss").into(),
                Command::new("app.dismiss_banner"),
            ));
            cx.state.banner = Some(Banner {
                level: Level::Warn,
                text: t.clone(),
                actions,
            });
        }
        None if ours.is_some() && cx.state.banner.as_ref().map(|b| &b.text) == ours.as_ref() => {
            cx.state.banner = None;
        }
        None => {}
    }
    if let Some(a) = cx.state.ext.get_mut::<ActiveKeymap>() {
        a.banner = text;
    }
}

fn active<'a>(cx: &'a Cx<'_>) -> Option<&'a ActiveKeymap> {
    cx.state.ext.get::<ActiveKeymap>()
}

fn active_km(cx: &Cx<'_>) -> Option<(Arc<Keymap>, KeymapCfg)> {
    active(cx).map(|a| (Arc::clone(&a.keymap), a.cfg.clone()))
}

fn chord_arg(km: &Keymap, s: Option<&str>) -> Option<Chord> {
    Chord::parse(s?, km.os()).ok()
}

/// Строки `keymap.show`: `[контекст] сочетание = команды (слой)`.
pub fn effective_lines(km: &Keymap) -> Vec<String> {
    km.effective()
        .map(|e| {
            let cmds: Vec<String> = e.binding.cmds.iter().map(ToString::to_string).collect();
            format!(
                "[{}] {} = {} ({:?})",
                e.binding.src.context,
                e.chord.display(km.os()),
                cmds.join(", "),
                e.binding.layer
            )
        })
        .collect()
}

/// Действующая раскладка как файл keymap (S3 §2.2): можно положить в
/// `keymaps/user.toml` и править. Поздняя привязка того же сочетания в том же
/// контексте перекрывает раннюю — как при загрузке.
pub fn dump_toml(km: &Keymap) -> String {
    let mut contexts = toml::Table::new();
    for e in km.effective() {
        let b = e.binding;
        let one = |c: &Command| toml::Value::String(c.to_string());
        let value = match (b.cmds.as_ref(), b.repeat) {
            ([c], None) => one(c),
            ([c], Some(r)) => {
                let mut t = toml::Table::new();
                t.insert("cmd".into(), one(c));
                t.insert("_repeat".into(), toml::Value::Boolean(r));
                toml::Value::Table(t)
            }
            (cmds, _) => toml::Value::Array(cmds.iter().map(one).collect()),
        };
        let ctx = contexts
            .entry(b.src.context.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = ctx {
            t.insert(e.chord.display(km.os()).to_string(), value);
        }
    }
    let mut root = toml::Table::new();
    root.insert("format".into(), toml::Value::Integer(1));
    root.insert("name".into(), toml::Value::String("dump".into()));
    root.insert("context".into(), toml::Value::Table(contexts));
    let body = toml::to_string(&root).unwrap_or_default();
    format!(
        "# Действующая раскладка Urfin (keymap.dump). Свои правки — в keymaps/user.toml.\n{body}"
    )
}

/// `keymap.show`: таблица действующих привязок для ui (`UiEvent::Ext`).
fn show(cx: &mut Cx<'_>) {
    let lines: Vec<Value> = active(cx)
        .map(|a| effective_lines(&a.keymap))
        .unwrap_or_default()
        .into_iter()
        .map(|l| Value::Str(l.into()))
        .collect();
    let c = Command::new("keymap.show").arg("lines", Value::List(lines.into()));
    cx.ui(UiEvent::Ext {
        id: c.id,
        args: c.args,
    });
    cx.keep_rev();
}

/// `keymap.dump`: запись на `Lane::Bulk`, итог — тостом.
fn dump(cx: &mut Cx<'_>, km: &Keymap, path: PathBuf) {
    let text = dump_toml(km);
    cx.rt
        .spawn(Lane::Bulk, "keymap.dump", cx.rt.token(), move |tcx| {
            let res = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, text));
            let done = msg("keymap.dumped", move |cx| {
                let mut args = FluentArgs::new();
                args.set("path", path.display().to_string());
                let (level, key) = match res {
                    Ok(()) => (Level::Info, "keymap-dumped"),
                    Err(e) => {
                        args.set("error", e.to_string());
                        (Level::Warn, "keymap-dump-failed")
                    }
                };
                cx.toast(level, urfin_i18n::current().fmt(key, &args));
            });
            let _ = tcx.sink.send(done, &tcx.cancel);
        });
}

/// Сопоставление строки поиска: введённое + неявная `*`. Без `*`, `?` и `>` — префикс
/// без учёта регистра (быстрый путь); иначе — core `Mask`: `>txt` — расширение
/// начинается с `txt` (DN), кавычки защищают `,;|` в имени.
#[derive(Debug)]
pub enum Matcher {
    Prefix(SmallVec<[char; 16]>),
    Mask(Mask),
}

impl Matcher {
    pub fn new(text: &str) -> Option<Self> {
        if !text.contains(['*', '?']) && !text.starts_with('>') {
            return Some(Matcher::Prefix(text.chars().map(lower).collect()));
        }
        let text: String = text.chars().filter(|c| *c != '"').collect();
        let pat = match text.strip_prefix('>') {
            Some(ext) => format!("\"*.{ext}*\""),
            None => format!("\"{text}*\""),
        };
        Mask::parse(&pat).ok().map(Matcher::Mask)
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            Matcher::Prefix(needle) => {
                let mut chars = name.chars();
                needle.iter().all(|&n| {
                    chars
                        .next()
                        .is_some_and(|c| c == n || (!c.is_ascii_lowercase() && lower(c) == n))
                })
            }
            Matcher::Mask(m) => m.matches(name),
        }
    }
}

/// Строчная буква, если она одна (`İ` остаётся как есть).
fn lower(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// Имена строк таба без `Tab::row` на каждую строку (там проверка `..` по пути).
struct Names<'t> {
    /// 1, если первая строка — `..`.
    off: u32,
    t: &'t Tab,
}

impl<'t> Names<'t> {
    fn new(t: &'t Tab) -> Self {
        let order = u32::try_from(t.listing.order.len()).unwrap_or(u32::MAX);
        Names {
            off: t.rows() - order,
            t,
        }
    }

    fn get(&self, r: u32) -> Option<&'t str> {
        let i = r.checked_sub(self.off)? as usize;
        let idx = *self.t.listing.order.get(i)? as usize;
        self.t.listing.entries.get(idx).map(|e| e.name.as_str())
    }
}

fn name_at(t: &Tab, r: u32) -> Option<&str> {
    match t.row(r) {
        Some(Row::Entry { entry, .. }) => Some(entry.name.as_str()),
        _ => None,
    }
}

fn view<'a>(cx: &'a mut Cx<'_>) -> &'a mut KeymapView {
    if cx.state.ext.get::<KeymapView>().is_none() {
        cx.state.ext.insert(KeymapView::default());
    }
    cx.state
        .ext
        .get_mut::<KeymapView>()
        .expect("inserted above")
}

fn qs(cx: &Cx<'_>) -> Option<QuickSearch> {
    cx.state.ext.get::<KeymapView>()?.qs.clone()
}

fn set_focus(cx: &mut Cx<'_>, focus: Focus) {
    if cx.state.focus != focus {
        cx.state.focus = focus;
        cx.ui(UiEvent::FocusChanged(focus));
    }
}

/// Соседнее совпадение по кругу.
fn step(cx: &mut Cx<'_>, forward: bool) {
    let (rows, cur) = {
        let t = cx.state.active_tab();
        (t.rows(), t.cursor.row)
    };
    let m = qs(cx)
        .filter(|q| !q.mask.is_empty() && rows > 0)
        .and_then(|q| Matcher::new(&q.mask));
    let Some(m) = m else {
        cx.keep_rev();
        return;
    };
    let names = Names::new(cx.state.active_tab());
    let hit = (1..rows)
        .map(|k| {
            if forward {
                (cur + k) % rows
            } else {
                (cur + rows - k) % rows
            }
        })
        .find(|&r| names.get(r).is_some_and(|name| m.matches(name)));
    match hit {
        Some(r) => go(cx, r),
        None => cx.keep_rev(),
    }
}

fn go(cx: &mut Cx<'_>, row: u32) {
    let t = cx.state.active_tab();
    if row == t.cursor.row {
        return;
    }
    if let Some(name) = name_at(t, row) {
        let c = Command::new("panel.cursor.to").arg("name", name);
        cx.dispatch(c);
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;
