//! The parts of the API that must run on the main thread (the menu bar, the tray, Dock progress,
//! Apple event handlers), exercised on the real main thread: this test has no harness, so `main` is
//! the process's main thread. Nothing is shown on screen: the app never runs its event loop.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(all(target_os = "macos", feature = "menu"))]
fn menu() {
    use ashlar_native::menu::{Item, MenuNode, MenuSpec, NativeMenuBar, Role, Submenu, Update};

    let spec = |label: &str, extra: bool| {
        let mut file = vec![
            Item::new("file.open", "Open…").accelerator("Mod+O").into(),
            MenuNode::Separator,
            Item::new("file.save", label).accelerator("Mod+S").into(),
        ];
        if extra {
            file.push(Item::new("file.close", "Close").accelerator("Mod+W").into());
        }
        MenuSpec::new(vec![
            Submenu::new("App", vec![MenuNode::role(Role::About), MenuNode::Separator, MenuNode::role(Role::Quit)]),
            Submenu::new("File", file),
            Submenu::new("View", vec![Item::new("view.grid", "Grid").checked(true).into()]),
        ])
    };
    let mut bar = NativeMenuBar::install(&spec("Save", false), ashlar_native::no_waker()).expect("menu bar on the main thread");
    assert_eq!(bar.update(&spec("Save", false)).unwrap(), Update::Unchanged);
    assert_eq!(bar.update(&spec("Save As…", false)).unwrap(), Update::Patched);
    assert_eq!(bar.update(&spec("Save As…", true)).unwrap(), Update::Rebuilt);
    assert_eq!(bar.update(&spec("Save As…", true)).unwrap(), Update::Unchanged);
    assert!(bar.try_recv().is_none());
}

#[cfg(all(target_os = "macos", feature = "open-files"))]
fn open_files() {
    let q = ashlar_native::open_files::OpenFiles::new(ashlar_native::no_waker());
    q.listen_os().expect("Apple event handlers on the main thread");
    // A second call only swaps the queue.
    q.listen_os().unwrap();
}

#[cfg(all(target_os = "macos", feature = "progress"))]
fn progress() {
    use ashlar_native::progress::{Progress, set, set_badge};
    // An unbundled binary may have no icon to draw on: an error is fine, a crash is not.
    let _ = set(None, Progress::Normal(0.4));
    set(None, Progress::None).unwrap();
    set_badge(Some("3")).unwrap();
    set_badge(None).unwrap();
}

#[cfg(all(target_os = "macos", feature = "tray"))]
fn tray() {
    use ashlar_native::menu::{Item, MenuNode, MenuSpec, Submenu};
    use ashlar_native::tray::{Icon, Tray, TraySpec};

    let menu = |label: &str| {
        MenuSpec::new(vec![Submenu::new("Tray", vec![Item::new("show", label).into(), MenuNode::Separator, Item::new("quit", "Quit").into()])])
    };
    let icon = Icon { rgba: vec![255; 16 * 16 * 4], width: 16, height: 16 };
    // With the menu bar's events routed too, a tray item must not reach the bar.
    let tray = Tray::new(TraySpec::new("test", "ashlar test", icon.clone(), menu("Show")).template(true), ashlar_native::no_waker())
        .expect("tray on the main thread");
    tray.set_tooltip("still here").unwrap();
    tray.set_icon(&icon).unwrap();
    tray.set_menu(&menu("Show window")).unwrap();
    assert!(tray.events().is_empty());
    assert!(Tray::new(TraySpec::new("test", "dup", icon.clone(), menu("x")), ashlar_native::no_waker()).is_err(), "one tray per id");
    drop(tray);
    // the id is free again once dropped
    drop(Tray::new(TraySpec::new("test", "again", icon, menu("x")), ashlar_native::no_waker()).unwrap());
}

#[cfg(all(target_os = "macos", feature = "hotkey"))]
fn hotkey() {
    use ashlar_native::hotkey::GlobalShortcuts;
    let menu_chord = |t: &str| ashlar_command::shortcut::Chord::parse(t).ok();

    let mut g = GlobalShortcuts::new(ashlar_native::no_waker()).expect("global shortcuts on the main thread");
    let rare = menu_chord("Ctrl+Alt+Shift+F19").unwrap();
    // Another app may hold it: an error is fine, a crash is not.
    if g.register("test.rare", &rare).is_ok() {
        assert_eq!(g.ids(), ["test.rare"]);
        g.register("test.rare", &rare).unwrap();
        assert!(g.register("test.other", &rare).is_err(), "one id per chord");
        assert!(g.unregister("test.rare"));
    }
    assert!(g.register("test.plain", &menu_chord("Shift+F19").unwrap()).is_err());
    assert!(g.drain().is_empty());
}

fn main() {
    #[cfg(all(target_os = "macos", feature = "menu"))]
    menu();
    #[cfg(all(target_os = "macos", feature = "open-files"))]
    open_files();
    #[cfg(all(target_os = "macos", feature = "progress"))]
    progress();
    #[cfg(all(target_os = "macos", feature = "tray"))]
    tray();
    #[cfg(all(target_os = "macos", feature = "hotkey"))]
    hotkey();
    println!("main-thread checks passed");
}
