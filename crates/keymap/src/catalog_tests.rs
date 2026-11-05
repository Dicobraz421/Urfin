use std::sync::Arc;

use urfin_core::{Paths, Value};

use super::*;
use crate::compile::tests::{flags, reg};
use crate::keymap::Layer;
use crate::{FlagRegistry, KeymapCfg, Os};

const OS: Os = Os::Windows;

fn i18n() -> Arc<I18n> {
    urfin_i18n::current()
}

fn load(user: &str) -> Arc<Keymap> {
    let base = "format = 1\nname = \"p\"\n[context.panel]\n\"F5\" = \"a.one\"\n\"Shift+F5\" = { cmd = \"ops.copy\", current = true }\n\"Ctrl+K Ctrl+H\" = \"a.two\"\n[context.\"panel && cmdline_empty\"]\n\"Delete\" = \"a.one\"\n[context.viewer]\n\"F5\" = \"a.three\"\n";
    let u = format!("name = \"mine\"\n{user}");
    Keymap::from_str_layers(
        &[(base, Layer::Preset), (&u, Layer::User)],
        &reg(),
        &flags(),
        OS,
    )
    .0
}

fn row<'c>(c: &'c Catalog, key: &str) -> &'c KeyRow {
    c.rows.iter().find(|r| r.target.key() == key).unwrap()
}

#[test]
fn places_read_like_people_speak() {
    assert_eq!(place("panel"), "keys-where-panel");
    assert_eq!(place("panel && cmdline_empty"), "keys-where-panel");
    assert_eq!(place("panel || cmdline"), "keys-where-both");
    assert_eq!(
        place("(panel || cmdline) && !cmdline_empty"),
        "keys-where-both"
    );
    assert_eq!(place("panel && in_pkg && cmdline_empty"), "keys-where-pkg");
    assert_eq!(place("panel && !in_du"), "keys-where-panel");
    assert_eq!(place("viewer && cast_view"), "keys-where-cast");
    assert_eq!(place("overlay && qs"), "keys-where-qs");
    assert_eq!(place("overlay"), "keys-where-overlay");
    assert_eq!(place("global"), "keys-where-window");
    assert_eq!(place("os_windows"), "keys-where-other");
}

#[test]
fn row_merges_contexts_of_one_place_and_shows_unbound_commands() {
    let c = Catalog::build(&load(""), &i18n(), false);
    let one = row(&c, "a.one");
    let keys: Vec<&str> = one.chips.iter().map(|k| k.label.as_str()).collect();
    assert_eq!(keys, ["F5", "Delete"]);
    assert_eq!(one.chips[1].context, "panel && cmdline_empty");
    assert!(!one.changed);
    // Команда без клавиши видна; с обязательным аргументом — нет.
    let unbound = row(&c, "editor.new");
    assert!(unbound.chips.is_empty());
    assert!(c.rows.iter().all(|r| r.target.key() != "panel.sort"));
    // Аккорд — как есть.
    assert_eq!(row(&c, "a.two").chips[0].label, "Ctrl+K Ctrl+H");
    assert_eq!(row(&c, "a.three").place, "keys-where-viewer");
}

#[test]
fn user_changes_are_visible_and_removed_key_keeps_row() {
    let c = Catalog::build(
        &load("[context.panel]\n\"Shift+F5\" = false\n\"F5\" = \"a.three\"\n"),
        &i18n(),
        false,
    );
    assert!(row(&c, "a.one").changed);
    let labels: Vec<&str> = row(&c, "a.one")
        .chips
        .iter()
        .map(|k| k.label.as_str())
        .collect();
    assert_eq!(labels, ["Delete"], "F5 перекрыта в том же контексте");
    let args = c
        .rows
        .iter()
        .find(|r| r.target.key().starts_with("ops.copy"))
        .unwrap();
    assert!(args.chips.is_empty() && args.changed, "строка не пропала");
}

#[test]
fn search_by_title_id_and_key() {
    let c = Catalog::build(&load(""), &i18n(), false);
    let keys = |q: &str| -> Vec<String> {
        c.filter(q)
            .into_iter()
            .map(|i| c.rows[i as usize].target.key())
            .collect()
    };
    assert_eq!(keys("delete"), ["a.one"]);
    assert_eq!(keys("ctrl+k"), ["a.two"]);
    assert!(keys("a.THREE").contains(&"a.three".to_owned()));
    assert!(keys("f5").len() >= 3);
    assert_eq!(keys("нет такого"), Vec::<String>::new());
    assert_eq!(keys("").len(), c.rows.len());
    assert_eq!(keys("f5 a.three"), ["a.three"], "все слова запроса");
}

#[test]
fn conflict_only_where_contexts_meet() {
    // Одна клавиша в одном месте.
    let km = load(
        "[context.panel]\n\"F8\" = \"a.one\"\n[context.\"panel || cmdline\"]\n\"F8\" = \"a.two\"\n",
    );
    let c = Catalog::build(&km, &i18n(), false);
    assert_eq!(c.conflicts, 1);
    assert_eq!(row(&c, "a.one").conflicts.len(), 1);
    let two = c
        .rows
        .iter()
        .find(|r| r.place == "keys-where-both")
        .unwrap();
    assert_eq!(two.conflicts.len(), 1);
    // В разных контекстах (панели и просмотр) — не конфликт; F5 такая в пресете.
    assert_eq!(Catalog::build(&load(""), &i18n(), false).conflicts, 0);
    let km = load("[context.viewer]\n\"F8\" = \"a.one\"\n[context.panel]\n\"F8\" = \"a.two\"\n");
    assert_eq!(Catalog::build(&km, &i18n(), false).conflicts, 0);
}

#[test]
fn clashes_respect_overlap() {
    let km = load("");
    let f5 = Chord::parse("F5", OS).unwrap();
    let t = Target::command("a.two");
    let found = clashes(&km, &f5, "panel", &t);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].target.key(), "a.one");
    // В редакторе F5 свободна: контексты не пересекаются.
    assert_eq!(clashes(&km, &f5, "editor", &t), Vec::new());
    // Своя же команда — не спор.
    assert_eq!(
        clashes(&km, &f5, "panel", &Target::command("a.one")),
        Vec::new()
    );
    // Частичное пересечение: `panel || cmdline` спорит с `panel`.
    assert_eq!(clashes(&km, &f5, "panel || cmdline", &t).len(), 1);
}

#[test]
fn alt_letter_row_and_args_title() {
    let km = load("");
    let c = Catalog::build(&km, &i18n(), false);
    assert!(c.rows.iter().all(|r| !r.alt_letter));
    let args = c
        .rows
        .iter()
        .find(|r| r.target.key().starts_with("ops.copy"))
        .unwrap();
    assert!(args.title.contains("ops.copy") || !args.title.is_empty());
    assert_eq!(arg_text("n", &Value::Int(3)), "3");
}

/// Настоящие пресеты: вшитый Urfin = Far, таблица строится, конфликтов нет.
#[test]
fn builtin_presets_have_no_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        config_dir: dir.path().join("c"),
        state_dir: dir.path().join("s"),
        log_dir: dir.path().join("l"),
        cache_dir: dir.path().join("k"),
        portable: true,
    };
    let reg = all_presets_registry();
    let flags = FlagRegistry::with_builtin();
    let load = |p: &str| {
        let cfg = KeymapCfg {
            preset: p.into(),
            ..KeymapCfg::default()
        };
        Keymap::load(&paths, &cfg, reg, &flags, OS).0
    };
    let far = Catalog::build(&load("far"), &i18n(), true);
    let urfin = Catalog::build(&load("urfin"), &i18n(), true);
    assert_eq!(far.rows.len(), urfin.rows.len());
    for p in ["far", "dn", "urfin"] {
        let c = Catalog::build(&load(p), &i18n(), true);
        let list: Vec<String> = c
            .rows
            .iter()
            .filter(|r| !r.conflicts.is_empty())
            .map(|r| format!("{} {:?} {:?}", r.title, r.place, r.conflicts))
            .collect();
        assert_eq!(c.conflicts, 0, "{p}: {list:#?}");
    }
}

/// Все команды из текстов пресетов — чтобы пресеты загрузились целиком.
fn all_presets_registry() -> &'static urfin_core::CommandRegistry {
    use std::sync::OnceLock;
    static R: OnceLock<urfin_core::CommandRegistry> = OnceLock::new();
    R.get_or_init(|| {
        let mut r = urfin_core::CommandRegistry::new();
        for text in [
            include_str!("../presets/base.toml"),
            include_str!("../presets/far.toml"),
            include_str!("../presets/dn.toml"),
        ] {
            let table: toml::Table = toml::from_str(text).unwrap();
            let mut ids = Vec::new();
            collect(&toml::Value::Table(table), &mut ids);
            for id in ids {
                let id: &'static str = Box::leak(id.into_boxed_str());
                if r.get(id).is_none() {
                    let meta: &'static [urfin_core::CommandMeta] =
                        Box::leak(Box::new([urfin_core::CommandMeta {
                            id,
                            title: id,
                            category: "t",
                            args: ANY,
                            repeatable: false,
                        }]));
                    let _ = r.register(meta);
                }
            }
        }
        r
    })
}

/// Аргументы пресетов (проверка значений — в `known_commands`).
const ANY: &[urfin_core::ArgSpec] = &[];

fn collect(v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::Table(t) => {
            if let Some(toml::Value::String(c)) = t.get("cmd") {
                out.push(c.split(' ').next().unwrap_or(c).to_owned());
            }
            for (k, x) in t {
                if k == "cmd" {
                    continue;
                }
                match x {
                    toml::Value::String(s) if s.contains('.') => {
                        out.push(s.split(' ').next().unwrap_or(s).to_owned());
                    }
                    other => collect(other, out),
                }
            }
        }
        toml::Value::Array(a) => {
            for x in a {
                match x {
                    toml::Value::String(s) => out.push(s.split(' ').next().unwrap_or(s).to_owned()),
                    other => collect(other, out),
                }
            }
        }
        _ => {}
    }
}
