//! The message composer: a growing text field that sends on Enter, with an `@` / `/` popup.

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{Event, Id, Key, KeyboardShortcut, Modifiers, Ui};

use super::popup_list::PopupList;
use super::{ChatLabels, default_labels};
use crate::automation::{self, Details, Role};

/// The `@` or `/` word the caret is in, which opens the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    /// `'@'` or `'/'`.
    pub symbol: char,
    /// The text typed after the symbol (no whitespace).
    pub query: String,
}

/// What [`Composer::show`] reports.
pub struct ComposerResponse {
    /// The text field.
    pub response: egui::Response,
    /// Enter was pressed on a non-blank draft: the draft, taken (the field is now empty).
    pub submitted: Option<String>,
    /// A suggestion was picked (Enter, Tab or a click); the composer already replaced the
    /// trigger word with it.
    pub popup_pick: Option<usize>,
    /// Escape with no popup open (for example: stop the reply).
    pub cancelled: bool,
    /// The trigger word under the caret, for the app to filter its suggestions by.
    pub trigger: Option<Trigger>,
}

#[derive(Clone, Copy, Default)]
struct State {
    /// An input method is composing text (Enter belongs to it).
    composing: bool,
    cursor: usize,
    /// Escape closed the popup for the trigger starting at this byte.
    dismissed_at: Option<usize>,
    /// Hash of the last query (resets the popup cursor when it changes).
    query_hash: u64,
    /// Pass number of the last input-method event.
    ime_pass: u64,
    /// The field had the focus last frame.
    had_focus: bool,
}

/// A multi-line text field for chat input. Enter sends, Shift+Enter adds a line; Enter never
/// sends while an input method is composing. Typing `@` (anywhere) or `/` (at the start of a
/// line) opens [`Composer::popup`] when it has suggestions: Up/Down move, Enter or Tab pick,
/// Escape closes it. Escape with the popup closed reports [`ComposerResponse::cancelled`].
pub struct Composer<'a> {
    id: &'a str,
    draft: &'a mut String,
    placeholder: &'a str,
    suggestions: &'a [String],
    enabled: bool,
    rows: usize,
    labels: Option<&'a ChatLabels>,
}

impl<'a> Composer<'a> {
    pub fn new(id: &'a str, draft: &'a mut String) -> Self {
        Composer { id, draft, placeholder: "", suggestions: &[], enabled: true, rows: 2, labels: None }
    }
    pub fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }
    /// Suggestions for the current [`Trigger`] (the app filters them by its query).
    pub fn popup(mut self, suggestions: &'a [String]) -> Self {
        self.suggestions = suggestions;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    /// Lines shown before the field scrolls (default 2, at most 20).
    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = rows.clamp(1, 20);
        self
    }
    pub fn labels(mut self, labels: &'a ChatLabels) -> Self {
        self.labels = Some(labels);
        self
    }

    pub fn show(mut self, ui: &mut Ui) -> ComposerResponse {
        let labels: &ChatLabels = match self.labels {
            Some(l) => l,
            None => default_labels(),
        };
        let edit_id = ui.make_persistent_id(("ashlar_ui::chat::composer", self.id));
        let state_id = edit_id.with("state");
        let mut st: State = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
        // egui drops the focus on Escape before widgets run; the composer keeps it
        let escape = ui.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: Key::Escape, pressed: true, .. })));
        if self.enabled && st.had_focus && escape && !ui.memory(|m| m.has_focus(edit_id)) {
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
        let focused = self.enabled && ui.memory(|m| m.has_focus(edit_id));
        let pass = ui.ctx().cumulative_pass_nr();

        // input methods: Enter while composing confirms the composition, it never sends
        ui.input(|i| {
            for e in &i.events {
                if let Event::Ime(ime) = e {
                    st.ime_pass = pass;
                    match ime {
                        egui::ImeEvent::Preedit { text, .. } => st.composing = !text.is_empty(),
                        egui::ImeEvent::Commit(_) => st.composing = false,
                        _ => {}
                    }
                }
            }
        });

        let caret = caret_byte(ui.ctx(), edit_id, self.draft);
        let found = trigger_at(self.draft, caret);
        let trigger = found.as_ref().map(|f| Trigger { symbol: f.symbol, query: f.query.clone() });
        match &found {
            None => st.dismissed_at = None,
            Some(f) => {
                let hash = egui::util::hash((f.start, &f.query));
                if hash != st.query_hash {
                    st.query_hash = hash;
                    st.cursor = 0;
                }
                if st.dismissed_at.is_some_and(|d| d != f.start) {
                    st.dismissed_at = None;
                }
            }
        }
        let n = self.suggestions.len();
        let popup_open = focused && n > 0 && found.as_ref().is_some_and(|f| st.dismissed_at != Some(f.start));

        let mut submitted = None;
        let mut popup_pick = None;
        let mut cancelled = false;
        // the Enter that confirms a composition can arrive in the same frame or the next
        let ime_recent = st.ime_pass != 0 && pass <= st.ime_pass.saturating_add(1);
        if focused && !st.composing && !ime_recent {
            if popup_open {
                if consume(ui, Key::ArrowDown, |m| m.is_none()) {
                    st.cursor = (st.cursor + 1) % n;
                }
                if consume(ui, Key::ArrowUp, |m| m.is_none()) {
                    st.cursor = (st.cursor + n - 1) % n;
                }
                if consume(ui, Key::Enter, |m| m.is_none()) || consume(ui, Key::Tab, |m| m.is_none()) {
                    popup_pick = Some(st.cursor.min(n - 1));
                }
                if consume(ui, Key::Escape, |_| true) {
                    st.dismissed_at = found.as_ref().map(|f| f.start);
                }
            } else {
                if consume(ui, Key::Escape, |_| true) {
                    cancelled = true;
                }
                if consume(ui, Key::Enter, |m| !m.shift && !m.alt) && !self.draft.trim().is_empty() {
                    submitted = Some(std::mem::take(self.draft));
                }
            }
        }
        if let (Some(i), Some(f)) = (popup_pick, &found) {
            self.apply_pick(ui, edit_id, f, i);
            st.dismissed_at = None;
        }

        let response = ui.add_enabled(
            self.enabled,
            egui::TextEdit::multiline(self.draft)
                .id(edit_id)
                .hint_text(self.placeholder)
                .desired_rows(self.rows)
                .desired_width(f32::INFINITY)
                .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter))
                // Tab picks while the popup is open; Escape never drops the focus
                .event_filter(egui::EventFilter { tab: popup_open, horizontal_arrows: true, vertical_arrows: true, escape: true }),
        );
        if submitted.is_some() {
            set_caret(ui.ctx(), edit_id, 0);
        }

        // the popup: keys stay with the field, a click picks
        let open_now = popup_open && popup_pick.is_none();
        if open_now && let Some(f) = &found {
            let popup_id = format!("{}.popup", self.id);
            let mut cursor = st.cursor;
            if let Some(i) = PopupList::new(&popup_id, self.suggestions, &mut cursor).label(&labels.suggestions).show_above(ui, response.rect) {
                self.apply_pick(ui, edit_id, f, i);
                popup_pick = Some(i);
                ui.memory_mut(|m| m.request_focus(edit_id));
                crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
            }
            st.cursor = cursor;
        }
        st.had_focus = response.has_focus();
        ui.data_mut(|d| d.insert_temp(state_id, st));
        automation::register_with(
            ui,
            &response,
            self.id,
            &labels.composer,
            Role::Field,
            Details { value: Some(self.draft.as_str()), selected: None },
        );
        ComposerResponse { response, submitted, popup_pick, cancelled, trigger }
    }

    /// Replace the trigger word with `<symbol><suggestion> ` and put the caret after it.
    fn apply_pick(&mut self, ui: &Ui, edit_id: Id, f: &Found, i: usize) {
        let Some(s) = self.suggestions.get(i) else { return };
        let Some(range) = self.draft.get(f.start..f.end).map(|_| f.start..f.end) else { return };
        let insert = format!("{}{s} ", f.symbol);
        self.draft.replace_range(range, &insert);
        let caret_chars = self.draft.get(..f.start + insert.len()).map_or(0, |p| p.chars().count());
        set_caret(ui.ctx(), edit_id, caret_chars);
    }
}

/// Remove the first press of `key` whose modifiers pass `mods`; true if one was there.
fn consume(ui: &Ui, key: Key, mods: impl Fn(Modifiers) -> bool) -> bool {
    ui.input_mut(|i| {
        let pos = i.events.iter().position(|e| matches!(e, Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && mods(*modifiers)));
        match pos {
            Some(p) => {
                i.events.remove(p);
                true
            }
            None => false,
        }
    })
}

/// The caret as a byte offset into `text` (the end when unknown).
fn caret_byte(ctx: &egui::Context, id: Id, text: &str) -> usize {
    let chars = TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index);
    match chars {
        Some(c) => text.char_indices().nth(c.into()).map_or(text.len(), |(b, _)| b),
        None => text.len(),
    }
}

fn set_caret(ctx: &egui::Context, id: Id, chars: usize) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(chars))));
    state.store(ctx, id);
}

struct Found {
    symbol: char,
    query: String,
    /// Byte range of the word, symbol included.
    start: usize,
    end: usize,
}

/// The `@word` or `/word` ending at `caret`: `@` after whitespace or at the start, `/` at the
/// start of a line.
fn trigger_at(text: &str, caret: usize) -> Option<Found> {
    let before = text.get(..caret)?;
    let start = before.rfind(char::is_whitespace).map_or(0, |i| i + before.get(i..).and_then(|s| s.chars().next()).map_or(1, char::len_utf8));
    let word = before.get(start..)?;
    let symbol = word.chars().next()?;
    let line_start = start == 0 || before.get(..start).is_some_and(|p| p.ends_with('\n'));
    let ok = match symbol {
        '@' => true,
        '/' => line_start,
        _ => false,
    };
    ok.then(|| Found { symbol, query: word.get(symbol.len_utf8()..).unwrap_or("").to_owned(), start, end: caret })
}

#[cfg(test)]
pub(super) fn trigger_for_test(text: &str, caret: usize) -> Option<(char, String, usize)> {
    trigger_at(text, caret).map(|f| (f.symbol, f.query, f.start))
}
