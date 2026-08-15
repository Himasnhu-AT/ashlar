//! The on-disk spool: one folder per session, holding the envelope header and item segments.
//!
//! A segment line is one category byte followed by the item as JSON. The category byte lets a consent
//! change remove a category's lines, and lets a batch be assembled, without parsing any item.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracer_proto::{AppInfo, ConsentCategory, Device, Item, ItemBody, UnixMs};

use super::DropCounts;
use super::files::{read_json, write_atomic};
use super::inspect::Outbox;

pub(super) const SPOOL_DIR: &str = "spool";
const HEADER_FILE: &str = "header.json";
const SEGMENT_EXTENSION: &str = "jsonl";

/// Category byte of an item that belongs to no consent category, such as `drops`.
pub(super) const CAT_META: u8 = b'-';

/// Largest single line the spool accepts. Keeps any one segment well inside one envelope.
const MAX_LINE_BYTES: u64 = 256 * 1024;

/// Largest segment before a new one is started.
const MAX_SEGMENT_BYTES: u64 = 256 * 1024;

pub(super) const DROP_EVICTED: &str = "spool_evicted";
pub(super) const DROP_TOO_LARGE: &str = "item_too_large";
pub(super) const DROP_IO: &str = "io_error";

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
}

struct Segment {
    seq: u64,
    bytes: u64,
}

struct SessionDir {
    id: String,
    created: UnixMs,
    header_bytes: u64,
    next_seq: u64,
    segments: Vec<Segment>,
}

struct OpenSegment {
    session: String,
    file: File,
}

pub(super) struct Spool {
    root: PathBuf,
    max_bytes: u64,
    max_age: Duration,
    segment_bytes: u64,
    /// Oldest first. Every entry has at least one segment.
    sessions: Vec<SessionDir>,
    /// Bytes on disk: every header and every segment.
    total: u64,
    /// The one segment that may still be appended to.
    open: Option<OpenSegment>,
    outbox: Option<Outbox>,
}

impl Spool {
    /// A spool that knows nothing yet about what is on disk. Touches no file.
    pub(super) fn new(dir: &Path, max_bytes: u64, max_age: Duration, outbox: Option<Outbox>) -> Self {
        let segment_bytes = (max_bytes / 8).clamp(512, MAX_SEGMENT_BYTES);
        Self { root: dir.join(SPOOL_DIR), max_bytes, max_age, segment_bytes, sessions: Vec::new(), total: 0, open: None, outbox }
    }

    #[cfg(test)]
    fn open(dir: &Path, max_bytes: u64, max_age: Duration, outbox: Option<Outbox>) -> Self {
        let mut spool = Self::new(dir, max_bytes, max_age, outbox);
        spool.scan();
        spool
    }

    /// Indexes what earlier runs left behind. Creates nothing; unreadable folders are removed.
    pub(super) fn scan(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let session = entry.file_name().to_str().and_then(|id| Self::scan_session(&path, id));
            match session {
                Some(session) => {
                    self.total += session.header_bytes + session.segments.iter().map(|segment| segment.bytes).sum::<u64>();
                    self.sessions.push(session);
                }
                None if path.is_dir() => {
                    let _ = std::fs::remove_dir_all(&path);
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        self.sessions.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    }

    /// Reads one session folder. `None` when it holds nothing that can be sent.
    fn scan_session(path: &Path, id: &str) -> Option<SessionDir> {
        let header_path = path.join(HEADER_FILE);
        let header = read_json::<Header>(&header_path).filter(|header| header.session_id == id)?;
        let header_bytes = std::fs::metadata(&header_path).ok()?.len();
        let mut segments = Vec::new();
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            let file = entry.path();
            let seq = file.file_stem().and_then(|stem| stem.to_str()).and_then(|stem| stem.parse::<u64>().ok());
            let is_segment = file.extension().is_some_and(|extension| extension == SEGMENT_EXTENSION);
            match (seq, entry.metadata()) {
                (Some(seq), Ok(metadata)) if is_segment && metadata.len() > 0 => segments.push(Segment { seq, bytes: metadata.len() }),
                _ if file != header_path => {
                    // A leftover temporary file or an empty segment.
                    let _ = std::fs::remove_file(&file);
                }
                _ => {}
            }
        }
        segments.sort_by_key(|segment| segment.seq);
        let next_seq = segments.last()?.seq + 1;
        Some(SessionDir { id: id.to_owned(), created: header.created, header_bytes, next_seq, segments })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Bytes the spool holds on disk.
    #[cfg(test)]
    fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Appends one item to the session named in `header`, making room by evicting the oldest segments.
    /// Returns false, having counted the drop, when the item could not be stored.
    pub(super) fn append(&mut self, header: &Header, category: u8, json: &str, drops: &DropCounts) -> bool {
        let line_bytes = json.len() as u64 + 2;
        if line_bytes > MAX_LINE_BYTES {
            drops.add(DROP_TOO_LARGE, 1);
            return false;
        }
        // The header is only needed, and only serialised, when the session has no folder yet. Making
        // room can remove that folder, so this is looked at again after every eviction.
        let mut header_json = None;
        loop {
            let exists = self.sessions.iter().any(|session| session.id == header.session_id);
            if !exists && header_json.is_none() {
                let Ok(json) = serde_json::to_vec(header) else {
                    drops.add(DROP_IO, 1);
                    return false;
                };
                header_json = Some(json);
            }
            let header_bytes = if exists { 0 } else { header_json.as_ref().map_or(0, |json| json.len() as u64) };
            if self.total + line_bytes + header_bytes <= self.max_bytes {
                break;
            }
            if !self.evict_oldest(drops) {
                drops.add(DROP_EVICTED, 1);
                return false;
            }
        }
        if !self.write_line(header, header_json.as_deref(), category, json) {
            self.open = None;
            drops.add(DROP_IO, 1);
            return false;
        }
        if let Some(outbox) = &mut self.outbox {
            outbox.push(json);
        }
        true
    }

    /// `header_json` must be given when the session has no folder yet.
    fn write_line(&mut self, header: &Header, header_json: Option<&[u8]>, category: u8, json: &str) -> bool {
        let line_bytes = json.len() as u64 + 2;
        let dir = self.root.join(&header.session_id);
        let index = match self.sessions.iter().position(|session| session.id == header.session_id) {
            Some(index) => index,
            None => {
                let Some(header_json) = header_json else { return false };
                if std::fs::create_dir_all(&dir).is_err() || write_atomic(&dir.join(HEADER_FILE), header_json).is_err() {
                    return false;
                }
                let session = SessionDir {
                    id: header.session_id.clone(),
                    created: header.created,
                    header_bytes: header_json.len() as u64,
                    next_seq: 0,
                    segments: Vec::new(),
                };
                self.total += session.header_bytes;
                let index = self.sessions.partition_point(|other| (other.created, &other.id) <= (session.created, &session.id));
                self.sessions.insert(index, session);
                index
            }
        };
        let segment_bytes = self.segment_bytes;
        let Some(session) = self.sessions.get_mut(index) else { return false };
        let reuse = self.open.as_ref().is_some_and(|open| open.session == session.id)
            && session.segments.last().is_some_and(|segment| segment.bytes + line_bytes <= segment_bytes);
        if !reuse {
            self.open = None;
            let seq = session.next_seq;
            let opened = OpenOptions::new().create(true).append(true).open(dir.join(segment_name(seq)));
            let Ok(file) = opened else {
                if session.segments.is_empty() {
                    self.remove_empty_sessions();
                }
                return false;
            };
            session.next_seq += 1;
            session.segments.push(Segment { seq, bytes: 0 });
            self.open = Some(OpenSegment { session: session.id.clone(), file });
        }
        let mut line = Vec::with_capacity(json.len() + 2);
        line.push(category);
        line.extend_from_slice(json.as_bytes());
        line.push(b'\n');
        let (Some(open), Some(segment)) = (self.open.as_mut(), session.segments.last_mut()) else { return false };
        // One write per line, so a line is on disk whole or, after a crash, as a tail without a newline
        // that readers skip.
        let written = open.file.write_all(&line).is_ok();
        if written {
            segment.bytes += line_bytes;
            self.total += line_bytes;
        } else if segment.bytes == 0 {
            let seq = segment.seq;
            self.delete_segment(index, seq, None);
            self.remove_empty_sessions();
        }
        written
    }

    /// Removes the oldest segment and counts what it held. False when there is nothing left to remove.
    fn evict_oldest(&mut self, drops: &DropCounts) -> bool {
        let Some(seq) = self.sessions.first().and_then(|session| session.segments.first()).map(|segment| segment.seq) else {
            return false;
        };
        self.delete_segment(0, seq, Some(drops));
        self.remove_empty_sessions();
        true
    }

    /// Removes segments older than the age limit, counting what they held.
    pub(super) fn expire(&mut self, now: SystemTime, drops: &DropCounts) {
        let mut expired = Vec::new();
        for session in &self.sessions {
            for segment in &session.segments {
                let modified = std::fs::metadata(self.segment_path(&session.id, segment.seq)).and_then(|metadata| metadata.modified());
                if modified.is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > self.max_age)) {
                    expired.push((session.id.clone(), segment.seq));
                }
            }
        }
        for (id, seq) in expired {
            if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
                self.delete_segment(index, seq, Some(drops));
            }
        }
        self.remove_empty_sessions();
    }

    /// The oldest session's segments, as many as fit in `budget` bytes and at least one. Lines whose
    /// category `allowed` refuses are left out. The segments stay on disk until [`Spool::complete`].
    pub(super) fn next_batch(&mut self, budget: u64, allowed: &dyn Fn(u8) -> bool) -> Option<Batch> {
        loop {
            let session = self.sessions.first()?;
            let id = session.id.clone();
            let Some(header) = read_json::<Header>(&self.root.join(&id).join(HEADER_FILE)) else {
                // Without its header the session cannot be sent.
                while self.sessions.first().is_some_and(|session| session.id == id) && self.evict_oldest_silently() {}
                continue;
            };
            // Seal the segment being written, so that what is sent is exactly what is deleted afterwards.
            if self.open.as_ref().is_some_and(|open| open.session == id) {
                self.open = None;
            }
            let mut batch = Batch { header, segments: Vec::new(), items: Vec::new() };
            let mut used = 0;
            for segment in &self.sessions.first()?.segments {
                if !batch.segments.is_empty() && used + segment.bytes > budget {
                    break;
                }
                used += segment.bytes;
                batch.segments.push(segment.seq);
                let text = std::fs::read_to_string(self.segment_path(&id, segment.seq)).unwrap_or_default();
                batch.items.extend(complete_lines(&text).filter(|(category, _)| allowed(*category)).map(|(_, json)| json.to_owned()));
            }
            return Some(batch);
        }
    }

    fn evict_oldest_silently(&mut self) -> bool {
        let Some(seq) = self.sessions.first().and_then(|session| session.segments.first()).map(|segment| segment.seq) else {
            return false;
        };
        self.delete_segment(0, seq, None);
        self.remove_empty_sessions();
        true
    }

    /// Deletes a batch's segments once the server has answered for them.
    pub(super) fn complete(&mut self, batch: &Batch) {
        if let Some(index) = self.sessions.iter().position(|session| session.id == batch.header.session_id) {
            for seq in &batch.segments {
                self.delete_segment(index, *seq, None);
            }
            self.remove_empty_sessions();
        }
    }

    /// Deletes every stored line of one category. Not counted as a drop: the user asked for it.
    pub(super) fn purge(&mut self, category: u8) {
        self.open = None;
        for index in 0..self.sessions.len() {
            let Some(session) = self.sessions.get(index) else { continue };
            let id = session.id.clone();
            let segments: Vec<u64> = session.segments.iter().map(|segment| segment.seq).collect();
            for seq in segments {
                let path = self.segment_path(&id, seq);
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let mut kept = Vec::with_capacity(text.len());
                for line in text.split_inclusive('\n').filter(|line| line.ends_with('\n') && line.as_bytes().first() != Some(&category)) {
                    kept.extend_from_slice(line.as_bytes());
                }
                if kept.is_empty() {
                    self.delete_segment(index, seq, None);
                } else if kept.len() != text.len() && write_atomic(&path, &kept).is_ok() {
                    let segment = self.sessions.get_mut(index).and_then(|session| session.segments.iter_mut().find(|segment| segment.seq == seq));
                    if let Some(segment) = segment {
                        self.total = self.total.saturating_sub(segment.bytes) + kept.len() as u64;
                        segment.bytes = kept.len() as u64;
                    }
                }
            }
        }
        self.remove_empty_sessions();
    }

    /// Deletes everything, including the inspect mirror.
    pub(super) fn wipe(&mut self) {
        self.open = None;
        self.sessions.clear();
        self.total = 0;
        let _ = std::fs::remove_dir_all(&self.root);
        if let Some(outbox) = &mut self.outbox {
            outbox.wipe();
        }
    }

    fn segment_path(&self, session: &str, seq: u64) -> PathBuf {
        self.root.join(session).join(segment_name(seq))
    }

    /// Removes one segment from disk and from the index. With `drops`, counts its items as evicted.
    fn delete_segment(&mut self, index: usize, seq: u64, drops: Option<&DropCounts>) {
        let Some(session) = self.sessions.get_mut(index) else { return };
        let Some(position) = session.segments.iter().position(|segment| segment.seq == seq) else { return };
        let segment = session.segments.remove(position);
        let last = position == session.segments.len();
        let path = self.root.join(&session.id).join(segment_name(seq));
        if last && self.open.as_ref().is_some_and(|open| open.session == session.id) {
            self.open = None;
        }
        if let Some(drops) = drops {
            count_evicted(&std::fs::read_to_string(&path).unwrap_or_default(), drops);
        }
        let _ = std::fs::remove_file(&path);
        self.total = self.total.saturating_sub(segment.bytes);
    }

    fn remove_empty_sessions(&mut self) {
        let (root, mut freed) = (&self.root, 0);
        self.sessions.retain(|session| {
            if session.segments.is_empty() {
                let _ = std::fs::remove_dir_all(root.join(&session.id));
                freed += session.header_bytes;
            }
            !session.segments.is_empty()
        });
        self.total = self.total.saturating_sub(freed);
        if self.open.as_ref().is_some_and(|open| !self.sessions.iter().any(|session| session.id == open.session)) {
            self.open = None;
        }
    }
}

fn segment_name(seq: u64) -> String {
    format!("{seq:08}.{SEGMENT_EXTENSION}")
}

/// The whole lines of a segment as (category byte, item JSON). A tail without a newline is a write that
/// was cut short, and is skipped.
fn complete_lines(text: &str) -> impl Iterator<Item = (u8, &str)> {
    text.split_inclusive('\n').filter_map(|line| {
        let line = line.strip_suffix('\n')?;
        Some((*line.as_bytes().first()?, line.get(1..)?))
    })
}

/// Counts the items of a segment that is being thrown away. A `drops` item in it is not itself counted:
/// its counts go back into `drops` so they are reported with a later batch.
fn count_evicted(text: &str, drops: &DropCounts) {
    let mut evicted = 0;
    for (category, json) in complete_lines(text) {
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
    use tracer_proto::{Drops, ProcessKind, SCHEMA_VERSION};

    use super::super::files::testing::TempDir;
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn header(session: &str, created: UnixMs) -> Header {
        Header {
            schema: SCHEMA_VERSION,
            app: AppInfo { name: "host".to_owned(), version: "1.0.0".to_owned(), build: None, process: ProcessKind::Gui },
            install_id: "i".repeat(32),
            session_id: session.to_owned(),
            device: None,
            created,
        }
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

    #[test]
    fn appended_items_come_back_in_order_and_are_deleted_on_complete() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
        assert!(spool.is_empty());
        assert!(!dir.path().join(SPOOL_DIR).exists());
        for number in 0..10 {
            assert!(spool.append(&header("a", 1), b'u', &item(number), &drops));
        }
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!(batch.header, header("a", 1));
        assert_eq!(batch.items, (0..10).map(item).collect::<Vec<_>>());
        spool.complete(&batch);
        assert!(spool.is_empty());
        assert_eq!(spool.total_bytes(), 0);
        assert_eq!(disk_bytes(dir.path()), 0);
        assert!(drops.take().is_empty());
    }

    #[test]
    fn items_added_while_a_batch_is_out_survive_its_completion() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
        spool.append(&header("a", 1), b'u', &item(0), &drops);
        let batch = spool.next_batch(u64::MAX, &all).unwrap();
        spool.append(&header("a", 1), b'u', &item(1), &drops);
        spool.complete(&batch);
        let next = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!(next.items, vec![item(1)]);
    }

    #[test]
    fn reopening_finds_earlier_sessions_oldest_first() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        {
            let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
            spool.append(&header("newer", 20), b'u', &item(2), &drops);
            spool.append(&header("older", 10), b'u', &item(1), &drops);
        }
        std::fs::create_dir_all(dir.path().join(SPOOL_DIR).join("junk")).unwrap();
        std::fs::write(dir.path().join(SPOOL_DIR).join("junk").join("00000000.jsonl"), b"u{}\n").unwrap();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
        assert!(!dir.path().join(SPOOL_DIR).join("junk").exists());
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        let first = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!(first.header.session_id, "older");
        spool.complete(&first);
        // New items for an old session go to a new segment, never into one from an earlier run.
        spool.append(&header("newer", 20), b'u', &item(3), &drops);
        let second = spool.next_batch(u64::MAX, &all).unwrap();
        assert_eq!(second.items, vec![item(2), item(3)]);
        assert_eq!(second.segments, vec![0, 1]);
    }

    #[test]
    fn never_exceeds_the_cap_and_counts_evictions() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let cap = 6 * 1024;
        let mut spool = Spool::open(dir.path(), cap, DAY, None);
        let total = 400;
        for number in 0..total {
            assert!(spool.append(&header(if number < 200 { "a" } else { "b" }, number as u64 / 200), b'u', &item(number), &drops));
            assert!(spool.total_bytes() <= cap);
            assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        }
        let mut kept = Vec::new();
        while let Some(batch) = spool.next_batch(u64::MAX, &all) {
            kept.extend(batch.items.clone());
            spool.complete(&batch);
        }
        let evicted = drops.take().get(DROP_EVICTED).copied().unwrap_or(0) as usize;
        assert!(evicted > 0);
        assert_eq!(kept.len() + evicted, total);
        // The newest items are the ones kept.
        assert_eq!(kept.last(), Some(&item(total - 1)));
        assert_eq!(kept, (total - kept.len()..total).map(item).collect::<Vec<_>>());
    }

    #[test]
    fn an_item_larger_than_the_cap_is_refused() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 600, DAY, None);
        assert!(!spool.append(&header("a", 1), b'c', &"x".repeat(700), &drops));
        assert!(!spool.append(&header("a", 1), b'c', &"x".repeat(MAX_LINE_BYTES as usize), &drops));
        let counts = drops.take();
        assert_eq!(counts.get(DROP_EVICTED), Some(&1));
        assert_eq!(counts.get(DROP_TOO_LARGE), Some(&1));
        assert_eq!(disk_bytes(dir.path()), 0);
    }

    #[test]
    fn evicting_a_drops_item_keeps_its_counts() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 4096, DAY, None);
        let carried = Item { id: "d".repeat(32), ts: 1, body: ItemBody::Drops(Drops { reasons: [("queue_full".to_owned(), 7)].into() }) };
        spool.append(&header("a", 1), CAT_META, &serde_json::to_string(&carried).unwrap(), &drops);
        for number in 0..200 {
            spool.append(&header("a", 1), b'u', &item(number), &drops);
        }
        let counts = drops.take();
        assert_eq!(counts.get("queue_full"), Some(&7));
        let mut kept = 0;
        while let Some(batch) = spool.next_batch(u64::MAX, &all) {
            kept += batch.items.len() as u64;
            spool.complete(&batch);
        }
        assert_eq!(counts.get(DROP_EVICTED).copied().unwrap_or(0) + kept, 200);
    }

    #[test]
    fn batches_respect_the_budget_and_cover_everything_once() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 64 * 1024, DAY, None);
        for number in 0..300 {
            spool.append(&header("a", 1), b'u', &item(number), &drops);
        }
        let mut seen = Vec::new();
        let mut batches = 0;
        while let Some(batch) = spool.next_batch(10_000, &all) {
            assert!(batch.items.iter().map(|json| json.len() as u64 + 2).sum::<u64>() <= 10_000);
            seen.extend(batch.items.clone());
            spool.complete(&batch);
            batches += 1;
        }
        assert!(batches > 1);
        assert_eq!(seen, (0..300).map(item).collect::<Vec<_>>());
    }

    #[test]
    fn purge_removes_one_category_only() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
        for number in 0..30 {
            let category = b"ucp"[number % 3];
            spool.append(&header(if number < 15 { "a" } else { "b" }, number as u64 / 15), category, &item(number), &drops);
        }
        spool.append(&header("only_usage", 9), b'u', &item(99), &drops);
        spool.purge(b'u');
        assert_eq!(spool.total_bytes(), disk_bytes(dir.path()));
        assert!(!dir.path().join(SPOOL_DIR).join("only_usage").exists());
        let mut kept = Vec::new();
        while let Some(batch) = spool.next_batch(u64::MAX, &all) {
            kept.extend(batch.items.clone());
            spool.complete(&batch);
        }
        assert_eq!(kept, (0..30).filter(|number| number % 3 != 0).map(item).collect::<Vec<_>>());
        assert!(drops.take().is_empty());
    }

    #[test]
    fn a_batch_leaves_out_categories_that_are_not_allowed() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
        spool.append(&header("a", 1), b'u', &item(0), &drops);
        spool.append(&header("a", 1), b'c', &item(1), &drops);
        let batch = spool.next_batch(u64::MAX, &|category| category != b'u').unwrap();
        assert_eq!(batch.items, vec![item(1)]);
    }

    #[test]
    fn a_torn_last_line_is_skipped() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        {
            let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
            spool.append(&header("a", 1), b'u', &item(0), &drops);
        }
        let segment = dir.path().join(SPOOL_DIR).join("a").join(segment_name(0));
        let mut file = OpenOptions::new().append(true).open(segment).unwrap();
        file.write_all(br#"u{"id":"torn","ts"#).unwrap();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, None);
        assert_eq!(spool.next_batch(u64::MAX, &all).unwrap().items, vec![item(0)]);
    }

    #[test]
    fn old_segments_expire_and_are_counted() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 1 << 20, Duration::from_secs(60), None);
        for number in 0..5 {
            spool.append(&header("a", 1), b'u', &item(number), &drops);
        }
        spool.expire(SystemTime::now(), &drops);
        assert!(!spool.is_empty());
        spool.expire(SystemTime::now() + Duration::from_secs(3600), &drops);
        assert!(spool.is_empty());
        assert_eq!(disk_bytes(dir.path()), 0);
        assert_eq!(drops.take().get(DROP_EVICTED), Some(&5));
    }

    #[test]
    fn wipe_removes_everything() {
        let dir = TempDir::new("spool");
        let drops = DropCounts::default();
        let mut spool = Spool::open(dir.path(), 1 << 20, DAY, Some(Outbox::open(dir.path())));
        spool.append(&header("a", 1), b'u', &item(0), &drops);
        assert!(disk_bytes(dir.path()) > 0);
        spool.wipe();
        assert!(spool.is_empty());
        assert_eq!(disk_bytes(dir.path()), 0);
        assert!(spool.append(&header("a", 1), b'u', &item(1), &drops));
        assert_eq!(spool.next_batch(u64::MAX, &all).unwrap().items, vec![item(1)]);
    }
}
