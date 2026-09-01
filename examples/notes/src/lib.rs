//! Notes: the reference ashlar app. A list of notes and an editor, with typed commands, undo,
//! autosave and recovery, a command palette, a virtualized list that stays fast at 100 000 notes,
//! agent control and a `notes-cli` binary, all from one [`app`] definition.

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
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.notes.iter().position(|n| n.id == id)
    }

    pub fn get(&self, id: u64) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id).map(|n| &**n)
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
    lower_query.is_empty() || n.title.to_lowercase().contains(lower_query) || n.body.to_lowercase().contains(lower_query)
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

/// What the view keeps between frames.
#[derive(Default)]
struct ViewState {
    query: String,
    /// Indices of notes matching `query`, for (revision, query).
    filtered: Vec<usize>,
    filter_key: Option<(Revision, String)>,
    /// Editor buffers for the selected note, synced when it or the document changes.
    editing: Option<(u64, Revision)>,
    title: String,
    body: String,
    scroll_to: Option<usize>,
    /// Row of the selected note in `filtered`, for (filter key, selected id).
    sel_row: Option<usize>,
    sel_key: Option<(Option<(Revision, String)>, Option<u64>)>,
    /// The list itself changed the selection last frame (no need to scroll to it).
    picked_in_list: bool,
}

impl ViewState {
    fn refilter(&mut self, doc: &Notes, rev: Revision) {
        let key = (rev, self.query.clone());
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        let q = self.query.to_lowercase();
        self.filtered.clear();
        if q.is_empty() {
            self.filtered.extend(0..doc.notes.len());
        } else {
            self.filtered.extend(doc.notes.iter().enumerate().filter(|(_, n)| matches(n, &q)).map(|(i, _)| i));
        }
        self.filter_key = Some(key);
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
                let r = ui.button("New Note");
                automation::register(ui, &r, "notes.add", "New Note", Role::Button);
                if r.clicked() {
                    cx.dispatch("note.add", json!({}));
                }
                ui.label(format!("{} notes", doc.notes.len()));
            });
            if SearchField::new("notes.search", &mut st.query).hint("Search notes").show(ui).changed {
                st.refilter(&doc, rev);
            }
            ui.separator();
            let sel_key = (st.filter_key.clone(), selected);
            if st.sel_key.as_ref() != Some(&sel_key) {
                // O(rows) once per change of selection or filter, never per frame
                st.sel_row = selected.and_then(|id| st.filtered.iter().position(|&i| doc.notes.get(i).is_some_and(|n| n.id == id)));
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
                    let title = label(row.index);
                    ui.label(if title.is_empty() { "Untitled" } else { title });
                });
            if let Some(row) = out.select
                && let Some(n) = filtered.get(row).and_then(|&i| doc.notes.get(i))
            {
                cx.set_view("selected", &n.id);
                st.picked_in_list = true;
            }
        });

        let Some(id) = selected.filter(|id| doc.get(*id).is_some()) else {
            ui.centered_and_justified(|ui| ui.label("Select a note, or create one with New Note."));
            return;
        };
        if st.editing != Some((id, rev))
            && let Some(n) = doc.get(id)
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
            // typing in one note is one undo step until something else happens
            cx.edit_merged("Edit Note", &format!("note-{id}"), |d| {
                if let Some(n) = d.notes.iter_mut().find(|n| n.id == id) {
                    let n = Arc::make_mut(n);
                    n.title = title;
                    n.body = body;
                }
            });
            st.editing = Some((id, cx.revision()));
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
}
