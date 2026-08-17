//! The on-disk spool: one folder per session, holding a lock, the envelope header, the session marker and
//! item segments.
//!
//! Several processes of one host can share a data directory, so every session folder has one owner. A
//! running process owns the folder of its own session and holds an exclusive advisory lock on the `lock`
//! file inside it for as long as it lives; the operating system drops that lock when the process ends,
//! however it ends. A process appends only to its own folder. A folder whose lock is held is left
//! completely alone. A folder whose lock can be taken is an orphan: whoever takes the lock adopts it,
//! reports its unclean end if it left a marker, sends what it holds and deletes it.
//!
//! A segment line is one category byte followed by the item as JSON. The category byte lets a consent
//! change remove a category's lines, and lets a batch be assembled, without parsing any item.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracer_proto::{AppInfo, ConsentCategory, Device, Item, ItemBody, UnixMs};

use super::DropCounts;
use super::files::{read_json, write_atomic};
use super::inspect::Outbox;
use super::session::{Marker, SESSION_FILE};

pub(super) const SPOOL_DIR: &str = "spool";
const HEADER_FILE: &str = "header.json";
const LOCK_FILE: &str = "lock";
const SEGMENT_EXTENSION: &str = "jsonl";

/// Category byte of an item that belongs to no consent category, such as `drops`.
pub(super) const CAT_META: u8 = b'-';

/// Largest single line the spool accepts. Keeps any one segment well inside one envelope.
pub(super) const MAX_LINE_BYTES: u64 = 256 * 1024;

/// Largest segment before a new one is started.
const MAX_SEGMENT_BYTES: u64 = 256 * 1024;

/// A folder with neither header nor marker that was touched this recently may belong to a process that
/// is still starting, so it is not cleaned up yet.
const STARTING_GRACE: Duration = Duration::from_secs(60);

/// Where file locks do not work, a folder nobody has written to for this long counts as an orphan.
const UNLOCKED_ORPHAN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a starting process waits for its own lock, in case a scan by another process is holding it
/// for a moment.
const OWN_LOCK_ATTEMPTS: u32 = 500;

pub(super) const DROP_EVICTED: &str = "spool_evicted";
pub(super) const DROP_TOO_LARGE: &str = "item_too_large";
pub(super) const DROP_IO: &str = "io_error";
/// Lines of a segment that could not be used: cut short by a crash, or not valid text.
pub(super) const DROP_DAMAGED: &str = "spool_damaged";

/// The category byte stored in front of an item of this consent category.
pub(super) fn category_code(category: ConsentCategory) -> u8 {
    match category {
        ConsentCategory::Crash => b'c',
        ConsentCategory::Performance => b'p',
        ConsentCategory::Usage => b'u',
    }
}

/// Content of `header.json`: the part of an envelope that is the same for every batch of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Header {
    pub schema: u32,
    pub app: AppInfo,
    pub install_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
    /// When the session started. Orders session folders, oldest first.
    #[serde(default)]
    pub created: UnixMs,
}

/// Whole segments of one session, read and ready to send.
#[derive(Debug)]
pub(super) struct Batch {
    pub header: Header,
    /// Segment numbers to delete once the server has answered.
    pub segments: Vec<u64>,
    /// Items as JSON, in the order they were recorded.
    pub items: Vec<String>,
    /// Lines in those segments that could not be used. Counted as dropped when the batch is completed.
    pub damaged: u64,
}

/// An orphan that was just adopted and whose owner left a marker: it did not shut down cleanly.
#[derive(Debug)]
pub(super) struct Unclean {
    pub session_id: String,
    pub dir: PathBuf,
    pub marker: Marker,
}

struct Segment {
    seq: u64,
    bytes: u64,
}

/// A session folder this process may write to: its own, or an orphan it adopted.
struct SessionDir {
    dir: PathBuf,
    header: Header,
    /// The held lock. `None` only where file locks do not work.
    lock: Option<File>,
    /// Bytes of everything in the folder that is not a segment: header, marker, saved log lines.
    overhead: u64,
    next_seq: u64,
    segments: Vec<Segment>,
    /// An adopted folder whose marker has not been answered yet. It is kept even with no segments.
    awaiting_report: bool,
}

impl SessionDir {
    fn bytes(&self) -> u64 {
        self.overhead + self.segments.iter().map(|segment| segment.bytes).sum::<u64>()
    }

    fn segment_path(&self, seq: u64) -> PathBuf {
        self.dir.join(segment_name(seq))
    }

    /// Removes one segment from disk and from the index. With `drops`, counts its items as evicted.
    fn delete_segment(&mut self, seq: u64, drops: Option<&DropCounts>) {
        let Some(position) = self.segments.iter().position(|segment| segment.seq == seq) else { return };
        self.segments.remove(position);
        let path = self.segment_path(seq);
        if let Some(drops) = drops {
            count_evicted(&std::fs::read(&path).unwrap_or_default(), drops);
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Deletes every stored line of one category.
    fn purge(&mut self, category: u8) {
        for seq in self.segments.iter().map(|segment| segment.seq).collect::<Vec<_>>() {
            let path = self.segment_path(seq);
            // A segment that cannot be read right now is left as it is; what is sent from it later is
            // filtered by consent again.
            let Ok(text) = std::fs::read(&path) else { continue };
            let mut kept = Vec::with_capacity(text.len());
            for line in text.split_inclusive(|byte| *byte == b'\n').filter(|line| line.ends_with(b"\n") && line.first() != Some(&category)) {
                kept.extend_from_slice(line);
            }
            if kept.is_empty() {
                self.delete_segment(seq, None);
            } else if kept.len() != text.len()
                && write_atomic(&path, &kept).is_ok()
                && let Some(segment) = self.segments.iter_mut().find(|segment| segment.seq == seq)
            {
                segment.bytes = kept.len() as u64;
            }
        }
    }

    /// Removes segments last written before `cutoff`, counting what they held.
    fn expire(&mut self, cutoff: SystemTime, drops: &DropCounts) {
        for seq in self.segments.iter().map(|segment| segment.seq).collect::<Vec<_>>() {
            let modified = std::fs::metadata(self.segment_path(seq)).and_then(|metadata| metadata.modified());
            if modified.is_ok_and(|modified| modified < cutoff) {
                self.delete_segment(seq, Some(drops));
            }
        }
    }

    fn measure_overhead(&mut self) {
        self.overhead = folder_stats(&self.dir).0.saturating_sub(self.segments.iter().map(|segment| segment.bytes).sum::<u64>());
    }

    /// Deletes the folder and lets go of the lock.
    #[cfg_attr(unix, allow(unused_mut))]
    fn remove(mut self) {
        // Where an open file can be deleted, the lock is kept until the folder is gone, so that no other
        // process can adopt a folder that is half deleted. Elsewhere the open lock file would keep the
        // folder from being deleted, so it is closed first; a folder another process catches in between
        // is cleaned up by the rule for folders without a header.
        #[cfg(not(unix))]
        self.unlock();
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    fn unlock(&mut self) {
        if let Some(lock) = self.lock.take() {
            let _ = lock.unlock();
        }
    }
}

impl Drop for SessionDir {
    /// Unlocks outright instead of leaving it to the file being closed. A lock belongs to the open file,
    /// and a child process the host starts shares that open file for the moment before it runs its own
    /// program; closing alone would leave the folder looking owned for that moment.
    fn drop(&mut self) {
        self.unlock();
    }
}

pub(super) struct Spool {
    root: PathBuf,
    max_bytes: u64,
    max_age: Duration,
    segment_bytes: u64,
    /// This process's session folder. `None` before [`Spool::open_own`] and after [`Spool::release`].
    own: Option<SessionDir>,
    /// The segment of the own folder that may still be appended to.
    open: Option<File>,
    /// Adopted folders of sessions whose process is gone, oldest first. Each has at least one segment,
    /// except between being adopted with a marker and [`Spool::add_recovery`].
    orphans: Vec<SessionDir>,
    /// Bytes in the folders of other running processes, as of the last scan. They count towards the cap
    /// but are never touched.
    live_bytes: u64,
    /// False where file locks do not work; see [`UNLOCKED_ORPHAN_AGE`].
    locking: bool,
    outbox: Option<Outbox>,
}

impl Spool {
    /// A spool that knows nothing yet about what is on disk. Touches no file.
    pub(super) fn new(dir: &Path, max_bytes: u64, max_age: Duration, outbox: Option<Outbox>) -> Self {
        let segment_bytes = (max_bytes / 8).clamp(512, MAX_SEGMENT_BYTES);
        Self {
            root: dir.join(SPOOL_DIR),
            max_bytes,
            max_age,
            segment_bytes,
            own: None,
            open: None,
            orphans: Vec::new(),
            live_bytes: 0,
            locking: true,
            outbox,
        }
    }

    /// The folder of the session with this id, whether or not it exists.
    pub(super) fn session_dir(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id)
    }

    /// Creates and locks this process's session folder and writes its header. False when that fails.
    pub(super) fn open_own(&mut self, header: Header) -> bool {
        let dir = self.session_dir(&header.session_id);
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let Ok(file) = open_lock_file(&dir) else { return false };
        let mut lock = None;
        for _ in 0..OWN_LOCK_ATTEMPTS {
            match file.try_lock() {
                Ok(()) => {
                    lock = Some(file);
                    break;
                }
                // Another process's scan is looking at the new folder; it lets go at once.
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
                Err(TryLockError::Error(_)) => {
                    self.locking = false;
                    break;
                }
            }
        }
        if self.locking && lock.is_none() {
            return false;
        }
        let written = serde_json::to_vec(&header).is_ok_and(|json| write_atomic(&dir.join(HEADER_FILE), &json).is_ok());
        if !written {
            return false;
        }
        let mut own = SessionDir { dir, header, lock, overhead: 0, next_seq: 0, segments: Vec::new(), awaiting_report: false };
        own.measure_overhead();
        self.own = Some(own);
        true
    }

    /// The header new items are stored under.
    pub(super) fn own_header(&self) -> Option<&Header> {
        self.own.as_ref().map(|own| &own.header)
    }

    /// Changes the install id of the own session and rewrites its header file.
    pub(super) fn set_install_id(&mut self, install_id: &str) {
        if let Some(own) = &mut self.own {
            own.header.install_id = install_id.to_owned();
            if let Ok(json) = serde_json::to_vec(&own.header) {
                let _ = write_atomic(&own.dir.join(HEADER_FILE), &json);
            }
            own.measure_overhead();
        }
    }

    /// Measures again the files in the own folder that are not segments. Call after writing one.
    pub(super) fn note_own_files(&mut self) {
        if let Some(own) = &mut self.own {
            own.measure_overhead();
        }
    }

    /// True when this process holds nothing it could send.
    pub(super) fn is_empty(&self) -> bool {
        self.own.as_ref().is_none_or(|own| own.segments.is_empty()) && self.orphans.iter().all(|orphan| orphan.segments.is_empty())
    }

    /// Bytes the whole spool holds on disk, as far as this process knows: its own folder, the orphans it
    /// adopted, and the folders of other running processes as of the last scan.
    pub(super) fn total_bytes(&self) -> u64 {
        self.own.as_ref().map_or(0, SessionDir::bytes) + self.orphans.iter().map(SessionDir::bytes).sum::<u64>() + self.live_bytes
    }

    /// Appends one item to the own session, making room by evicting the oldest segments this process may
    /// touch. Returns false, having counted the drop, when the item could not be stored.
    pub(super) fn append(&mut self, category: u8, json: &str, drops: &DropCounts) -> bool {
        if self.own.is_none() {
            return false;
        }
        let line_bytes = json.len() as u64 + 2;
        if line_bytes > MAX_LINE_BYTES {
            drops.add(DROP_TOO_LARGE, 1);
            return false;
        }
        while self.total_bytes() + line_bytes > self.max_bytes {
            if !self.evict_oldest(drops) {
                drops.add(DROP_EVICTED, 1);
                return false;
            }
        }
        if !self.write_line(category, json) {
            self.open = None;
            drops.add(DROP_IO, 1);
            return false;
        }
        if let Some(outbox) = &mut self.outbox {
            outbox.push(json);
        }
        true
    }

    fn write_line(&mut self, category: u8, json: &str) -> bool {
        let line_bytes = json.len() as u64 + 2;
        let segment_bytes = self.segment_bytes;
        let Some(own) = &mut self.own else { return false };
        let reuse = self.open.is_some() && own.segments.last().is_some_and(|segment| segment.bytes + line_bytes <= segment_bytes);
        if !reuse {
            self.open = None;
            let seq = own.next_seq;
            let Ok(file) = OpenOptions::new().create(true).append(true).open(own.segment_path(seq)) else { return false };
            own.next_seq += 1;
            own.segments.push(Segment { seq, bytes: 0 });
            self.open = Some(file);
        }
        let mut line = Vec::with_capacity(json.len() + 2);
        line.push(category);
        line.extend_from_slice(json.as_bytes());
        line.push(b'\n');
        let (Some(file), Some(segment)) = (self.open.as_mut(), own.segments.last_mut()) else { return false };
        // One write per line, so a line is on disk whole or, after a crash, as a tail without a newline
        // that readers skip.
        let written = file.write_all(&line).is_ok();
        if written {
            segment.bytes += line_bytes;
        } else if segment.bytes == 0 {
            let seq = segment.seq;
            own.delete_segment(seq, None);
        }
        written
    }

    /// Removes the oldest segment this process may touch and counts what it held: orphans first, oldest
    /// session first, then the own session. False when there is nothing left to remove.
    fn evict_oldest(&mut self, drops: &DropCounts) -> bool {
        if let Some(orphan) = self.orphans.iter_mut().find(|orphan| !orphan.segments.is_empty()) {
            if let Some(seq) = orphan.segments.first().map(|segment| segment.seq) {
                orphan.delete_segment(seq, Some(drops));
            }
            self.remove_empty_orphans();
            return true;
        }
        let Some(own) = &mut self.own else { return false };
        let Some(seq) = own.segments.first().map(|segment| segment.seq) else { return false };
        own.delete_segment(seq, Some(drops));
        if own.segments.is_empty() {
            self.open = None;
        }
        true
    }

    /// Looks at every other session folder. Those whose owner is gone are adopted; those returned left a
    /// marker and must be answered with [`Spool::add_recovery`] before the spool lock is released.
    /// Folders recorded under another install id were forgotten by the user and are deleted.
    pub(super) fn adopt_orphans(&mut self, install_id: &str) -> Vec<Unclean> {
        self.scan(Some(install_id))
    }

    /// `install_id` of `None` deletes every folder whose owner is gone instead of adopting it.
    fn scan(&mut self, install_id: Option<&str>) -> Vec<Unclean> {
        let mut unclean = Vec::new();
        let mut live_bytes = 0;
        let own_dir = self.own.as_ref().map(|own| own.dir.clone());
        for entry in std::fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let dir = entry.path();
            if !dir.is_dir() || Some(&dir) == own_dir.as_ref() || self.orphans.iter().any(|orphan| orphan.dir == dir) {
                continue;
            }
            let (bytes, newest) = folder_stats(&dir);
            let age = newest.and_then(|newest| SystemTime::now().duration_since(newest).ok()).unwrap_or_default();
            // With working locks, taking the lock proves the owner is gone. Without them, only a long
            // silence suggests it, and that is not proof enough to call the end of the session unclean.
            let (claimed, lock) = if self.locking {
                // The lock file is opened here, not created: creating it could put a file back into a
                // folder its owner is deleting at this very moment, and leave the folder behind. Only a
                // folder too old to have an owner still setting it up is given one.
                let opened = match OpenOptions::new().write(true).open(dir.join(LOCK_FILE)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound && age > STARTING_GRACE => open_lock_file(&dir),
                    other => other,
                };
                match opened.map(|file| file.try_lock().map(|()| file)) {
                    Ok(Ok(file)) => (true, Some(file)),
                    Ok(Err(TryLockError::WouldBlock)) | Err(_) => (false, None),
                    Ok(Err(TryLockError::Error(_))) => (age > UNLOCKED_ORPHAN_AGE, None),
                }
            } else {
                (age > UNLOCKED_ORPHAN_AGE, None)
            };
            if !claimed {
                live_bytes += bytes;
                continue;
            }
            let Some(install_id) = install_id else {
                drop(lock);
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let marker = read_json::<Marker>(&dir.join(SESSION_FILE)).filter(|marker| marker.header.session_id == name);
            let header = read_json::<Header>(&dir.join(HEADER_FILE)).filter(|header| header.session_id == name);
            let Some(header) = header.or_else(|| marker.as_ref().map(|marker| marker.header.clone())) else {
                drop(lock);
                if age > STARTING_GRACE {
                    let _ = std::fs::remove_dir_all(&dir);
                } else {
                    live_bytes += bytes;
                }
                continue;
            };
            let (segments, next_seq) = index_segments(&dir);
            let mut orphan = SessionDir { dir: dir.clone(), header, lock, overhead: 0, next_seq, segments, awaiting_report: false };
            let reportable = orphan.lock.is_some();
            if orphan.header.install_id != install_id || (orphan.segments.is_empty() && !(marker.is_some() && reportable)) {
                orphan.remove();
                continue;
            }
            match marker {
                Some(marker) if reportable => {
                    orphan.awaiting_report = true;
                    unclean.push(Unclean { session_id: name, dir, marker });
                }
                _ => {
                    let _ = std::fs::remove_file(orphan.dir.join(SESSION_FILE));
                }
            }
            orphan.measure_overhead();
            let position = self
                .orphans
                .partition_point(|other| (other.header.created, &other.header.session_id) <= (orphan.header.created, &orphan.header.session_id));
            self.orphans.insert(position, orphan);
        }
        self.live_bytes = live_bytes;
        if let Some(own) = &mut self.own {
            own.measure_overhead();
        }
        unclean
    }

    /// Stores the items that report an adopted session's unclean end, as (category byte, item JSON), and
    /// removes its marker so that it is reported once. The items go in as one whole segment.
    pub(super) fn add_recovery(&mut self, session_id: &str, lines: &[(u8, String)]) -> bool {
        let Some(orphan) = self.orphans.iter_mut().find(|orphan| orphan.header.session_id == session_id) else { return false };
        let mut stored = false;
        if !lines.is_empty() {
            let mut bytes = Vec::new();
            for (category, json) in lines {
                bytes.push(*category);
                bytes.extend_from_slice(json.as_bytes());
                bytes.push(b'\n');
            }
            let seq = orphan.next_seq;
            if write_atomic(&orphan.segment_path(seq), &bytes).is_ok() {
                orphan.next_seq += 1;
                orphan.segments.push(Segment { seq, bytes: bytes.len() as u64 });
                stored = true;
                if let Some(outbox) = &mut self.outbox {
                    for (_, json) in lines {
                        outbox.push(json);
                    }
                }
            }
        }
        let _ = std::fs::remove_file(orphan.dir.join(SESSION_FILE));
        orphan.awaiting_report = false;
        orphan.measure_overhead();
        self.remove_empty_orphans();
        stored
    }

    /// Removes segments older than the age limit from the folders this process may touch, counting what
    /// they held.
    pub(super) fn expire(&mut self, now: SystemTime, drops: &DropCounts) {
        let Some(cutoff) = now.checked_sub(self.max_age) else { return };
        for orphan in &mut self.orphans {
            orphan.expire(cutoff, drops);
        }
        self.remove_empty_orphans();
        if let Some(own) = &mut self.own {
            let last = own.segments.last().map(|segment| segment.seq);
            own.expire(cutoff, drops);
            if own.segments.last().map(|segment| segment.seq) != last {
                self.open = None;
            }
        }
    }

    /// Segments of one session, as many as fit in `budget` bytes and at least one: the oldest adopted
    /// orphan first, then the own session. Lines whose category `allowed` refuses are left out. The
    /// segments stay on disk until [`Spool::complete`].
    pub(super) fn next_batch(&mut self, budget: u64, allowed: &dyn Fn(u8) -> bool) -> Option<Batch> {
        if let Some(batch) = self.orphans.iter().find_map(|orphan| read_batch(orphan, budget, allowed)) {
            return Some(batch);
        }
        // Seal the segment being written, so that what is sent is exactly what is deleted afterwards.
        self.open = None;
        read_batch(self.own.as_ref()?, budget, allowed)
    }

    /// Deletes a batch's segments once the server has answered for them, and counts the lines in them
    /// that could not be used. An orphan that is empty afterwards is deleted altogether.
    pub(super) fn complete(&mut self, batch: &Batch, drops: &DropCounts) {
        drops.add(DROP_DAMAGED, batch.damaged);
        let id = &batch.header.session_id;
        if let Some(orphan) = self.orphans.iter_mut().find(|orphan| &orphan.header.session_id == id) {
            for seq in &batch.segments {
                orphan.delete_segment(*seq, None);
            }
            self.remove_empty_orphans();
        } else if let Some(own) = self.own.as_mut().filter(|own| &own.header.session_id == id) {
            for seq in &batch.segments {
                own.delete_segment(*seq, None);
            }
        }
    }

    /// Deletes every stored line of one category from the folders this process may touch. Not counted
    /// as a drop: the user asked for it.
    pub(super) fn purge(&mut self, category: u8) {
        self.open = None;
        for orphan in &mut self.orphans {
            orphan.purge(category);
        }
        self.remove_empty_orphans();
        if let Some(own) = &mut self.own {
            own.purge(category);
        }
    }

    /// Deletes everything this process may touch: its own items, every folder whose owner is gone, and
    /// the inspect mirror. The folders of other running processes are left for them to clear.
    pub(super) fn wipe(&mut self) {
        self.open = None;
        for orphan in std::mem::take(&mut self.orphans) {
            orphan.remove();
        }
        self.scan(None);
        if let Some(own) = &mut self.own {
            for seq in own.segments.iter().map(|segment| segment.seq).collect::<Vec<_>>() {
                own.delete_segment(seq, None);
            }
        }
        if let Some(outbox) = &mut self.outbox {
            outbox.wipe();
        }
    }

    /// Gives up every folder at the end of the session. The own folder is deleted if nothing is left in
    /// it; otherwise it stays, without a marker, for the next process to send. Nothing can be appended
    /// afterwards.
    pub(super) fn release(&mut self) {
        self.open = None;
        self.orphans.clear();
        if let Some(own) = self.own.take()
            && own.segments.is_empty()
        {
            own.remove();
        }
    }

    fn remove_empty_orphans(&mut self) {
        let (kept, empty): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.orphans).into_iter().partition(|orphan| !orphan.segments.is_empty() || orphan.awaiting_report);
        self.orphans = kept;
        for orphan in empty {
            orphan.remove();
        }
    }

    #[cfg(test)]
    fn without_locking(mut self) -> Self {
        self.locking = false;
        self
    }
}

fn open_lock_file(dir: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK_FILE))
}

fn segment_name(seq: u64) -> String {
    format!("{seq:08}.{SEGMENT_EXTENSION}")
}

/// The segments in a folder, in order, and the number the next one gets.
fn index_segments(dir: &Path) -> (Vec<Segment>, u64) {
    let mut segments = Vec::new();
    let mut next_seq = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let file = entry.path();
        let seq = file.file_stem().and_then(|stem| stem.to_str()).and_then(|stem| stem.parse::<u64>().ok());
        let is_segment = file.extension().is_some_and(|extension| extension == SEGMENT_EXTENSION);
        if let (Some(seq), true, Ok(metadata)) = (seq, is_segment, entry.metadata()) {
            next_seq = next_seq.max(seq + 1);
            if metadata.len() > 0 {
                segments.push(Segment { seq, bytes: metadata.len() });
            } else {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
    segments.sort_by_key(|segment| segment.seq);
    (segments, next_seq)
}

/// Total size of the files directly in a folder, and when the newest of them (or the folder itself) was
/// last written.
fn folder_stats(dir: &Path) -> (u64, Option<SystemTime>) {
    let mut bytes = 0;
    let mut newest = std::fs::metadata(dir).and_then(|metadata| metadata.modified()).ok();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_file() {
            bytes += metadata.len();
            if let Ok(modified) = metadata.modified() {
                newest = Some(newest.map_or(modified, |newest| newest.max(modified)));
            }
        }
    }
    (bytes, newest)
}

/// Reads segments of one session into a batch: as many as fit in `budget` bytes, and at least one. A
/// segment that cannot be read right now is left out, and so is not deleted with the batch. `None` when
/// the session has nothing that can be read.
fn read_batch(session: &SessionDir, budget: u64, allowed: &dyn Fn(u8) -> bool) -> Option<Batch> {
    let mut batch = Batch { header: session.header.clone(), segments: Vec::new(), items: Vec::new(), damaged: 0 };
    let mut used = 0;
    for segment in &session.segments {
        if !batch.segments.is_empty() && used + segment.bytes > budget {
            break;
        }
        let bytes = match std::fs::read(session.segment_path(segment.seq)) {
            Ok(bytes) => bytes,
            // Gone from disk: taking it into the batch takes it out of the index.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => continue,
        };
        used += segment.bytes;
        batch.segments.push(segment.seq);
        let (lines, damaged) = usable_lines(&bytes);
        batch.damaged += damaged;
        batch.items.extend(lines.into_iter().filter(|(category, _)| allowed(*category)).map(|(_, json)| json.to_owned()));
    }
    (!batch.segments.is_empty()).then_some(batch)
}

/// The usable lines of a segment as (category byte, item JSON), and how many lines are not usable. Each
/// line is judged on its own: a tail without a newline is a write that was cut short, and a line that is
/// not valid UTF-8 is damaged; neither takes its neighbours with it.
fn usable_lines(bytes: &[u8]) -> (Vec<(u8, &str)>, u64) {
    let mut lines = Vec::new();
    let mut damaged = 0;
    let mut pieces = bytes.split(|byte| *byte == b'\n').peekable();
    while let Some(piece) = pieces.next() {
        let whole = pieces.peek().is_some();
        match piece.split_first() {
            None => {}
            Some((category, rest)) if whole => match std::str::from_utf8(rest) {
                Ok(json) => lines.push((*category, json)),
                Err(_) => damaged += 1,
            },
            Some(_) => damaged += 1,
        }
    }
    (lines, damaged)
}

/// Counts the items of a segment that is being thrown away. A `drops` item in it is not itself counted:
/// its counts go back into `drops` so they are reported with a later batch.
fn count_evicted(bytes: &[u8], drops: &DropCounts) {
    let (lines, damaged) = usable_lines(bytes);
    let mut evicted = damaged;
    for (category, json) in lines {
        if category == CAT_META
            && let Ok(Item { body: ItemBody::Drops(carried), .. }) = serde_json::from_str::<Item>(json)
        {
            drops.merge(carried.reasons);
        } else {
            evicted += 1;
        }
    }
    drops.add(DROP_EVICTED, evicted);
}

#[cfg(test)]
mod tests {
    use tracer_proto::{Drops, ProcessKind, SCHEMA_VERSION, SessionTotals};

    use super::super::files::testing::TempDir;
    use super::super::files::write_json;
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const INSTALL: &str = "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii";

    fn header(session: &str, created: UnixMs) -> Header {
        Header {
            schema: SCHEMA_VERSION,
            app: AppInfo { name: "host".to_owned(), version: "1.0.0".to_owned(), build: None, process: ProcessKind::Gui },
            install_id: INSTALL.to_owned(),
            session_id: session.to_owned(),
            device: None,
            created,
        }
    }

    /// A spool with its own session folder open, as a running process has.
    fn running(dir: &Path, session: &str, created: UnixMs, max_bytes: u64) -> Spool {
        let mut spool = Spool::new(dir, max_bytes, DAY, None);
        assert!(spool.open_own(header(session, created)));
        spool
    }

    /// Leaves a session folder behind as a process that was killed would: items, a marker, no lock.
    fn killed(dir: &Path, session: &str, created: UnixMs, items: std::ops::Range<usize>, panicked: bool) {
        let drops = DropCounts::default();
        let mut spool = running(dir, session, created, 1 << 20);
        for number in items {
            assert!(spool.append(b'u', &item(number), &drops));
        }
        let marker = Marker {
            header: header(session, created),
            started: created,
            updated: created + 5,
            totals: SessionTotals::default(),
            errors: 0,
            panicked,
        };
        assert!(write_json(&spool.session_dir(session).join(SESSION_FILE), &marker));
    }

    /// Leaves a session folder behind as a process that shut down cleanly but could not send would.
    fn ended(dir: &Path, session: &str, created: UnixMs, items: std::ops::Range<usize>) {
        let drops = DropCounts::default();
        let mut spool = running(dir, session, created, 1 << 20);
        for number in items {
            assert!(spool.append(b'u', &item(number), &drops));
        }
        spool.release();
    }

    fn item(number: usize) -> String {
        format!(r#"{{"id":"{number:032}","ts":1,"type":"event","name":"file.open","category":"file"}}"#)
    }

    fn all(_: u8) -> bool {
        true
    }

    fn disk_bytes(dir: &Path) -> u64 {
        let mut total = 0;
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let metadata = entry.metadata().unwrap();
            total += if metadata.is_dir() { disk_bytes(&entry.path()) } else { metadata.len() };
        }
        total
    }

    /// Sends everything the spool will give, returning (session id, item) in order.
    fn drain(spool: &mut Spool) -> Vec<(String, String)> {
        let mut sent = Vec::new();
        while let Some(batch) = spool.next_batch(u64::MAX, &all) {
            sent.extend(batch.items.iter().map(|json| (batch.header.session_id.clone(), json.clone())));
            spool.complete(&batch, &DropCounts::default());
        }
        sent
    }

    fn items_of(sent: &[(String, String)]) -> Vec<String> {
        sent.iter().map(|(_, json)| json.clone()).collect()
    }

    #[test]
    fn appended_items_come_back_in_order_and_are_deleted_on_complete() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::new(dir.path(), 1 << 20, DAY, None);
        assert!(!dir.path().join(SPOOL_DIR).exists());
        assert!(!spool.append(b'u', &item(0), &drops), "nothing is stored before the session folder is open");
        assert!(spool.open_own(header("a", 1)));
        assert!(spool.is_empty());
        for number in 0..10 {
            assert!(spool.append(b'u', &item(number), &drops));
        }
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!(batch.header, header("a", 1));
        assert_eq!(batch.items, (0..10).map(item).collect::<Vec<_>>());
        spool.complete(&batch, &drops);
        assert!(spool.is_empty());
        // The folder itself stays for as long as the session runs: lock and header.
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        assert!(spool.session_dir("a").join(HEADER_FILE).exists());
        assert!(drops.take().is_empty());
    }

    #[test]
    fn items_added_while_a_batch_is_out_survive_its_completion() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = running(dir.path(), "a", 1, 1 << 20);
        spool.append(b'u', &item(0), &drops);
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        spool.append(b'u', &item(1), &drops);
        spool.complete(&batch, &drops);
        assert_eq!(items_of(&drain(&mut spool)), vec![item(1)]);
    }

    #[test]
    fn release_deletes_an_empty_folder_and_leaves_a_full_one_for_the_next_process() {
        let dir = TempDir::new("spool");
        ended(dir.path(), "empty", 1, 0..0);
        assert!(!dir.path().join(SPOOL_DIR).join("empty").exists());
        ended(dir.path(), "full", 2, 0..3);
        assert!(dir.path().join(SPOOL_DIR).join("full").exists());

        let drops = DropCounts::default();
        let mut next = running(dir.path(), "next", 3, 1 << 20);
        assert!(next.adopt_orphans(INSTALL).is_empty(), "a clean end leaves no marker");
        next.append(b'u', &item(9), &drops);
        assert_eq!(next.total_bytes(), disk_bytes(dir.path()));
        let sent = drain(&mut next);
        let expected: Vec<(String, String)> = (0..3).map(|number| ("full".to_owned(), item(number))).chain([("next".to_owned(), item(9))]).collect();
        assert_eq!(sent, expected);
        assert!(!dir.path().join(SPOOL_DIR).join("full").exists());
        // Released, the spool accepts nothing more.
        next.release();
        assert!(!next.append(b'u', &item(10), &drops));
    }

    #[test]
    fn orphans_are_sent_oldest_first() {
        let dir = TempDir::new("spool");
        ended(dir.path(), "newer", 20, 2..3);
        ended(dir.path(), "older", 10, 1..2);
        let mut spool = running(dir.path(), "now", 30, 1 << 20);
        spool.adopt_orphans(INSTALL);
        spool.append(b'u', &item(3), &DropCounts::default());
        let sent = drain(&mut spool);
        assert_eq!(sent.iter().map(|(session, _)| session.as_str()).collect::<Vec<_>>(), vec!["older", "newer", "now"]);
        assert_eq!(items_of(&sent), vec![item(1), item(2), item(3)]);
    }

    #[test]
    fn a_running_process_is_left_completely_alone() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut first = running(dir.path(), "first", 1, 8 * 1024);
        for number in 0..40 {
            assert!(first.append(b'u', &item(number), &drops));
        }
        let first_dir = first.session_dir("first");
        assert!(write_json(&first_dir.join(SESSION_FILE), &1u32));
        let before = disk_bytes(&first_dir);

        let mut second = running(dir.path(), "second", 2, 8 * 1024);
        assert!(second.adopt_orphans(INSTALL).is_empty());
        // Its data is not sent, purged, expired or wiped by anyone else.
        assert!(second.next_batch(u64::MAX, &all).is_none());
        second.purge(b'u');
        second.expire(SystemTime::now() + 2 * DAY, &drops);
        second.wipe();
        assert_eq!(disk_bytes(&first_dir), before);
        // It counts towards the shared cap, but making room never evicts it: the second process loses
        // its own oldest items instead.
        assert_eq!(second.total_bytes(), disk_bytes(dir.path()));
        for number in 100..200 {
            second.append(b'u', &item(number), &drops);
            assert!(second.total_bytes() <= 8 * 1024);
        }
        assert_eq!(disk_bytes(&first_dir), before);
        assert!(drops.take().get(DROP_EVICTED).copied().unwrap_or(0) > 0);
        assert_eq!(items_of(&drain(&mut first)), (0..40).map(item).collect::<Vec<_>>());
        let kept = items_of(&drain(&mut second));
        assert!(!kept.is_empty() && kept.last() == Some(&item(199)));
    }

    #[test]
    fn an_unclean_session_is_adopted_and_reported_by_exactly_one_process() {
        let dir = TempDir::new("spool");
        killed(dir.path(), "gone", 1, 0..3, false);
        let mut first = running(dir.path(), "first", 2, 1 << 20);
        let mut second = running(dir.path(), "second", 3, 1 << 20);

        let unclean = first.adopt_orphans(INSTALL);
        assert_eq!(unclean.len(), 1);
        assert_eq!((unclean[0].session_id.as_str(), unclean[0].marker.updated, unclean[0].marker.panicked), ("gone", 6, false));
        assert_eq!(unclean[0].dir, first.session_dir("gone"));
        // The first process holds it now, so to the second it is a folder with a live owner.
        assert!(second.adopt_orphans(INSTALL).is_empty());
        assert!(second.next_batch(u64::MAX, &all).is_none());

        assert!(first.add_recovery("gone", &[(b'u', item(90)), (b'c', item(91))]));
        assert!(!first.session_dir("gone").join(SESSION_FILE).exists());
        assert!(first.adopt_orphans(INSTALL).is_empty());
        assert_eq!(first.total_bytes(), disk_bytes(dir.path()));
        let sent = drain(&mut first);
        assert!(sent.iter().all(|(session, _)| session == "gone"));
        assert_eq!(items_of(&sent), vec![item(0), item(1), item(2), item(90), item(91)]);
        assert!(!first.session_dir("gone").exists());
        assert!(second.adopt_orphans(INSTALL).is_empty());
    }

    #[test]
    fn an_adopter_that_dies_before_sending_hands_the_orphan_on() {
        let dir = TempDir::new("spool");
        killed(dir.path(), "gone", 1, 0..2, true);
        {
            let mut first = running(dir.path(), "first", 2, 1 << 20);
            let unclean = first.adopt_orphans(INSTALL);
            assert!(unclean[0].marker.panicked);
            first.add_recovery("gone", &[(b'u', item(90))]);
            first.release();
        }
        let mut second = running(dir.path(), "second", 3, 1 << 20);
        // The marker is gone, so there is nothing to report again: only data to send.
        assert!(second.adopt_orphans(INSTALL).is_empty());
        assert_eq!(items_of(&drain(&mut second)), vec![item(0), item(1), item(90)]);
    }

    #[test]
    fn a_marker_without_items_is_still_reported_and_nothing_to_say_removes_the_folder() {
        let dir = TempDir::new("spool");
        killed(dir.path(), "quiet", 1, 0..0, false);
        killed(dir.path(), "silent", 2, 0..0, false);
        let mut spool = running(dir.path(), "now", 3, 1 << 20);
        let mut found: Vec<String> = spool.adopt_orphans(INSTALL).into_iter().map(|unclean| unclean.session_id).collect();
        found.sort();
        assert_eq!(found, vec!["quiet".to_owned(), "silent".to_owned()]);
        assert!(spool.add_recovery("quiet", &[(b'c', item(7))]));
        // Consent may leave nothing to report; the folder is then simply deleted.
        assert!(!spool.add_recovery("silent", &[]));
        assert!(!spool.session_dir("silent").exists());
        assert_eq!(drain(&mut spool), vec![("quiet".to_owned(), item(7))]);
    }

    #[test]
    fn folders_of_a_forgotten_install_are_deleted_not_sent() {
        let dir = TempDir::new("spool");
        killed(dir.path(), "gone", 1, 0..3, false);
        ended(dir.path(), "ended", 2, 3..5);
        let mut spool = running(dir.path(), "now", 3, 1 << 20);
        assert!(spool.adopt_orphans("another-install-id").is_empty());
        assert!(spool.is_empty());
        assert!(!spool.session_dir("gone").exists() && !spool.session_dir("ended").exists());
    }

    #[test]
    fn a_folder_that_is_still_being_set_up_is_not_cleaned_away() {
        let dir = TempDir::new("spool");
        let starting = dir.path().join(SPOOL_DIR).join("starting");
        std::fs::create_dir_all(&starting).unwrap();
        let mut spool = running(dir.path(), "now", 1, 1 << 20);
        assert!(spool.adopt_orphans(INSTALL).is_empty());
        assert!(starting.exists());
        // The scan let go of the lock, so the process that is starting can take it.
        let mut late = Spool::new(dir.path(), 1 << 20, DAY, None);
        assert!(late.open_own(header("starting", 2)));
        // Long abandoned, the same folder is removed.
        let stale = dir.path().join(SPOOL_DIR).join("stale");
        std::fs::create_dir_all(&stale).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        File::open(&stale).unwrap().set_modified(old).unwrap();
        spool.adopt_orphans(INSTALL);
        assert!(!stale.exists());
    }

    #[test]
    fn never_exceeds_the_cap_and_counts_evictions() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let cap = 6 * 1024;
        ended(dir.path(), "a", 1, 0..20);
        let mut spool = running(dir.path(), "b", 2, cap);
        spool.adopt_orphans(INSTALL);
        let total = 400;
        for number in 20..total {
            assert!(spool.append(b'u', &item(number), &drops));
            assert!(spool.total_bytes() <= cap);
            assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        }
        // The orphan, being oldest, went first and entirely.
        assert!(!spool.session_dir("a").exists());
        let kept = items_of(&drain(&mut spool));
        let evicted = drops.take().get(DROP_EVICTED).copied().unwrap_or(0) as usize;
        assert!(evicted > 0);
        assert_eq!(kept.len() + evicted, total);
        // The newest items are the ones kept.
        assert_eq!(kept, (total - kept.len()..total).map(item).collect::<Vec<_>>());
    }

    #[test]
    fn an_item_larger_than_the_cap_is_refused() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = running(dir.path(), "a", 1, 600);
        assert!(!spool.append(b'c', &"x".repeat(700), &drops));
        assert!(!spool.append(b'c', &"x".repeat(MAX_LINE_BYTES as usize), &drops));
        let counts = drops.take();
        assert_eq!(counts.get(DROP_EVICTED), Some(&1));
        assert_eq!(counts.get(DROP_TOO_LARGE), Some(&1));
        assert!(spool.is_empty());
    }

    #[test]
    fn evicting_a_drops_item_keeps_its_counts() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = running(dir.path(), "a", 1, 4096);
        let carried = Item { id: "d".repeat(32), ts: 1, body: ItemBody::Drops(Drops { reasons: [("queue_full".to_owned(), 7)].into() }) };
        spool.append(CAT_META, &serde_json::to_string(&carried).unwrap(), &drops);
        for number in 0..200 {
            spool.append(b'u', &item(number), &drops);
        }
        let counts = drops.take();
        assert_eq!(counts.get("queue_full"), Some(&7));
        let kept = drain(&mut spool).len() as u64;
        assert_eq!(counts.get(DROP_EVICTED).copied().unwrap_or(0) + kept, 200);
    }

    #[test]
    fn batches_respect_the_budget_and_cover_everything_once() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = running(dir.path(), "a", 1, 64 * 1024);
        for number in 0..300 {
            spool.append(b'u', &item(number), &drops);
        }
        let mut seen = Vec::new();
        let mut batches = 0;
        while let Some(batch) = spool.next_batch(10_000, &all) {
            assert!(batch.items.iter().map(|json| json.len() as u64 + 2).sum::<u64>() <= 10_000);
            seen.extend(batch.items.clone());
            spool.complete(&batch, &drops);
            batches += 1;
        }
        assert!(batches > 1);
        assert_eq!(seen, (0..300).map(item).collect::<Vec<_>>());
    }

    #[test]
    fn purge_removes_one_category_only() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        {
            let mut earlier = running(dir.path(), "a", 1, 1 << 20);
            for number in 0..15 {
                earlier.append(b"ucp"[number % 3], &item(number), &drops);
            }
            earlier.release();
        }
        ended(dir.path(), "only_usage", 2, 99..100);
        let mut spool = running(dir.path(), "b", 3, 1 << 20);
        spool.adopt_orphans(INSTALL);
        for number in 15..30 {
            spool.append(b"ucp"[number % 3], &item(number), &drops);
        }
        spool.purge(b'u');
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        assert!(!spool.session_dir("only_usage").exists());
        assert_eq!(items_of(&drain(&mut spool)), (0..30).filter(|number| number % 3 != 0).map(item).collect::<Vec<_>>());
        assert!(drops.take().is_empty());
    }

    #[test]
    fn a_batch_leaves_out_categories_that_are_not_allowed() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = running(dir.path(), "a", 1, 1 << 20);
        spool.append(b'u', &item(0), &drops);
        spool.append(b'c', &item(1), &drops);
        let batch = spool.next_batch(u64::MAX, &|category| category != b'u').unwrap();
        assert_eq!(batch.items, vec![item(1)]);
    }

    #[test]
    fn a_torn_last_line_is_skipped() {
        let dir = TempDir::new("spool");
        ended(dir.path(), "a", 1, 0..1);
        let segment = dir.path().join(SPOOL_DIR).join("a").join(segment_name(0));
        let mut file = OpenOptions::new().append(true).open(segment).unwrap();
        file.write_all(br#"u{"id":"torn","ts"#).unwrap();
        let mut spool = running(dir.path(), "b", 2, 1 << 20);
        spool.adopt_orphans(INSTALL);
        assert_eq!(items_of(&drain(&mut spool)), vec![item(0)]);
    }

    #[test]
    fn a_line_cut_inside_a_character_does_not_cost_the_segment() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        ended(dir.path(), "a", 1, 0..3);
        let segment = dir.path().join(SPOOL_DIR).join("a").join(segment_name(0));
        // The process died in the middle of an append, inside a two-byte character.
        OpenOptions::new().append(true).open(&segment).unwrap().write_all(b"c{\"id\":\"x\",\"message\":\"caf\xC3").unwrap();
        let mut spool = running(dir.path(), "b", 2, 1 << 20);
        spool.adopt_orphans(INSTALL);
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!(batch.items, (0..3).map(item).collect::<Vec<_>>());
        assert_eq!(batch.damaged, 1);
        spool.complete(&batch, &drops);
        assert_eq!(drops.take().get(DROP_DAMAGED), Some(&1));
        assert!(!segment.exists());
    }

    #[test]
    fn a_damaged_line_in_the_middle_is_skipped_alone_everywhere() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = running(dir.path(), "a", 1, 1 << 20);
        spool.append(b'u', &item(0), &drops);
        let segment = spool.session_dir("a").join(segment_name(0));
        OpenOptions::new().append(true).open(&segment).unwrap().write_all(b"c{\"bad\":\"\xFF\xFE\"}\n").unwrap();
        spool.open = None;
        spool.append(b'c', &item(1), &drops);
        spool.append(b'u', &item(2), &drops);
        // Removing one category keeps the others, damaged or not, byte for byte.
        spool.purge(b'u');
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!((batch.items.clone(), batch.damaged), (vec![item(1)], 1));
        // Thrown away instead of sent, the damaged line counts as one lost item.
        spool.own.as_mut().unwrap().delete_segment(batch.segments[0], Some(&drops));
        let counts = drops.take();
        assert_eq!(counts.get(DROP_EVICTED).copied().unwrap_or(0), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_segment_that_cannot_be_read_is_neither_sent_nor_deleted() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        ended(dir.path(), "a", 1, 0..3);
        let segment = dir.path().join(SPOOL_DIR).join("a").join(segment_name(0));
        let mut spool = running(dir.path(), "b", 2, 1 << 20);
        spool.adopt_orphans(INSTALL);
        spool.append(b'u', &item(9), &drops);
        std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&segment).is_ok() {
            // Running with rights that ignore file modes; the read error cannot be staged.
            return;
        }
        // The unreadable session does not hold up the one after it, and loses nothing.
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!((batch.header.session_id.as_str(), batch.items.clone()), ("b", vec![item(9)]));
        spool.complete(&batch, &drops);
        spool.purge(b'u');
        assert!(spool.next_batch(u64::MAX, &all).is_none());
        assert!(segment.exists());
        std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o644)).unwrap();
        // purge skipped it while it was unreadable; once readable again its lines are still there.
        assert_eq!(items_of(&drain(&mut spool)), (0..3).map(item).collect::<Vec<_>>());
        assert!(drops.take().is_empty());
    }

    #[test]
    fn two_unclean_sessions_found_in_one_scan_are_both_kept_until_answered() {
        let dir = TempDir::new("spool");
        killed(dir.path(), "first", 1, 0..0, false);
        killed(dir.path(), "second", 2, 0..0, false);
        let mut spool = running(dir.path(), "now", 3, 1 << 20);
        let mut unclean = spool.adopt_orphans(INSTALL);
        unclean.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(unclean.len(), 2);
        // Answering one must leave the other, marker and saved files and all, for its own answer.
        assert!(spool.add_recovery("first", &[(b'c', item(1))]));
        assert!(spool.session_dir("second").join(SESSION_FILE).exists());
        assert!(spool.add_recovery("second", &[(b'c', item(2))]));
        let mut sent = drain(&mut spool);
        sent.sort();
        assert_eq!(sent, vec![("first".to_owned(), item(1)), ("second".to_owned(), item(2))]);
    }

    #[test]
    fn old_segments_expire_and_are_counted() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        ended(dir.path(), "a", 1, 0..2);
        let mut spool = Spool::new(dir.path(), 1 << 20, Duration::from_secs(60), None);
        assert!(spool.open_own(header("b", 2)));
        spool.adopt_orphans(INSTALL);
        for number in 2..5 {
            spool.append(b'u', &item(number), &drops);
        }
        spool.expire(SystemTime::now(), &drops);
        assert!(!spool.is_empty());
        spool.expire(SystemTime::now() + Duration::from_secs(3600), &drops);
        assert!(spool.is_empty());
        assert!(!spool.session_dir("a").exists());
        assert_eq!(drops.take().get(DROP_EVICTED), Some(&5));
        // The own folder goes on being used.
        assert!(spool.append(b'u', &item(5), &drops));
        assert_eq!(items_of(&drain(&mut spool)), vec![item(5)]);
    }

    #[test]
    fn wipe_removes_everything_this_process_may_touch() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        killed(dir.path(), "gone", 1, 0..2, false);
        ended(dir.path(), "adopted", 2, 2..4);
        let mut spool = Spool::new(dir.path(), 1 << 20, DAY, Some(Outbox::open(dir.path())));
        assert!(spool.open_own(header("now", 3)));
        // One orphan is already adopted, the other has not been looked at yet.
        std::fs::rename(spool.session_dir("gone"), dir.path().join("aside")).unwrap();
        spool.adopt_orphans(INSTALL);
        std::fs::rename(dir.path().join("aside"), spool.session_dir("gone")).unwrap();
        spool.append(b'u', &item(4), &drops);
        spool.wipe();
        assert!(spool.is_empty());
        assert!(!spool.session_dir("gone").exists() && !spool.session_dir("adopted").exists());
        assert!(!dir.path().join("outbox.jsonl").exists());
        assert!(spool.next_batch(u64::MAX, &all).is_none());
        assert!(spool.append(b'u', &item(5), &drops));
        assert_eq!(items_of(&drain(&mut spool)), vec![item(5)]);
    }

    #[test]
    fn changing_the_install_id_rewrites_the_header() {
        let dir = TempDir::new("spool");
        let mut spool = running(dir.path(), "a", 1, 1 << 20);
        spool.set_install_id("new-install");
        assert_eq!(spool.own_header().map(|header| header.install_id.as_str()), Some("new-install"));
        let saved: Header = read_json(&spool.session_dir("a").join(HEADER_FILE)).unwrap();
        assert_eq!(saved.install_id, "new-install");
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
    }

    #[test]
    fn without_file_locks_only_long_silent_folders_are_adopted_and_none_is_called_unclean() {
        let dir = TempDir::new("spool");
        killed(dir.path(), "recent", 1, 0..2, false);
        killed(dir.path(), "silent", 2, 2..4, false);
        let silent = dir.path().join(SPOOL_DIR).join("silent");
        let long_ago = SystemTime::now() - 2 * DAY;
        for entry in std::fs::read_dir(&silent).unwrap().flatten() {
            OpenOptions::new().write(true).open(entry.path()).unwrap().set_modified(long_ago).unwrap();
        }
        File::open(&silent).unwrap().set_modified(long_ago).unwrap();

        let mut spool = running(dir.path(), "now", 3, 1 << 20).without_locking();
        assert!(spool.adopt_orphans(INSTALL).is_empty(), "silence is not proof of an unclean end");
        assert!(!silent.join(SESSION_FILE).exists());
        assert_eq!(drain(&mut spool), vec![("silent".to_owned(), item(2)), ("silent".to_owned(), item(3))]);
        // The recent one may belong to a running process, so it is untouched, marker and all.
        assert!(dir.path().join(SPOOL_DIR).join("recent").join(SESSION_FILE).exists());
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
    }
}
