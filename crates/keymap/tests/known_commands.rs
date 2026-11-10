//! S3-09: встроенные пресеты против stub-реестра всех команд S3 §3. Команд других
//! сессий (S5–S8) в сборке ещё нет; против настоящего реестра пресеты проверяет app.
//!
//! Шпаргалка `docs/keys.md` генерируется отсюда: `URFIN_UPDATE_DOCS=1 cargo nextest run
//! -p urfin-keymap keys_doc`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use urfin_core::{
    AppState, ArgSpec, CommandMeta, CommandRegistry, Focus, Location, OverlayKind, Panel, Paths,
    Tab, TabId, ValueKind,
};
use urfin_keymap::{FlagRegistry, KeyCode, Keymap, KeymapCfg, KeymapWarning, Mods, Os};

const fn arg(name: &'static str, kind: ValueKind, required: bool) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
    }
}

const fn meta(id: &'static str, args: &'static [ArgSpec], repeatable: bool) -> CommandMeta {
    CommandMeta {
        id,
        title: id,
        category: "stub",
        args,
        repeatable,
    }
}

/// Все id и аргументы, на которые ссылаются пресеты (S3 §3, запросы §7), команды S3 и
/// S7 §7. У команд, которые уже есть в сборке, — их настоящие аргументы (вьювер и
/// редактор — `crates/view/src/handler.rs`).
static KNOWN: &[CommandMeta] = &[
    meta("app.quit", &[], false),
    meta("app.save_session", &[], false),
    meta("arc.add", &[], false),
    meta("arc.commands", &[], false),
    meta("arc.enter", &[], false),
    meta("arc.extract", &[], false),
    meta("arc.test", &[], false),
    meta("cmdline.apply", &[], false),
    meta("cmdline.backspace", &[], true),
    meta("cmdline.delete", &[], true),
    meta("cmdline.undo", &[], true),
    meta("cmdline.redo", &[], true),
    meta("cmdline.clear", &[], false),
    meta("cmdline.copy", &[], false),
    meta("cmdline.cursor.left", &[], true),
    meta("cmdline.cursor.right", &[], true),
    meta("cmdline.del.to_end", &[], false),
    meta("cmdline.del.word_left", &[], true),
    meta("cmdline.del.word_right", &[], true),
    meta("cmdline.end", &[], false),
    meta(
        "cmdline.exec",
        &[arg("detached", ValueKind::Bool, false)],
        false,
    ),
    meta("cmdline.complete", &[], false),
    meta("cmdline.complete_native", &[], false),
    meta("cmdline.history", &[], false),
    meta("cmdline.history.next", &[], true),
    meta("cmdline.history.prev", &[], true),
    meta("cmdline.home", &[], false),
    meta("cmdline.menu.up", &[], true),
    meta("cmdline.menu.down", &[], true),
    meta("cmdline.menu.page_up", &[], true),
    meta("cmdline.menu.page_down", &[], true),
    meta(
        "cmdline.menu.accept",
        &[arg("exec", ValueKind::Bool, false)],
        false,
    ),
    meta("cmdline.menu.delete", &[], false),
    meta("cmdline.menu.lock", &[], false),
    meta("cmdline.menu.scope", &[], false),
    meta("cmdline.menu.close", &[], false),
    meta(
        "cmdline.insert",
        &[arg("text", ValueKind::Str, true)],
        false,
    ),
    meta(
        "cmdline.insert_name",
        &[
            arg("what", ValueKind::Str, true),
            arg("side", ValueKind::Str, false),
        ],
        false,
    ),
    meta(
        "cmdline.paste",
        &[arg("text", ValueKind::Str, false)],
        false,
    ),
    meta("cmdline.word.left", &[], true),
    meta("cmdline.word.right", &[], true),
    meta("du.refresh", &[arg("full", ValueKind::Bool, false)], false),
    meta("du.stop", &[], false),
    meta(
        "du.filter",
        &[
            arg("kind", ValueKind::Str, true),
            arg("group", ValueKind::Str, false),
        ],
        false,
    ),
    meta("du.map", &[], false),
    meta("du.verify_hash", &[], false),
    // SSH-пара (F03-09, `urfin-net`): Ctrl+Tab в `in_sshpair`.
    meta("ssh.pair_focus", &[], false),
    // Мультивыполнение (F03-11, `urfin-net`): Ctrl+Alt+G, Esc в `ssh_group`.
    meta("ssh.group.edit", &[], false),
    meta("ssh.group.leave", &[], false),
    // Подсветка вывода (F03-10, `urfin-term`): Ctrl+Alt+H в терминале и cmdline.
    meta("term.highlight.toggle", &[], false),
    // Запись сеанса (F03-12, `urfin-term`): Ctrl+Alt+R в терминале и cmdline.
    meta("term.record.toggle", &[], false),
    meta("editor.block.shift_left", &[], true),
    meta("editor.block.shift_right", &[], true),
    meta("editor.close", &[], false),
    meta(
        "editor.codepage",
        &[arg("cp", ValueKind::Str, false)],
        false,
    ),
    meta("editor.codepage_menu", &[], false),
    meta("editor.copy", &[], false),
    meta("editor.cut", &[], false),
    meta("editor.delete", &[arg("to", ValueKind::Str, true)], true),
    meta("editor.delete_line", &[], true),
    meta("editor.eol", &[arg("to", ValueKind::Str, true)], false),
    meta("editor.eol_menu", &[], false),
    meta("editor.fold", &[arg("at", ValueKind::Int, false)], false),
    meta("editor.goto", &[arg("to", ValueKind::Str, false)], false),
    meta("editor.highlight", &[], false),
    meta("editor.md.bold", &[], true),
    meta("editor.md.code", &[], true),
    meta(
        "editor.md.heading",
        &[arg("level", ValueKind::Int, false)],
        false,
    ),
    meta("editor.md.italic", &[], true),
    meta("editor.md.link", &[], true),
    meta("editor.md.list", &[], true),
    meta("editor.md.numbered", &[], true),
    meta("editor.md.table", &[], true),
    meta("editor.md.task", &[], true),
    meta(
        "editor.move",
        &[
            arg("to", ValueKind::Str, true),
            arg("select", ValueKind::Str, false),
        ],
        true,
    ),
    meta("editor.newline", &[], true),
    meta(
        "editor.new",
        &[
            arg("path", ValueKind::Loc, false),
            arg("alt", ValueKind::Bool, false),
            arg("internal", ValueKind::Bool, false),
            arg("line", ValueKind::Int, false),
            arg("col", ValueKind::Int, false),
        ],
        false,
    ),
    meta(
        "editor.open",
        &[
            arg("path", ValueKind::Loc, false),
            arg("alt", ValueKind::Bool, false),
            arg("internal", ValueKind::Bool, false),
            arg("line", ValueKind::Int, false),
            arg("col", ValueKind::Int, false),
        ],
        false,
    ),
    meta("editor.overwrite", &[], false),
    meta("editor.paste", &[], false),
    meta("editor.preview", &[], false),
    meta("editor.redo", &[], true),
    meta("editor.reload", &[], false),
    meta("editor.replace", &[], false),
    meta("editor.save", &[], false),
    meta("editor.save_all", &[], false),
    meta("editor.save_as", &[], false),
    meta("editor.save_close", &[], false),
    meta("editor.search", &[], false),
    meta("editor.search_next", &[], true),
    meta("editor.search_prev", &[], true),
    meta("editor.select_all", &[], false),
    meta("editor.tab", &[], true),
    meta("editor.to_viewer", &[], false),
    meta("editor.toolbar", &[], false),
    meta("editor.undo", &[], true),
    meta("editor.wrap", &[], false),
    meta("file.clip.copy", &[], false),
    meta("file.clip.cut", &[], false),
    meta(
        "file.clip.paste",
        &[arg("move", ValueKind::Bool, false)],
        false,
    ),
    meta(
        "file.open",
        &[arg("elevated", ValueKind::Bool, false)],
        false,
    ),
    meta("file.open_with", &[], false),
    meta("file.reveal", &[], false),
    meta("keymap.dump", &[arg("path", ValueKind::Str, false)], false),
    meta("keymap.preset", &[arg("name", ValueKind::Str, true)], false),
    meta("keymap.qs.backspace", &[], true),
    meta("keymap.qs.cancel", &[], false),
    meta(
        "keymap.qs.input",
        &[arg("text", ValueKind::Str, true)],
        false,
    ),
    meta("keymap.qs.next", &[], true),
    meta("keymap.qs.prev", &[], true),
    meta(
        "keymap.qs.start",
        &[arg("text", ValueKind::Str, false)],
        false,
    ),
    meta("keymap.reload", &[], false),
    meta("keymap.show", &[], false),
    meta("ops.attrs", &[], false),
    meta("ops.compare", &[], false),
    meta("ops.copy", &[arg("current", ValueKind::Bool, false)], false),
    meta(
        "ops.delete",
        &[
            arg("current", ValueKind::Bool, false),
            arg("permanent", ValueKind::Bool, false),
        ],
        false,
    ),
    meta("ops.link", &[], false),
    meta("ops.mkdir", &[], false),
    meta("ops.move", &[], false),
    meta("ops.multi_rename", &[], false),
    meta("ops.rename", &[], false),
    meta("panel.back", &[], false),
    meta(
        "panel.bookmark.go",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
    meta(
        "panel.bookmark.set",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
    meta(
        "panel.cursor.down",
        &[arg("n", ValueKind::Int, false)],
        true,
    ),
    meta("panel.cursor.end", &[], false),
    meta("panel.cursor.home", &[], false),
    meta(
        "panel.cursor.left",
        &[arg("n", ValueKind::Int, false)],
        true,
    ),
    meta(
        "panel.cursor.page_down",
        &[arg("n", ValueKind::Int, false)],
        true,
    ),
    meta(
        "panel.cursor.page_up",
        &[arg("n", ValueKind::Int, false)],
        true,
    ),
    meta(
        "panel.cursor.right",
        &[arg("n", ValueKind::Int, false)],
        true,
    ),
    meta("panel.cursor.up", &[arg("n", ValueKind::Int, false)], true),
    meta("panel.enter", &[], false),
    meta("panel.forward", &[], false),
    meta("panel.filter", &[arg("mask", ValueKind::Str, false)], false),
    meta("panel.hidden.toggle", &[], false),
    meta(
        "panel.mode",
        &[
            arg("mode", ValueKind::Str, true),
            arg("side", ValueKind::Str, false),
        ],
        false,
    ),
    meta("panel.refresh", &[arg("tab", ValueKind::Int, false)], false),
    meta("panel.root", &[], false),
    meta(
        "panel.sort",
        &[
            arg("key", ValueKind::Str, true),
            arg("desc", ValueKind::Bool, false),
        ],
        false,
    ),
    meta("panel.swap", &[], false),
    meta("panel.switch", &[arg("side", ValueKind::Str, false)], false),
    meta("panel.toggle", &[arg("side", ValueKind::Str, true)], false),
    meta("panel.up", &[], false),
    meta(
        "panel.view_mode",
        &[arg("mode", ValueKind::Str, true)],
        false,
    ),
    meta("pkg.filter", &[arg("name", ValueKind::Str, true)], false),
    meta("pkg.info", &[], false),
    meta("pkg.install", &[arg("id", ValueKind::Str, false)], false),
    meta("pkg.output", &[], false),
    meta("pkg.pick", &[], false),
    meta("pkg.refresh", &[], false),
    meta(
        "pkg.uninstall",
        &[arg("targets", ValueKind::List, false)],
        false,
    ),
    meta(
        "pkg.upgrade",
        &[arg("targets", ValueKind::List, false)],
        false,
    ),
    meta("pkg.upgrade_all", &[], false),
    meta("search.again", &[arg("id", ValueKind::Int, false)], false),
    meta("search.cancel", &[arg("id", ValueKind::Int, false)], false),
    meta("search.close", &[], false),
    meta("search.cursor", &[arg("to", ValueKind::Str, true)], true),
    meta("search.dirs", &[], false),
    meta("search.edit", &[], false),
    meta("search.errors", &[arg("id", ValueKind::Int, true)], false),
    meta("search.files", &[], false),
    meta("search.goto", &[], false),
    meta(
        "search.run",
        &[
            arg("name", ValueKind::Str, false),
            arg("text", ValueKind::Str, false),
            arg("where", ValueKind::List, false),
            arg("case", ValueKind::Bool, false),
            arg("regex", ValueKind::Bool, false),
            arg("recursive", ValueKind::Bool, false),
            arg("kinds", ValueKind::Str, false),
            arg("target", ValueKind::Str, false),
        ],
        false,
    ),
    meta("search.to_panel", &[], false),
    meta("search.view", &[], false),
    meta("sel.all", &[], false),
    meta("sel.invert", &[arg("dirs", ValueKind::Bool, false)], false),
    meta("sel.move", &[arg("motion", ValueKind::Str, true)], true),
    meta("sel.none", &[], false),
    meta("sel.restore", &[], false),
    meta(
        "sel.same",
        &[
            arg("by", ValueKind::Str, true),
            arg("on", ValueKind::Bool, false),
        ],
        false,
    ),
    meta(
        "sel.toggle",
        &[arg("advance", ValueKind::Bool, false)],
        true,
    ),
    meta("tab.close", &[arg("side", ValueKind::Str, false)], false),
    meta("tab.dup", &[arg("side", ValueKind::Str, false)], false),
    meta(
        "tab.goto",
        &[
            arg("n", ValueKind::Int, true),
            arg("side", ValueKind::Str, false),
        ],
        false,
    ),
    meta("tab.new", &[arg("side", ValueKind::Str, false)], false),
    meta("tab.next", &[arg("side", ValueKind::Str, false)], false),
    meta("tab.prev", &[arg("side", ValueKind::Str, false)], false),
    meta("term.copy", &[], false),
    meta("term.focus", &[], false),
    meta(
        "term.key",
        &[
            arg("key", ValueKind::Str, true),
            arg("mods", ValueKind::Str, false),
        ],
        true,
    ),
    meta("term.input", &[arg("text", ValueKind::Str, true)], false),
    meta("term.paste", &[arg("text", ValueKind::Str, false)], false),
    meta(
        "term.scroll",
        &[
            arg("lines", ValueKind::Int, false),
            arg("pages", ValueKind::Int, false),
        ],
        true,
    ),
    meta("ui.cancel", &[], false),
    meta(
        "ui.copy_names",
        &[arg("full", ValueKind::Bool, false)],
        false,
    ),
    meta(
        "ui.draft",
        &[
            arg("command", ValueKind::Str, true),
            arg("arg", ValueKind::Str, true),
            arg("init", ValueKind::Str, false),
        ],
        false,
    ),
    meta("ui.drives", &[arg("side", ValueKind::Str, false)], false),
    meta("ui.help", &[], false),
    meta("ui.history.dirs", &[], false),
    meta("ui.jobs.toggle", &[], false),
    meta("ui.keybar.toggle", &[], false),
    meta(
        "ui.menu",
        &[
            arg("menu", ValueKind::Str, false),
            arg("side", ValueKind::Str, false),
            arg("path", ValueKind::Loc, false),
        ],
        false,
    ),
    meta("ui.menu.sort", &[], false),
    meta("ui.palette", &[], false),
    meta("ui.panel.setup", &[], false),
    meta("ui.panels.toggle_all", &[], false),
    meta("ui.rename", &[], false),
    meta(
        "ui.path_edit",
        &[
            arg("side", ValueKind::Str, false),
            arg("path", ValueKind::Str, false),
        ],
        false,
    ),
    meta("ui.screens", &[], false),
    meta("ui.settings", &[], false),
    meta(
        "ui.split",
        &[
            arg("delta", ValueKind::Int, false),
            arg("reset", ValueKind::Bool, false),
        ],
        true,
    ),
    meta(
        "ui.split.height",
        &[arg("delta", ValueKind::Int, false)],
        true,
    ),
    meta("ui.tree.collapse", &[], false),
    meta("ui.tree.down", &[], true),
    meta("ui.tree.end", &[], false),
    meta("ui.tree.enter", &[], false),
    meta("ui.tree.expand", &[], false),
    meta("ui.tree.home", &[], false),
    meta("ui.tree.page_down", &[], true),
    meta("ui.tree.page_up", &[], true),
    meta("ui.tree.up", &[], true),
    meta("ui.user_menu", &[], false),
    meta("ui.utilities", &[], false),
    meta("ui.window.hide", &[], false),
    meta("ui.window.maximize", &[], false),
    meta("ui.window.minimize", &[], false),
    meta("ui.zoom", &[arg("delta", ValueKind::Int, false)], false),
    meta("vfs.dir_size", &[], false),
    meta("viewer.back", &[], false),
    meta(
        "viewer.bookmark.go",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
    meta(
        "viewer.bookmark.set",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
    meta("viewer.cast.play", &[], false),
    meta(
        "viewer.cast.seek",
        &[
            arg("s", ValueKind::Int, false),
            arg("to", ValueKind::Int, false),
        ],
        true,
    ),
    meta(
        "viewer.cast.speed",
        &[arg("n", ValueKind::Int, true)],
        false,
    ),
    meta("viewer.close", &[], false),
    meta(
        "viewer.codepage",
        &[arg("cp", ValueKind::Str, false)],
        false,
    ),
    meta("viewer.codepage_menu", &[], false),
    meta("viewer.copy", &[], false),
    meta("viewer.dock", &[], false),
    meta("viewer.follow", &[], false),
    meta("viewer.goto", &[arg("to", ValueKind::Str, false)], false),
    meta("viewer.hex", &[], false),
    meta(
        "viewer.history",
        &[arg("kind", ValueKind::Str, false)],
        false,
    ),
    meta("viewer.mode", &[arg("mode", ValueKind::Str, false)], false),
    meta("viewer.next_file", &[], false),
    meta(
        "viewer.open",
        &[
            arg("path", ValueKind::Loc, false),
            arg("alt", ValueKind::Bool, false),
            arg("internal", ValueKind::Bool, false),
            arg("placement", ValueKind::Str, false),
        ],
        false,
    ),
    meta("viewer.prev_file", &[], false),
    meta(
        "viewer.quick",
        &[
            arg("side", ValueKind::Str, true),
            arg("path", ValueKind::Loc, true),
        ],
        false,
    ),
    meta("viewer.reveal", &[], false),
    meta(
        "viewer.scroll",
        &[
            arg("dir", ValueKind::Str, true),
            arg("n", ValueKind::Int, false),
        ],
        true,
    ),
    meta(
        "viewer.search",
        &[
            arg("text", ValueKind::Str, false),
            arg("kind", ValueKind::Str, false),
            arg("case", ValueKind::Bool, false),
            arg("word", ValueKind::Bool, false),
            arg("back", ValueKind::Bool, false),
        ],
        false,
    ),
    meta("viewer.search_cancel", &[], false),
    meta("viewer.search_next", &[], true),
    meta("viewer.search_prev", &[], true),
    meta("viewer.select_all", &[], false),
    meta("viewer.to_editor", &[], false),
    meta("viewer.wrap", &[], false),
    meta("viewer.wrap_mode", &[], false),
];

const PRESETS: [&str; 2] = ["far", "dn"];
const OSES: [Os; 3] = [Os::Windows, Os::Macos, Os::Linux];

fn registry() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    r.register(KNOWN).unwrap();
    r
}

fn load(preset: &str, os: Os) -> (Arc<Keymap>, Vec<KeymapWarning>) {
    // Каталога нет: только вшитые слои.
    let paths = Paths::in_dir(std::env::temp_dir().join("urfin-keymap-no-such-dir"));
    let cfg = KeymapCfg {
        preset: preset.into(),
        user: None,
        ..KeymapCfg::default()
    };
    Keymap::load(&paths, &cfg, &registry(), &FlagRegistry::with_builtin(), os)
}

fn state(focus: Focus) -> AppState {
    let loc = Location::local(if cfg!(windows) { r"C:\w" } else { "/w" });
    let mut st = AppState::new(
        Panel::new(Tab::new(TabId(1), loc.clone())),
        Panel::new(Tab::new(TabId(2), loc)),
    );
    st.focus = focus;
    st
}

/// Панель на виртуальной схеме.
fn on_backend(backend: &str) -> AppState {
    let mut loc = Location::local(if cfg!(windows) { r"C:\w" } else { "/w" });
    loc.backend = backend.into();
    AppState::new(
        Panel::new(Tab::new(TabId(1), loc.clone())),
        Panel::new(Tab::new(TabId(2), loc)),
    )
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "windows",
        Os::Macos => "macos",
        Os::Linux => "linux",
    }
}

/// Печатная клавиша основного блока (AltGr+она печатает символ).
fn printable(k: KeyCode) -> bool {
    let KeyCode::Key(k) = k else {
        return false;
    };
    k.name().chars().count() == 1
        || matches!(
            k,
            urfin_keymap::ev::Key::Minus
                | urfin_keymap::ev::Key::Plus
                | urfin_keymap::ev::Key::Equals
                | urfin_keymap::ev::Key::OpenBracket
                | urfin_keymap::ev::Key::CloseBracket
                | urfin_keymap::ev::Key::Backslash
                | urfin_keymap::ev::Key::Semicolon
                | urfin_keymap::ev::Key::Quote
                | urfin_keymap::ev::Key::Comma
                | urfin_keymap::ev::Key::Period
                | urfin_keymap::ev::Key::Slash
                | urfin_keymap::ev::Key::Backtick
        )
}

/// Без предупреждений на трёх ОС: нет неизвестных команд и аргументов, `Reserved`,
/// `AltGrRisk`, `Ambiguous`, `PrefixShadow`.
#[test]
fn presets_load_without_warnings() {
    for preset in PRESETS {
        for os in OSES {
            let (km, w) = load(preset, os);
            let w: Vec<String> = w.iter().map(ToString::to_string).collect();
            assert_eq!(w, Vec::<String>::new(), "{preset} {os:?}");
            assert!(km.bindings().len() > 100, "{preset} {os:?}");
        }
    }
}

/// Нет `Ctrl+Alt+печатная` (`AltGr`) и Cmd вне macOS (S3 §0); исключение владельца —
/// Ctrl+Alt+H, Ctrl+Alt+R и Ctrl+Alt+G (№88).
#[test]
fn no_altgr_or_super_combinations() {
    for preset in PRESETS {
        for os in OSES {
            let (km, _) = load(preset, os);
            for b in km.bindings() {
                for s in &b.chord.0 {
                    let ctrl = s.mods.intersects(Mods::CTRL | Mods::LCTRL | Mods::RCTRL);
                    let alt = s.mods.intersects(Mods::ALT | Mods::LALT | Mods::RALT);
                    let cmd = s.mods.contains(Mods::CMD);
                    assert!(
                        !(ctrl
                            && alt
                            && !cmd
                            && printable(s.key)
                            && !urfin_keymap::keymap::altgr_owner_ok(*s)),
                        "{preset} {os:?}: {} in [{}]",
                        b.chord,
                        b.src.context
                    );
                    assert!(!cmd || os == Os::Macos, "{preset} {os:?}: {}", b.chord);
                }
            }
        }
    }
}

fn effective(km: &Keymap) -> Vec<String> {
    km.effective()
        .map(|e| {
            let cmds: Vec<String> = e.binding.cmds.iter().map(ToString::to_string).collect();
            format!(
                "[{}] {} = {} ({:?})",
                e.binding.src.context,
                e.chord,
                cmds.join(", "),
                e.binding.layer
            )
        })
        .collect()
}

#[test]
fn effective_keymap_snapshots() {
    for preset in PRESETS {
        for os in OSES {
            let (km, _) = load(preset, os);
            insta::assert_snapshot!(
                format!("{preset}_{}", os_name(os)),
                effective(&km).join("\n")
            );
        }
    }
}

/// Ключи Fluent одного языка: `ключ = …` в начале строки.
fn ftl_keys(lang: &str) -> BTreeSet<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../i18n/locales")
        .join(lang);
    let mut keys = BTreeSet::new();
    for f in std::fs::read_dir(&dir).unwrap() {
        let text = std::fs::read_to_string(f.unwrap().path()).unwrap();
        for line in text.lines() {
            if let Some((k, _)) = line.split_once(" =")
                && !k.starts_with(['#', ' ', '.'])
            {
                keys.insert(k.to_owned());
            }
        }
    }
    keys
}

/// Подписи keybar F1–F12 (S2): `keybar-<id>[-арг]` или запасная `cmd-<id>` — в EN и RU.
#[test]
fn keybar_labels_exist() {
    let langs = [("en", ftl_keys("en")), ("ru", ftl_keys("ru"))];
    let mods = [
        Mods::empty(),
        Mods::SHIFT,
        Mods::CTRL,
        Mods::ALT,
        Mods::CTRL | Mods::SHIFT,
        Mods::ALT | Mods::SHIFT,
        Mods::CTRL | Mods::ALT,
    ];
    let mut missing = BTreeSet::new();
    for preset in PRESETS {
        for os in OSES {
            let (km, _) = load(preset, os);
            for focus in [Focus::Panel, Focus::Viewer, Focus::Editor] {
                let st = state(focus);
                for m in mods {
                    for item in km.keybar(m, &st).into_iter().flatten() {
                        let fallback = format!("cmd-{}", item.cmd.id.as_str().replace('.', "-"));
                        for (lang, keys) in &langs {
                            if !keys.contains(item.label_key.as_str()) && !keys.contains(&fallback)
                            {
                                missing.insert(format!("{lang}: {}", item.label_key));
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(missing, BTreeSet::new());
}

/// Фокус, клавиша, модификаторы, напечатанный символ (en, ru), команда.
type Case = (
    Focus,
    urfin_keymap::ev::Key,
    urfin_keymap::ev::Modifiers,
    Option<(&'static str, &'static str)>,
    &'static str,
);

/// S7-24a: экраны, история и закладки вьювера — свои клавиши у пресета.
fn preset_cases(preset: &str) -> Vec<Case> {
    use urfin_keymap::ev::{Key, Modifiers};
    let (alt, ctrl, shift) = (Modifiers::ALT, Modifiers::CTRL, Modifiers::SHIFT);
    let set = (
        Focus::Viewer,
        Key::Num3,
        ctrl | shift,
        None,
        "viewer.bookmark.set",
    );
    if preset == "far" {
        vec![
            (Focus::Viewer, Key::F12, Modifiers::NONE, None, "ui.screens"),
            (Focus::Viewer, Key::F11, alt, None, "viewer.history"),
            (Focus::Editor, Key::F12, Modifiers::NONE, None, "ui.screens"),
            (Focus::Editor, Key::F11, alt, None, "viewer.history"),
            set,
            (Focus::Viewer, Key::Num3, ctrl, None, "viewer.bookmark.go"),
        ]
    } else {
        vec![
            (Focus::Viewer, Key::Num0, alt, None, "ui.screens"),
            (Focus::Viewer, Key::PageDown, alt, None, "viewer.history"),
            (Focus::Editor, Key::Num0, alt, None, "ui.screens"),
            (Focus::Editor, Key::PageUp, alt, None, "viewer.history"),
            set,
            (Focus::Viewer, Key::Num3, alt, None, "viewer.bookmark.go"),
        ]
    }
}

/// S7-01c: привязки S7 §7 разрешаются в `Focus::Viewer` / `Focus::Editor` обоих пресетов
/// на раскладках en и ru. Буква — по клавише, а не по символу: на ru `Shift+F` печатает
/// «А», но `key` тот же; напечатанный символ поглощается.
#[test]
#[allow(clippy::too_many_lines, reason = "таблица клавиш")]
fn viewer_editor_bindings_on_en_and_ru_layouts() {
    use urfin_keymap::KeyInput;
    use urfin_keymap::ev::{Event, Key, Modifiers, RawInput};
    let alt = Modifiers::ALT;
    let shift = Modifiers::SHIFT;
    let ctrl = Modifiers::CTRL;
    let none = Modifiers::NONE;
    let cases: &[Case] = &[
        (
            Focus::Viewer,
            Key::F,
            shift,
            Some(("F", "А")),
            "viewer.follow",
        ),
        (Focus::Viewer, Key::C, ctrl, None, "viewer.copy"),
        (Focus::Viewer, Key::Insert, ctrl, None, "viewer.copy"),
        (Focus::Viewer, Key::A, ctrl, None, "viewer.select_all"),
        (Focus::Viewer, Key::Backspace, alt, None, "viewer.back"),
        (Focus::Viewer, Key::F4, shift, None, "viewer.mode"),
        (Focus::Viewer, Key::F8, none, None, "viewer.codepage"),
        (Focus::Viewer, Key::F8, shift, None, "viewer.codepage_menu"),
        (Focus::Viewer, Key::F8, alt, None, "viewer.goto"),
        (Focus::Editor, Key::F8, alt, None, "editor.goto"),
        (Focus::Editor, Key::F8, none, None, "editor.codepage"),
        (Focus::Editor, Key::F8, shift, None, "editor.codepage_menu"),
        (Focus::Editor, Key::F10, shift, None, "editor.save_close"),
        // S7-24a: Markdown-команды и предпросмотр.
        (Focus::Editor, Key::B, ctrl, None, "editor.md.bold"),
        (Focus::Editor, Key::I, ctrl, None, "editor.md.italic"),
        (Focus::Editor, Key::K, ctrl, None, "editor.md.link"),
        (Focus::Editor, Key::V, ctrl | shift, None, "editor.preview"),
        // S7-38: правка и движения — команды движка, буквенные — на любой раскладке.
        (Focus::Editor, Key::C, ctrl, None, "editor.copy"),
        (Focus::Editor, Key::V, ctrl, None, "editor.paste"),
        (Focus::Editor, Key::X, ctrl, None, "editor.cut"),
        (Focus::Editor, Key::A, ctrl, None, "editor.select_all"),
        (Focus::Editor, Key::Z, ctrl, None, "editor.undo"),
        (Focus::Editor, Key::Insert, ctrl, None, "editor.copy"),
        (Focus::Editor, Key::Insert, shift, None, "editor.paste"),
        (Focus::Editor, Key::Delete, shift, None, "editor.cut"),
        (Focus::Editor, Key::ArrowLeft, none, None, "editor.move"),
        (Focus::Editor, Key::ArrowRight, shift, None, "editor.move"),
        (Focus::Editor, Key::Home, ctrl, None, "editor.move"),
        (Focus::Editor, Key::PageDown, none, None, "editor.move"),
        (Focus::Editor, Key::Backspace, none, None, "editor.delete"),
        (Focus::Editor, Key::Delete, ctrl, None, "editor.delete"),
        (Focus::Editor, Key::Enter, none, None, "editor.newline"),
        (Focus::Editor, Key::Tab, none, None, "editor.tab"),
        (
            Focus::Editor,
            Key::Tab,
            shift,
            None,
            "editor.block.shift_left",
        ),
        (Focus::Editor, Key::Insert, none, None, "editor.overwrite"),
    ];
    let mut wrong = Vec::new();
    for preset in PRESETS {
        let (km, _) = load(preset, Os::Windows);
        let own = preset_cases(preset);
        for (layout, ru) in [("en", false), ("ru", true)] {
            for &(focus, key, mods, text, want) in cases.iter().chain(&own) {
                let mut ki = KeyInput::new(Arc::clone(&km), &KeymapCfg::default());
                let st = state(focus);
                let mut events = vec![Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: mods,
                }];
                if let Some((en, ru_text)) = text {
                    events.push(Event::Text((if ru { ru_text } else { en }).to_owned()));
                }
                let mut raw = RawInput { events };
                let mut out = Vec::new();
                ki.process(&mut raw, &st, std::time::Instant::now(), &mut out);
                let got: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
                let typed = raw.events.iter().any(|e| matches!(e, Event::Text(_)));
                if got != [want] || typed {
                    wrong.push(format!(
                        "{preset} {layout} {focus:?} {key:?} {mods:?}: {got:?}, текст остался: {typed}"
                    ));
                }
            }
            // Ctrl+C приходит и событием `Copy`.
            let mut ki = KeyInput::new(Arc::clone(&km), &KeymapCfg::default());
            let mut raw = RawInput {
                events: vec![Event::Copy],
            };
            let mut out = Vec::new();
            ki.process(
                &mut raw,
                &state(Focus::Viewer),
                std::time::Instant::now(),
                &mut out,
            );
            let got: Vec<&str> = out.iter().map(|c| c.id.as_str()).collect();
            if got != ["viewer.copy"] {
                wrong.push(format!("{preset} {layout} Copy: {got:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{}",
        wrong.join(
            "
"
        )
    );
}

/// S8 §2.5: привязки поиска разрешаются на раскладках en и ru; вне `search:`, `cmp:` и
/// вне экрана поиска клавиши прежние; Enter на экране различает сам экран.
#[test]
fn search_bindings_on_en_and_ru_layouts() {
    use urfin_keymap::KeyInput;
    use urfin_keymap::ev::{Event, Key, Modifiers, RawInput};
    let (alt, ctrl, none) = (Modifiers::ALT, Modifiers::CTRL, Modifiers::NONE);
    let screen = Focus::Overlay(OverlayKind::Search);
    let panel_cases = [
        (Key::PageUp, ctrl, "search.goto"),
        (Key::F7, alt, "search.files"),
        (Key::Pause, ctrl, "search.cancel"),
    ];
    let screen_cases = [
        (Key::ArrowUp, none, "search.cursor to=up"),
        (Key::ArrowDown, none, "search.cursor to=down"),
        (Key::ArrowLeft, none, "search.cursor to=left"),
        (Key::ArrowRight, none, "search.cursor to=right"),
        (Key::PageUp, none, "search.cursor to=page_up"),
        (Key::PageDown, none, "search.cursor to=page_down"),
        (Key::Home, none, "search.cursor to=home"),
        (Key::End, none, "search.cursor to=end"),
        (Key::F3, none, "search.view"),
        (Key::F4, none, "search.edit"),
        (Key::Enter, ctrl, "search.to_panel"),
        (Key::F7, alt, "search.files"),
        (Key::Escape, none, "search.close"),
    ];
    for preset in PRESETS {
        let (km, _) = load(preset, Os::Windows);
        let press = |st: &AppState, key: Key, mods: Modifiers, text: Option<&str>| {
            let mut ki = KeyInput::new(Arc::clone(&km), &KeymapCfg::default());
            let mut events = vec![Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: mods,
            }];
            events.extend(text.map(|t| Event::Text(t.to_owned())));
            let mut raw = RawInput { events };
            let mut out = Vec::new();
            ki.process(&mut raw, st, std::time::Instant::now(), &mut out);
            out.iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        for backend in ["search", "cmp"] {
            let st = on_backend(backend);
            for (key, mods, want) in panel_cases {
                assert_eq!(press(&st, key, mods, None), [want], "{preset} {backend}");
            }
        }
        // На ru символ клавиши другой, клавиша та же.
        for text in [None, Some("ё")] {
            let st = state(screen);
            for (key, mods, want) in screen_cases {
                assert_eq!(press(&st, key, mods, text), [want], "{preset} {key:?}");
            }
        }
        let none_cmds = Vec::<String>::new();
        assert_eq!(press(&state(screen), Key::Enter, none, None), none_cmds);
        // Вне результатов поиска и экрана — прежние клавиши.
        let local = state(Focus::Panel);
        assert_eq!(press(&local, Key::PageUp, ctrl, None), ["panel.up"]);
        assert_eq!(press(&local, Key::Pause, ctrl, None), none_cmds);
        let menu = state(Focus::Overlay(OverlayKind::Menu));
        assert_eq!(press(&menu, Key::Escape, none, None), ["ui.cancel"]);
    }
}

/// S8 §3.1, №39: Ctrl+Shift+PgDn — `arc.enter` во всех пресетах на раскладках en и ru, вне
/// архива и внутри него; `arc.test` без клавиши (зовётся из `arc.commands`).
#[test]
fn arc_bindings_on_en_and_ru_layouts() {
    use urfin_keymap::KeyInput;
    use urfin_keymap::ev::{Event, Key, Modifiers, RawInput};
    let file = Location::local(if cfg!(windows) {
        r"C:\w\a.zip"
    } else {
        "/w/a.zip"
    });
    let mut inside = file.clone();
    inside.backend = "zip".into();
    inside.path = "/".into();
    inside.outer = Some(Arc::new(file));
    let in_arc = AppState::new(
        Panel::new(Tab::new(TabId(1), inside)),
        Panel::new(Tab::new(
            TabId(2),
            Location::local(if cfg!(windows) { r"C:\w" } else { "/w" }),
        )),
    );
    for preset in ["far", "dn", "urfin"] {
        let (km, w) = load(preset, Os::Windows);
        assert!(w.is_empty(), "{preset}: {w:?}");
        let press = |st: &AppState, key: Key, mods: Modifiers, text: Option<&str>| {
            let mut ki = KeyInput::new(Arc::clone(&km), &KeymapCfg::default());
            let mut events = vec![Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: mods,
            }];
            events.extend(text.map(|t| Event::Text(t.to_owned())));
            let mut raw = RawInput { events };
            let mut out = Vec::new();
            ki.process(&mut raw, st, std::time::Instant::now(), &mut out);
            out.iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        for st in [&state(Focus::Panel), &in_arc] {
            for text in [None, Some("ё")] {
                assert_eq!(
                    press(st, Key::PageDown, cs, text),
                    ["arc.enter"],
                    "{preset}"
                );
            }
            // Соседние клавиши прежние.
            assert_eq!(
                press(st, Key::PageDown, Modifiers::CTRL, None),
                ["panel.enter"],
                "{preset}"
            );
            assert_eq!(press(st, Key::F2, Modifiers::SHIFT, None), ["arc.extract"]);
        }
        assert!(
            km.effective()
                .all(|e| e.binding.cmds.iter().all(|c| c.id.as_str() != "arc.test")),
            "{preset}"
        );
    }
}

/// F03 §3.6: клавиши плеера записи сильнее клавиш вьювера в обоих пресетах и не зависят от
/// раскладки; вне записи клавиши — прежние.
#[test]
fn cast_view_keys() {
    use urfin_keymap::KeyInput;
    use urfin_keymap::ev::{Event, Key, Modifiers, RawInput};

    let press = |flag: bool, preset: &str, key: Key, physical: Key| -> Vec<String> {
        let mut flags = FlagRegistry::with_builtin();
        if flag {
            flags.register("cast_view", |_| true);
        }
        let paths = Paths::in_dir(std::env::temp_dir().join("urfin-keymap-no-such-dir"));
        let cfg = KeymapCfg {
            preset: preset.into(),
            user: None,
            ..KeymapCfg::default()
        };
        let (km, _) = Keymap::load(&paths, &cfg, &registry(), &flags, Os::Windows);
        let mut ki = KeyInput::new(km, &KeymapCfg::default());
        let mut raw = RawInput {
            events: vec![Event::Key {
                key,
                physical_key: Some(physical),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        };
        let mut out = Vec::new();
        ki.process(
            &mut raw,
            &state(Focus::Viewer),
            std::time::Instant::now(),
            &mut out,
        );
        out.iter().map(ToString::to_string).collect()
    };
    for preset in PRESETS {
        for (key, want) in [
            (Key::Space, "viewer.cast.play"),
            (Key::ArrowLeft, "viewer.cast.seek s=-5"),
            (Key::ArrowRight, "viewer.cast.seek s=5"),
            (Key::Num1, "viewer.cast.speed n=1"),
            (Key::Num2, "viewer.cast.speed n=2"),
            (Key::Num4, "viewer.cast.speed n=4"),
        ] {
            assert_eq!(press(true, preset, key, key), [want], "{preset} {key:?}");
        }
        // AZERTY: на месте 4 — апостроф, клавиша цифры определяется по положению.
        assert_eq!(
            press(true, preset, Key::Quote, Key::Num4),
            ["viewer.cast.speed n=4"],
            "{preset}"
        );
        assert_eq!(
            press(false, preset, Key::ArrowLeft, Key::ArrowLeft),
            ["viewer.scroll dir=left"],
            "{preset}"
        );
    }
}

/// Строка шпаргалки: контекст, сочетание, команды и где действует (W, M, L).
struct Row {
    context: String,
    chord: String,
    cmds: String,
    on: [bool; 3],
}

/// `docs/keys.md`: шпаргалка по пресетам.
fn keys_doc() -> String {
    let mut out = String::from(
        "# Сочетания клавиш\n\n\
         <!-- Генерируется тестом `keys_doc_is_current` (crates/keymap/tests/known_commands.rs):\n\
         URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-keymap keys_doc -->\n\n\
         Встроенные пресеты (S3 §3). Пресет — `[keymap] preset` в `config.toml`, свои \
         привязки — `keymaps/user.toml` рядом с ним (S3 §2.2). Команда работает, когда её \
         сессия есть в сборке (`docs/COMMANDS.md`). `cmdline_empty` — командная строка пуста. \
         ОС: W — Windows, M — macOS, L — Linux.\n",
    );
    for preset in PRESETS {
        let _ = writeln!(out, "\n## {preset}\n");
        // В порядке загрузки Windows, затем добавленное оверлеями.
        let mut rows: Vec<Row> = Vec::new();
        for (i, os) in OSES.into_iter().enumerate() {
            let (km, _) = load(preset, os);
            for e in km.effective() {
                let cmds: Vec<String> = e.binding.cmds.iter().map(ToString::to_string).collect();
                let (context, chord, cmds) = (
                    e.binding.src.context.to_string(),
                    e.chord.display(os).to_string(),
                    cmds.join(", "),
                );
                if let Some(r) = rows
                    .iter_mut()
                    .find(|r| r.context == context && r.chord == chord && r.cmds == cmds)
                {
                    r.on[i] = true;
                } else {
                    let mut on = [false; 3];
                    on[i] = true;
                    rows.push(Row {
                        context,
                        chord,
                        cmds,
                        on,
                    });
                }
            }
        }
        let mut contexts: Vec<&str> = Vec::new();
        for r in &rows {
            if !contexts.contains(&r.context.as_str()) {
                contexts.push(&r.context);
            }
        }
        for ctx in contexts {
            let ctx_md = ctx.replace('|', "\\|");
            let _ = writeln!(
                out,
                "### `{ctx_md}`\n\n| Сочетание | Команда | ОС |\n|---|---|---|"
            );
            for r in rows.iter().filter(|r| r.context == ctx) {
                let oses = ["W", "M", "L"]
                    .iter()
                    .zip(r.on)
                    .filter(|(_, on)| *on)
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = writeln!(
                    out,
                    "| `{}` | `{}` | {oses} |",
                    r.chord.replace('|', "\\|"),
                    r.cmds.replace('|', "\\|")
                );
            }
            out.push('\n');
        }
    }
    out
}

#[test]
fn keys_doc_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/keys.md");
    let doc = keys_doc();
    if std::env::var_os("URFIN_UPDATE_DOCS").is_some() {
        std::fs::write(&path, &doc).unwrap();
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    // Генерируемый документ обновляет полоса docs-gen раз в день (скилл pets-dev):
    // устаревший не блокирует PR; строго — с `URFIN_DOCS_STRICT=1`.
    if on_disk != doc {
        let msg =
            "docs/keys.md устарел: URFIN_UPDATE_DOCS=1 cargo nextest run -p urfin-keymap keys_doc";
        assert!(std::env::var_os("URFIN_DOCS_STRICT").is_none(), "{msg}");
        eprintln!("{msg}");
    }
}
