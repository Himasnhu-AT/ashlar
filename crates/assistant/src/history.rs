//! Saved chats: each conversation in its own folder under the app's history folder, listed newest
//! first, searchable, renamed, deleted, and reopened with its messages and tool calls intact.
//!
//! ```text
//! <root>/<id>/chat.json    the chat: format version, meta, messages, usage, the app's extra data
//! <root>/<id>/meta.json    the meta alone (title, times, preview), read by `list`
//! <root>/<id>/images/…     pictures tool results returned, one file each, named by content
//! ```
//!
//! Every file is replaced atomically, so a crash mid-save leaves the previous version; a turn
//! interrupted by a crash is closed on [`HistoryStore::load`] (its open tool calls get "not run"
//! results). Files stay small: tool results longer than [`HistoryLimits::max_result_bytes`] keep
//! their start and end, pictures go in their own files (the JSON names them), and past
//! [`HistoryLimits::max_image_bytes`] a chat's oldest pictures are left out. A chat written by a
//! newer format is refused, not misread.
//!
//! Saving runs off the UI thread through [`HistorySaver`], which keeps only the newest
//! version of each chat waiting.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{Block, Conversation, Image, Message, Role, Usage};

/// The format written. Files with a higher version are refused.
pub const VERSION: u32 = 1;
pub const CHAT_FILE: &str = "chat.json";
pub const META_FILE: &str = "meta.json";
pub const IMAGES_DIR: &str = "images";
/// The title of a chat before its first message.
pub const NEW_TITLE: &str = "New chat";
/// What a tool call open when the app stopped is answered with on load.
pub const INTERRUPTED: &str = "Not run: the app closed before this call finished.";
/// How a picture kept in its own file is named in `chat.json` (`data`: `file:images/<name>`).
const FILE_REF: &str = "file:";

/// What went wrong with the history, in words for the person.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    #[error("There is no saved chat `{0}`.")]
    NotFound(String),
    #[error("`{0}` is not a chat id.")]
    BadId(String),
    #[error("This chat was saved by a newer version of the app (format {0}). Update the app to open it.")]
    Newer(u32),
    #[error("The saved chat could not be read: {0}")]
    Corrupt(String),
    #[error("The chat history could not be written: {0}")]
    Io(String),
}

impl From<std::io::Error> for HistoryError {
    fn from(e: std::io::Error) -> Self {
        HistoryError::Io(e.to_string())
    }
}

/// Where a chat's title came from (a name the person gave is never replaced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource {
    /// [`NEW_TITLE`] or the first message's first line.
    #[default]
    Auto,
    /// A few words the model wrote.
    Model,
    /// The person renamed it.
    Person,
}

/// A chat's entry in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMeta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub title_source: TitleSource,
    /// Milliseconds since 1970.
    pub created_ms: u64,
    pub updated_ms: u64,
    /// The person's messages.
    #[serde(default)]
    pub messages: u32,
    /// The model of the last turn.
    #[serde(default)]
    pub model: String,
    /// The start of the first message.
    #[serde(default)]
    pub preview: String,
    /// The chat's words for [`HistoryStore::search`] (the start of each message).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub search_text: String,
}

/// A saved chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub meta: ChatMeta,
    pub conversation: Conversation,
    /// The whole chat's tokens.
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// The app's own data (drafts, view state), kept as is.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub app: Value,
}

impl Default for Chat {
    fn default() -> Self {
        Self::new()
    }
}

impl Chat {
    /// An empty chat with a new id.
    pub fn new() -> Self {
        let now = now_ms();
        Self {
            meta: ChatMeta {
                id: new_id(),
                title: NEW_TITLE.into(),
                title_source: TitleSource::Auto,
                created_ms: now,
                updated_ms: now,
                messages: 0,
                model: String::new(),
                preview: String::new(),
                search_text: String::new(),
            },
            conversation: Conversation::new(),
            usage: Usage::default(),
            cost_usd: Some(0.0),
            app: Value::Null,
        }
    }

    pub fn id(&self) -> &str {
        &self.meta.id
    }

    /// No message yet (not worth saving).
    pub fn is_empty(&self) -> bool {
        self.conversation.is_empty()
    }

    /// Set the title unless the person named the chat.
    pub fn set_title(&mut self, title: &str, source: TitleSource) -> bool {
        let t = title.trim();
        if t.is_empty() || (self.meta.title_source == TitleSource::Person && source != TitleSource::Person) {
            return false;
        }
        self.meta.title = t.chars().take(120).collect();
        self.meta.title_source = source;
        true
    }

    /// Bring the meta up to date with the conversation (counts, preview, search words; an
    /// automatic title from the first message).
    pub fn refresh_meta(&mut self) {
        let person: Vec<&Message> = self.conversation.messages().iter().filter(|m| m.is_person()).collect();
        self.meta.messages = u32::try_from(person.len()).unwrap_or(u32::MAX);
        let first = person.first().map(|m| m.text()).unwrap_or_default();
        self.meta.preview = first.chars().take(200).collect();
        if self.meta.title_source == TitleSource::Auto && !first.trim().is_empty() {
            self.meta.title = crate::compact::fallback_title(&first);
        }
        let mut words = String::new();
        for m in self.conversation.messages() {
            let t = m.text();
            if t.trim().is_empty() {
                continue;
            }
            words.extend(t.chars().take(400));
            words.push('\n');
            if words.len() > 8_000 {
                break;
            }
        }
        self.meta.search_text = words;
    }
}

/// How small the files are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    /// A tool result longer than this (bytes) keeps its first 80 % and last 20 %.
    pub max_result_bytes: usize,
    /// Pictures a chat keeps, newest first, in bytes; older ones are left out.
    pub max_image_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self { max_result_bytes: 24_000, max_image_bytes: 24 * 1024 * 1024 }
    }
}

/// The history folder.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    root: PathBuf,
    limits: HistoryLimits,
}

/// Ids are letters, digits, `-` and `_` (never a path).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Serialize, Deserialize)]
struct FileHeader {
    version: u32,
}

#[derive(Serialize, Deserialize)]
struct ChatFile {
    version: u32,
    #[serde(flatten)]
    chat: Chat,
}

impl HistoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), limits: HistoryLimits::default() }
    }

    pub fn with_limits(mut self, limits: HistoryLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, id: &str) -> Result<PathBuf, HistoryError> {
        if !valid_id(id) {
            return Err(HistoryError::BadId(id.chars().take(80).collect()));
        }
        Ok(self.root.join(id))
    }

    pub fn exists(&self, id: &str) -> bool {
        self.dir(id).is_ok_and(|d| d.join(CHAT_FILE).is_file())
    }

    /// Every saved chat, the most recently updated first. Unreadable entries are skipped.
    pub fn list(&self) -> Vec<ChatMeta> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut out: Vec<ChatMeta> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let id = e.file_name().to_str()?.to_string();
                if !valid_id(&id) {
                    return None;
                }
                self.read_meta(&id)
            })
            .collect();
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then_with(|| b.id.cmp(&a.id)));
        out
    }

    fn read_meta(&self, id: &str) -> Option<ChatMeta> {
        let dir = self.dir(id).ok()?;
        let meta = std::fs::read(dir.join(META_FILE)).ok().and_then(|b| serde_json::from_slice::<ChatMeta>(&b).ok()).filter(|m| m.id == id);
        match meta {
            Some(m) => Some(m),
            // no or a broken meta file: the chat itself
            None => self.load(id).ok().map(|c| c.meta),
        }
    }

    /// The chats whose title or words contain every word of `query` (any case), newest first.
    pub fn search(&self, query: &str) -> Vec<ChatMeta> {
        filter_metas(self.list(), query)
    }

    /// Read a chat. Its pictures are read back from their files (a missing one is left out and
    /// named), and a turn the app did not finish is closed ([`INTERRUPTED`]).
    pub fn load(&self, id: &str) -> Result<Chat, HistoryError> {
        let dir = self.dir(id)?;
        let bytes = match std::fs::read(dir.join(CHAT_FILE)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(HistoryError::NotFound(id.into())),
            Err(e) => return Err(e.into()),
        };
        let header: FileHeader = serde_json::from_slice(&bytes).map_err(|e| HistoryError::Corrupt(e.to_string()))?;
        if header.version > VERSION {
            return Err(HistoryError::Newer(header.version));
        }
        let file: ChatFile = serde_json::from_slice(&bytes).map_err(|e| HistoryError::Corrupt(e.to_string()))?;
        let mut chat = file.chat;
        chat.meta.id = id.to_string();
        let mut messages = std::mem::take(&mut chat.conversation).into_messages();
        for m in &mut messages {
            for b in &mut m.blocks {
                let Block::ToolResult(r) = b else { continue };
                let mut missing = 0usize;
                r.images.retain_mut(|img| {
                    let Some(name) = img.data.strip_prefix(FILE_REF) else { return true };
                    match read_image(&dir, name) {
                        Some(data) => {
                            img.data = data;
                            true
                        }
                        None => {
                            missing += 1;
                            false
                        }
                    }
                });
                if missing > 0 {
                    r.content.push_str(&format!("\n[{missing} picture(s) from this result are no longer in the history]"));
                }
            }
        }
        chat.conversation = Conversation::from_messages(messages);
        chat.conversation.close_open_calls(INTERRUPTED);
        Ok(chat)
    }

    /// Write `chat` (its meta is brought up to date first: counts, preview, the time). An empty
    /// chat is not written.
    pub fn save(&self, chat: &mut Chat) -> Result<ChatMeta, HistoryError> {
        let dir = self.dir(&chat.meta.id)?;
        chat.refresh_meta();
        chat.meta.updated_ms = now_ms().max(chat.meta.created_ms);
        if chat.is_empty() {
            return Ok(chat.meta.clone());
        }
        std::fs::create_dir_all(&dir)?;
        let stored = self.stored(chat, &dir)?;
        let json = serde_json::to_vec(&ChatFile { version: VERSION, chat: stored }).map_err(|e| HistoryError::Io(e.to_string()))?;
        ashlar_fs::atomic_write(&dir.join(CHAT_FILE), &json)?;
        let meta = serde_json::to_vec_pretty(&chat.meta).map_err(|e| HistoryError::Io(e.to_string()))?;
        ashlar_fs::atomic_write(&dir.join(META_FILE), &meta)?;
        self.prune_images(&dir, &json);
        Ok(chat.meta.clone())
    }

    /// The chat as written: long results cut, pictures in their files (newest kept within the
    /// budget).
    fn stored(&self, chat: &Chat, dir: &Path) -> Result<Chat, HistoryError> {
        let mut messages = chat.conversation.messages().to_vec();
        let mut budget = self.limits.max_image_bytes;
        // newest first for the picture budget
        for m in messages.iter_mut().rev() {
            if m.role != Role::User {
                continue;
            }
            for b in m.blocks.iter_mut().rev() {
                let Block::ToolResult(r) = b else { continue };
                r.content = trim_result(&r.content, self.limits.max_result_bytes);
                let mut dropped = 0usize;
                let mut kept = Vec::with_capacity(r.images.len());
                for img in r.images.drain(..) {
                    // already a file reference (a chat saved again unchanged): keep it
                    if img.data.starts_with(FILE_REF) {
                        kept.push(img);
                        continue;
                    }
                    let size = img.data.len() / 4 * 3;
                    let Some(rest) = budget.checked_sub(size) else {
                        dropped += 1;
                        continue;
                    };
                    match write_image(dir, &img)? {
                        Some(name) => {
                            budget = rest;
                            kept.push(Image { media_type: img.media_type, data: format!("{FILE_REF}{name}") });
                        }
                        None => dropped += 1,
                    }
                }
                r.images = kept;
                if dropped > 0 {
                    r.content.push_str(&format!("\n[{dropped} picture(s) from this result were left out of the history]"));
                }
            }
        }
        Ok(Chat {
            meta: chat.meta.clone(),
            conversation: Conversation::from_messages(messages),
            usage: chat.usage,
            cost_usd: chat.cost_usd,
            app: chat.app.clone(),
        })
    }

    /// Remove picture files `chat.json` no longer names.
    fn prune_images(&self, dir: &Path, json: &[u8]) {
        let Ok(entries) = std::fs::read_dir(dir.join(IMAGES_DIR)) else { return };
        let text = String::from_utf8_lossy(json);
        for e in entries.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().to_string();
            if !text.contains(&format!("{FILE_REF}{IMAGES_DIR}/{name}")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Give a chat a name of the person's (it is never replaced by an automatic one).
    pub fn rename(&self, id: &str, title: &str) -> Result<ChatMeta, HistoryError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(HistoryError::Io("A chat's name can't be empty.".into()));
        }
        let dir = self.dir(id)?;
        let bytes = std::fs::read(dir.join(CHAT_FILE)).map_err(|_| HistoryError::NotFound(id.into()))?;
        let mut v: Value = serde_json::from_slice(&bytes).map_err(|e| HistoryError::Corrupt(e.to_string()))?;
        if v.get("version").and_then(Value::as_u64).unwrap_or(0) > u64::from(VERSION) {
            return Err(HistoryError::Newer(v.get("version").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(u32::MAX)));
        }
        let title: String = title.chars().take(120).collect();
        // only the meta changes: the messages (and their picture references) are written back as read
        let meta = v.get_mut("meta").and_then(Value::as_object_mut).ok_or_else(|| HistoryError::Corrupt("no meta".into()))?;
        meta.insert("title".into(), Value::String(title));
        meta.insert("title_source".into(), serde_json::json!(TitleSource::Person));
        let parsed: ChatMeta = serde_json::from_value(Value::Object(meta.clone())).map_err(|e| HistoryError::Corrupt(e.to_string()))?;
        let json = serde_json::to_vec(&v).map_err(|e| HistoryError::Io(e.to_string()))?;
        ashlar_fs::atomic_write(&dir.join(CHAT_FILE), &json)?;
        let m = serde_json::to_vec_pretty(&parsed).map_err(|e| HistoryError::Io(e.to_string()))?;
        ashlar_fs::atomic_write(&dir.join(META_FILE), &m)?;
        Ok(parsed)
    }

    /// Remove a chat and its pictures.
    pub fn delete(&self, id: &str) -> Result<(), HistoryError> {
        let dir = self.dir(id)?;
        if !dir.join(CHAT_FILE).exists() && !dir.join(META_FILE).exists() {
            return Err(HistoryError::NotFound(id.into()));
        }
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}

/// The metas whose title, preview or words contain every word of `query` (any case).
pub fn filter_metas(metas: Vec<ChatMeta>, query: &str) -> Vec<ChatMeta> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return metas;
    }
    metas
        .into_iter()
        .filter(|m| {
            let hay = format!("{}\n{}\n{}", m.title, m.preview, m.search_text).to_lowercase();
            words.iter().all(|w| hay.contains(w))
        })
        .collect()
}

fn trim_result(s: &str, max: usize) -> String {
    if s.len() <= max || max < 64 {
        return s.to_string();
    }
    let mut head = max / 5 * 4;
    while !s.is_char_boundary(head) {
        head = head.saturating_sub(1);
    }
    let mut tail = s.len().saturating_sub(max / 5);
    while !s.is_char_boundary(tail) {
        tail = tail.saturating_add(1);
    }
    format!(
        "{}\n… ({} bytes left out of the history) …\n{}",
        s.get(..head).unwrap_or_default(),
        tail.saturating_sub(head),
        s.get(tail..).unwrap_or_default()
    )
}

fn extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

/// FNV-1a: names a picture by its content (the same frame twice is one file).
fn content_hash(data: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Write a picture's file once; `None` when its data isn't base64.
fn write_image(dir: &Path, img: &Image) -> Result<Option<String>, HistoryError> {
    let Ok(bytes) = B64.decode(img.data.as_bytes()) else { return Ok(None) };
    let name = format!("{IMAGES_DIR}/{:016x}.{}", content_hash(&img.data), extension(&img.media_type));
    let path = dir.join(&name);
    if !path.exists() {
        std::fs::create_dir_all(dir.join(IMAGES_DIR))?;
        ashlar_fs::atomic_write(&path, &bytes)?;
    }
    Ok(Some(name))
}

fn read_image(dir: &Path, name: &str) -> Option<String> {
    // only `images/<file>`: a name from the file is never a path elsewhere
    let file = name.strip_prefix(IMAGES_DIR)?.strip_prefix('/')?;
    if file.is_empty() || !file.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') || file.starts_with('.') {
        return None;
    }
    std::fs::read(dir.join(IMAGES_DIR).join(file)).ok().map(|b| B64.encode(b))
}

/// Milliseconds since 1970 (0 where the platform has no clock).
pub fn now_ms() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// A new chat id: the time in hex and a counter, so ids sort by creation.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    // no process ids on wasm32 (asking panics there)
    #[cfg(not(target_arch = "wasm32"))]
    let pid = std::process::id();
    #[cfg(target_arch = "wasm32")]
    let pid = 0u32;
    let salt = pid.wrapping_mul(2_654_435_761) ^ n.wrapping_mul(40_503);
    format!("{:011x}-{:04x}", now_ms(), salt & 0xffff)
}

/// Saves chats on a background thread. [`Self::save`] queues a copy; when several copies of
/// one chat wait, only the newest is written. Results come back through [`Self::poll`].
pub struct HistorySaver {
    tx: Option<Sender<SaveJob>>,
    done: Receiver<Result<ChatMeta, HistoryError>>,
    worker: Option<std::thread::JoinHandle<()>>,
    store: HistoryStore,
}

enum SaveJob {
    Save(Box<Chat>),
    Flush(Sender<()>),
}

impl HistorySaver {
    /// Start the saver (without a thread, saves happen on the caller's thread).
    pub fn new(store: HistoryStore) -> Self {
        let (tx, rx) = channel::<SaveJob>();
        let (dtx, drx) = channel();
        let s = store.clone();
        let worker = std::thread::Builder::new()
            .name("assistant-history".into())
            .spawn(move || {
                let mut pending: Vec<Box<Chat>> = Vec::new();
                let mut flushes: Vec<Sender<()>> = Vec::new();
                while let Ok(job) = rx.recv() {
                    let mut take = |job| match job {
                        SaveJob::Save(c) => match pending.iter_mut().find(|p: &&mut Box<Chat>| p.meta.id == c.meta.id) {
                            Some(p) => *p = c,
                            None => pending.push(c),
                        },
                        SaveJob::Flush(f) => flushes.push(f),
                    };
                    take(job);
                    // gather what else is waiting, then write each chat once
                    while let Ok(more) = rx.try_recv() {
                        take(more);
                    }
                    for mut c in pending.drain(..) {
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.save(&mut c)))
                            .unwrap_or_else(|_| Err(HistoryError::Io("saving the chat failed unexpectedly".into())));
                        if let Err(e) = &r {
                            log::warn!(target: "assistant", "saving chat {}: {e}", c.meta.id);
                        }
                        let _ = dtx.send(r);
                    }
                    for f in flushes.drain(..) {
                        let _ = f.send(());
                    }
                }
            })
            .ok();
        let tx = worker.as_ref().map(|_| tx);
        Self { tx, done: drx, worker, store }
    }

    pub fn store(&self) -> &HistoryStore {
        &self.store
    }

    /// Queue `chat` to be written (an empty chat is skipped).
    pub fn save(&self, chat: &Chat) {
        if chat.is_empty() {
            return;
        }
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(SaveJob::Save(Box::new(chat.clone())));
            }
            None => {
                let mut c = chat.clone();
                if let Err(e) = self.store.save(&mut c) {
                    log::warn!(target: "assistant", "saving chat {}: {e}", c.meta.id);
                }
            }
        }
    }

    /// What the writes since the last poll did.
    pub fn poll(&self) -> Vec<Result<ChatMeta, HistoryError>> {
        self.done.try_iter().collect()
    }

    /// Wait up to `timeout` for every queued save to be written (before quitting, in tests).
    pub fn flush(&self, timeout: Duration) -> bool {
        let Some(tx) = &self.tx else { return true };
        let (ftx, frx) = channel();
        if tx.send(SaveJob::Flush(ftx)).is_err() {
            return false;
        }
        frx.recv_timeout(timeout).is_ok()
    }
}

impl Drop for HistorySaver {
    fn drop(&mut self) {
        // close the queue; the worker writes what is waiting and ends
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ToolCall, ToolResult};
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ashlar-history-{name}-{}-{}", std::process::id(), new_id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn png(seed: u8) -> Image {
        Image { media_type: "image/png".into(), data: B64.encode(vec![seed; 3000]) }
    }

    fn chat_with_tools() -> Chat {
        let mut c = Chat::new();
        c.conversation.push(Message::user_with_mentions("make @Title red please", &[crate::mention::Mention::new("layer", "#2", "Title")]));
        let call = ToolCall { id: "a".into(), name: "render_frame".into(), input: json!({"t": 1}), invalid_input: None };
        c.conversation.push(Message { role: Role::Assistant, blocks: vec![Block::ToolCall(call)], native: None });
        c.conversation.push(Message::tool_results(vec![ToolResult {
            call_id: "a".into(),
            content: "x".repeat(50_000),
            is_error: false,
            images: vec![png(1), png(1)],
        }]));
        c.conversation.push(Message { role: Role::Assistant, blocks: vec![Block::Text { text: "Done, it's red.".into() }], native: None });
        c
    }

    #[test]
    fn a_chat_round_trips_with_its_tools_and_pictures_in_files() {
        let root = temp("roundtrip");
        let store = HistoryStore::new(&root);
        let mut c = chat_with_tools();
        let meta = store.save(&mut c).unwrap();
        assert_eq!(meta.title, "make @Title red please");
        assert_eq!(meta.messages, 1);
        let dir = root.join(c.id());
        let json = std::fs::read_to_string(dir.join(CHAT_FILE)).unwrap();
        assert!(json.len() < 30_000, "results trimmed, pictures not inline: {}", json.len());
        assert_eq!(std::fs::read_dir(dir.join(IMAGES_DIR)).unwrap().count(), 1, "the same picture twice is one file");
        let back = store.load(c.id()).unwrap();
        assert_eq!(back.conversation.len(), 4);
        let r = back.conversation.messages()[2].tool_results_iter().next().unwrap().clone();
        assert_eq!(r.images, vec![png(1), png(1)]);
        assert!(r.content.contains("bytes left out of the history"));
        assert_eq!(back.conversation.messages()[0].mentions().count(), 1);
        // saving the reopened chat again keeps its pictures
        let mut again = back.clone();
        store.save(&mut again).unwrap();
        assert_eq!(store.load(c.id()).unwrap().conversation.messages()[2].tool_results_iter().next().unwrap().images.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_search_rename_delete() {
        let root = temp("list");
        let store = HistoryStore::new(&root);
        let mut a = Chat::new();
        a.conversation.push_user_text("animate the logo");
        store.save(&mut a).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let mut b = Chat::new();
        b.conversation.push_user_text("colour grade the sky");
        store.save(&mut b).unwrap();
        let mut empty = Chat::new();
        store.save(&mut empty).unwrap();
        let ids: Vec<String> = store.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![b.id().to_string(), a.id().to_string()], "newest first, empty chats not saved");
        assert_eq!(store.search("LOGO").len(), 1);
        assert_eq!(store.search("the").len(), 2);
        assert_eq!(store.search("logo sky").len(), 0, "every word");
        let m = store.rename(a.id(), "Logo sting").unwrap();
        assert_eq!((m.title.as_str(), m.title_source), ("Logo sting", TitleSource::Person));
        let mut reopened = store.load(a.id()).unwrap();
        assert!(!reopened.set_title("model words", TitleSource::Model), "the person's name stays");
        reopened.conversation.push_user_text("more");
        store.save(&mut reopened).unwrap();
        assert_eq!(store.load(a.id()).unwrap().meta.title, "Logo sting");
        store.delete(a.id()).unwrap();
        assert!(matches!(store.load(a.id()), Err(HistoryError::NotFound(_))));
        assert!(matches!(store.delete(a.id()), Err(HistoryError::NotFound(_))));
        assert!(matches!(store.load("../etc"), Err(HistoryError::BadId(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_interrupted_turn_is_closed_and_newer_files_are_refused() {
        let root = temp("crash");
        let store = HistoryStore::new(&root);
        let mut c = Chat::new();
        c.conversation.push_user_text("go");
        let call = ToolCall { id: "x".into(), name: "t".into(), input: json!({}), invalid_input: None };
        c.conversation.push(Message { role: Role::Assistant, blocks: vec![Block::ToolCall(call)], native: None });
        store.save(&mut c).unwrap();
        let back = store.load(c.id()).unwrap();
        assert!(back.conversation.unanswered_calls().is_empty());
        assert_eq!(back.conversation.last().and_then(|m| m.tool_results_iter().next()).map(|r| r.content.as_str()), Some(INTERRUPTED));
        // a newer format
        let path = root.join(c.id()).join(CHAT_FILE);
        let text = std::fs::read_to_string(&path).unwrap().replacen("\"version\":1", "\"version\":9", 1);
        std::fs::write(&path, text).unwrap();
        assert_eq!(store.load(c.id()), Err(HistoryError::Newer(9)));
        // a torn file
        std::fs::write(&path, "{\"version\":1,\"meta\":").unwrap();
        assert!(matches!(store.load(c.id()), Err(HistoryError::Corrupt(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_picture_budget_keeps_the_newest() {
        let root = temp("budget");
        let store = HistoryStore::new(&root).with_limits(HistoryLimits { max_result_bytes: 1000, max_image_bytes: 4000 });
        let mut c = Chat::new();
        c.conversation.push_user_text("go");
        for (i, seed) in [1u8, 2].iter().enumerate() {
            let id = format!("c{i}");
            let call = ToolCall { id: id.clone(), name: "t".into(), input: json!({}), invalid_input: None };
            c.conversation.push(Message { role: Role::Assistant, blocks: vec![Block::ToolCall(call)], native: None });
            c.conversation.push(Message::tool_results(vec![ToolResult {
                call_id: id,
                content: "ok".into(),
                is_error: false,
                images: vec![png(*seed)],
            }]));
        }
        store.save(&mut c).unwrap();
        let back = store.load(c.id()).unwrap();
        let results: Vec<ToolResult> = back.conversation.messages().iter().flat_map(|m| m.tool_results_iter().cloned()).collect();
        assert!(results[0].images.is_empty() && results[0].content.contains("left out of the history"));
        assert_eq!(results[1].images, vec![png(2)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_saver_writes_the_newest_copy() {
        let root = temp("saver");
        let saver = HistorySaver::new(HistoryStore::new(&root));
        let mut c = Chat::new();
        for i in 0..20 {
            c.conversation.push_user_text(format!("message {i}"));
            saver.save(&c);
        }
        assert!(saver.flush(Duration::from_secs(10)));
        let ok: Vec<ChatMeta> = saver.poll().into_iter().filter_map(Result::ok).collect();
        assert!(!ok.is_empty() && ok.len() <= 20);
        assert_eq!(saver.store().load(c.id()).unwrap().conversation.len(), 20);
        drop(saver);
        let _ = std::fs::remove_dir_all(&root);
    }
}
