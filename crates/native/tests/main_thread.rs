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

/// The appearance guard against a stand-in for winit's window delegate class: changes without an
/// `NSAppearance` on both sides (or for another key path) never reach the original observer.
#[cfg(all(target_os = "macos", feature = "mac-shell"))]
#[allow(unsafe_code)]
fn mac_shell() {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ashlar_native::mac_shell;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
    use objc2::{ClassType, msg_send, sel};
    use objc2_foundation::{NSKeyValueChangeNewKey, NSKeyValueChangeOldKey, NSString};

    assert!(mac_shell::guard_appearance().is_err(), "no window delegate class yet");
    assert!(mac_shell::listen_reopen(ashlar_native::no_waker()).is_err(), "no application delegate without an event loop");

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    extern "C-unwind" fn observe(_: *mut AnyObject, _: Sel, _: *mut AnyObject, _: *mut AnyObject, _: *mut AnyObject, _: *mut c_void) {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }
    let mut b = ClassBuilder::new(c"WinitWindowDelegate", NSObject::class()).unwrap();
    // SAFETY: the function's signature is the selector's (v@:@@@^v).
    unsafe { b.add_method(sel!(observeValueForKeyPath:ofObject:change:context:), observe as extern "C-unwind" fn(_, _, _, _, _, _)) };
    let cls = b.register();
    mac_shell::guard_appearance().unwrap();
    mac_shell::guard_appearance().unwrap(); // a second call does nothing

    let class = |name: &std::ffi::CStr| AnyClass::get(name).unwrap();
    // SAFETY: plain Foundation / AppKit messages with their documented argument types.
    unsafe {
        let delegate: *mut AnyObject = msg_send![cls, new];
        let send = |key: &str, change: *mut AnyObject| {
            let key = NSString::from_str(key);
            let _: () = msg_send![delegate, observeValueForKeyPath: &*key, ofObject: std::ptr::null_mut::<AnyObject>(), change: change, context: std::ptr::null_mut::<c_void>()];
        };
        let aqua = NSString::from_str("NSAppearanceNameAqua");
        let appearance: *mut AnyObject = msg_send![class(c"NSAppearance"), appearanceNamed: &*aqua];
        assert!(!appearance.is_null());
        let null: *mut AnyObject = msg_send![class(c"NSNull"), null];
        let change = |old: *mut AnyObject, new: *mut AnyObject| -> *mut AnyObject {
            let d: *mut AnyObject = msg_send![class(c"NSMutableDictionary"), dictionary];
            let _: () = msg_send![d, setObject: old, forKey: NSKeyValueChangeOldKey];
            let _: () = msg_send![d, setObject: new, forKey: NSKeyValueChangeNewKey];
            d
        };
        send("effectiveAppearance", std::ptr::null_mut());
        send("effectiveAppearance", change(appearance, null));
        send("effectiveAppearance", change(null, appearance));
        send("somethingElse", change(appearance, appearance));
        assert_eq!(CALLS.load(Ordering::SeqCst), 0, "unsafe changes are dropped");
        send("effectiveAppearance", change(appearance, appearance));
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "a real change reaches the original observer");
        let _: () = msg_send![delegate, release];
    }
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
    #[cfg(all(target_os = "macos", feature = "tray"))]
    tray();
    #[cfg(all(target_os = "macos", feature = "hotkey"))]
    hotkey();
    #[cfg(all(target_os = "macos", feature = "mac-shell"))]
    mac_shell();
    println!("main-thread checks passed");
}
