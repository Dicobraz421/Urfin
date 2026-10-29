use super::*;

/// Реестр команд core, ui, vfs и keymap, как в приложении.
fn registered() -> urfin_core::CommandRegistry {
    crate::tests::registry()
}

#[test]
fn builtin_menu_has_only_registered_commands() {
    let (menus, warnings) = parse(MENU_TOML, &registered()).unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    let ids: Vec<&str> = menus.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["left", "files", "commands", "utilities", "options", "right"]
    );
    assert_eq!(menus[0].side, Some(Side::Left));
    // Подписи пунктов есть в обоих языках.
    let catalog = urfin_i18n::Catalog::embedded();
    for tag in ["en", "ru"] {
        let i18n = catalog.build(&tag.parse().unwrap()).0;
        for m in &menus {
            assert!(i18n.has(&m.title), "{tag}: {}", m.title);
            for item in &m.items {
                if let Item::Cmd { label, .. } = item {
                    assert!(i18n.has(label), "{tag}: {label}");
                }
            }
        }
    }
}

/// Во вшитом меню буквы выбора не повторяются ни в одном языке.
#[test]
fn builtin_menu_letters_are_unique() {
    let (menus, _) = parse(MENU_TOML, &registered()).unwrap();
    let catalog = urfin_i18n::Catalog::embedded();
    for tag in ["en", "ru"] {
        let i18n = catalog.build(&tag.parse().unwrap()).0;
        assert_eq!(
            letter_conflicts(&menus, &i18n),
            Vec::<String>::new(),
            "{tag}"
        );
    }
}

/// Неизвестный id — предупреждение, пункт скрыт; разделители по краям и подряд — нет.
#[test]
fn unknown_command_is_hidden_with_warning() {
    let text = r#"
[[menu]]
id = "x"
title = "menu-files"
items = [
    { sep = true },
    { cmd = "no.such" },
    { cmd = "sel.all" },
    { sep = true },
    { sep = true },
    { cmd = "sel.none" },
    { sep = true },
]
"#;
    let (menus, warnings) = parse(text, &registered()).unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("no.such"));
    let items = &menus[0].items;
    assert_eq!(items.len(), 3, "{items:?}");
    assert!(matches!(items[1], Item::Sep));
    assert!(parse("[[menu]]\nid = 1", &registered()).is_err());
}

/// Аргументы проверяются по реестру: лишний, не того типа, без обязательного.
#[test]
fn bad_arguments_hide_the_item() {
    let text = r#"
[[menu]]
id = "x"
title = "menu-files"
items = [
    { cmd = "panel.sort bogus=1" },
    { cmd = "ui.zoom delta=big" },
    { cmd = "panel.view_mode" },
    { cmd = "panel.sort key=size" },
]
"#;
    let (menus, warnings) = parse(text, &registered()).unwrap();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert_eq!(menus[0].items.len(), 1);
}

#[test]
fn neighbor_wraps() {
    let (menus, _) = parse(MENU_TOML, &registered()).unwrap();
    assert_eq!(neighbor(&menus, "left", -1).as_deref(), Some("right"));
    assert_eq!(neighbor(&menus, "files", 1).as_deref(), Some("commands"));
}
