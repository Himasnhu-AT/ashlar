use super::*;

fn sample(dirty: bool, recent: &[&str]) -> MenuSpec {
    let recent_items: Vec<MenuNode> = recent.iter().enumerate().map(|(i, r)| Item::new(format!("file.openRecent.{i}"), *r).into()).collect();
    MenuSpec::new(vec![
        Submenu::new(
            "App",
            vec![
                MenuNode::role(Role::About),
                MenuNode::Separator,
                MenuNode::role(Role::Services),
                MenuNode::Separator,
                MenuNode::role(Role::Hide),
                MenuNode::role(Role::HideOthers),
                MenuNode::role(Role::ShowAll),
                MenuNode::Separator,
                Item::new("app.quit", "Quit").accelerator("Mod+Q").into(),
            ],
        ),
        Submenu::new(
            "File",
            vec![
                Item::new("file.open", "Open…").accelerator("Mod+O").into(),
                Submenu::new("Open Recent", recent_items).into(),
                MenuNode::Separator,
                Item::new("file.save", if dirty { "Save*" } else { "Save" }).accelerator("Mod+S").enabled(dirty).into(),
            ],
        ),
        Submenu::new("View", vec![Item::new("view.grid", "Show Grid").checked(dirty).into(), Item::new("view.tool", "Tool").accelerator("V").into()]),
        Submenu::new("Window", vec![MenuNode::role(Role::Minimize), MenuNode::role(Role::Zoom)]).with_role(SubmenuRole::Window),
    ])
}

#[test]
fn items_find_and_paths() {
    let spec = sample(false, &["a.txt"]);
    let ids: Vec<&str> = spec.items().iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["app.quit", "file.open", "file.openRecent.0", "file.save", "view.grid", "view.tool"]);
    assert_eq!(spec.find("file.save").map(|i| i.enabled), Some(false));
    assert!(spec.find("nope").is_none());
    assert!(matches!(spec.node(&[1, 1, 0]), Some(MenuNode::Item(i)) if i.id == "file.openRecent.0"));
    assert!(spec.node(&[1, 3, 0]).is_none(), "an item has no children");
    assert!(spec.node(&[9, 0]).is_none());
    assert!(spec.node(&[]).is_none());
    assert!(spec.validate().is_ok());
}

#[test]
fn state_changes_patch_and_structure_changes_rebuild() {
    let clean = sample(false, &["a.txt"]);
    let dirty = sample(true, &["a.txt"]);
    // Label, enabled and check changes: same structure, different state.
    assert_eq!(clean.structure_hash(), dirty.structure_hash());
    assert_ne!(clean.state_hash(), dirty.state_hash());
    // A new recent file: the structure changes.
    let more = sample(false, &["a.txt", "b.txt"]);
    assert_ne!(clean.structure_hash(), more.structure_hash());
    // Nothing changed: both hashes are stable.
    assert_eq!(clean.structure_hash(), sample(false, &["a.txt"]).structure_hash());
    assert_eq!(clean.state_hash(), sample(false, &["a.txt"]).state_hash());
    // A non-menu-safe accelerator changing does not rebuild (it is never a native key).
    let mut typed = clean.clone();
    if let Some(MenuNode::Item(i)) = typed.menus.get_mut(2).and_then(|m| m.children.get_mut(1)) {
        i.accelerator = Some("B".into());
    }
    assert_eq!(clean.structure_hash(), typed.structure_hash());
    // A menu-safe one does.
    if let Some(MenuNode::Item(i)) = typed.menus.get_mut(2).and_then(|m| m.children.get_mut(1)) {
        i.accelerator = Some("Mod+B".into());
    }
    assert_ne!(clean.structure_hash(), typed.structure_hash());
}

#[test]
fn validation_reports_every_problem() {
    let mut spec = sample(false, &[]);
    spec.menus.push(Submenu::new(
        "Bad",
        vec![
            Item::new("file.open", "Duplicate").into(),
            Item::new("", "No id").into(),
            Item::new("x", "Bad key").accelerator("Mod+Banana").into(),
            Item::new("y", "x".repeat(MAX_TEXT + 1)).into(),
        ],
    ));
    let problems = spec.validate().err().unwrap_or_default();
    assert_eq!(problems.len(), 4, "{problems:?}");
    // Too deep.
    let mut deep: MenuNode = Item::new("leaf", "Leaf").into();
    for i in 0..(MAX_DEPTH + 3) {
        deep = Submenu::new(format!("L{i}"), vec![deep]).into();
    }
    let deep = MenuSpec::new(vec![Submenu::new("Deep", vec![deep])]);
    assert!(deep.validate().is_err());
    // Walking a too-deep tree stops instead of overflowing.
    assert!(deep.items().is_empty());
}

#[test]
fn the_spec_round_trips_through_json_for_agents() {
    let spec = sample(true, &["a.txt"]);
    let json = serde_json::to_value(&spec).unwrap();
    assert_eq!(json["menus"][1]["children"][0]["type"], "item");
    assert_eq!(json["menus"][0]["children"][0], serde_json::json!({"type": "role", "role": "about"}));
    let back: MenuSpec = serde_json::from_value(json).unwrap();
    assert_eq!(back, spec);
    // Defaults: an item without `enabled` is enabled.
    let item: MenuNode = serde_json::from_str(r#"{"type":"item","id":"a","label":"A"}"#).unwrap();
    assert_eq!(item, Item::new("a", "A").into());
}

#[test]
fn a_bar_is_mirrored_from_commands() {
    let cmd = |id: &str, menu: &[&str], shortcut: Option<&str>, enabled: bool| ashlar_command::CommandInfo {
        id: id.into(),
        label: id.into(),
        menu: menu.iter().map(|s| s.to_string()).collect(),
        shortcut: shortcut.map(Into::into),
        params_schema: serde_json::Value::Null,
        params_text: None,
        enabled,
        disabled_reason: None,
        mutates: false,
    };
    let commands = vec![
        cmd("edit.undo", &["Edit"], Some("Mod+Z"), false),
        cmd("file.save", &["File"], Some("Mod+S"), true),
        cmd("file.export.png", &["File", "Export"], None, true),
        cmd("hidden", &[], None, true),
        cmd("file.export.svg", &["File", "Export"], None, true),
        cmd("help.docs", &["Help"], None, true),
    ];
    let spec = MenuSpec::from_commands(&commands, &["File", "Edit"]);
    let tops: Vec<&str> = spec.menus.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(tops, ["File", "Edit", "Help"]);
    let ids: Vec<&str> = spec.items().iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["file.save", "file.export.png", "file.export.svg", "edit.undo", "help.docs"]);
    assert!(matches!(spec.node(&[0, 1]), Some(MenuNode::Submenu(s)) if s.label == "Export" && s.children.len() == 2));
    assert_eq!(spec.find("edit.undo").map(|i| (i.enabled, i.accelerator.as_deref())), Some((false, Some("Mod+Z"))));
    assert!(spec.validate().is_ok());
}
