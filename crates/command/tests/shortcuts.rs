#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ashlar_command::Keymap;
use ashlar_command::keymap::Overrides;
use ashlar_command::shortcut::{self, Chord, Key, Mods, Named, Platform, display, normalize, parse_list};
use serde_json::json;

fn c(s: &str) -> Chord {
    Chord::parse(s).unwrap()
}

#[test]
fn portable_round_trips() {
    for s in [
        "Mod+S",
        "Mod+Shift+S",
        "Mod+Ctrl+Alt+Shift+K",
        "Alt+Left",
        "Shift+Delete",
        "F5",
        "Mod+F12",
        "Mod++",
        "Mod+Shift+/",
        "Mod+]",
        "Space",
        "Escape",
        "PageDown",
        "Ctrl+Tab",
        "1",
        "Mod+Enter",
    ] {
        let chord = c(s);
        assert_eq!(chord.portable(), s);
        assert_eq!(c(&chord.portable()), chord);
        // the macOS display form parses back to the same chord
        assert_eq!(c(&chord.display(Platform::Mac)), chord, "{s} -> {}", chord.display(Platform::Mac));
        // and serde uses the portable form
        assert_eq!(serde_json::to_value(chord).unwrap(), json!(s));
        assert_eq!(serde_json::from_value::<Chord>(json!(s)).unwrap(), chord);
    }
}

#[test]
fn legacy_spellings() {
    assert_eq!(normalize("Cmd+Z").as_deref(), Some("Mod+Z"));
    assert_eq!(normalize("cmd+shift+z").as_deref(), Some("Mod+Shift+Z"));
    assert_eq!(normalize("CmdOrCtrl+Option+n").as_deref(), Some("Mod+Alt+N"));
    assert_eq!(normalize("Shift+Cmd+S").as_deref(), Some("Mod+Shift+S"));
    assert_eq!(normalize("Ctrl+Z").as_deref(), Some("Ctrl+Z"));
    assert_eq!(normalize("Mod+Y / Mod+Shift+Z").as_deref(), Some("Mod+Y / Mod+Shift+Z"));
    assert_eq!(normalize("Esc").as_deref(), Some("Escape"));
    assert_eq!(normalize("Return").as_deref(), Some("Enter"));
    assert_eq!(normalize("Mod+Plus").as_deref(), Some("Mod++"));
    assert_eq!(normalize("⌘⇧S").as_deref(), Some("Mod+Shift+S"));
    assert_eq!(normalize("⌥←").as_deref(), Some("Alt+Left"));
    assert_eq!(c("+").key, Key::Char('+'));
}

#[test]
fn junk_is_refused() {
    for s in ["", " ", "Mod+", "Hyper+S", "Mod+Shift", "F0", "F25", "F1x", "SS", "Mod+\u{7}", "Mod+ +S", " / ", &"Mod+".repeat(40)] {
        assert!(Chord::parse(s).is_err() || parse_list(s).is_err(), "{s:?} parsed");
        assert_eq!(normalize(s), None, "{s:?}");
    }
}

#[test]
fn platform_display() {
    assert_eq!(display("Mod+Shift+S", Platform::Mac).as_deref(), Some("⇧⌘S"));
    assert_eq!(display("Mod+Shift+S", Platform::Other).as_deref(), Some("Ctrl+Shift+S"));
    assert_eq!(display("Ctrl+Alt+Delete", Platform::Mac).as_deref(), Some("⌃⌥⌦"));
    assert_eq!(display("Mod+Y / Mod+Shift+Z", Platform::Other).as_deref(), Some("Ctrl+Y / Ctrl+Shift+Z"));
    assert_eq!(display("Mod+Ctrl+F", Platform::Other).as_deref(), Some("Ctrl+F"));
    assert_eq!(c("Alt+F4").display(Platform::Other), "Alt+F4");
}

#[test]
fn mod_and_ctrl_fold_off_mac() {
    assert_ne!(c("Mod+Z").on(Platform::Mac), c("Ctrl+Z").on(Platform::Mac));
    assert_eq!(c("Mod+Z").on(Platform::Other), c("Ctrl+Z").on(Platform::Other));
    // what a UI produces for Ctrl+Z on Windows
    let event = Chord::new(Mods { ctrl: true, ..Mods::NONE }, Key::Char('Z'));
    let km = Keymap::new(Platform::Other, [("edit.undo", Some("Mod+Z"))]);
    assert_eq!(km.lookup(&event), Some("edit.undo"));
    let km = Keymap::new(Platform::Mac, [("edit.undo", Some("Mod+Z"))]);
    assert_eq!(km.lookup(&event), None);
    assert_eq!(km.lookup(&Chord::new(Mods::COMMAND, Key::Char('Z'))), Some("edit.undo"));
}

fn keymap(platform: Platform) -> Keymap {
    Keymap::new(
        platform,
        [
            ("edit.undo", Some("Mod+Z")),
            ("edit.redo", Some("Mod+Shift+Z / Mod+Y")),
            ("file.save", Some("Mod+S")),
            ("view.zoomIn", Some("Mod+=")),
            ("timeline.split", Some("Ctrl+K")),
            ("timeline.cut", Some("Mod+K")),
            ("help.search", None),
            ("broken", Some("Hyper+Q")),
        ],
    )
}

#[test]
fn keymap_lookup_overrides_and_conflicts() {
    let mut km = keymap(Platform::Other);
    assert_eq!(km.invalid_defaults(), [("broken".to_string(), "Hyper+Q".to_string())]);
    assert_eq!(km.lookup(&c("Mod+Y")), Some("edit.redo"));
    assert_eq!(km.lookup(&c("Mod+Shift+Z")), Some("edit.redo"));
    // Ctrl+K and Mod+K are one key off macOS: a conflict there, not on macOS
    let conflicts = km.conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].ids, ["timeline.split", "timeline.cut"]);
    assert!(keymap(Platform::Mac).conflicts().is_empty());

    // the user binds a command with no default key, and steals Mod+S
    assert!(km.bind("help.search", vec![c("Mod+S"), c("F1")]));
    assert_eq!(km.would_conflict("help.search", &c("Mod+S")), ["file.save"]);
    assert_eq!(km.lookup(&c("Mod+S")), Some("help.search"), "user bindings win");
    assert_eq!(km.lookup(&c("F1")), Some("help.search"));
    // unbinding a default
    km.bind("edit.undo", vec![]);
    assert_eq!(km.lookup(&c("Mod+Z")), None);
    assert!(km.keys_for("edit.undo").is_empty());
    assert!(!km.bind("no.such", vec![c("F2")]));

    // saved and restored through settings
    let saved = serde_json::to_value(km.overrides()).unwrap();
    assert_eq!(saved, json!({"bindings": {"edit.undo": [], "help.search": ["Mod+S", "F1"]}}));
    let mut fresh = keymap(Platform::Other);
    assert_eq!(fresh.apply_overrides_json(&saved), 0);
    assert_eq!(fresh.overrides(), km.overrides());
    assert_eq!(fresh.lookup(&c("Mod+S")), Some("help.search"));

    // binding back to the defaults drops the override
    km.bind("edit.undo", vec![c("Mod+Z")]);
    km.reset("help.search");
    assert_eq!(km.overrides(), Overrides::default());
    assert_eq!(km.lookup(&c("Mod+S")), Some("file.save"));
}

#[test]
fn saved_overrides_are_read_leniently() {
    let mut km = keymap(Platform::Mac);
    let junk = json!({"bindings": {
        "file.save": ["Mod+Alt+S", "Hyper+S", 7],
        "no.such": ["F3"],
        "edit.redo": "Mod+Y",
        &"x".repeat(1000): ["F4"]
    }});
    let skipped = km.apply_overrides_json(&junk);
    assert_eq!(skipped, 4);
    assert_eq!(km.keys_for("file.save"), [c("Mod+Alt+S")]);
    assert_eq!(km.keys_for("edit.redo"), [c("Mod+Shift+Z"), c("Mod+Y")]);
    assert_eq!(km.apply_overrides_json(&json!(null)), 0);
    assert_eq!(km.apply_overrides_json(&json!({"bindings": 3})), 0);
}

#[test]
fn named_keys_have_canonical_names() {
    assert_eq!(Named::PageUp.name(), "PageUp");
    assert_eq!(shortcut::format_list(&[c("pgup"), c("arrowleft")]), "PageUp / Left");
}
