//! Keymap editor: every command with its shortcut; click a shortcut to record a new one, see
//! conflicts, reset or clear a binding.
//!
//! The editor changes the [`Keymap`] directly and reports each change so the app can save
//! `keymap.overrides()`. While it records, it swallows the key presses (check
//! [`KeymapEditor::is_recording`] and skip the app's own shortcut dispatch). Escape cancels a
//! recording.

use std::collections::HashMap;

use ashlar_command::shortcut::{Chord, display_list};
use ashlar_command::{CommandInfo, Keymap, fuzzy_match};
use egui::{Color32, Sense, Ui, vec2};

use super::keys::chord_from_egui;
use crate::automation::{self, Details, Role};
use crate::cache;
use crate::search::SearchField;
use crate::tokens::{self, Tokens};
use crate::virtual_list::{Column, RowHeights, VirtualTable};

/// A change the editor made to the keymap.
#[derive(Clone, Debug, PartialEq)]
pub enum KeymapChange {
    /// The command's keys were replaced (empty: unbound).
    Bound { id: String, keys: Vec<Chord> },
    /// The command is back to its default keys.
    Reset { id: String },
}

/// Labels the editor shows (localized by the app).
#[derive(Clone, Debug)]
pub struct KeymapLabels {
    pub search: String,
    pub command: String,
    pub shortcut: String,
    pub recording: String,
    pub unbound: String,
    pub reset: String,
    pub clear: String,
    pub conflicts_with: String,
}

impl Default for KeymapLabels {
    fn default() -> Self {
        KeymapLabels {
            search: "Search commands or keys".into(),
            command: "Command".into(),
            shortcut: "Shortcut".into(),
            recording: "Press a shortcut… (Esc cancels)".into(),
            unbound: "—".into(),
            reset: "Reset".into(),
            clear: "Clear".into(),
            conflicts_with: "Also used by".into(),
        }
    }
}

/// Editor state (keep it while the editor is shown).
#[derive(Default)]
pub struct KeymapEditor {
    filter: String,
    recording: Option<String>,
    rows: Vec<usize>,
    rows_key: Option<(u64, u64, u64, usize)>,
    conflicts: HashMap<String, Vec<String>>,
    conflicts_key: Option<(u64, u64)>,
    edits: u64,
    pub labels: KeymapLabels,
}

impl KeymapEditor {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }
    /// Start recording a shortcut for `id` (as a click on its shortcut does).
    pub fn record(&mut self, id: &str) {
        self.recording = Some(id.to_owned());
    }
    /// Commands shown for the current filter.
    pub fn visible_ids<'c>(&self, commands: &'c [CommandInfo]) -> Vec<&'c str> {
        self.rows.iter().filter_map(|i| commands.get(*i)).map(|c| c.id.as_str()).collect()
    }
    /// Other commands bound to one of `id`'s keys.
    pub fn conflicts_of(&self, id: &str) -> &[String] {
        self.conflicts.get(id).map_or(&[], Vec::as_slice)
    }

    fn refresh(&mut self, keymap: &Keymap, commands: &[CommandInfo], revision: u64) {
        let ckey = (revision, self.edits);
        if self.conflicts_key != Some(ckey) {
            self.conflicts.clear();
            for c in keymap.conflicts() {
                for id in &c.ids {
                    let others = self.conflicts.entry(id.clone()).or_default();
                    for o in c.ids.iter().filter(|o| *o != id) {
                        if !others.contains(o) {
                            others.push(o.clone());
                        }
                    }
                }
            }
            self.conflicts_key = Some(ckey);
        }
        let rkey = (cache::hash_of(&self.filter), revision, self.edits, commands.len());
        if self.rows_key == Some(rkey) {
            return;
        }
        self.rows_key = Some(rkey);
        self.rows.clear();
        let q = self.filter.trim();
        let ql = q.to_lowercase();
        let platform = keymap.platform();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for (i, c) in commands.iter().enumerate() {
            if q.is_empty() {
                scored.push((0, i));
                continue;
            }
            let keys = display_list(keymap.keys_for(&c.id), platform).to_lowercase();
            let portable = ashlar_command::shortcut::format_list(keymap.keys_for(&c.id)).to_lowercase();
            if let Some(m) = fuzzy_match(q, &c.label) {
                scored.push((m.score, i));
            } else if !keys.is_empty() && (keys.contains(&ql) || portable.contains(&ql)) {
                scored.push((0, i));
            }
        }
        scored.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
        self.rows.extend(scored.into_iter().map(|(_, i)| i));
    }

    /// Record from this frame's key presses (when recording).
    fn capture(&mut self, ui: &Ui, keymap: &mut Keymap) -> Option<KeymapChange> {
        let id = self.recording.clone()?;
        let platform = keymap.platform();
        let mut out = None;
        let mut cancel = false;
        ui.input_mut(|i| {
            let mut used = Vec::new();
            for (n, e) in i.events.iter().enumerate() {
                if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
                    used.push(n);
                    if *key == egui::Key::Escape && modifiers.is_none() {
                        cancel = true;
                        break;
                    }
                    if let Some(chord) = chord_from_egui(*key, *modifiers, platform) {
                        out = Some(chord);
                        break;
                    }
                }
            }
            for n in used.into_iter().rev() {
                if n < i.events.len() {
                    i.events.remove(n);
                }
            }
        });
        if cancel {
            self.recording = None;
            return None;
        }
        let chord = out?;
        self.recording = None;
        keymap.bind(&id, vec![chord]).then(|| {
            self.edits += 1;
            KeymapChange::Bound { id, keys: vec![chord] }
        })
    }

    /// Show the editor. `revision` changes when the commands or the keymap changed elsewhere.
    pub fn show(&mut self, ui: &mut Ui, keymap: &mut Keymap, commands: &[CommandInfo], revision: u64) -> Option<KeymapChange> {
        let t = tokens::current(ui.ctx());
        let mut change = self.capture(ui, keymap);
        SearchField::new("keymap.search", &mut self.filter).hint(&self.labels.search).show(ui);
        self.refresh(keymap, commands, revision);
        let total_conflicts = self.conflicts.len();
        if total_conflicts > 0 {
            ui.add_space(t.space.xs);
            let r =
                ui.label(egui::RichText::new(format!("{total_conflicts} commands share a shortcut")).color(t.status.danger).size(t.type_scale.small));
            automation::register(ui, &r, "keymap.conflicts", "conflicts", Role::Status);
        }
        ui.add_space(t.space.xs);
        let width = ui.available_width();
        let cols = [
            Column::new(&self.labels.command, (width * 0.45).max(120.0)),
            Column::new(&self.labels.shortcut, (width * 0.35).max(100.0)),
            Column::new("", (width * 0.2).max(90.0)),
        ];
        let platform = keymap.platform();
        let rows = &self.rows;
        let recording = self.recording.as_deref();
        let conflicts = &self.conflicts;
        let labels = &self.labels;
        let keymap_ro: &Keymap = keymap;
        let mut action: Option<(usize, Action)> = None;
        let row_h = t.control_height + 2.0;
        let label_of = |r: usize| rows.get(r).and_then(|i| commands.get(*i)).map_or("", |c| c.label.as_str());
        VirtualTable::new("keymap.table", &cols, rows.len(), RowHeights::Uniform(row_h)).row_label(&label_of).show(ui, |ui, r, col| {
            let Some(c) = rows.get(r).and_then(|i| commands.get(*i)) else { return };
            match col {
                0 => {
                    let color = if conflicts.contains_key(&c.id) { t.status.danger } else { t.text.primary };
                    text(ui, &t, ("label", &c.id), &c.label, color);
                }
                1 => {
                    let rec = recording == Some(c.id.as_str());
                    let keys = keymap_ro.keys_for(&c.id);
                    let shown = if rec {
                        labels.recording.clone()
                    } else if keys.is_empty() {
                        labels.unbound.clone()
                    } else {
                        super::keys::fit_glyphs(ui.ctx(), display_list(keys, platform), &Tokens::font(t.type_scale.small))
                    };
                    let resp = chip(ui, &t, &shown, rec);
                    let id = &c.id;
                    automation::register_fmt(
                        ui,
                        resp.rect,
                        format_args!("keymap.{id}.shortcut"),
                        &shown,
                        Role::Button,
                        true,
                        Details { value: Some(&shown), selected: Some(rec) },
                    );
                    if resp.clicked() {
                        action = Some((r, Action::Record));
                    }
                    if let Some(others) = conflicts.get(&c.id) {
                        let names: Vec<&str> = others.iter().filter_map(|o| commands.iter().find(|x| &x.id == o)).map(|x| x.label.as_str()).collect();
                        resp.on_hover_text(format!("{}: {}", labels.conflicts_with, names.join(", ")));
                    }
                }
                _ => {
                    let customized = keymap_ro.keys_for(&c.id) != keymap_ro.default_keys(&c.id);
                    let id = &c.id;
                    if customized {
                        let r_reset = link(ui, &t, &labels.reset);
                        automation::register_fmt(
                            ui,
                            r_reset.rect,
                            format_args!("keymap.{id}.reset"),
                            &labels.reset,
                            Role::Button,
                            true,
                            Details::default(),
                        );
                        if r_reset.clicked() {
                            action = Some((r, Action::Reset));
                        }
                    }
                    if !keymap_ro.keys_for(&c.id).is_empty() {
                        let r_clear = link(ui, &t, &labels.clear);
                        automation::register_fmt(
                            ui,
                            r_clear.rect,
                            format_args!("keymap.{id}.clear"),
                            &labels.clear,
                            Role::Button,
                            true,
                            Details::default(),
                        );
                        if r_clear.clicked() {
                            action = Some((r, Action::Clear));
                        }
                    }
                }
            }
        });
        if let Some((r, a)) = action
            && let Some(c) = self.rows.get(r).and_then(|i| commands.get(*i))
        {
            let id = c.id.clone();
            match a {
                Action::Record => self.recording = Some(id),
                Action::Reset => {
                    keymap.reset(&id);
                    self.edits += 1;
                    change = Some(KeymapChange::Reset { id });
                }
                Action::Clear => {
                    if keymap.bind(&id, Vec::new()) {
                        self.edits += 1;
                        change = Some(KeymapChange::Bound { id, keys: Vec::new() });
                    }
                }
            }
        }
        if self.recording.is_some() {
            // keep listening without input arriving
            crate::wake::wake_after(ui.ctx(), crate::wake::Reason::TIMER, std::time::Duration::from_millis(250));
        }
        change
    }
}

#[derive(Clone, Copy)]
enum Action {
    Record,
    Reset,
    Clear,
}

fn text(ui: &mut Ui, t: &Tokens, key: (&str, &str), s: &str, color: Color32) {
    let font = Tokens::font(t.type_scale.body);
    let g = cache::galley(ui, ("ashlar_ui::keymap", key), cache::hash_of(&(s, color, font.size.to_bits())), || {
        egui::text::LayoutJob::simple_singleline(s.to_owned(), font, color)
    });
    let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(rect.min, g, color);
}

fn chip(ui: &mut Ui, t: &Tokens, s: &str, active: bool) -> egui::Response {
    let font = Tokens::font(t.type_scale.small);
    let color = if active { t.text.on_accent } else { t.text.primary };
    let g = ui.painter().layout_no_wrap(s.to_owned(), font, color);
    let size = g.size() + vec2(2.0 * t.space.sm, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if active {
        t.accent
    } else if resp.hovered() {
        t.surface.hover
    } else {
        t.surface.sunken
    };
    ui.painter().rect_filled(rect, t.radius.sm, fill);
    ui.painter().rect_stroke(rect, t.radius.sm, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    ui.painter().galley(egui::Align2::CENTER_CENTER.align_size_within_rect(g.size(), rect).min, g, color);
    resp
}

fn link(ui: &mut Ui, t: &Tokens, s: &str) -> egui::Response {
    let r = ui.add(egui::Label::new(egui::RichText::new(s).color(t.accent).size(t.type_scale.small)).sense(Sense::click()));
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::menu::tests::info;
    use crate::test_support::click;
    use ashlar_command::shortcut::Platform;
    use egui_kittest::Harness;

    struct S {
        editor: KeymapEditor,
        keymap: Keymap,
        cmds: Vec<CommandInfo>,
        changes: Vec<KeymapChange>,
    }

    fn harness() -> Harness<'static, S> {
        let cmds = vec![
            info("file.save", "Save", &["File"], Some("Mod+S"), true),
            info("file.save_as", "Save As", &["File"], Some("Mod+Shift+S"), true),
            info("view.play", "Play", &["View"], Some("Space"), true),
            info("edit.find", "Find", &["Edit"], None, true),
        ];
        let keymap = Keymap::new(Platform::Other, cmds.iter().map(|c| (c.id.as_str(), c.shortcut.as_deref())));
        let mut h = Harness::builder().with_size(vec2(700.0, 400.0)).build_ui_state(
            |ui, s: &mut S| {
                if let Some(c) = s.editor.show(ui, &mut s.keymap, &s.cmds, 1) {
                    s.changes.push(c);
                }
            },
            S { editor: KeymapEditor::new(), keymap, cmds, changes: Vec::new() },
        );
        automation::install(&h.ctx);
        h.step();
        h.step();
        h
    }

    #[test]
    fn record_a_shortcut_shows_and_clears_conflicts() {
        let mut h = harness();
        assert!(automation::find(&h.ctx, "keymap.conflicts").is_none());
        let play = automation::find(&h.ctx, "keymap.view.play.shortcut").unwrap();
        assert_eq!(play.value, "Space");
        click(&mut h, play.center());
        assert!(h.state().editor.is_recording());
        h.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::COMMAND, egui::Key::S);
        h.step();
        h.step();
        assert!(!h.state().editor.is_recording());
        let chord = Chord::parse("Mod+S").unwrap();
        assert_eq!(h.state().changes, [KeymapChange::Bound { id: "view.play".into(), keys: vec![chord] }]);
        assert_eq!(h.state().editor.conflicts_of("view.play"), ["file.save".to_string()]);
        assert!(automation::find(&h.ctx, "keymap.conflicts").is_some());
        // reset restores the default and the conflict disappears
        let reset = automation::find(&h.ctx, "keymap.view.play.reset").unwrap();
        click(&mut h, reset.center());
        h.step();
        assert_eq!(h.state().changes.last(), Some(&KeymapChange::Reset { id: "view.play".into() }));
        assert!(h.state().editor.conflicts_of("view.play").is_empty());
        assert_eq!(h.state().keymap.keys_for("view.play"), [Chord::parse("Space").unwrap()]);
    }

    #[test]
    fn escape_cancels_recording_and_clear_unbinds() {
        let mut h = harness();
        let save = automation::find(&h.ctx, "keymap.file.save.shortcut").unwrap();
        click(&mut h, save.center());
        h.key_press(egui::Key::Escape);
        h.step();
        assert!(!h.state().editor.is_recording());
        assert!(h.state().changes.is_empty());
        let clear = automation::find(&h.ctx, "keymap.file.save.clear").unwrap();
        click(&mut h, clear.center());
        assert_eq!(h.state().changes, [KeymapChange::Bound { id: "file.save".into(), keys: vec![] }]);
        assert!(h.state().keymap.keys_for("file.save").is_empty());
    }

    #[test]
    fn filter_matches_labels_and_keys() {
        let mut h = harness();
        let search = automation::find(&h.ctx, "keymap.search").unwrap();
        click(&mut h, search.center());
        h.event(egui::Event::Text("ctrl+shift".into()));
        h.step();
        h.step();
        assert_eq!(h.state().editor.visible_ids(&h.state().cmds), ["file.save_as"]);
        h.state_mut().editor.filter = "fnd".into();
        h.step();
        assert_eq!(h.state().editor.visible_ids(&h.state().cmds), ["edit.find"]);
    }
}
