//! Autosave ring and crash recovery.
//!
//! Each running session owns a directory `<root>/<session id>/` holding:
//! - `lock`: an OS file lock held while the session runs. A crash, `kill -9` or power cut releases
//!   it, so a later launch can tell a dead session from a live one;
//! - `session.json`: [`SessionMeta`] (what is open, whether it has unsaved changes, the latest
//!   slot);
//! - `slot-<n>.<ext>`: the ring of the last N autosaves, overwritten oldest first, each written
//!   atomically.
//!
//! The core API has no threads and no clock of its own: the app calls [`Autosave::tick`] (every
//! frame or on a timer) with a monotonic `now`, the document's revision and whether it is dirty,
//! and a closure that encodes the document; it writes only when the interval has passed and the
//! revision is new. [`BackgroundWriter`] moves the encoding and the write off the UI thread.
//!
//! On start, [`Autosave::start`] lists [`RecoveryCandidate`]s: sessions whose lock is free and
//! that had unsaved changes. Sessions that ended cleanly ([`Autosave::close`]) leave nothing behind.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::revision::Revision;

const LOCK: &str = "lock";
const META: &str = "session.json";
/// Largest number of slots in a ring.
pub const MAX_SLOTS: u32 = 1000;

/// Autosave settings.
#[derive(Clone, Debug)]
pub struct AutosaveConfig {
    /// Directory holding one subdirectory per session (e.g. `<data dir>/Recovery`).
    pub root: PathBuf,
    /// Slots in the ring (1…[`MAX_SLOTS`]).
    pub slots: u32,
    /// Shortest time between two autosaves.
    pub interval: Duration,
    /// File extension of the slots (without the dot).
    pub ext: String,
}

impl AutosaveConfig {
    pub fn new(root: impl Into<PathBuf>) -> AutosaveConfig {
        AutosaveConfig { root: root.into(), slots: 5, interval: Duration::from_secs(60), ext: "autosave".into() }
    }
}

/// What a session records about itself (`session.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    pub pid: u32,
    /// Wall-clock seconds since 1970 when the session started / last wrote a slot.
    pub started_unix: i64,
    pub saved_unix: i64,
    /// Display name of the document (`"Untitled"`, the file name…).
    pub label: String,
    /// Where the document lives, if it was ever saved.
    pub doc_path: Option<String>,
    /// The document had unsaved changes at the last update.
    pub dirty: bool,
    /// Revision written to the latest slot.
    pub revision: u64,
    /// The latest slot written, if any.
    pub slot: Option<u32>,
    /// Slots in the ring when the latest slot was written.
    pub slots: u32,
}

/// A slot reserved for writing ([`Autosave::begin`]), finished by [`Autosave::finish`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotWrite {
    pub path: PathBuf,
    pub slot: u32,
    pub revision: Revision,
}

/// Unsaved work left by a session that is no longer running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryCandidate {
    /// The session directory's name.
    pub id: String,
    #[serde(skip)]
    pub dir: PathBuf,
    pub meta: SessionMeta,
    /// Slot files, newest first.
    pub slots: Vec<PathBuf>,
}

impl RecoveryCandidate {
    /// The newest autosave to offer.
    pub fn latest(&self) -> Option<&Path> {
        self.slots.first().map(PathBuf::as_path)
    }

    /// Delete the session's files (the user declined, or recovered and saved).
    pub fn discard(&self) -> io::Result<()> {
        fs::remove_dir_all(&self.dir)
    }
}

/// The autosave state of one running session.
#[derive(Debug)]
pub struct Autosave {
    cfg: AutosaveConfig,
    dir: PathBuf,
    meta: SessionMeta,
    _lock: Option<File>,
    /// `now` of the last write (or of the first tick).
    last: Option<Duration>,
    written: Option<Revision>,
    in_flight: Option<Revision>,
}

impl Autosave {
    /// Start this session: create its directory and take its lock, then list what dead sessions
    /// left behind (empty, clean ones are deleted).
    pub fn start(cfg: AutosaveConfig) -> io::Result<(Autosave, Vec<RecoveryCandidate>)> {
        let mut cfg = cfg;
        cfg.slots = cfg.slots.clamp(1, MAX_SLOTS);
        fs::create_dir_all(&cfg.root)?;
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let started = unix_now();
        let pid = ashlar_fs::atomic::pid();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = cfg.root.join(format!("{started}-{pid}-{seq}"));
        fs::create_dir_all(&dir)?;
        let lock = File::options().create(true).truncate(false).read(true).write(true).open(dir.join(LOCK))?;
        // Unsupported locking only weakens stale detection; it must not stop the app.
        let _ = lock.try_lock();
        let meta = SessionMeta { pid, started_unix: started, label: "Untitled".into(), slots: cfg.slots, ..SessionMeta::default() };
        let a = Autosave { cfg, dir, meta, _lock: Some(lock), last: None, written: None, in_flight: None };
        a.write_meta()?;
        let candidates = scan(&a.cfg.root, Some(&a.dir));
        Ok((a, candidates))
    }

    pub fn session_dir(&self) -> &Path {
        &self.dir
    }

    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    /// Name and location of the open document, shown when offering recovery.
    pub fn set_document(&mut self, label: &str, doc_path: Option<&Path>) -> io::Result<()> {
        self.meta.label = label.to_string();
        self.meta.doc_path = doc_path.map(|p| p.to_string_lossy().into_owned());
        self.write_meta()
    }

    /// Is an autosave due? Only when `dirty`, the revision differs from the last written (or
    /// in-flight) one, and `interval` has passed since the last write (or the first tick).
    pub fn due(&mut self, now: Duration, revision: Revision, dirty: bool) -> bool {
        let since = *self.last.get_or_insert(now);
        dirty && self.in_flight.is_none() && self.written != Some(revision) && now.saturating_sub(since) >= self.cfg.interval
    }

    /// Write an autosave if one is due. `encode` runs only then. Returns the slot written.
    pub fn tick(&mut self, now: Duration, revision: Revision, dirty: bool, encode: impl FnOnce() -> Vec<u8>) -> io::Result<Option<PathBuf>> {
        if !self.due(now, revision, dirty) {
            return Ok(None);
        }
        let w = self.begin(now, revision);
        let r = ashlar_fs::atomic_write(&w.path, &encode());
        self.finish(&w, r)?;
        Ok(Some(w.path))
    }

    /// Reserve the next slot for a write done elsewhere (a background thread); call
    /// [`Autosave::finish`] with its result. Further ticks are not due until then.
    pub fn begin(&mut self, now: Duration, revision: Revision) -> SlotWrite {
        self.last = Some(now);
        self.in_flight = Some(revision);
        let slot = match self.meta.slot {
            Some(s) => s % self.cfg.slots + 1,
            None => 1,
        };
        SlotWrite { path: self.slot_path(slot), slot, revision }
    }

    /// Record the result of a write started with [`Autosave::begin`]. On success the session
    /// meta points at the new slot; the error is returned as is.
    pub fn finish(&mut self, w: &SlotWrite, result: io::Result<()>) -> io::Result<()> {
        if self.in_flight == Some(w.revision) {
            self.in_flight = None;
        }
        result?;
        self.written = Some(w.revision);
        self.meta.slot = Some(w.slot);
        self.meta.slots = self.cfg.slots;
        self.meta.revision = w.revision.0;
        self.meta.saved_unix = unix_now();
        self.meta.dirty = true;
        self.write_meta()
    }

    /// The document was saved (or the user discarded its changes): nothing to recover now.
    pub fn mark_clean(&mut self) -> io::Result<()> {
        if !self.meta.dirty {
            return Ok(());
        }
        self.meta.dirty = false;
        self.write_meta()
    }

    /// Clean exit: delete this session's directory. Pass `keep = true` when the user quit without
    /// saving and wants the work offered next time; the session then shows up as a candidate.
    pub fn close(self, keep: bool) -> io::Result<()> {
        if keep && self.meta.slot.is_some() {
            return Ok(());
        }
        let dir = self.dir.clone();
        drop(self);
        fs::remove_dir_all(dir)
    }

    fn slot_path(&self, slot: u32) -> PathBuf {
        self.dir.join(format!("slot-{slot}.{}", self.cfg.ext))
    }

    fn write_meta(&self) -> io::Result<()> {
        let text = serde_json::to_vec_pretty(&self.meta).map_err(io::Error::other)?;
        ashlar_fs::atomic_write(&self.dir.join(META), &text)
    }
}

/// Sessions under `root` that are no longer running and left unsaved work, newest first.
/// Dead sessions with nothing to recover are deleted. `skip` is the caller's own session.
pub fn scan(root: &Path, skip: Option<&Path>) -> Vec<RecoveryCandidate> {
    let Ok(rd) = fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() || Some(dir.as_path()) == skip || !lock_is_free(&dir) {
            continue;
        }
        let meta = fs::read(dir.join(META)).ok().and_then(|b| serde_json::from_slice::<SessionMeta>(&b).ok());
        let slots = slots_newest_first(&dir, meta.as_ref());
        match meta {
            // An unreadable meta with slots on disk is still offered: losing work is worse.
            Some(m) if !m.dirty => {
                let _ = fs::remove_dir_all(&dir);
            }
            m if !slots.is_empty() => {
                out.push(RecoveryCandidate { id: e.file_name().to_string_lossy().into_owned(), dir, meta: m.unwrap_or_default(), slots });
            }
            _ => {
                let _ = fs::remove_dir_all(&dir);
            }
        }
    }
    out.sort_by(|a, b| b.meta.saved_unix.cmp(&a.meta.saved_unix).then_with(|| b.id.cmp(&a.id)));
    out
}

/// Slot files of a session, newest first: walking the ring back from the meta's latest slot, or
/// by modification time when the meta is missing.
fn slots_newest_first(dir: &Path, meta: Option<&SessionMeta>) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(u32, PathBuf, std::time::SystemTime)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n: u32 = name.strip_prefix("slot-")?.split('.').next()?.parse().ok()?;
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            Some((n, e.path(), m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH)))
        })
        .collect();
    match meta.and_then(|m| Some((m.slot?, m.slots.clamp(1, MAX_SLOTS)))) {
        Some((latest, ring)) => {
            // Distance back from the latest slot in the ring; slots outside the ring go last.
            let (latest, ring) = (u64::from(latest), u64::from(ring));
            let back = |n: u32| {
                let n = u64::from(n);
                if (1..=ring).contains(&n) { (latest + ring - n) % ring } else { ring + n }
            };
            found.sort_by_key(|f| back(f.0));
        }
        None => found.sort_by_key(|f| std::cmp::Reverse(f.2)),
    }
    found.into_iter().map(|f| f.1).collect()
}

/// Is the session directory's lock free (its owner gone)?
fn lock_is_free(dir: &Path) -> bool {
    let Ok(f) = File::options().read(true).write(true).open(dir.join(LOCK)) else { return true };
    match f.try_lock() {
        Ok(()) => true,
        Err(fs::TryLockError::WouldBlock) => false,
        // Locking unsupported here: treat as free (the work is offered, never deleted silently).
        Err(fs::TryLockError::Error(_)) => true,
    }
}

/// Wall-clock seconds since 1970 (0 where there is no clock).
pub fn unix_now() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    return std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    #[cfg(target_arch = "wasm32")]
    0
}

// ------------------------------------------------------------------ background writes

#[cfg(not(target_arch = "wasm32"))]
pub use background::BackgroundWriter;

#[cfg(not(target_arch = "wasm32"))]
mod background {
    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use super::{Autosave, SlotWrite};
    use crate::revision::Revision;

    type Job = (SlotWrite, Box<dyn FnOnce() -> Vec<u8> + Send>);

    /// Encodes and writes autosaves on a thread of its own. The UI thread hands over an encoder
    /// closure (typically holding an `Arc` snapshot of the document) and collects results with
    /// [`BackgroundWriter::pump`].
    pub struct BackgroundWriter {
        tx: Option<Sender<Job>>,
        done: Receiver<(SlotWrite, io::Result<()>)>,
        thread: Option<JoinHandle<()>>,
    }

    impl std::fmt::Debug for BackgroundWriter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("BackgroundWriter")
        }
    }

    impl BackgroundWriter {
        pub fn start() -> io::Result<BackgroundWriter> {
            let (tx, rx) = channel::<Job>();
            let (dtx, done) = channel();
            let thread = std::thread::Builder::new().name("autosave".into()).spawn(move || {
                for (w, encode) in rx {
                    let r = catch_unwind(AssertUnwindSafe(|| {
                        let bytes = encode();
                        ashlar_fs::atomic_write(&w.path, &bytes)
                    }))
                    .unwrap_or_else(|_| Err(io::Error::other("autosave encoder panicked")));
                    if dtx.send((w, r)).is_err() {
                        break;
                    }
                }
            })?;
            Ok(BackgroundWriter { tx: Some(tx), done, thread: Some(thread) })
        }

        /// Like [`Autosave::tick`], but the encoding and the write happen on the writer thread.
        /// Returns true when a write was queued.
        pub fn tick(
            &mut self,
            a: &mut Autosave,
            now: Duration,
            revision: Revision,
            dirty: bool,
            encode: impl FnOnce() -> Vec<u8> + Send + 'static,
        ) -> bool {
            self.pump(a);
            if !a.due(now, revision, dirty) {
                return false;
            }
            let w = a.begin(now, revision);
            match &self.tx {
                Some(tx) => match tx.send((w.clone(), Box::new(encode))) {
                    Ok(()) => true,
                    Err(_) => {
                        let _ = a.finish(&w, Err(io::Error::other("autosave thread is gone")));
                        false
                    }
                },
                None => false,
            }
        }

        /// Collect finished writes into `a`; returns the errors of failed ones.
        pub fn pump(&mut self, a: &mut Autosave) -> Vec<io::Error> {
            let mut errors = Vec::new();
            while let Ok((w, r)) = self.done.try_recv() {
                if let Err(e) = a.finish(&w, r) {
                    errors.push(e);
                }
            }
            errors
        }

        /// Wait for queued writes, collect them into `a`, and stop the thread.
        pub fn shutdown(mut self, a: &mut Autosave) -> Vec<io::Error> {
            self.tx = None;
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            self.pump(a)
        }
    }

    impl Drop for BackgroundWriter {
        fn drop(&mut self) {
            self.tx = None;
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ashlar-autosave-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn cfg(root: &Path, slots: u32) -> AutosaveConfig {
        AutosaveConfig { root: root.to_path_buf(), slots, interval: Duration::from_secs(10), ext: "doc".into() }
    }

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn writes_only_when_dirty_new_and_after_interval() {
        let r = root("tick");
        let (mut a, c) = Autosave::start(cfg(&r, 3)).unwrap();
        assert!(c.is_empty());
        let rev1 = Revision::next();
        let mut encodes = 0;
        let enc = |n: &mut i32| {
            *n += 1;
            b"x".to_vec()
        };
        assert_eq!(a.tick(s(0), rev1, true, || enc(&mut encodes)).unwrap(), None, "first tick starts the clock");
        assert_eq!(a.tick(s(5), rev1, true, || enc(&mut encodes)).unwrap(), None);
        assert_eq!(a.tick(s(10), rev1, false, || enc(&mut encodes)).unwrap(), None, "clean: nothing to do");
        let p = a.tick(s(10), rev1, true, || enc(&mut encodes)).unwrap().unwrap();
        assert!(p.ends_with("slot-1.doc"));
        assert_eq!(a.tick(s(15), rev1, true, || enc(&mut encodes)).unwrap(), None, "same revision");
        assert_eq!(encodes, 1);
        let rev2 = Revision::next();
        assert_eq!(a.tick(s(18), rev2, true, || enc(&mut encodes)).unwrap(), None, "interval since the last write");
        assert!(a.tick(s(20), rev2, true, || enc(&mut encodes)).unwrap().is_some());
        assert_eq!(a.meta().revision, rev2.0);
        a.close(false).unwrap();
        assert_eq!(fs::read_dir(&r).unwrap().count(), 0, "clean exit leaves nothing");
        fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn ring_overwrites_oldest() {
        let r = root("ring");
        let (mut a, _) = Autosave::start(cfg(&r, 3)).unwrap();
        a.tick(s(0), Revision::NONE, true, Vec::new).unwrap();
        for i in 1..=5u8 {
            let p = a.tick(i as u32 * s(10), Revision::next(), true, || vec![i]).unwrap().unwrap();
            assert!(p.ends_with(format!("slot-{}.doc", (i - 1) % 3 + 1)), "{p:?}");
        }
        let dir = a.session_dir().to_path_buf();
        let slots = slots_newest_first(&dir, Some(a.meta()));
        let contents: Vec<u8> = slots.iter().map(|p| fs::read(p).unwrap()[0]).collect();
        assert_eq!(contents, [5, 4, 3], "newest first");
        drop(a);
        fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn crash_is_recovered_and_clean_sessions_are_not() {
        let r = root("recover");
        // A session that crashed with unsaved work: never closed, lock released by drop.
        {
            let (mut a, _) = Autosave::start(cfg(&r, 2)).unwrap();
            a.set_document("Plan", Some(Path::new("/docs/plan.doc"))).unwrap();
            a.tick(s(0), Revision::NONE, true, Vec::new).unwrap();
            a.tick(s(10), Revision::next(), true, || b"v1".to_vec()).unwrap();
            a.tick(s(20), Revision::next(), true, || b"v2".to_vec()).unwrap();
            a.tick(s(30), Revision::next(), true, || b"v3".to_vec()).unwrap();
        }
        // A session that saved its document before dying: nothing to offer.
        {
            let (mut a, _) = Autosave::start(cfg(&r, 2)).unwrap();
            a.tick(s(0), Revision::NONE, true, Vec::new).unwrap();
            a.tick(s(10), Revision::next(), true, || b"w".to_vec()).unwrap();
            a.mark_clean().unwrap();
        }
        // A session that never wrote anything.
        drop(Autosave::start(cfg(&r, 2)).unwrap());
        let (live, c) = Autosave::start(cfg(&r, 2)).unwrap();
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].meta.label, "Plan");
        assert_eq!(c[0].meta.doc_path.as_deref(), Some("/docs/plan.doc"));
        assert_eq!(fs::read(c[0].latest().unwrap()).unwrap(), b"v3");
        assert_eq!(c[0].slots.len(), 2);
        // Only the candidate and the live session remain.
        assert_eq!(fs::read_dir(&r).unwrap().count(), 2);
        // A second instance does not offer the live session's work.
        let (other, c2) = Autosave::start(cfg(&r, 2)).unwrap();
        assert_eq!(c2.len(), 1);
        c2[0].discard().unwrap();
        live.close(false).unwrap();
        other.close(false).unwrap();
        assert_eq!(fs::read_dir(&r).unwrap().count(), 0);
        fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn close_keep_leaves_a_candidate() {
        let r = root("keep");
        let (mut a, _) = Autosave::start(cfg(&r, 2)).unwrap();
        a.tick(s(0), Revision::NONE, true, Vec::new).unwrap();
        a.tick(s(10), Revision::next(), true, || b"k".to_vec()).unwrap();
        a.close(true).unwrap();
        let (b, c) = Autosave::start(cfg(&r, 2)).unwrap();
        assert_eq!(c.len(), 1);
        b.close(false).unwrap();
        fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn hostile_session_dirs() {
        let r = root("hostile");
        fs::create_dir_all(&r).unwrap();
        // Corrupt meta but a slot on disk: still offered.
        let d1 = r.join("junk-1");
        fs::create_dir_all(&d1).unwrap();
        fs::write(d1.join(META), b"{not json").unwrap();
        fs::write(d1.join("slot-2.doc"), b"data").unwrap();
        // Meta pointing at absurd slots.
        let d2 = r.join("junk-2");
        fs::create_dir_all(&d2).unwrap();
        fs::write(d2.join(META), br#"{"dirty":true,"slot":4294967295,"slots":4294967295}"#).unwrap();
        fs::write(d2.join("slot-1.doc"), b"a").unwrap();
        fs::write(d2.join("slot-99999999999.doc"), b"b").unwrap();
        fs::write(d2.join("slot-x.doc"), b"c").unwrap();
        // A stray file in the root.
        fs::write(r.join("stray"), b"").unwrap();
        let (a, c) = Autosave::start(cfg(&r, 0)).unwrap();
        assert_eq!(c.len(), 2, "{c:?}");
        let j2 = c.iter().find(|c| c.id == "junk-2").unwrap();
        assert_eq!(j2.slots.len(), 1);
        a.close(false).unwrap();
        fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn failed_write_is_reported_and_retried() {
        let r = root("fail");
        let (mut a, _) = Autosave::start(cfg(&r, 2)).unwrap();
        let rev = Revision::next();
        let w = a.begin(s(0), rev);
        assert!(!a.due(s(100), rev, true), "in flight");
        assert!(a.finish(&w, Err(io::Error::other("disk full"))).is_err());
        assert!(a.due(s(100), rev, true), "retried later");
        assert_eq!(a.meta().slot, None);
        a.close(false).unwrap();
        fs::remove_dir_all(&r).unwrap();
    }

    #[test]
    fn background_writer() {
        let r = root("bg");
        let (mut a, _) = Autosave::start(cfg(&r, 2)).unwrap();
        let mut w = BackgroundWriter::start().unwrap();
        let rev = Revision::next();
        assert!(!w.tick(&mut a, s(0), rev, true, || b"a".to_vec()));
        assert!(w.tick(&mut a, s(10), rev, true, || b"a".to_vec()));
        assert!(!w.tick(&mut a, s(20), Revision::next(), true, || b"b".to_vec()), "one write at a time");
        let errors = w.shutdown(&mut a);
        assert!(errors.is_empty());
        assert_eq!(a.meta().slot, Some(1));
        assert_eq!(fs::read(a.session_dir().join("slot-1.doc")).unwrap(), b"a");
        // A panicking encoder is an error, not a crash.
        let mut w = BackgroundWriter::start().unwrap();
        assert!(w.tick(&mut a, s(40), Revision::next(), true, || panic!("encoder bug")));
        let errors = w.shutdown(&mut a);
        assert_eq!(errors.len(), 1);
        a.close(false).unwrap();
        fs::remove_dir_all(&r).unwrap();
    }
}
