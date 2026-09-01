//! The session: the state every command runs on (document, file, view state), and the host glue
//! that makes each top-level command exactly one undo step.
//!
//! While a top-level command runs, its edits go to a working copy of the document. When the
//! command succeeds the copy is committed to the [`Store`] as one step named after the command;
//! when it fails or panics the copy is dropped, and the document, history, view state and
//! requested effects are as they were.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ashlar_command::{CommandError, CommandSpec, Host, Journal};
use ashlar_store::{Format, FormatError, Revision, Store};
use ashlar_ui::toasts::ToastLevel;
use ashlar_ui::tokens::ThemeMode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::telemetry::Telemetry;

/// What a document type needs: cheap to clone (keep large parts in `Arc`s), a default (the new,
/// empty document) and serde (the file format).
pub trait Document: Clone + Default + Serialize + DeserializeOwned + Send + Sync + 'static {}
impl<T: Clone + Default + Serialize + DeserializeOwned + Send + Sync + 'static> Document for T {}

/// Something a command asks the window to do. A headless session (CLI, MCP) has no window and
/// drops them (toasts go to the log).
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Toast {
        level: ToastLevel,
        text: String,
    },
    OpenPalette,
    OpenKeymap,
    Theme(ThemeMode),
    /// Ask the user which file to open (then run `file.open` with it).
    PickOpen,
    /// Ask the user where to save (then run `file.save_as` with it).
    PickSaveAs,
    /// The document has unsaved changes: ask "save, don't save, cancel" before running
    /// `command` again with `discard: true`.
    ConfirmDiscard {
        command: String,
        params: Value,
    },
    /// Close the app (asking first when there are unsaved changes, unless `discard`).
    Quit {
        discard: bool,
    },
}

pub(crate) fn now() -> Option<std::time::Instant> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(std::time::Instant::now())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

struct Work<D> {
    doc: Arc<D>,
    /// The store was changed directly during the command (undo, open…): don't commit.
    replaced: bool,
    /// Merge key shared by every edit of the command (`Some(None)`: edits without one).
    merge: Option<Option<String>>,
    effects: usize,
    view: Map<String, Value>,
    view_gen: u64,
    started: Option<std::time::Instant>,
}

/// The state commands run on: the document [`Store`], the file it came from, view state and the
/// effects commands ask of the window. Commands are `fn(&mut Session<D>, &Value)`.
pub struct Session<D: Document> {
    store: Store<D>,
    journal: Journal,
    format: Arc<Format<D>>,
    path: Option<PathBuf>,
    /// Schema version of the opened file when it was migrated on load.
    migrated_from: Option<u64>,
    work: Option<Work<D>>,
    view: Map<String, Value>,
    view_gen: u64,
    file_gen: u64,
    effects: Vec<Effect>,
    windowed: bool,
    pub(crate) telemetry: Telemetry,
}

impl<D: Document> Session<D> {
    /// A session with a new, empty document.
    pub fn new(format: Arc<Format<D>>) -> Session<D> {
        Session {
            store: Store::new(D::default()),
            journal: Journal::new(),
            format,
            path: None,
            migrated_from: None,
            work: None,
            view: Map::new(),
            view_gen: 0,
            file_gen: 0,
            effects: Vec::new(),
            windowed: false,
            telemetry: Telemetry::default(),
        }
    }

    pub(crate) fn set_windowed(&mut self, yes: bool) {
        self.windowed = yes;
    }

    /// Whether a window can show dialogs (false in CLI / MCP sessions).
    pub fn is_windowed(&self) -> bool {
        self.windowed
    }

    // ------------------------------------------------------------------ the document

    /// The current document (inside a command: its working copy).
    pub fn doc(&self) -> &D {
        match &self.work {
            Some(w) => &w.doc,
            None => self.store.doc(),
        }
    }

    /// The current document as an `Arc` (for a job or another thread).
    pub fn snapshot(&self) -> Arc<D> {
        match &self.work {
            Some(w) => Arc::clone(&w.doc),
            None => self.store.snapshot(),
        }
    }

    /// The store (history, revision, dirty state). Changing it directly from inside a command
    /// drops the command's own edits; use [`Session::edit`] there.
    pub fn store(&self) -> &Store<D> {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store<D> {
        if let Some(w) = &mut self.work {
            w.replaced = true;
        }
        &mut self.store
    }

    pub fn revision(&self) -> Revision {
        self.store.revision()
    }

    pub fn is_dirty(&self) -> bool {
        self.store.is_dirty()
    }

    pub fn format(&self) -> &Format<D> {
        &self.format
    }

    /// Change the document. Inside a command the change joins the command's single undo step;
    /// outside (a view, a job's apply) it is a step of its own called `label`.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut D) -> R) -> R {
        self.edit_with(label, None, f)
    }

    /// [`Session::edit`] that folds into the previous step with the same label and merge key
    /// (one undo step for a whole drag).
    pub fn edit_merged<R>(&mut self, label: &str, merge_key: &str, f: impl FnOnce(&mut D) -> R) -> R {
        self.edit_with(label, Some(merge_key), f)
    }

    /// A change that may fail: on error nothing changes.
    pub fn try_edit<R, E>(&mut self, label: &str, f: impl FnOnce(&mut D) -> Result<R, E>) -> Result<R, E> {
        match &mut self.work {
            Some(w) => {
                let mut copy = Arc::clone(&w.doc);
                let r = f(Arc::make_mut(&mut copy))?;
                w.doc = copy;
                note_merge(w, None);
                Ok(r)
            }
            None => self.store.try_edit(label, f),
        }
    }

    fn edit_with<R>(&mut self, label: &str, merge_key: Option<&str>, f: impl FnOnce(&mut D) -> R) -> R {
        match &mut self.work {
            Some(w) => {
                let r = f(Arc::make_mut(&mut w.doc));
                note_merge(w, merge_key);
                r
            }
            None => match merge_key {
                Some(k) => self.store.edit_merged(label, k, f),
                None => self.store.edit(label, f),
            },
        }
    }

    pub fn can_undo(&self) -> bool {
        self.store.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.store.can_redo()
    }

    /// Undo the last step; returns its label.
    pub fn undo(&mut self) -> Option<String> {
        let r = self.store_mut().undo();
        self.sync_work();
        r
    }

    /// Redo the next step; returns its label.
    pub fn redo(&mut self) -> Option<String> {
        let r = self.store_mut().redo();
        self.sync_work();
        r
    }

    /// After a direct store change inside a command, the working copy follows the store.
    fn sync_work(&mut self) {
        if let Some(w) = &mut self.work {
            w.doc = self.store.snapshot();
        }
    }

    // ------------------------------------------------------------------ files

    /// The file the document was opened from or saved to.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The document's display name: the file name, or "Untitled".
    pub fn display_name(&self) -> String {
        self.path.as_deref().and_then(Path::file_name).map_or_else(|| "Untitled".to_string(), |n| n.to_string_lossy().into_owned())
    }

    /// The schema version the open file had before it was upgraded on load, if it was.
    pub fn migrated_from(&self) -> Option<u64> {
        self.migrated_from
    }

    /// Changes when the file (path) changes; with the revision it keys caches of file state.
    pub fn file_generation(&self) -> u64 {
        self.file_gen
    }

    /// Start a new, empty document.
    pub fn new_document(&mut self) {
        self.replace(D::default(), None, None);
    }

    /// Open `path` (migrating older files). On error the current document stays.
    pub fn open(&mut self, path: &Path) -> Result<(), FormatError> {
        let loaded = self.format.load(path)?;
        let current = self.format.schema_version();
        let migrated = loaded.migrated(current).then_some(loaded.envelope.schema_version);
        self.replace(loaded.doc, Some(path.to_path_buf()), migrated);
        Ok(())
    }

    /// Replace the document with `doc` as if opened from `path`: clears the history, clean.
    pub fn replace(&mut self, doc: D, path: Option<PathBuf>, migrated_from: Option<u64>) {
        self.store_mut().replace(doc, ());
        self.sync_work();
        self.path = path;
        self.migrated_from = migrated_from;
        self.file_gen = self.file_gen.wrapping_add(1);
    }

    /// Replace the document with `doc` as an undoable, unsaved change (recovered work, an
    /// import).
    pub fn replace_as_edit(&mut self, label: &str, doc: D) {
        let doc = Arc::new(doc);
        match &mut self.work {
            Some(w) => {
                w.doc = doc;
                note_merge(w, None);
            }
            None => self.store.commit(label, None, doc),
        }
    }

    /// Save to the current file. Errors when the document has never been saved.
    pub fn save(&mut self) -> Result<PathBuf, FormatError> {
        let Some(path) = self.path.clone() else {
            return Err(FormatError::Io(std::io::Error::other("the document has no file yet; use Save As")));
        };
        self.save_as(&path)?;
        Ok(path)
    }

    /// Save to `path` and make it the document's file.
    pub fn save_as(&mut self, path: &Path) -> Result<(), FormatError> {
        let snap = self.store.snapshot();
        self.format.save(path, &snap)?;
        self.store.mark_saved();
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.file_gen = self.file_gen.wrapping_add(1);
        }
        self.migrated_from = None;
        self.telemetry.saved();
        Ok(())
    }

    // ------------------------------------------------------------------ view state

    /// App-defined view state (`ui.set` merges into it; it is not part of the document).
    pub fn view_state(&self) -> &Map<String, Value> {
        &self.view
    }

    pub fn view<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.view.get(key).and_then(|v| T::deserialize(v).ok())
    }

    pub fn view_or<T: DeserializeOwned>(&self, key: &str, default: T) -> T {
        self.view(key).unwrap_or(default)
    }

    pub fn set_view<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) {
        let Ok(v) = serde_json::to_value(value) else { return };
        self.set_view_value(key, v);
    }

    pub fn set_view_value(&mut self, key: &str, value: Value) {
        if self.view.get(key) != Some(&value) {
            self.view.insert(key.to_string(), value);
            self.view_gen = self.view_gen.wrapping_add(1);
        }
    }

    /// Changes whenever the view state changes.
    pub fn view_generation(&self) -> u64 {
        self.view_gen
    }

    // ------------------------------------------------------------------ effects

    /// Ask the window for something (ignored headless).
    pub fn request(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    /// Show a message (headless: logged).
    pub fn toast(&mut self, level: ToastLevel, text: impl Into<String>) {
        self.effects.push(Effect::Toast { level, text: text.into() });
    }

    pub(crate) fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Drop effects a headless session cannot act on (toasts go to the log).
    pub(crate) fn drain_headless(&mut self) {
        for e in self.effects.drain(..) {
            if let Effect::Toast { level, text } = e {
                match level {
                    ToastLevel::Error | ToastLevel::Warning => log::warn!("{text}"),
                    _ => log::info!("{text}"),
                }
            }
        }
    }

    // ------------------------------------------------------------------ command steps

    fn begin(&mut self) {
        self.work = Some(Work {
            doc: self.store.snapshot(),
            replaced: false,
            merge: None,
            effects: self.effects.len(),
            view: self.view.clone(),
            view_gen: self.view_gen,
            started: now(),
        });
    }

    /// End a top-level command: commit its working copy as one step, or undo everything it did.
    fn end(&mut self, label: &str, ok: bool) -> Option<std::time::Duration> {
        let w = self.work.take()?;
        let took = w.started.map(|s| s.elapsed());
        if ok {
            if !w.replaced {
                let merge = w.merge.flatten();
                self.store.commit(label.trim_end_matches('…'), merge.as_deref(), w.doc);
            }
        } else {
            self.effects.truncate(w.effects);
            if self.view_gen != w.view_gen {
                self.view = w.view;
                self.view_gen = self.view_gen.wrapping_add(1);
            }
        }
        took
    }
}

fn note_merge<D>(w: &mut Work<D>, key: Option<&str>) {
    let key = key.map(str::to_string);
    w.merge = match w.merge.take() {
        None => Some(key),
        Some(prev) if prev == key => Some(prev),
        Some(_) => Some(None),
    };
}

impl<D: Document> Host for Session<D> {
    type Error = CommandError;

    fn journal(&mut self) -> &mut Journal {
        &mut self.journal
    }

    fn before(&mut self, _spec: &CommandSpec<Self>, _params: &mut Value, depth: usize) -> Result<(), CommandError> {
        if depth == 0 {
            self.begin();
        }
        Ok(())
    }

    fn after(&mut self, spec: &CommandSpec<Self>, _params: &Value, result: &Result<Value, CommandError>, depth: usize) {
        if depth != 0 {
            return;
        }
        let took = self.end(spec.label, result.is_ok());
        self.telemetry.command(spec.id, result.is_ok(), took);
        match result {
            Ok(_) => log::debug!("command {} ok", spec.id),
            Err(e) => log::info!("command {} failed: {e}", spec.id),
        }
    }

    fn rejected(&mut self, id: &str, error: &CommandError, depth: usize) {
        if depth == 0 {
            self.end("", false);
            log::debug!("command {id} refused: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ashlar_command::{Param, Registry, always, cmd};
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Doc {
        items: Vec<String>,
    }

    type S = Session<Doc>;

    fn add_two(s: &mut S, _: &Value) -> Result<Value, CommandError> {
        s.edit("a", |d| d.items.push("one".into()));
        s.edit("b", |d| d.items.push("two".into()));
        Ok(Value::Null)
    }

    fn add_then_fail(s: &mut S, _: &Value) -> Result<Value, CommandError> {
        s.edit("a", |d| d.items.push("half".into()));
        s.set_view("selected", &3);
        s.toast(ToastLevel::Info, "never shown");
        Err(CommandError::failed("", "nope"))
    }

    fn nested(s: &mut S, _: &Value) -> Result<Value, CommandError> {
        s.edit("x", |d| d.items.push("outer".into()));
        registry().execute(s, "add_two", json!({}))
    }

    fn panics(s: &mut S, _: &Value) -> Result<Value, CommandError> {
        s.edit("x", |d| d.items.push("lost".into()));
        Err(CommandError::Panicked { id: String::new() })
    }

    fn drag(s: &mut S, p: &Value) -> Result<Value, CommandError> {
        let v = p.get("v").and_then(Value::as_str).unwrap_or_default().to_string();
        s.edit_merged("Drag", "drag-1", |d| d.items = vec![v]);
        Ok(Value::Null)
    }

    fn registry() -> Registry<S> {
        Registry::new(vec![
            cmd!("add_two", "Add Two", [], None, [], always, add_two),
            cmd!("add_then_fail", "Add Then Fail", [], None, [], always, add_then_fail),
            cmd!("nested", "Nested", [], None, [], always, nested),
            cmd!("panics", "Panics", [], None, [], always, panics),
            cmd!("drag", "Drag", [], None, [Param::string("v")], always, drag),
        ])
    }

    fn session() -> S {
        Session::new(Arc::new(Format::new("test.doc", "test")))
    }

    #[test]
    fn a_command_with_several_edits_is_one_undo_step_named_after_it() {
        let (r, mut s) = (registry(), session());
        r.execute(&mut s, "add_two", json!({})).unwrap();
        assert_eq!(s.doc().items, ["one", "two"]);
        assert_eq!(s.store().history().depth(), 1);
        assert_eq!(s.store().history().undo_label(), Some("Add Two"));
        assert!(s.is_dirty());
        s.undo();
        assert!(s.doc().items.is_empty());
        assert!(!s.is_dirty());
    }

    #[test]
    fn nested_commands_join_the_top_level_step() {
        let (r, mut s) = (registry(), session());
        r.execute(&mut s, "nested", json!({})).unwrap();
        assert_eq!(s.doc().items, ["outer", "one", "two"]);
        assert_eq!(s.store().history().depth(), 1);
        assert_eq!(s.store().history().undo_label(), Some("Nested"));
    }

    #[test]
    fn a_failed_command_changes_nothing() {
        let (r, mut s) = (registry(), session());
        r.execute(&mut s, "add_two", json!({})).unwrap();
        s.undo(); // leaves a redo line, which must survive the failure
        let rev = s.revision();
        assert!(r.execute(&mut s, "add_then_fail", json!({})).is_err());
        assert!(r.execute(&mut s, "panics", json!({})).is_err());
        assert!(r.execute(&mut s, "drag", json!({"bogus": 1})).is_err(), "bad params are refused");
        assert_eq!(s.revision(), rev);
        assert!(s.doc().items.is_empty());
        assert!(s.can_redo(), "redo line kept");
        assert_eq!(s.view::<i32>("selected"), None, "view state restored");
        assert!(s.take_effects().is_empty(), "effects of the failed command dropped");
        assert_eq!(s.store().history().depth(), 0);
    }

    #[test]
    fn repeated_commands_with_one_merge_key_merge() {
        let (r, mut s) = (registry(), session());
        for v in ["a", "b", "c"] {
            r.execute(&mut s, "drag", json!({"v": v})).unwrap();
        }
        assert_eq!(s.doc().items, ["c"]);
        assert_eq!(s.store().history().depth(), 1);
    }

    #[test]
    fn edits_outside_commands_are_steps_of_their_own() {
        let mut s = session();
        s.edit("One", |d| d.items.push("1".into()));
        s.edit("Two", |d| d.items.push("2".into()));
        assert_eq!(s.store().history().depth(), 2);
    }

    #[test]
    fn save_open_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.doc");
        let (r, mut s) = (registry(), session());
        r.execute(&mut s, "add_two", json!({})).unwrap();
        s.save_as(&p).unwrap();
        assert!(!s.is_dirty());
        assert_eq!(s.display_name(), "a.doc");
        let mut t = session();
        t.open(&p).unwrap();
        assert_eq!(t.doc().items, ["one", "two"]);
        assert!(!t.can_undo());
    }
}
