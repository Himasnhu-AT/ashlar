#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Every repaint the runtime asks for goes through `ashlar_app::wake`, so `app.stats` can name its
//! reason.

#[test]
fn repaints_go_through_the_wake_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root];
    let mut offenders = Vec::new();
    let mut scanned = 0;
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("wake.rs") {
                scanned += 1;
                let text = std::fs::read_to_string(&p).unwrap();
                for (i, line) in text.lines().enumerate() {
                    if line.contains(".request_repaint(") || line.contains(".request_repaint_after(") {
                        offenders.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
    }
    assert!(scanned >= 5, "scanned only {scanned} files");
    assert!(offenders.is_empty(), "use ashlar_app::wake::wake / wake_after / waker instead: {offenders:?}");
}
