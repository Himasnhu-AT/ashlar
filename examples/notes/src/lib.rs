//! Notes: the reference ashlar app. A list of notes and an editor, with typed commands, undo,
//! autosave and recovery, a command palette, a virtualized list that stays fast at 100 000 notes,
//! agent control and a `notes-cli` binary, all from one [`app`] definition.

use std::collections::HashMap;
use std::sync::Arc;

use ashlar::prelude::*;
use ashlar::ui::automation::{self, Role};
use ashlar::ui::search::SearchField;
use ashlar::ui::virtual_list::{RowHeights, VirtualList};
use serde::{Deserialize, Serialize};

/// The file format id.
pub const FORMAT_ID: &str = "ashlar.notes";

/// One note. `Arc`ed in the document, so an edit copies one note, not 100 000.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// The document (schema v2: notes have a title and a body).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notes {
    pub notes: Vec<Arc<Note>>,
    #[serde(default)]
    pub next_id: u64,
}

impl Notes {
    /// Where note `id` is. Notes this app writes are in ascending id order, so this is a binary
    /// search; a note out of order (a hand-edited file) is still found, by a scan.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        let i = self.notes.partition_point(|n| n.id < id);
        if self.notes.get(i).is_some_and(|n| n.id == id) {
            return Some(i);
        }
        self.notes.iter().position(|n| n.id == id)
    }

    pub fn get(&self, id: u64) -> Option<&Note> {
        self.index_of(id).and_then(|i| self.notes.get(i)).map(|n| &**n)
    }
}

/// v1 → v2: `{"text"}` notes become `{"id", "title", "body"}` (first line is the title).
fn v1_to_v2(mut file: Value) -> Result<Value, String> {
    let doc = file.get_mut("document").ok_or("no document")?;
    let notes = doc.get("notes").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        let text = n.get("text").and_then(Value::as_str).unwrap_or_default();
        let (title, body) = text.split_once('\n').unwrap_or((text, ""));
        out.push(json!({"id": i as u64 + 1, "title": title, "body": body}));
    }
    *doc = json!({"notes": out, "next_id": notes.len() as u64 + 1});
    Ok(file)
}

/// The file format: schema v2, migrating v1 files.
pub fn format() -> Format<Notes> {
    Format::new(FORMAT_ID, concat!("notes ", env!("CARGO_PKG_VERSION"))).migrations(vec![v1_to_v2])
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

type S = Session<Notes>;
type R = Result<Value, CommandError>;

fn str_param(p: &Value, k: &str) -> Option<String> {
    p.get(k).and_then(Value::as_str).map(str::to_string)
}

fn id_param(s: &S, p: &Value) -> Option<u64> {
    p.get("id").and_then(Value::as_u64).or_else(|| s.view::<u64>("selected"))
}

fn has_target(s: &S) -> Result<(), String> {
    match s.view::<u64>("selected") {
        Some(id) if s.doc().get(id).is_some() => Ok(()),
        _ => Err("no note selected".into()),
    }
}

fn add(s: &mut S, p: &Value) -> R {
    let title = str_param(p, "title").unwrap_or_else(|| "Untitled note".into());
    let body = str_param(p, "body").unwrap_or_default();
    let id = s.edit("New Note", |d| {
        d.next_id = d.next_id.max(1);
        let id = d.next_id;
        d.next_id += 1;
        d.notes.push(Arc::new(Note { id, title, body }));
        id
    });
    s.set_view("selected", &id);
    Ok(json!({"id": id, "count": s.doc().notes.len()}))
}

fn edit(s: &mut S, p: &Value) -> R {
    let id = id_param(s, p).ok_or_else(|| CommandError::bad_params("", "`id` is required (no note selected)"))?;
    let (title, body) = (str_param(p, "title"), str_param(p, "body"));
    s.try_edit("Edit Note", |d| {
        let i = d.index_of(id).ok_or_else(|| CommandError::failed("", format!("no note {id}")))?;
        let Some(n) = d.notes.get_mut(i) else { return Err(CommandError::failed("", format!("no note {id}"))) };
        let n = Arc::make_mut(n);
        if let Some(t) = title {
            n.title = t;
        }
        if let Some(b) = body {
            n.body = b;
        }
        Ok(())
    })?;
    Ok(json!({"id": id}))
}

fn delete(s: &mut S, p: &Value) -> R {
    let id = id_param(s, p).ok_or_else(|| CommandError::bad_params("", "`id` is required (no note selected)"))?;
    let i = s.try_edit("Delete Note", |d| {
        let i = d.index_of(id).ok_or_else(|| CommandError::failed("", format!("no note {id}")))?;
        d.notes.remove(i);
        Ok::<usize, CommandError>(i)
    })?;
    let next = s.doc().notes.get(i).or_else(|| s.doc().notes.get(i.wrapping_sub(1))).map(|n| n.id);
    s.set_view_value("selected", next.map_or(Value::Null, |v| json!(v)));
    Ok(json!({"deleted": id, "count": s.doc().notes.len()}))
}

fn generate(s: &mut S, p: &Value) -> R {
    let count = p.get("count").and_then(Value::as_u64).unwrap_or(1000);
    s.edit("Generate Notes", |d| {
        d.next_id = d.next_id.max(1);
        d.notes.reserve(count as usize);
        for i in 0..count {
            let id = d.next_id;
            d.next_id += 1;
            d.notes.push(Arc::new(Note { id, title: format!("Note {id}"), body: format!("Generated note number {i}.") }));
        }
    });
    Ok(json!({"count": s.doc().notes.len()}))
}

fn select(s: &mut S, p: &Value) -> R {
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| CommandError::bad_params("", "`id` is required"))?;
    if s.doc().get(id).is_none() {
        return Err(CommandError::failed("", format!("no note {id}")));
    }
    s.set_view("selected", &id);
    Ok(json!({"selected": id}))
}

fn list(s: &mut S, p: &Value) -> R {
    let offset = p.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(100).min(10_000) as usize;
    let q = str_param(p, "query").unwrap_or_default().to_lowercase();
    let notes: Vec<&Arc<Note>> = s.doc().notes.iter().filter(|n| matches(n, &q)).skip(offset).take(limit).collect();
    Ok(json!({"count": s.doc().notes.len(), "notes": notes}))
}

fn matches(n: &Note, lower_query: &str) -> bool {
    matches_text(&n.title, &n.body, lower_query)
}

fn matches_text(title: &str, body: &str, lower_query: &str) -> bool {
    contains_lower(title, lower_query) || contains_lower(body, lower_query)
}

/// Whether `hay` contains `needle` (already lowercase) ignoring case, without allocating: bytes
/// for ASCII, `char::to_lowercase` otherwise (which, unlike `str::to_lowercase`, does not make a
/// word-final sigma `ς`).
fn contains_lower(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.is_ascii() && hay.is_ascii() {
        let n = needle.as_bytes();
        return hay.as_bytes().windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    hay.char_indices().any(|(i, _)| {
        let mut h = hay.get(i..).unwrap_or_default().chars().flat_map(char::to_lowercase);
        needle.chars().all(|c| h.next() == Some(c))
    })
}

const ADD: &[Param] = &[Param::string("title"), Param::string("body")];
const EDIT: &[Param] = &[Param::int_in("id", 1, i64::MAX).about("default: the selected note"), Param::string("title"), Param::string("body")];
const ID: &[Param] = &[Param::int_in("id", 1, i64::MAX).about("default: the selected note")];
const LIST: &[Param] = &[Param::int_in("offset", 0, i64::MAX), Param::int_in("limit", 1, 10_000), Param::string("query")];

/// The notes commands (after the built-in File and Edit ones).
pub fn commands() -> Vec<CommandSpec<S>> {
    vec![
        CommandSpec::new("note.add", "New Note", add).menu(&["Note"]).shortcut("Mod+Shift+N").params(Params::Static(ADD)),
        CommandSpec::new("note.edit", "Edit Note", edit).params(Params::Static(EDIT)),
        CommandSpec::new("note.delete", "Delete Note", delete)
            .menu(&["Note"])
            .shortcut("Mod+Backspace")
            .params(Params::Static(ID))
            .enabled(has_target),
        cmd!("note.generate", "Generate Notes…", ["Note"], None, [Param::int_in("count", 1, 1_000_000).about("how many")], always, generate),
        CommandSpec::new("note.select", "Select Note", select).params(Params::Static(ID)).query(),
        CommandSpec::new("note.list", "List Notes", list).params(Params::Static(LIST)).query(),
    ]
}

// ---------------------------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------------------------

/// Finds a note's index by id in the view, for one revision: a binary search when the ids are in
/// order (they always are in files this app writes), a map otherwise. Either way a missing id
/// costs no scan.
#[derive(Default)]
enum Ids {
    #[default]
    Sorted,
    Map(HashMap<u64, usize>),
}

impl Ids {
    fn new(doc: &Notes) -> Self {
        if doc.notes.is_sorted_by(|a, b| a.id < b.id) {
            Ids::Sorted
        } else {
            let mut map = HashMap::with_capacity(doc.notes.len());
            for (i, n) in doc.notes.iter().enumerate() {
                // the first note with an id wins, as `Notes::index_of` finds it
                map.entry(n.id).or_insert(i);
            }
            Ids::Map(map)
        }
    }

    fn find(&self, doc: &Notes, id: u64) -> Option<usize> {
        let i = match self {
            Ids::Sorted => doc.notes.partition_point(|n| n.id < id),
            Ids::Map(map) => *map.get(&id)?,
        };
        doc.notes.get(i).is_some_and(|n| n.id == id).then_some(i)
    }
}

/// What the view keeps between frames.
#[derive(Default)]
struct ViewState {
    query: String,
    /// Indices of notes matching `filter_query` at `filter_rev`, ascending.
    filtered: Vec<usize>,
    filter_rev: Option<Revision>,
    filter_query: String,
    /// `filter_query`, lowercased.
    lower_query: String,
    /// Bumped whenever `filtered` changes.
    filter_gen: u64,
    /// Note index by id, for `ids_rev`.
    ids: Ids,
    ids_rev: Option<Revision>,
    /// Editor buffers for the selected note, synced when it or the document changes.
    editing: Option<(u64, Revision)>,
    title: String,
    body: String,
    scroll_to: Option<usize>,
    /// Row of the selected note in `filtered`, for (filter_gen, selected id).
    sel_row: Option<usize>,
    sel_key: Option<(u64, Option<u64>)>,
    /// The list itself changed the selection last frame (no need to scroll to it).
    picked_in_list: bool,
    /// "N notes" for `count_of` notes, rebuilt only when the count changes.
    count_text: String,
    count_of: Option<usize>,
    /// The undo merge key of `merge_of` (`note-<id>`), rebuilt only when the edited note changes.
    merge_key: String,
    merge_of: Option<u64>,
}

impl ViewState {
    /// Bring the id index and the filter up to `rev` and the current query: O(notes) once per
    /// revision or query change, nothing per frame.
    fn refilter(&mut self, doc: &Notes, rev: Revision) {
        if self.ids_rev != Some(rev) {
            self.ids = Ids::new(doc);
            self.ids_rev = Some(rev);
        }
        if self.filter_rev == Some(rev) && self.filter_query == self.query {
            return;
        }
        if self.filter_query != self.query || self.filter_rev.is_none() {
            self.filter_query.clone_from(&self.query);
            self.lower_query = self.query.to_lowercase();
        }
        self.filtered.clear();
        if self.lower_query.is_empty() {
            self.filtered.extend(0..doc.notes.len());
        } else {
            let q = self.lower_query.as_str();
            self.filtered.extend(doc.notes.iter().enumerate().filter(|(_, n)| matches(n, q)).map(|(i, _)| i));
        }
        self.filter_rev = Some(rev);
        self.filter_gen = self.filter_gen.wrapping_add(1);
    }

    /// The view edited the text of note `index` (which now matches the query or not: `hit`),
    /// taking the document from `before` to `after`. The ids did not move, and only that note can
    /// have entered or left the filter, so re-check just it instead of every note.
    fn note_edited(&mut self, index: usize, hit: bool, before: Revision, after: Revision) {
        if self.ids_rev == Some(before) {
            self.ids_rev = Some(after);
        }
        if self.filter_rev != Some(before) || self.filter_query != self.query {
            // out of date anyway: the next frame filters from scratch
            return;
        }
        self.filter_rev = Some(after);
        if self.lower_query.is_empty() {
            return;
        }
        match (self.filtered.binary_search(&index), hit) {
            (Ok(pos), false) => {
                self.filtered.remove(pos);
                self.filter_gen = self.filter_gen.wrapping_add(1);
            }
            (Err(pos), true) => {
                self.filtered.insert(pos, index);
                self.filter_gen = self.filter_gen.wrapping_add(1);
            }
            _ => {}
        }
    }
}

fn view() -> impl FnMut(&mut egui::Ui, &mut Cx<'_, Notes>) {
    let mut st = ViewState::default();
    move |ui, cx| {
        let doc = cx.snapshot();
        let rev = cx.revision();
        st.refilter(&doc, rev);
        let selected: Option<u64> = cx.view("selected");

        egui::Panel::left("notes.sidebar").default_size(280.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                let r = ui.button(ashlar::ui::cache::button_text(ui, "notes.add", "New Note"));
                automation::register(ui, &r, "notes.add", "New Note", Role::Button);
                if r.clicked() {
                    cx.dispatch("note.add", json!({}));
                }
                if st.count_of != Some(doc.notes.len()) {
                    st.count_text = format!("{} notes", doc.notes.len());
                    st.count_of = Some(doc.notes.len());
                }
                ashlar::ui::cache::label(ui, "notes.count", &st.count_text);
            });
            if SearchField::new("notes.search", &mut st.query).hint("Search notes").show(ui).changed {
                st.refilter(&doc, rev);
            }
            ui.separator();
            let sel_key = (st.filter_gen, selected);
            if st.sel_key != Some(sel_key) {
                // `filtered` is ascending, so a binary search for the note's index
                st.sel_row = selected.and_then(|id| st.ids.find(&doc, id)).and_then(|i| st.filtered.binary_search(&i).ok());
                if !std::mem::take(&mut st.picked_in_list) {
                    st.scroll_to = st.sel_row;
                }
                st.sel_key = Some(sel_key);
            }
            let sel_row = st.sel_row;
            let filtered = &st.filtered;
            let label = |row: usize| -> &str { filtered.get(row).and_then(|&i| doc.notes.get(i)).map_or("", |n| n.title.as_str()) };
            let out = VirtualList::new("notes.list", filtered.len(), RowHeights::Uniform(22.0))
                .selected(sel_row)
                .scroll_to(st.scroll_to.take())
                .row_label(&label)
                .show(ui, |ui, row| {
                    // a cached galley per note (a steady frame lays out nothing); not selectable, so a
                    // click on the title selects the note
                    let id = filtered.get(row.index).and_then(|&i| doc.notes.get(i)).map_or(u64::MAX, |n| n.id);
                    let title = label(row.index);
                    ashlar::ui::cache::row_label(ui, ("notes.row", id), if title.is_empty() { "Untitled" } else { title });
                });
            if let Some(row) = out.select
                && let Some(n) = filtered.get(row).and_then(|&i| doc.notes.get(i))
            {
                cx.set_view("selected", &n.id);
                st.picked_in_list = true;
            }
        });

        let Some((id, index)) = selected.and_then(|id| Some((id, st.ids.find(&doc, id)?))) else {
            ui.centered_and_justified(|ui| ui.label("Select a note, or create one with New Note."));
            return;
        };
        if st.editing != Some((id, rev))
            && let Some(n) = doc.notes.get(index)
        {
            st.title.clone_from(&n.title);
            st.body.clone_from(&n.body);
            st.editing = Some((id, rev));
        }
        let t = ui.add(egui::TextEdit::singleline(&mut st.title).desired_width(f32::INFINITY).font(egui::TextStyle::Heading));
        automation::register_with(ui, &t, "notes.title", "Title", Role::Field, automation::Details { value: Some(&st.title), selected: None });
        let b = ui.add_sized(ui.available_size(), egui::TextEdit::multiline(&mut st.body));
        automation::register_with(ui, &b, "notes.body", "Body", Role::Field, automation::Details { value: Some(&st.body), selected: None });
        if t.changed() || b.changed() {
            let (title, body) = (st.title.clone(), st.body.clone());
            let before = cx.revision();
            if st.merge_of != Some(id) {
                st.merge_key = format!("note-{id}");
                st.merge_of = Some(id);
            }
            // typing in one note is one undo step until something else happens
            cx.edit_merged("Edit Note", &st.merge_key, |d| {
                // `index` is where the note was in the snapshot this frame drew
                let i = if d.notes.get(index).is_some_and(|n| n.id == id) { Some(index) } else { d.index_of(id) };
                // a new note rather than `make_mut`, which would copy the old text first
                if let Some(n) = i.and_then(|i| d.notes.get_mut(i)) {
                    *n = Arc::new(Note { id, title, body });
                }
            });
            let after = cx.revision();
            let hit = matches_text(&st.title, &st.body, &st.lower_query);
            st.note_edited(index, hit, before, after);
            st.editing = Some((id, after));
        }
    }
}

/// The whole app: format, commands, view. `notes` runs it in a window, `notes-cli` headless.
pub fn app() -> App<Notes> {
    App::new("notes", env!("CARGO_PKG_VERSION"))
        .title("Notes")
        .format(format())
        .file_extension("notes")
        .commands(commands())
        .instructions("A note-taking app. Notes have an id, a title and a body. Use note.add, note.edit, note.delete and note.list; file.save_as / file.open with a `path` to save and load.")
        .view(view())
        .scene("empty", |_: &mut S| {})
        .scene("two-notes", |s: &mut S| {
            for (title, body) in [("Groceries", "milk, eggs"), ("Ideas", "a faster list")] {
                let _ = add(s, &json!({"title": title, "body": body}));
            }
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn matching_ignores_case_like_lowercasing_does() {
        let hays = ["Note 42", "GENERATED note number 7.", "Straße und Ärger", "ÉCOLE", "İstanbul", "", "abc"];
        let queries = ["", "note", "NOTE 4", "number 7.", "straSSe", "ärg", "école", "i̇st", "abcd", "c", "x"];
        for h in hays {
            for q in queries {
                let lq = q.to_lowercase();
                assert_eq!(contains_lower(h, &lq), h.to_lowercase().contains(&lq), "{h:?} contains {q:?}");
            }
        }
    }

    fn doc(ids: &[u64]) -> Notes {
        Notes { notes: ids.iter().map(|&id| Arc::new(Note { id, ..Note::default() })).collect(), next_id: 0 }
    }

    #[test]
    fn notes_are_found_by_id_in_order_or_not() {
        for ids in [&[1, 2, 5, 9][..], &[9, 1, 5, 2], &[3, 3, 1], &[]] {
            let d = doc(ids);
            let index = Ids::new(&d);
            for id in 0..11 {
                let expect = ids.iter().position(|&x| x == id);
                assert_eq!(d.index_of(id), expect, "index_of {id} in {ids:?}");
                assert_eq!(index.find(&d, id), expect, "Ids::find {id} in {ids:?}");
            }
        }
        assert!(matches!(Ids::new(&doc(&[1, 2, 3])), Ids::Sorted));
        assert!(matches!(Ids::new(&doc(&[2, 1])), Ids::Map(_)));
    }
}
