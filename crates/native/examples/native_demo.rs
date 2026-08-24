//! Try each native feature by hand (not run in CI):
//!
//! ```text
//! cargo run -p ashlar-native --features full --example native_demo -- <command>
//! ```
//!
//! Commands: `prefs`, `open`, `open-many`, `save`, `folder`, `clip-text`, `clip-image`,
//! `clip-custom`, `awake <seconds>`, `args <files…>`, `single`, `notify`, `secret`,
//! `reveal <path>`, `url <url>`, `menu` (macOS: shows a menu bar and Dock progress until Quit).
//! Clipboard and secret commands change the real clipboard / keychain.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use ashlar_native::dialogs::FileDialog;
use ashlar_native::{clipboard, notify, open_files, power, secrets, single_instance, system};

fn show<T: std::fmt::Debug>(what: &str, r: ashlar_native::Result<T>) {
    match r {
        Ok(v) => println!("{what}: {v:?}"),
        Err(e) => println!("{what}: error: {e}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("prefs");
    let rest = args.get(1..).unwrap_or_default();
    let dialog = FileDialog::new().title("ashlar-native demo").directory(std::env::temp_dir()).filter("Text", &["txt", "md"]).filter("Any", &["*"]);
    match cmd {
        "prefs" => println!("{:#?}", system::preferences()),
        "open" => show("open", dialog.open_file()),
        "open-many" => show("open", dialog.open_files()),
        "save" => show("save", dialog.clone().file_name("Untitled.txt").save_file()),
        "folder" => show("folder", dialog.pick_folder()),
        "clip-text" => {
            show("set", clipboard::set_text("hello from ashlar-native"));
            show("get", clipboard::text());
        }
        "clip-image" => {
            let rgba = (0..64 * 64).flat_map(|i| [(i % 64 * 4) as u8, (i / 64 * 4) as u8, 128, 255]).collect();
            show("set", clipboard::set_image(&clipboard::Image { width: 64, height: 64, rgba }));
            show("get", clipboard::image().map(|i| i.map(|i| (i.width, i.height))));
        }
        "clip-custom" => {
            show("set", clipboard::set_custom(&[("com.example.ashlar.demo", b"\x00binary"), ("public.utf8-plain-text", b"plain fallback")]));
            show("types", clipboard::types());
            show("custom", clipboard::custom("com.example.ashlar.demo"));
        }
        "awake" => {
            let secs: u64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(10);
            let guard = power::KeepAwake::new(power::Awake::Display, "ashlar-native demo");
            println!("keeping the display awake for {secs}s (check `pmset -g assertions`): {guard:?}");
            std::thread::sleep(Duration::from_secs(secs));
        }
        "args" => {
            let q = open_files::OpenFiles::new(ashlar_native::no_waker());
            q.push_args(rest, &std::env::current_dir().unwrap_or_default());
            println!("{:#?}", q.drain());
        }
        "single" => {
            let r =
                single_instance::SingleInstance::new("com.example.ashlar-demo").claim(rest, |m| println!("forwarded from a second launch: {m:?}"));
            match r {
                Ok(single_instance::Launch::Primary(_guard)) => {
                    println!("first instance; run the same command again in another terminal (60s)");
                    std::thread::sleep(Duration::from_secs(60));
                }
                other => show("claim", other),
            }
        }
        "notify" => show("notify", notify::notify(&notify::Notification::new("ashlar-native", "Export finished"))),
        "secret" => {
            let k = secrets::Keychain::new("com.example.ashlar-demo").unwrap();
            show("set", k.set("DEMO_TOKEN", "not-a-real-secret"));
            show("get", k.get("DEMO_TOKEN"));
            show("delete", k.delete("DEMO_TOKEN"));
        }
        "reveal" => show("reveal", system::reveal(Path::new(rest.first().map(String::as_str).unwrap_or(".")))),
        "url" => show("url", system::open_url(rest.first().map(String::as_str).unwrap_or("https://example.com"))),
        "menu" => menu(),
        other => println!("unknown command `{other}`"),
    }
}

#[cfg(target_os = "macos")]
fn menu() {
    use std::cell::RefCell;

    use ashlar_native::menu::{Item, MenuNode, MenuSpec, NativeMenuBar, Role, Submenu, SubmenuRole};
    use ashlar_native::progress::{self, Progress};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    thread_local! {
        static BAR: RefCell<Option<(NativeMenuBar, u32)>> = const { RefCell::new(None) };
    }
    fn spec(clicks: u32) -> MenuSpec {
        MenuSpec::new(vec![
            Submenu::new(
                "Demo",
                vec![
                    MenuNode::role(Role::About),
                    MenuNode::Separator,
                    MenuNode::role(Role::Services),
                    MenuNode::Separator,
                    MenuNode::role(Role::Hide),
                    MenuNode::role(Role::HideOthers),
                    MenuNode::role(Role::ShowAll),
                    MenuNode::Separator,
                    MenuNode::role(Role::Quit),
                ],
            ),
            Submenu::new(
                "File",
                vec![
                    Item::new("file.count", format!("Clicked {clicks} times")).accelerator("Mod+K").into(),
                    Item::new("file.toggle", "Even").checked(clicks.is_multiple_of(2)).into(),
                    Item::new("file.disabled", "Disabled when odd").enabled(clicks.is_multiple_of(2)).into(),
                ],
            ),
            Submenu::new("Window", vec![MenuNode::role(Role::Minimize), MenuNode::role(Role::Zoom)]).with_role(SubmenuRole::Window),
        ])
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // The waker runs on the main thread inside muda's event handler: drain and re-render there.
    let waker: ashlar_native::Waker = std::sync::Arc::new(|| {
        BAR.with(|b| {
            if let Some((bar, clicks)) = b.borrow_mut().as_mut() {
                for id in bar.drain() {
                    println!("activated: {id}");
                    *clicks += 1;
                }
                let r = bar.update(&spec(*clicks));
                println!("update: {r:?}");
                let _ = progress::set(None, Progress::Normal(f64::from(*clicks % 10) / 10.0));
                let _ = progress::set_badge(Some(&clicks.to_string()));
            }
        })
    });
    let bar = NativeMenuBar::install(&spec(0), waker).expect("menu bar");
    BAR.with(|b| *b.borrow_mut() = Some((bar, 0)));
    let q = open_files::OpenFiles::new(std::sync::Arc::new(|| println!("files arrived (drop a file on the Dock icon of a bundled build)")));
    let _ = q.listen_os();
    println!("menu bar installed; use File ▸ Clicked… (⌘K), Demo ▸ Quit to exit");
    app.activate();
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn menu() {
    println!("the menu demo drives the macOS menu bar; on Windows attach NativeMenuBar to a window");
}
