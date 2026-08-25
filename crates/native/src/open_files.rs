//! Files and URLs the app is asked to open: from the command line, from the file manager (a
//! double-click on a document of an associated type, a drop on the Dock icon) and from a second
//! launch forwarded by [`crate::single_instance`]. They all arrive in one queue, [`OpenFiles`].
//!
//! - Command line (every platform): [`OpenFiles::push_args`] at startup.
//! - macOS: Finder sends `odoc` (open documents) and `GURL` (open URL) Apple events, at launch
//!   and while running, instead of passing arguments. [`OpenFiles::listen_os`] installs handlers
//!   for them; call it on the main thread before the event loop starts, so the launch documents
//!   are not missed. It works alongside any windowing library: it uses the Apple event manager,
//!   not the application delegate.
//! - Windows and Linux: the file manager starts the app with the files as arguments; a second
//!   launch while one runs is forwarded with [`crate::single_instance`].
//!
//! File associations themselves are declared when the app is packaged (`CFBundleDocumentTypes`
//! in Info.plist, registry ProgIDs, `.desktop` `MimeType=`), not at run time.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::{NativeError, Result, Waker};

/// At most this many entries are taken from one launch or event.
pub const MAX_ITEMS: usize = 1024;
/// Longest accepted argument or URL.
pub const MAX_LEN: usize = 32 * 1024;

/// Something to open.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Opened {
    /// An absolute path (relative arguments are resolved against the launch's directory).
    File(PathBuf),
    /// A URL with a scheme other than `file:` (`myapp://open?doc=1`, `https://…`).
    Url(String),
}

/// Whether `s` starts with a URL scheme (`scheme:`), RFC 3986: a letter, then letters, digits,
/// `+`, `-`, `.`; at least two characters so `C:\file` stays a path.
fn url_scheme(s: &str) -> Option<&str> {
    let (scheme, _) = s.split_once(':')?;
    let mut chars = scheme.chars();
    let ok =
        scheme.len() >= 2 && chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    ok.then_some(scheme)
}

/// A `file:` URL as a path (`file:///Users/a/My%20Doc.txt`). Only local (`file:///` or
/// `file://localhost/`) URLs.
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://").or_else(|| url.strip_prefix("FILE://"))?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    if s.contains('\0') {
        return None;
    }
    // `file:///C:/x` on Windows.
    #[cfg(windows)]
    let s = if s.get(2..3) == Some(":") { s.get(1..).unwrap_or_default().to_string() } else { s };
    Some(PathBuf::from(s))
}

/// Classify one string: a URL (`file:` URLs become paths) or a path resolved against `cwd`.
pub fn classify(item: &str, cwd: &Path) -> Option<Opened> {
    if item.is_empty() || item.len() > MAX_LEN || item.contains('\0') {
        return None;
    }
    if let Some(scheme) = url_scheme(item) {
        if scheme.eq_ignore_ascii_case("file") {
            return file_url_to_path(item).map(Opened::File);
        }
        // `C:\x` has a one-letter scheme and is excluded above; `a:b` style relative paths are
        // rare enough that a scheme wins.
        return Some(Opened::Url(item.to_string()));
    }
    let p = Path::new(item);
    Some(Opened::File(if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }))
}

/// What to open from command-line arguments (without the program name): flags (`-x`, `--x`,
/// macOS' `-psn_…`) are skipped, everything after `--` is taken literally, relative paths are
/// resolved against `cwd`. Non-UTF-8 arguments are taken as paths.
pub fn from_args<I, S>(args: I, cwd: &Path) -> Vec<Opened>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut out = vec![];
    let mut literal = false;
    for a in args.into_iter().take(MAX_ITEMS * 4) {
        let a = a.as_ref();
        match a.to_str() {
            Some("--") if !literal => literal = true,
            Some(s) if !literal && s.starts_with('-') => {}
            Some(s) => out.extend(classify(s, cwd)),
            None => {
                let p = Path::new(a);
                out.push(Opened::File(if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }));
            }
        }
        if out.len() >= MAX_ITEMS {
            break;
        }
    }
    out
}

/// A handle that queues into an [`OpenFiles`] from any thread.
#[derive(Clone)]
pub struct OpenSender {
    tx: Sender<Opened>,
    waker: Waker,
}

impl OpenSender {
    /// Queue items and wake the app once. Returns `false` when the queue is gone.
    pub fn send(&self, items: impl IntoIterator<Item = Opened>) -> bool {
        let mut any = false;
        for item in items.into_iter().take(MAX_ITEMS) {
            if self.tx.send(item).is_err() {
                return false;
            }
            any = true;
        }
        if any {
            (self.waker)();
        }
        true
    }
}

/// The queue of things to open.
pub struct OpenFiles {
    sender: OpenSender,
    rx: Receiver<Opened>,
}

impl OpenFiles {
    /// A queue that calls `waker` whenever something arrives from another thread or the OS.
    pub fn new(waker: Waker) -> Self {
        let (tx, rx) = channel();
        Self { sender: OpenSender { tx, waker }, rx }
    }

    /// Queue what the command line names (`std::env::args_os().skip(1)`).
    pub fn push_args<I, S>(&self, args: I, cwd: &Path)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.sender.send(from_args(args, cwd));
    }

    pub fn push(&self, item: Opened) {
        self.sender.send([item]);
    }

    /// A sender for other sources (the single-instance listener).
    pub fn sender(&self) -> OpenSender {
        self.sender.clone()
    }

    /// Everything queued since the last call, oldest first.
    pub fn drain(&self) -> Vec<Opened> {
        self.rx.try_iter().take(MAX_ITEMS).collect()
    }

    /// Receive what the OS sends while the app runs. macOS: installs the `odoc` / `GURL` Apple
    /// event handlers (main thread, before the event loop starts; a later call replaces the
    /// queue they feed). Elsewhere `Unsupported`: files arrive as arguments.
    pub fn listen_os(&self) -> Result<()> {
        imp::listen(self.sender.clone())
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use std::ptr::NonNull;
    use std::sync::{Mutex, Once, PoisonError};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{AnyThread, MainThreadMarker, define_class, msg_send, sel};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSString};

    use super::*;

    const fn four(c: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*c)
    }
    const CORE_EVENT_CLASS: u32 = four(b"aevt");
    const OPEN_DOCUMENTS: u32 = four(b"odoc");
    const INTERNET_EVENT_CLASS: u32 = four(b"GURL");
    const GET_URL: u32 = four(b"GURL");
    const DIRECT_OBJECT: u32 = four(b"----");
    const TYPE_FILE_URL: u32 = four(b"furl");

    /// Where the handlers deliver. Replaced by each `listen`.
    static SINK: Mutex<Option<OpenSender>> = Mutex::new(None);

    fn deliver(items: Vec<Opened>) {
        if items.is_empty() {
            return;
        }
        let sink = SINK.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(s) = sink {
            s.send(items);
        }
    }

    /// The files an `odoc` event names: its direct object is a list of file references (or a
    /// single one), each coerced to a `file:` URL.
    pub(super) fn files_of(event: &NSAppleEventDescriptor) -> Vec<Opened> {
        let Some(direct) = event.paramDescriptorForKeyword(DIRECT_OBJECT) else { return vec![] };
        let n = direct.numberOfItems();
        let items: Vec<Retained<NSAppleEventDescriptor>> = if n <= 0 {
            vec![direct]
        } else {
            // Apple event lists are 1-based.
            (1..=n.min(MAX_ITEMS as isize)).filter_map(|i| direct.descriptorAtIndex(i)).collect()
        };
        items
            .iter()
            .filter_map(|d| d.coerceToDescriptorType(TYPE_FILE_URL))
            .filter_map(|u| {
                let data = u.data();
                if data.len() > MAX_LEN {
                    return None;
                }
                let text = String::from_utf8(data.to_vec()).ok()?;
                file_url_to_path(text.trim_end_matches('\0')).map(Opened::File)
            })
            .collect()
    }

    /// The URL a `GURL` event carries.
    pub(super) fn url_of(event: &NSAppleEventDescriptor) -> Vec<Opened> {
        let Some(s) = event.paramDescriptorForKeyword(DIRECT_OBJECT).and_then(|d| d.stringValue()) else { return vec![] };
        let s = s.to_string();
        let cwd = PathBuf::from("/");
        classify(&s, &cwd).into_iter().collect()
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this class adds no ivars and
        // implements no `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "AshlarNativeOpenEventHandler"]
        struct Handler;

        impl Handler {
            // SAFETY: the signature matches what the Apple event manager calls:
            // `- (void)handle:(NSAppleEventDescriptor *)event withReplyEvent:(NSAppleEventDescriptor *)reply`.
            #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
            fn handle_open(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                // No panic may unwind into the Apple event manager.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deliver(files_of(event))));
            }

            // SAFETY: as above.
            #[unsafe(method(handleGetURL:withReplyEvent:))]
            fn handle_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deliver(url_of(event))));
            }
        }
    );

    impl Handler {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: `init` is NSObject's designated initializer, called once on a fresh
            // allocation.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// The one handler object, created and installed on the main thread.
    fn install_handlers() {
        thread_local! {
            static HANDLER: Retained<Handler> = Handler::new();
        }
        HANDLER.with(|h| {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            let obj: &AnyObject = h.as_ref();
            // SAFETY: `obj` is a `Handler`, which implements both selectors with the signature
            // the Apple event manager calls. The manager does not retain the handler; it lives in
            // a main-thread thread-local for the rest of the process, so it outlives the
            // registration.
            unsafe {
                manager.setEventHandler_andSelector_forEventClass_andEventID(
                    obj,
                    sel!(handleOpenDocuments:withReplyEvent:),
                    CORE_EVENT_CLASS,
                    OPEN_DOCUMENTS,
                );
                manager.setEventHandler_andSelector_forEventClass_andEventID(obj, sel!(handleGetURL:withReplyEvent:), INTERNET_EVENT_CLASS, GET_URL);
            }
        });
    }

    pub fn listen(sender: OpenSender) -> Result<()> {
        if MainThreadMarker::new().is_none() {
            return Err(NativeError::Os("open-file events must be set up on the main thread".into()));
        }
        *SINK.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            install_handlers();
            // NSApplication installs its own handlers while it finishes launching; put ours back
            // right after (the launch documents are sent after this notification).
            let block = RcBlock::new(|_: NonNull<NSNotification>| install_handlers());
            let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
            // SAFETY: no object filter and no queue (the block runs on the posting thread, the
            // main thread); the block captures nothing, so it is sendable.
            let token = unsafe { NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block) };
            // The observation lasts for the life of the process.
            std::mem::forget(token);
        });
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn odoc_event(urls: &[&str]) -> Retained<NSAppleEventDescriptor> {
        use objc2_foundation::NSData;
        let list = NSAppleEventDescriptor::listDescriptor();
        for (i, u) in urls.iter().enumerate() {
            if let Some(d) = NSAppleEventDescriptor::descriptorWithDescriptorType_data(TYPE_FILE_URL, Some(&NSData::with_bytes(u.as_bytes()))) {
                list.insertDescriptor_atIndex(&d, i as isize + 1);
            }
        }
        let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
            CORE_EVENT_CLASS,
            OPEN_DOCUMENTS,
            None,
            -1,
            0,
        );
        event.setParamDescriptor_forKeyword(&list, DIRECT_OBJECT);
        event
    }

    #[cfg(test)]
    pub(super) fn gurl_event(url: &str) -> Retained<NSAppleEventDescriptor> {
        let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
            INTERNET_EVENT_CLASS,
            GET_URL,
            None,
            -1,
            0,
        );
        event.setParamDescriptor_forKeyword(&NSAppleEventDescriptor::descriptorWithString(&NSString::from_str(url)), DIRECT_OBJECT);
        event
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn listen(_sender: OpenSender) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_become_files_and_urls() {
        let cwd = Path::new("/work");
        let got =
            from_args(["-psn_0_1234", "--verbose", "a.txt", "/abs/b.txt", "myapp://open?x=1", "file:///tmp/My%20Doc.txt", "--", "-dash.txt"], cwd);
        assert_eq!(
            got,
            vec![
                Opened::File("/work/a.txt".into()),
                Opened::File("/abs/b.txt".into()),
                Opened::Url("myapp://open?x=1".into()),
                Opened::File("/tmp/My Doc.txt".into()),
                Opened::File("/work/-dash.txt".into()),
            ]
        );
    }

    #[test]
    fn hostile_input_is_bounded() {
        let cwd = Path::new("/w");
        assert_eq!(from_args(std::iter::repeat_n("x", 100_000), cwd).len(), MAX_ITEMS);
        assert!(classify(&"y".repeat(MAX_LEN + 1), cwd).is_none());
        assert!(classify("", cwd).is_none());
        assert!(classify("a\0b", cwd).is_none());
        assert_eq!(file_url_to_path("file:///a%2"), None);
        assert_eq!(file_url_to_path("file:///a%zz"), None);
        assert_eq!(file_url_to_path("file:///a%00b"), None);
        assert_eq!(file_url_to_path("file://otherhost/x"), None);
        assert_eq!(file_url_to_path("file://localhost/x%C3%A9"), Some(PathBuf::from("/xé")));
        assert_eq!(file_url_to_path("file:///%FF"), None, "not UTF-8");
        // A drive letter is not a URL scheme.
        assert!(matches!(classify("C:\\x.txt", cwd), Some(Opened::File(_))));
    }

    #[test]
    fn the_queue_wakes_the_app_once_per_batch() {
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = wakes.clone();
        let q = OpenFiles::new(std::sync::Arc::new(move || {
            w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        q.push_args(["a", "b"], Path::new("/"));
        q.push_args(Vec::<String>::new(), Path::new("/"));
        let s = q.sender();
        std::thread::spawn(move || s.send([Opened::Url("x://y".into())])).join().unwrap_or(false);
        assert_eq!(q.drain().len(), 3);
        assert!(q.drain().is_empty());
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apple_events_are_read() {
        let ev = imp::odoc_event(&["file:///tmp/One.txt", "file:///tmp/Two%20Words.txt", "notaurl"]);
        assert_eq!(imp::files_of(&ev), vec![Opened::File("/tmp/One.txt".into()), Opened::File("/tmp/Two Words.txt".into())]);
        assert_eq!(imp::url_of(&imp::gurl_event("myapp://doc/1")), vec![Opened::Url("myapp://doc/1".into())]);
        // Off the main thread (test threads), listening is refused instead of crashing.
        let q = OpenFiles::new(crate::no_waker());
        assert!(q.listen_os().is_err());
    }
}
