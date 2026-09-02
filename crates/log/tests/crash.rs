#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The panic hook is process-wide, so every check runs in one test, in order.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ashlar_log::crash::{self, CrashConfig};

fn panic_in_thread(name: &str, msg: &'static str) {
    let r = std::thread::Builder::new().name(name.into()).spawn(move || panic!("{msg}")).unwrap().join();
    assert!(r.is_err());
}

#[test]
fn the_hook_follows_its_configuration_and_chains() {
    // a hook installed before ours (a telemetry reporter, say) still runs
    let earlier = Arc::new(AtomicUsize::new(0));
    let seen = earlier.clone();
    std::panic::set_hook(Box::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    }));

    // the convenience form: one `crash-<unix>-<pid>.log` per panic, with a backtrace
    let dir = tempfile::tempdir().unwrap();
    crash::install(Some(dir.path()), "hooktest", "0.0.1");
    let r = std::panic::catch_unwind(|| {
        let v: Vec<u8> = Vec::new();
        if v.is_empty() {
            panic!("boom from the crash test");
        }
    });
    assert!(r.is_err());
    assert_eq!(earlier.load(Ordering::SeqCst), 1, "the previous hook ran");
    let last = crash::last_panic().expect("recorded");
    assert!(last.message.contains("boom from the crash test"));
    assert!(!last.backtrace.is_empty());
    let file = last.log_file.clone().expect("written");
    let name = file.file_name().unwrap().to_str().unwrap();
    assert_eq!(name, format!("crash-{}-{}.log", last.unix, std::process::id()));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("hooktest 0.0.1 crashed"));
    assert!(text.contains("boom from the crash test"));
    assert!(text.contains("backtrace:"));
    assert_eq!(crash::take_last_panic(), Some(last));
    assert_eq!(crash::last_panic(), None);

    // a daily file that panics append to, without a backtrace; installing again only reconfigures
    let daily = tempfile::tempdir().unwrap();
    crash::install_with(CrashConfig {
        file_name: Box::new(|r| format!("crash-{}.log", r.unix / 86_400)),
        append: true,
        capture_backtrace: false,
        ..CrashConfig::new("daily", "1.2.3", Some(daily.path()))
    });
    panic_in_thread("worker-one", "first worker panic");
    panic_in_thread("worker-two", "second worker panic");
    assert_eq!(earlier.load(Ordering::SeqCst), 3, "chained once, not once per install");
    let last = crash::take_last_panic().expect("recorded");
    assert_eq!(last.thread, "worker-two");
    assert!(last.backtrace.is_empty());
    let file = last.log_file.expect("written");
    assert_eq!(file.file_name().unwrap().to_str().unwrap(), format!("crash-{}.log", last.unix / 86_400));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("first worker panic") && text.contains("second worker panic"), "{text}");
    assert!(text.contains("thread: worker-one") && text.contains("thread: worker-two"));
    assert_eq!(text.matches("daily 1.2.3 crashed").count(), 2);
    assert!(!text.contains("backtrace:"));
    assert_eq!(std::fs::read_dir(daily.path()).unwrap().count(), 1, "no temp files left behind");

    // a name that climbs out of the folder falls back to the default
    let odd = tempfile::tempdir().unwrap();
    crash::install_with(CrashConfig { file_name: Box::new(|_| "../escape.log".into()), ..CrashConfig::new("odd", "0", Some(odd.path())) });
    panic_in_thread("odd", "odd name");
    let file = crash::take_last_panic().unwrap().log_file.unwrap();
    assert_eq!(file.parent(), Some(odd.path()));
    assert!(file.file_name().unwrap().to_str().unwrap().starts_with("crash-"));
    assert!(!odd.path().parent().unwrap().join("escape.log").exists());

    // no folder: nothing written, the panic is still kept
    crash::install_with(CrashConfig::new("nodir", "0", None));
    panic_in_thread("nodir", "kept anyway");
    let last = crash::take_last_panic().unwrap();
    assert_eq!(last.message, "kept anyway");
    assert_eq!(last.log_file, None);

    // old crash logs are pruned
    let many = tempfile::tempdir().unwrap();
    for i in 0..crash::KEEP_CRASH_LOGS + 5 {
        std::fs::write(many.path().join(format!("crash-{i:04}-1.log")), "old").unwrap();
    }
    std::fs::write(many.path().join("app.log"), "not a crash log").unwrap();
    crash::install(Some(many.path()), "prune", "0");
    panic_in_thread("prune", "prune");
    let names: Vec<_> = std::fs::read_dir(many.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names.iter().filter(|n| n.to_string_lossy().starts_with("crash-")).count(), crash::KEEP_CRASH_LOGS);
    assert!(many.path().join("app.log").exists());
}
