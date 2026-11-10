//! The parts of the API that must run on the main thread (the menu bar, Dock progress, Apple
//! event handlers), exercised on the real main thread: this test has no harness, so `main` is
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

/// On the main thread, before any window: the monitors, the primary first at the origin. A
/// headless machine may list none; what is listed must be usable.
#[cfg(all(any(target_os = "macos", target_os = "windows"), feature = "display"))]
fn monitors() {
    let list = ashlar_native::display::monitors();
    for m in &list {
        assert!(m.width >= 1.0 && m.height >= 1.0 && m.scale > 0.0 && m.x.is_finite() && m.y.is_finite(), "{list:?}");
    }
    if let Some(first) = list.first() {
        assert_eq!((first.x, first.y), (0.0, 0.0), "{list:?}");
    }
    // a desktop session (not CI) has at least the built-in or main display
    if std::env::var_os("CI").is_none() {
        assert!(!list.is_empty(), "no monitors listed on a desktop");
    }
    println!("monitors: {list:?}");
}

fn main() {
    #[cfg(all(any(target_os = "macos", target_os = "windows"), feature = "display"))]
    monitors();
    #[cfg(all(target_os = "macos", feature = "menu"))]
    menu();
    #[cfg(all(target_os = "macos", feature = "open-files"))]
    open_files();
    #[cfg(all(target_os = "macos", feature = "progress"))]
    progress();
    println!("main-thread checks passed");
}
