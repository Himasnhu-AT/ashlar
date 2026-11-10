//! A text field that filters a list of options as the user types.

use ashlar_command::fuzzy_match;
use egui::{Event, EventFilter, Id, ImeEvent, Key, Modifiers, Response, Ui};

use super::options::OptionList;
use super::popover::Popover;
use crate::automation::{self, Details, Role};
use crate::cache;
use crate::tokens;

/// What a [`Combobox`] keeps between frames: the typed text, the chosen option and the
/// filtered list (ranked again only when the text, the options or the revision change).
#[derive(Clone, Debug, Default)]
pub struct ComboboxState {
    /// The text in the field.
    pub text: String,
    /// The last chosen option (an index into the options).
    pub chosen: Option<usize>,
    open: bool,
    cursor: usize,
    /// An input method is composing text: Enter and the arrows belong to it.
    composing: bool,
    /// Typing since the last choice: the list is filtered by `text`.
    filtering: bool,
    hits: Vec<usize>,
    key: Option<(u64, usize, u64, bool)>,
}

impl ComboboxState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the option list is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether an input method is composing text in the field.
    pub fn is_composing(&self) -> bool {
        self.composing
    }

    /// The options listed now (indices into the options), best match first.
    pub fn matches(&self) -> &[usize] {
        &self.hits
    }

    fn rank<T: AsRef<str>>(&mut self, options: &[T], revision: u64) {
        let key = (cache::hash_of(&self.text), options.len(), revision, self.filtering);
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.hits.clear();
        let query = self.text.trim();
        if !self.filtering || query.is_empty() {
            self.hits.extend(0..options.len());
        } else {
            let mut scored: Vec<(i64, usize)> =
                options.iter().enumerate().filter_map(|(i, o)| fuzzy_match(query, o.as_ref()).map(|m| (i64::from(m.score), i))).collect();
            // best first; ties keep the options' order
            scored.sort_by_key(|&(score, i)| (std::cmp::Reverse(score), i));
            self.hits.extend(scored.into_iter().map(|(_, i)| i));
        }
        self.cursor = 0;
    }
}

/// What [`Combobox::show`] reports.
pub struct ComboboxResponse {
    /// The text field's response (`changed()` when the text changed).
    pub response: Response,
    /// The option chosen this frame.
    pub chosen: Option<usize>,
}

/// A text field with a filtered list of options. Typing opens the list and filters it with the
/// command palette's fuzzy matcher; the arrows move, Enter chooses (filling the field with the
/// option's label), Escape closes. While an input method composes text, Enter and the arrows
/// are left to it and nothing is chosen.
pub struct Combobox<'a, T: AsRef<str>> {
    id: &'a str,
    options: &'a [T],
    state: &'a mut ComboboxState,
    width: Option<f32>,
    hint: &'a str,
    label: &'a str,
    empty: &'a str,
    revision: u64,
}

impl<'a, T: AsRef<str>> Combobox<'a, T> {
    /// `id` is the automation id of the field; options register as `<id>.option.<n>`.
    pub fn new(id: &'a str, options: &'a [T], state: &'a mut ComboboxState) -> Self {
        Self { id, options, state, width: None, hint: "", label: "", empty: "No matches", revision: 0 }
    }

    /// Width of the field in points (default: 12 control heights).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width.is_finite().then_some(width.clamp(40.0, 4000.0));
        self
    }

    /// Placeholder text in the empty field.
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    /// The accessible name.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// Shown when nothing matches.
    pub fn empty_text(mut self, text: &'a str) -> Self {
        self.empty = text;
        self
    }

    /// Bump when the options change without their count changing.
    pub fn revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }

    pub fn show(self, ui: &mut Ui) -> ComboboxResponse {
        let t = tokens::current(ui.ctx());
        let id = Id::new(("ashlar_ui::combobox", self.id));
        let st = self.state;
        let options = self.options;
        let n = options.len();
        let label_of = |i: usize| options.get(i).map_or("", |o| o.as_ref());

        let focused = ui.memory(|m| m.has_focus(id));
        let (mut up, mut down, mut enter, mut escape) = (false, false, false, false);
        if focused {
            ui.input(|i| {
                for e in &i.events {
                    if let Event::Ime(ime) = e {
                        match ime {
                            ImeEvent::Preedit { text, .. } => st.composing = !text.is_empty(),
                            ImeEvent::Commit(_) => st.composing = false,
                            _ => {}
                        }
                    }
                }
            });
            if st.composing {
                // the input method owns these keys; nothing is chosen mid-composition
                ui.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, Key::Enter);
                });
            } else {
                (up, down, enter, escape) = ui.input_mut(|i| {
                    (
                        i.consume_key(Modifiers::NONE, Key::ArrowUp),
                        i.consume_key(Modifiers::NONE, Key::ArrowDown),
                        st.open && i.consume_key(Modifiers::NONE, Key::Enter),
                        st.open && i.consume_key(Modifiers::NONE, Key::Escape),
                    )
                });
            }
        } else {
            st.composing = false;
        }

        let width = self.width.unwrap_or(t.control_height * 12.0).min(ui.available_width().max(40.0));
        let filter = EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: st.open };
        let edit = egui::TextEdit::singleline(&mut st.text).id(id).hint_text(self.hint).desired_width(width).event_filter(filter).show(ui);
        let response = edit.response.response;
        if response.changed() {
            st.open = true;
            st.filtering = true;
        }
        if response.clicked() && !st.open {
            st.open = true;
        }
        if escape {
            st.open = false;
        }
        if st.open && response.lost_focus() && !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
            st.open = false;
        }
        st.rank(options, self.revision);
        let rows = st.hits.len();
        let mut moved = false;
        if down {
            if st.open {
                st.cursor = st.cursor.saturating_add(1).min(rows.saturating_sub(1));
            } else {
                st.open = true;
            }
            moved = true;
        }
        if up && st.open {
            st.cursor = st.cursor.saturating_sub(1);
            moved = true;
        }
        st.cursor = st.cursor.min(rows.saturating_sub(1));
        let mut choose = None;
        if enter {
            choose = st.hits.get(st.cursor).copied();
            if choose.is_none() {
                st.open = false;
            }
        }

        let hits = &st.hits;
        let option_of = |row: usize| hits.get(row).copied();
        let row_label = |row: usize| hits.get(row).map_or("", |&i| label_of(i));
        let id_str = self.id;
        let chosen = st.chosen;
        let cursor = st.cursor;
        let empty = self.empty;
        let mut open = st.open;
        let clicked = Popover::attached(id.with("popup")).width(response.rect.width()).show(&response, &mut open, |ui| {
            if rows == 0 {
                ui.label(egui::RichText::new(empty).color(t.text.secondary));
                return None;
            }
            OptionList {
                id: id_str,
                rows,
                option_of: &option_of,
                label: &row_label,
                cursor: Some(cursor),
                chosen,
                scroll_to: moved.then_some(cursor),
            }
            .show(ui)
        });
        st.open = open;
        if let Some(Some(row)) = clicked {
            choose = st.hits.get(row).copied();
        }
        if let Some(i) = choose
            && i < n
        {
            st.open = false;
            st.filtering = false;
            st.text.clear();
            st.text.push_str(label_of(i));
            st.chosen = Some(i);
            response.request_focus();
        }
        let chosen_now = choose.filter(|&i| i < n);

        let open = st.open;
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(egui::accesskit::Role::ComboBox);
            node.set_expanded(open);
            if !self.label.is_empty() {
                node.set_label(self.label);
            }
        });
        automation::register_with(
            ui,
            &response,
            self.id,
            self.label,
            Role::ComboBox,
            Details { value: Some(st.text.as_str()), selected: Some(open) },
        );
        ComboboxResponse { response, chosen: chosen_now }
    }
}
