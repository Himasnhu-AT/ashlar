//! Files the app is asked to open from outside: a second launch forwarded by
//! [`App::single_instance`](crate::App::single_instance), the macOS `odoc` / `GURL` Apple events
//! (a double-click in Finder, a drop on the Dock icon), and files dropped on the window
//! ([`App::accept_drops`](crate::App::accept_drops)).
//!
//! OS and forwarded items arrive on any thread in one [`Inbox`]; [`crate::Runtime::logic`] drains
//! it into `file.open` (a dirty document asks to save first, as File ▸ Open does).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use ashlar_native::open_files::{OpenFiles, Opened, from_args};
use ashlar_native::single_instance::Forwarded;
use ashlar_ui::wake::{self, Reason};

/// The wake reason for files from the OS or a second launch (`app.stats` → `wakes`).
pub const OPEN: Reason = Reason("open");

/// The queue of files from the OS and from later launches. Cheap; every runtime has one.
pub(crate) struct Inbox {
    files: OpenFiles,
    /// A forwarded launch arrived: bring the window to the front.
    focus: Arc<AtomicBool>,
    /// Filled once the window's context exists; items queued before then wait for the first frame.
    waker: Arc<OnceLock<wake::Waker>>,
}

impl Default for Inbox {
    fn default() -> Self {
        let waker: Arc<OnceLock<wake::Waker>> = Arc::default();
        let w = Arc::clone(&waker);
        let files = OpenFiles::new(Arc::new(move || {
            if let Some(w) = w.get() {
                w.wake();
            }
        }));
        Self { files, focus: Arc::default(), waker }
    }
}

impl Inbox {
    /// Wake `ctx` when something arrives (called once the context exists).
    pub(crate) fn set_ctx(&self, ctx: &egui::Context) {
        let _ = self.waker.set(wake::waker(ctx, OPEN));
    }

    /// What a later launch's `on_forward` runs (on the listener thread).
    pub(crate) fn forwarder(&self) -> impl Fn(Forwarded) + Send + 'static {
        let sender = self.files.sender();
        let focus = Arc::clone(&self.focus);
        let waker = Arc::clone(&self.waker);
        move |f: Forwarded| {
            focus.store(true, Ordering::SeqCst);
            let items = from_args(&f.args, &f.cwd);
            // a launch with no files still brings the window forward
            if items.is_empty() {
                if let Some(w) = waker.get() {
                    w.wake();
                }
            } else {
                sender.send(items);
            }
        }
    }

    /// Queue a forwarded launch now (tests).
    pub(crate) fn forward(&self, f: Forwarded) {
        (self.forwarder())(f);
    }

    /// macOS: receive Finder's open-document events. Main thread, before the event loop.
    #[cfg(target_os = "macos")]
    pub(crate) fn listen_os(&self) {
        if let Err(e) = self.files.listen_os() {
            log::warn!("open-document events are off: {e}");
        }
    }

    /// Everything queued since the last call, and whether a forwarded launch wants the window.
    pub(crate) fn drain(&self) -> (Vec<Opened>, bool) {
        (self.files.drain(), self.focus.swap(false, Ordering::SeqCst))
    }
}

/// True when `path` has one of `extensions` (without the dot, any case).
pub(crate) fn known_extension(path: &Path, extensions: &[String]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    extensions.iter().any(|k| k.trim_start_matches('.').eq_ignore_ascii_case(ext))
}

/// Split dropped paths into the document to open (the first with a known extension) and the rest
/// (for the app's view, [`crate::Cx::dropped_files`]).
pub(crate) fn split_drops(paths: Vec<PathBuf>, extensions: &[String]) -> (Option<PathBuf>, Vec<PathBuf>) {
    let mut open = None;
    let mut rest = Vec::new();
    for p in paths {
        if open.is_none() && known_extension(&p, extensions) {
            open = Some(p);
        } else {
            rest.push(p);
        }
    }
    (open, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_match_without_dot_and_case() {
        let exts = vec!["note".to_string(), ".TXT".to_string()];
        assert!(known_extension(Path::new("/a/b.NOTE"), &exts));
        assert!(known_extension(Path::new("/a/b.txt"), &exts));
        assert!(!known_extension(Path::new("/a/b.png"), &exts));
        assert!(!known_extension(Path::new("/a/note"), &exts));
        let (open, rest) = split_drops(vec!["/x.png".into(), "/y.note".into(), "/z.note".into()], &exts);
        assert_eq!(open, Some(PathBuf::from("/y.note")));
        assert_eq!(rest, vec![PathBuf::from("/x.png"), PathBuf::from("/z.note")]);
    }

    #[test]
    fn a_forward_queues_its_files_and_asks_for_focus() {
        let inbox = Inbox::default();
        let cwd = std::env::temp_dir();
        inbox.forward(Forwarded { cwd: cwd.clone(), args: vec!["--safe-mode".into(), "a.note".into()] });
        let (items, focus) = inbox.drain();
        assert_eq!(items, vec![Opened::File(cwd.join("a.note"))]);
        assert!(focus);
        assert_eq!(inbox.drain(), (vec![], false));
    }
}
